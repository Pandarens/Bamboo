//! Вывод в терминал.

use std::time::Duration;

use bamboo_analyze::WearInput;
use bamboo_collect::{ProcessTable, Tick};
use bamboo_core::process::IO_COUNTERS_NOTE;
use bamboo_core::{Bytes, DriveInfo, Result, SmartHealth};
use bamboo_sys::OwnMemory;

/// Ширина колонки с именем процесса.
const NAME_WIDTH: usize = 28;

pub fn system(tick: &Tick) {
    let memory = &tick.system.memory;
    let busy = tick.cpu_busy();
    let driver = tick.driver_time();

    println!(
        "Процессор {:>5.1}%   Память {} из {}   Коммит {:>5.1}%   Процессов {}",
        busy * 100.0,
        memory.physical_used(),
        memory.physical_total,
        memory.commit_pressure() * 100.0,
        tick.process_count,
    );

    // Условие из раздела 9.3 ТЗ: заметное время в DPC и прерываниях означает,
    // что искать виновника среди процессов бесполезно.
    if driver > 0.10 && busy > 0.15 {
        println!(
            "  Внимание: {:.1}% времени ушло в DPC и прерывания. Это уровень драйверов,\n\
             \x20 среди процессов виновника искать бесполезно.",
            driver * 100.0
        );
    }

    println!(
        "  Режим опроса: {:?}, интервал {} мс",
        tick.cadence, tick.interval_ms
    );
}

pub fn top_cpu(table: &ProcessTable, limit: usize) {
    println!(
        "{:<NAME_WIDTH$} {:>7} {:>6} {:>10} {:>7}",
        "Процесс", "PID", "CPU", "Память", "Потоки"
    );

    for process in table.top_by_cpu(limit) {
        println!(
            "{:<NAME_WIDTH$} {:>7} {:>5.1}% {:>10} {:>7}",
            clip(&process.image_name),
            process.pid(),
            process.cpu_share * 100.0,
            Bytes::from_kib(process.last_point().private_kib as u64).to_string(),
            process.threads,
        );
    }
}

pub fn top_memory(table: &ProcessTable, limit: usize) {
    println!("Больше всего памяти:");
    for process in table.top_by_memory(limit) {
        println!(
            "  {:<NAME_WIDTH$} {:>10}  (рабочий набор {})",
            clip(&process.image_name),
            Bytes::from_kib(process.last_point().private_kib as u64).to_string(),
            Bytes::from_kib(process.last_point().working_set_private_kib as u64),
        );
    }
}

pub fn top_write(table: &ProcessTable, limit: usize) {
    let top = table.top_by_write(limit);
    let anything = top.iter().any(|p| p.last_point().write_kib > 0);

    println!("Больше всего записи за интервал:");
    if !anything {
        println!("  за интервал никто ничего заметного не записал");
    } else {
        for process in top.iter().filter(|p| p.last_point().write_kib > 0) {
            println!(
                "  {:<NAME_WIDTH$} {:>10}",
                clip(&process.image_name),
                Bytes::from_kib(process.last_point().write_kib as u64),
            );
        }
    }
    println!();
    wrap(IO_COUNTERS_NOTE, 78, "  ");
}

pub fn changes(tick: &Tick) {
    let started = &tick.changes.started;
    let exited = &tick.changes.exited;
    if started.is_empty() && exited.is_empty() {
        return;
    }

    let names: Vec<String> = exited.iter().map(|(_, name)| name.to_string()).collect();
    println!(
        "  Запущено: {}   Завершилось: {}{}",
        started.len(),
        exited.len(),
        if names.is_empty() {
            String::new()
        } else {
            format!(" ({})", names.join(", "))
        }
    );
}

