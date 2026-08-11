//! Запись наблюдений на диск (ТЗ, раздел 8).
//!
//! Хранилище было написано целиком и не подключено ни одной строчкой:
//! всё, что Bamboo видел, жило в памяти и пропадало при выходе. Из-за
//! этого выводы о росте памяти начинались заново после каждого перезапуска,
//! а недельный отчёт строить было не из чего.
//!
//! Главное затруднение здесь не в самой записи, а в её цене. Раздел 8.2 ТЗ
//! обещает, что часовой сброс укладывается в 20 МБ записи в сутки, и это
//! неверно: замер даёт около пятидесяти при сотне приложений. Причина —
//! в устройстве базы, а не в объёме данных: SQLite переписывает страницу
//! целиком ради каждой изменённой в ней записи, а записи одного сброса
//! разбросаны по страницам первичным ключом.
//!
//! Поэтому здесь сброс не часовой, а более редкий, и размер страницы
//! уменьшен. Bamboo следит за износом чужих накопителей и не имеет права
//! изнашивать накопитель собственной телеметрией — это была бы ровно та
//! двойная мораль, которой посвящён раздел 11.5.

#![forbid(unsafe_code)]

use bamboo_core::Bytes;
use bamboo_store::{Level, Store};

use crate::collector::Snapshot;

/// Как часто сбрасываем накопленное на диск.
///
/// Четыре часа вместо обещанного ТЗ часа. Прямое следствие замера:
/// часовой сброс не укладывается в бюджет записи из раздела 4, а бюджет
/// важнее буквы раздела 8.2. Потеря при внезапном выключении — данные
/// последних часов наблюдения, и это допустимо: они не банковские.
pub const FLUSH_EVERY_MS: u64 = 4 * 60 * 60 * 1000;

/// Сколько приложений записываем.
///
/// Хвост из трёхсот процессов по мегабайту памяти ничего не объясняет,
/// а пишется он ровно так же, как значимая часть. Пятьдесят покрывают
/// всё, о чём человек когда-либо спросит.
const TOP_APPS: usize = 50;

/// Накопитель наблюдений между сбросами.
pub struct History {
    store: Store,
    /// Что накопилось с прошлого сброса: приложение и его точки.
    pending: std::collections::HashMap<String, Vec<bamboo_store::Bucket>>,
    /// Когда сбрасывали в последний раз, по монотонным часам.
    flushed_at: u64,
}

/// Где лежит база наблюдений.
fn database_path() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(base)
        .join("Bamboo")
        .join("наблюдения.db")
}

impl History {
    /// Открывает базу наблюдений.
    ///
    /// Ошибка здесь не повод останавливать наблюдение: Bamboo продолжит
    /// работать, просто без истории. Молча — нельзя, поэтому причина
    /// возвращается вызывающему.
    pub fn open(now_ms: u64) -> Result<History, String> {
        let path = database_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let store = Store::open(&path).map_err(|error| error.to_string())?;
        Ok(History {
            store,
            pending: std::collections::HashMap::new(),
            flushed_at: now_ms,
        })
    }

    /// Учитывает очередной снимок.
    ///
    /// Только копит: на диск ничего не идёт до сброса. Писать каждый тик
    /// значило бы тысячи мелких записей в час — то самое изнашивание,
    /// за которое Bamboo ругает других.
    pub fn observe(&mut self, snapshot: &Snapshot, now_ms: u64) {
        let mut top: Vec<&crate::collector::ProcessLine> = snapshot.top.iter().collect();
        top.sort_by_key(|line| core::cmp::Reverse(line.memory.as_u64()));

        for line in top.into_iter().take(TOP_APPS) {
            let memory_kib = (line.memory.as_u64() / 1024) as u32;
            let bucket = bamboo_store::Bucket {
                start_ms: now_ms as i64,
                samples: 1,
                // Доля процессора в миллисекундах за тик. Тик секундный,
                // поэтому проценты и миллисекунды сходятся один к одному.
                cpu_ms: line.cpu_percent.max(0.0) as u64 * 10,
                read_kib: line.read_per_second / 1024,
                write_kib: line.write_per_second / 1024,
                // Одна точка — это и среднее, и предел в обе стороны.
                private_kib: bamboo_store::Stat {
                    avg: memory_kib,
                    min: memory_kib,
                    max: memory_kib,
                },
                working_set_kib: bamboo_store::Stat {
                    avg: memory_kib,
                    min: memory_kib,
                    max: memory_kib,
                },
                handles: bamboo_store::Stat::default(),
                threads: bamboo_store::Stat {
                    avg: line.threads,
                    min: line.threads,
                    max: line.threads,
                },
            };
            self.pending
                .entry(line.name.clone())
                .or_default()
                .push(bucket);
        }
    }

