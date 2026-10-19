//! Дневник автоматики: что она решила, сделала и почему отказалась.
//!
//! Написано после того, как автоматика десять дней не сделала ничего,
//! и узнать это было неоткуда. Отказ политики, неудача действия, падение
//! кода в цикле — всё уходило в никуда: у программы с окном нет консоли,
//! а `None` из «применить» не говорил, почему. Теперь каждое такое событие —
//! строка в текстовом файле рядом с базой, который можно открыть блокнотом,
//! а последнее видно в окне.

#![forbid(unsafe_code)]

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Последнее событие: когда и что.
static LAST: Mutex<Option<(std::time::Instant, String)>> = Mutex::new(None);

/// Дальше этого размера дневник переносится в старый файл: следить надо
/// за свежим, а не хранить годы.
const LIMIT: u64 = 512 * 1024;

fn path() -> PathBuf {
    let base = std::env::var("BAMBOO_DATA_DIR")
        .or_else(|_| std::env::var("LOCALAPPDATA").map(|dir| format!("{dir}\\Bamboo")))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("автоматика.log")
}

/// Записывает событие. Ошибки записи молча проглатываются: дневник
/// не должен ронять то, о чём он пишет.
pub fn note(text: &str) {
    let path = path();
    if std::fs::metadata(&path)
        .map(|meta| meta.len() > LIMIT)
        .unwrap_or(false)
    {
        let _ = std::fs::rename(&path, path.with_extension("old.log"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{} {text}", bamboo_sys::clock::local_stamp());
    }
    if let Ok(mut last) = LAST.lock() {
        *last = Some((std::time::Instant::now(), text.to_string()));
    }
}

/// Последнее событие и сколько назад оно было.
pub fn last() -> Option<(std::time::Duration, String)> {
    LAST.lock()
        .ok()?
        .as_ref()
        .map(|(at, text)| (at.elapsed(), text.clone()))
}

/// Пишет в дневник любое падение кода — в любом потоке, — прежде чем
/// его обработает стандартный обработчик.
///
/// Падение в цикле окна прежде оставалось невидимым целиком: всё, что
/// шло в тике после места падения, просто переставало работать.
pub fn catch_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        note(&format!(
            "СБОЙ в потоке «{}»: {info}",
            thread.name().unwrap_or("без имени")
        ));
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_is_written_and_remembered() {
        let dir = std::env::temp_dir().join(format!("bamboo-diary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("BAMBOO_DATA_DIR", &dir);
        note("защита: проверка");
        let text = std::fs::read_to_string(dir.join("автоматика.log")).unwrap();
        assert!(text.contains("защита: проверка"), "{text}");
        let (ago, last) = last().expect("последнее событие");
        assert_eq!(last, "защита: проверка");
        assert!(ago.as_secs() < 5);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
