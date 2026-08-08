//! Запись сбросов видеорегистратора на диск (ТЗ, раздел 7.4).
//!
//! Кольцевой буфер живёт в памяти и пишется на диск только когда сработал
//! триггер — тогда получается ретроспектива за минуты до события. Формат
//! текстовый и читается глазами: сброс — это диагностический артефакт для
//! человека, разбирающего ночной фриз, а не бинарь для машины. По той же
//! причине время событий показываем как «за сколько до срабатывания», а не
//! абсолютной меткой: важна близость к триггеру.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::event::{friendly_image_name, ProcessEvent};
use crate::recorder::Dump;

/// Пишет сброс в каталог `dir`, создавая его при необходимости.
///
/// Имя файла — по времени срабатывания, чтобы сбросы не затирали друг друга
/// и сортировались по времени сами собой.
pub fn write_dump(dump: &Dump, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("bamboo-dump-{}.txt", dump.at_unix_ms));
    let mut file = std::fs::File::create(&path)?;
    file.write_all(render(dump).as_bytes())?;
    Ok(path)
}

/// Собирает текст сброса. Вынесено отдельно от записи, чтобы проверять
/// формат тестами, не трогая файловую систему.
pub fn render(dump: &Dump) -> String {
    let mut out = String::new();
    out.push_str("Сброс видеорегистратора Bamboo\n");
    out.push_str(&format!("Причина: {}\n", dump.reason.describe()));
    out.push_str(&format!("Событий в буфере: {}\n", dump.events.len()));
    out.push_str(
        "Это ретроспектива за минуты до срабатывания. Время — насколько\n\
         событие раньше триггера.\n\n",
    );

    for event in &dump.events {
        let before_ms = dump.at_unix_ms.saturating_sub(event.at_unix_ms());
        out.push_str(&format!(
            "{:>8}  {}\n",
            ago(before_ms),
            describe_event(event)
        ));
    }

    out
}

/// «За сколько до срабатывания» в удобочитаемом виде.
fn ago(before_ms: i64) -> String {
    if before_ms < 0 {
        // Событие позже сброса быть не должно, но если часы дрогнули —
        // показываем ноль, а не отрицательное время.
        return "0.0с".to_string();
    }
    let secs = before_ms as f64 / 1000.0;
    if secs < 60.0 {
        format!("-{secs:.1}с")
    } else {
        format!("-{:.1}м", secs / 60.0)
    }
}

/// Одна строка про событие.
fn describe_event(event: &ProcessEvent) -> String {
    match event {
        ProcessEvent::Started {
            pid,
            parent_pid,
            image_name,
            session_id,
            ..
        } => format!(
            "запуск  pid={pid} родитель={parent_pid} сессия={session_id} {}",
            friendly_image_name(image_name)
        ),
        ProcessEvent::Stopped { pid, exit_code, .. } => {
            let code = if *exit_code == 0 {
                "штатно".to_string()
            } else {
                format!("код {exit_code}")
            };
            format!("выход   pid={pid} {code}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::TriggerReason;

    fn started(at: i64, pid: u32, name: &str) -> ProcessEvent {
        ProcessEvent::Started {
            at_unix_ms: at,
            pid,
            parent_pid: 4,
            image_name: name.to_string(),
            session_id: 1,
        }
    }

    fn dump() -> Dump {
        Dump {
            at_unix_ms: 600_000,
            reason: TriggerReason::CpuWhileIdle { percent: 40 },
            events: vec![
                started(
                    540_000,
                    100,
                    "\\Device\\HarddiskVolume3\\Windows\\System32\\svchost.exe",
                ),
                ProcessEvent::Stopped {
                    at_unix_ms: 599_000,
                    pid: 100,
                    exit_code: 1,
                },
            ],
        }
    }

    #[test]
    fn the_header_names_the_reason_and_count() {
        let text = render(&dump());
        assert!(text.contains("Причина: процессор был занят на 40%"));
        assert!(text.contains("Событий в буфере: 2"));
    }

    #[test]
    fn events_show_time_before_the_trigger() {
        let text = render(&dump());
        // 600000 - 540000 = 60000 мс = ровно минута до срабатывания.
        assert!(
            text.contains("-1.0м"),
            "нет отметки времени до триггера:\n{text}"
        );
        // 600000 - 599000 = 1000 мс.
        assert!(text.contains("-1.0с"));
    }

    #[test]
    fn device_paths_are_shortened_to_the_image_name() {
        let text = render(&dump());
        assert!(text.contains("svchost.exe"));
        assert!(
            !text.contains("HarddiskVolume3"),
            "путь устройства не свёрнут до имени"
        );
    }

    #[test]
    fn a_nonzero_exit_code_is_shown() {
        let text = render(&dump());
        assert!(text.contains("выход   pid=100 код 1"));
    }

    #[test]
    fn writing_creates_a_file_named_by_time() {
        let dir = std::env::temp_dir().join(format!("bamboo-dump-test-{}", std::process::id()));
        let path = write_dump(&dump(), &dir).expect("сброс не записался");
        assert!(path.exists());
        assert!(path.to_string_lossy().contains("bamboo-dump-600000"));

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("Сброс видеорегистратора"));

        // Прибираемся, чтобы тест не оставлял мусор.
        let _ = std::fs::remove_dir_all(&dir);
    }
}