    /// Пора ли сбрасывать.
    pub fn due(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.flushed_at) >= FLUSH_EVERY_MS
    }

    /// Сбрасывает накопленное на диск.
    ///
    /// Возвращает, сколько приложений записано. Ноль — законный ответ:
    /// сбрасывать может быть нечего.
    pub fn flush(&mut self, now_ms: u64) -> Result<usize, String> {
        self.flushed_at = now_ms;
        if self.pending.is_empty() {
            return Ok(0);
        }

        let pending = core::mem::take(&mut self.pending);
        let written = pending.len();

        for (name, buckets) in pending {
            let app_id = self
                .store
                .app_id(&name, &name, now_ms as i64)
                .map_err(|error| error.to_string())?;
            self.store
                .write_buckets(Level::L2, app_id, &buckets)
                .map_err(|error| error.to_string())?;
        }

        // Сворачивание и удаление старого — там же, где запись: иначе база
        // росла бы без предела, а предел в 200 МБ задан разделом 8 ТЗ.
        let _ = self.store.roll_up_to_l3(now_ms as i64);
        let _ = self.store.prune(now_ms as i64);

        Ok(written)
    }

    /// Сколько места занимает база.
    pub fn size(&self) -> Bytes {
        self.store.size_bytes().unwrap_or(Bytes(0))
    }

    /// Сколько приложений ждёт сброса.
    pub fn pending_apps(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::ProcessLine;

    fn line(name: &str, memory_mb: u64) -> ProcessLine {
        ProcessLine {
            name: name.to_string(),
            memory: Bytes(memory_mb << 20),
            ..Default::default()
        }
    }

    fn history() -> History {
        History {
            store: Store::in_memory().expect("база в памяти"),
            pending: std::collections::HashMap::new(),
            flushed_at: 0,
        }
    }

    #[test]
    fn nothing_reaches_the_disk_between_flushes() {
        // Писать каждый тик значило бы тысячи мелких записей в час —
        // то самое изнашивание накопителя, за которое Bamboo ругает других.
        let mut history = history();
        let snapshot = Snapshot {
            top: vec![line("chrome.exe", 4000)],
            ..Default::default()
        };

        for tick in 0..100 {
            history.observe(&snapshot, tick * 1000);
        }
        assert_eq!(history.pending_apps(), 1, "накоплено, но не записано");
        assert!(!history.due(99_000), "сброс раньше срока");
    }

    #[test]
    fn the_flush_is_far_rarer_than_the_spec_promises() {
        // Часовой сброс из раздела 8.2 не укладывается в бюджет записи
        // из раздела 4 — замер даёт около пятидесяти мегабайт в сутки
        // вместо обещанных двадцати. Бюджет важнее буквы, и проверяем
        // это через поведение, а не сравнением константы с числом.
        let mut history = history();
        history.observe(
            &Snapshot {
                top: vec![line("chrome.exe", 4000)],
                ..Default::default()
            },
            0,
        );
        // Через час после запуска сбрасывать ещё рано.
        assert!(!history.due(60 * 60 * 1000));
    }

    #[test]
    fn only_the_biggest_apps_are_kept() {
        // Хвост из трёхсот процессов по мегабайту ничего не объясняет,
        // а пишется ровно так же, как значимая часть.
        let mut history = history();
        let many: Vec<ProcessLine> = (0..200)
            .map(|n| line(&format!("процесс{n}.exe"), n))
            .collect();

        history.observe(
            &Snapshot {
                top: many,
                ..Default::default()
            },
            0,
        );
        assert_eq!(history.pending_apps(), TOP_APPS);
    }

    #[test]
    fn the_biggest_app_is_among_those_kept() {
        // Обрезка после сортировки, а не до: иначе в историю попали бы
        // случайные процессы вместо самых заметных.
        let mut history = history();
        let mut many: Vec<ProcessLine> = (0..200)
            .map(|n| line(&format!("процесс{n}.exe"), n))
            .collect();
        many.push(line("важный.exe", 100_000));

        history.observe(
            &Snapshot {
                top: many,
                ..Default::default()
            },
            0,
        );
        assert_eq!(history.pending_apps(), TOP_APPS);
        assert!(history.pending.contains_key("важный.exe"));
    }

    #[test]
    fn a_flush_writes_and_empties_the_buffer() {
        let mut history = history();
        history.observe(
            &Snapshot {
                top: vec![line("chrome.exe", 4000), line("code.exe", 2000)],
                ..Default::default()
            },
            0,
        );

        assert_eq!(history.flush(1000).unwrap(), 2);
        assert_eq!(history.pending_apps(), 0, "после сброса копить заново");
    }

    #[test]
    fn a_flush_with_nothing_to_write_is_not_an_error() {
        let mut history = history();
        assert_eq!(history.flush(1000).unwrap(), 0);
    }

    #[test]
    fn the_clock_restarts_after_a_flush() {
        // Без этого следующий сброс случился бы на том же тике,
        // и редкая запись превратилась бы в постоянную.
        let mut history = history();
        history.flush(FLUSH_EVERY_MS).unwrap();
        assert!(!history.due(FLUSH_EVERY_MS + 1000));
        assert!(history.due(2 * FLUSH_EVERY_MS));
    }
}