pub fn drive(info: &DriveInfo, health: Result<SmartHealth>) {
    println!(
        "{} — {}, {}, прошивка {}",
        info.display_name(),
        info.bus,
        info.capacity,
        info.firmware
    );

    let health = match health {
        Ok(health) => health,
        Err(error) => {
            // Ни оценок, ни «вероятно всё хорошо»: если данных нет,
            // говорим почему и на этом останавливаемся.
            println!("  Здоровье прочитать не удалось: {error}");
            return;
        }
    };

    let mut facts: Vec<String> = Vec::new();
    if let Some(temperature) = health.temperature_c {
        facts.push(format!("{temperature} °C"));
    }
    if let Some(hours) = health.power_on_hours {
        facts.push(format!("наработка {hours} ч"));
    }
    if let Some(cycles) = health.power_cycles {
        facts.push(format!("включений {cycles}"));
    }
    if let Some(shutdowns) = health.unsafe_shutdowns {
        facts.push(format!("некорректных выключений {shutdowns}"));
    }
    if !facts.is_empty() {
        println!("  {}", facts.join(", "));
    }

    if let Some(written) = health.data_written {
        let read = health
            .data_read
            .map(|r| format!(", прочитано {r}"))
            .unwrap_or_default();
        println!("  Записано за всё время: {written}{read}");
    }

    // Истории записи пока нет — уровни L2 и L3 появятся в bamboo-store,
    // поэтому суточный темп и базовую линию передать нечем. Анализатор
    // это переживает: он просто не строит проекцию.
    let verdict = bamboo_analyze::wear::analyze(&WearInput {
        drive_name: &info.display_name(),
        capacity: info.capacity,
        health: &health,
        daily_write: None,
        baseline_daily_write: None,
        media_errors_grew: false,
        top_writers: &[],
    });

    println!();
    wrap(&verdict.observation.summary, 78, "  ");
    if let Some(detail) = &verdict.observation.detail {
        wrap(detail, 78, "  ");
    }
    println!(
        "  Паспортный ресурс: {}{}",
        verdict.rating.total,
        if verdict.rating.is_estimate {
            " (оценка)"
        } else {
            ""
        }
    );
    println!("  Суточный темп записи станет известен, когда накопится история наблюдений.");
}

pub fn budget_header(duration: Duration) {
    println!(
        "Измеряю собственное потребление {} с. Бюджет из раздела 4 ТЗ:\n\
         рабочий набор не больше 15 МБ, приватные байты не больше 12 МБ.\n",
        duration.as_secs()
    );
}

pub fn budget_report(peak: OwnMemory, ticks: u32, elapsed: Duration, table: &ProcessTable) {
    const WORKING_SET_LIMIT: Bytes = Bytes::from_mib(15);
    const PRIVATE_LIMIT: Bytes = Bytes::from_mib(12);

    println!(
        "Тиков: {ticks} за {:.0} с, процессов в таблице: {}",
        elapsed.as_secs_f64(),
        table.len()
    );
    println!(
        "История в памяти: {}",
        Bytes(table.estimated_memory_bytes() as u64)
    );
    println!();
    verdict("Рабочий набор", peak.working_set, WORKING_SET_LIMIT);
    verdict("Приватные байты", peak.private_bytes, PRIVATE_LIMIT);
    println!();
    if cfg!(debug_assertions) {
        println!(
            "Это отладочная сборка: она тащит отладочную информацию и собрана\n\
             без оптимизаций, так что цифры завышены. Для проверки бюджета\n\
             нужен релиз."
        );
    } else {
        println!(
            "Короткий прогон показывает только пиковую память. Бюджет по CPU\n\
             и записи на диск проверяется суточным прогоном — см. раздел 4 ТЗ."
        );
    }
}

fn verdict(label: &str, value: Bytes, limit: Bytes) {
    let status = if value <= limit {
        "в бюджете"
    } else {
        "ПРЕВЫШЕНИЕ"
    };
    println!(
        "{label:<18} {:>10}  лимит {:>9}  {status}",
        value.to_string(),
        limit.to_string()
    );
}

/// Обрезает имя по ширине колонки. Считаем в символах, а не в байтах:
/// имена бывают и не латиницей.
fn clip(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= NAME_WIDTH {
        return name.to_string();
    }
    chars[..NAME_WIDTH - 1].iter().collect::<String>() + "…"
}

/// Переносит длинный текст по словам.
fn wrap(text: &str, width: usize, indent: &str) {
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + word.chars().count() + 1 > width {
            println!("{indent}{line}");
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        println!("{indent}{line}");
    }
}
