//! Наполнение главного окна данными (ТЗ, раздел 14.3).
//!
//! Разделы «Обзор» и «Процессы» берут данные из общего снимка коллектора.
//! «Диск», «Питание» и «Журнал» загружаются по требованию при открытии
//! раздела: это разовые запросы, а не поток, и держать их в фоне незачем.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use bamboo_analyze::WearInput;

use crate::collector::Snapshot;

/// Где лежит журнал действий агента.
fn journal_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("Bamboo").join("journal.db")
}

/// Строка процесса для таблицы главного окна.
pub struct ProcessRow {
    pub name: String,
    pub pid: String,
    pub cpu: String,
    pub memory: String,
    pub threads: String,
    pub badge: String,
}

/// Готовит строки процессов из снимка.
pub fn process_rows(snapshot: &Snapshot) -> Vec<ProcessRow> {
    snapshot
        .top
        .iter()
        .map(|line| ProcessRow {
            name: line.name.clone(),
            pid: line.pid.to_string(),
            cpu: format!("{:.1}%", line.cpu_percent),
            memory: line.memory.to_string(),
            threads: line.threads.to_string(),
            badge: line.badge.clone(),
        })
        .collect()
}

/// Строка накопителя.
pub struct DriveRow {
    pub title: String,
    pub facts: String,
    pub verdict: String,
}

/// Загружает накопители и их здоровье. Разовый запрос при открытии раздела.
pub fn drive_rows() -> (Vec<DriveRow>, String) {
    let mut rows = Vec::new();
    let drives = bamboo_sys::enumerate_drives();

    for info in &drives {
        let health = bamboo_sys::read_smart(info);

        let (facts, verdict) = match health {
            Ok(health) => {
                let mut facts = Vec::new();
                if let Some(t) = health.temperature_c {
                    facts.push(format!("{t} °C"));
                }
                if let Some(h) = health.power_on_hours {
                    facts.push(format!("наработка {h} ч"));
                }
                if let Some(w) = health.data_written {
                    facts.push(format!("записано {w}"));
                }

                let verdict = bamboo_analyze::wear::analyze(&WearInput {
                    drive_name: &info.display_name(),
                    capacity: info.capacity,
                    health: &health,
                    daily_write: None,
                    baseline_daily_write: None,
                    media_errors_grew: false,
                    top_writers: &[],
                });
                (facts.join(", "), verdict.observation.summary)
            }
            Err(error) => (
                String::new(),
                format!("здоровье прочитать не удалось: {error}"),
            ),
        };

        rows.push(DriveRow {
            title: format!("{} — {}, {}", info.display_name(), info.bus, info.capacity),
            facts,
            verdict,
        });
    }

    let note = if drives.iter().any(|d| d.bus.name() == "SATA") {
        "SMART у SATA доступен только с правами администратора — под обычным \
         пользователем здесь будет отказ, а не оценка."
            .to_string()
    } else {
        String::new()
    };

    (rows, note)
}

/// Строка пробуждения.
pub struct WakeRow {
    pub when: String,
    pub source: String,
}

/// Загружает историю пробуждений.
pub fn wake_rows() -> (Vec<WakeRow>, String) {
    match bamboo_sys::wake_history(20) {
        Ok(events) if !events.is_empty() => {
            let rows = events
                .iter()
                .map(|event| WakeRow {
                    when: date(event.at_unix_ms),
                    source: event.source.describe(),
                })
                .collect();
            (rows, String::new())
        }
        Ok(_) => (
            Vec::new(),
            "Пробуждений из сна в журнале нет — так бывает на машинах без \
             спящего режима."
                .to_string(),
        ),
        Err(error) => (Vec::new(), format!("не удалось прочитать: {error}")),
    }
}

/// Строка журнала.
pub struct JournalRow {
    pub when: String,
    pub action: String,
    pub target: String,
    pub status: String,
}

/// Загружает журнал действий.
pub fn journal_rows() -> (Vec<JournalRow>, String) {
    let Ok(journal) = bamboo_journal::Journal::open(journal_path()) else {
        return (Vec::new(), "журнал действий недоступен".to_string());
    };

    match journal.since(0) {
        Ok(entries) if !entries.is_empty() => {
            let rows = entries
                .iter()
                .map(|entry| JournalRow {
                    when: date(entry.at_unix_ms),
                    action: entry.action.name().to_string(),
                    target: entry.target.describe(),
                    status: entry.status.as_str().to_string(),
                })
                .collect();
            (rows, String::new())
        }
        Ok(_) => (
            Vec::new(),
            "Записей нет — Bamboo пока ничего не менял в системе.".to_string(),
        ),
        Err(_) => (Vec::new(), "журнал не читается".to_string()),
    }
}

/// Краткая сводка для карточек «Обзор».
pub struct Overview {
    pub cpu: String,
    pub memory: String,
    pub processes: String,
}

pub fn overview(snapshot: &Snapshot) -> Overview {
    Overview {
        cpu: format!("{:.0}%", snapshot.cpu_busy * 100.0),
        memory: format!("{} из {}", snapshot.memory_used, snapshot.memory_total),
        processes: snapshot.process_count.to_string(),
    }
}

/// Дата и время из миллисекунд эпохи Unix, в UTC.
fn date(unix_ms: i64) -> String {
    let total_seconds = unix_ms.div_euclid(1000);
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
    )
}

/// Календарная дата из числа дней от эпохи (алгоритм Хиннанта).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (mp + if mp < 10 { 3 } else { -9 }) as u32;
    (year + if month <= 2 { 1 } else { 0 }, month, day)
}
