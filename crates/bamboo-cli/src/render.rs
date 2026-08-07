//! Вывод в терминал.

use std::time::Duration;

use bamboo_analyze::{BootPoint, WearInput};
use bamboo_collect::{ProcessTable, Tick};
use bamboo_core::process::IO_COUNTERS_NOTE;
use bamboo_core::{Bytes, DriveInfo, Result, SmartHealth};
use bamboo_sys::{BootCulprit, BootRecord, OwnMemory, WakeEvent};

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

pub fn boot(history: &[BootRecord], culprits: &[BootCulprit]) {
    if history.is_empty() {
        println!("Windows пока не записала ни одной загрузки в журнал диагностики.");
        return;
    }

    println!("Последние загрузки:");
    for record in history.iter().take(10) {
        let degradation = record
            .degradation_ms
            .map(|ms| format!("  (+{:.1} с к обычному)", ms as f64 / 1000.0))
            .unwrap_or_default();
        println!(
            "  {}   {:>6.1} с   до рабочего стола {:>5.1} с{degradation}",
            date(record.at_unix_ms),
            record.total_ms as f64 / 1000.0,
            record.main_path_ms as f64 / 1000.0,
        );
    }

    let points: Vec<BootPoint> = history
        .iter()
        .map(|record| BootPoint {
            at_unix_ms: record.at_unix_ms,
            total_ms: record.total_ms,
        })
        .collect();

    // Виновников берём только за последние загрузки: то, что тормозило
    // полгода назад, к сегодняшней регрессии отношения не имеет.
    let recent_after = points.get(4).map(|p| p.at_unix_ms).unwrap_or(0);
    let named: Vec<(String, u64)> = culprits
        .iter()
        .filter(|culprit| culprit.at_unix_ms >= recent_after)
        .map(|culprit| {
            (
                format!("{} ({})", culprit.name, culprit.kind.name()),
                culprit.degradation_ms.unwrap_or(culprit.total_ms),
            )
        })
        .collect();

    let verdict = bamboo_analyze::boot::analyze(&points, &named);

    println!();
    match verdict.observation {
        Some(observation) => {
            wrap(&observation.summary, 78, "  ");
            if let Some(detail) = &observation.detail {
                wrap(detail, 78, "  ");
            }
        }
        None => {
            println!(
                "  Загрузок в истории пока мало — сравнивать не с чем.\n\
                 \x20 Вывод о регрессии появится, когда наберётся хотя бы десяток."
            );
        }
    }
}

pub fn wakeups(events: &[WakeEvent]) {
    if events.is_empty() {
        println!(
            "Пробуждений из сна в журнале нет.\n\
             Так бывает на машинах, которые не уходят в сон, — например, \
             на стационарных с отключённым спящим режимом."
        );
        return;
    }

    println!("Последние пробуждения:");
    for event in events.iter().take(15) {
        let slept = event
            .sleep_duration_ms()
            .map(|ms| format!(", проспала {:.1} ч", ms as f64 / 3_600_000.0))
            .unwrap_or_default();
        println!(
            "  {}   {}{slept}",
            date(event.at_unix_ms),
            event.source.describe()
        );
    }

    let actionable = events.iter().filter(|e| e.source.is_actionable()).count();
    println!();
    if actionable == 0 {
        println!("  Ни одно из этих пробуждений не выглядит лишним.");
    } else {
        println!(
            "  Из них {actionable} вызваны таймером или сетевым адаптером —\n\
             \x20 такие источники отключаются обратимо. Отключение появится\n\
             \x20 вместе с брокером на этапе 0.4."
        );
    }
}

/// Дата и время из миллисекунд эпохи Unix, в UTC.
///
/// Перевод в местное время требует работы с часовыми поясами; до появления
/// интерфейса это лишняя сложность, поэтому честно помечаем зону.
fn date(unix_ms: i64) -> String {
    let total_seconds = unix_ms.div_euclid(1000);
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);

    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
    )
}

/// Обратная к `days_from_civil` из bamboo-core.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = (month_prime + if month_prime < 10 { 3 } else { -9 }) as u32;
    (year + if month <= 2 { 1 } else { 0 }, month, day)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip_against_the_parser() {
        // Разбор и вывод должны быть строго обратными друг другу.
        for text in [
            "1970-01-01T00:00:00Z",
            "2000-02-29T23:59:00Z",
            "2024-12-31T12:34:00Z",
            "2026-08-07T09:12:00Z",
        ] {
            let ms = bamboo_core::time::parse_iso8601_utc_ms(text).unwrap();
            let shown = date(ms);
            let expected = format!("{} {} UTC", &text[..10], &text[11..16]);
            assert_eq!(shown, expected);
        }
    }

    #[test]
    fn dates_before_the_epoch_do_not_break() {
        // На машине со сбитыми часами отметка может уехать за 1970 год.
        assert_eq!(date(-86_400_000), "1969-12-31 00:00 UTC");
    }

    #[test]
    fn long_names_are_clipped_by_characters() {
        let long = "очень-длинное-имя-процесса-которое-точно-не-влезет.exe";
        let clipped = clip(long);
        assert_eq!(clipped.chars().count(), NAME_WIDTH);
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn short_names_are_left_alone() {
        assert_eq!(clip("chrome.exe"), "chrome.exe");
    }
}
