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
    /// Что накопилось с прошлого сброса: по приложению — корзины,
    /// разложенные по началу 15-минутного интервала.
    ///
    /// Именно корзины, а не сырые точки. Первая редакция копила по точке
    /// на тик и сбрасывала их как есть — по строке в секунду на каждое
    /// из полусотни приложений. Сутки работы дали 181 тысячу строк
    /// и девять мегабайт: за месяц база вылезла бы за предел в 200 МБ
    /// из раздела 8 ТЗ. Замер с живой машины, не прикидка.
    pending:
        std::collections::HashMap<String, std::collections::HashMap<i64, bamboo_store::Bucket>>,
    /// Когда сбрасывали в последний раз, по монотонным часам.
    flushed_at: u64,
    /// Где лежит база: нужна, чтобы открыть её заново после повреждения.
    path: std::path::PathBuf,
    /// Последняя беда с записью. `None` — всё пишется.
    health: Option<String>,
    /// Что пришлось сделать с базой, пока о том не рассказали человеку.
    recovery_note: Option<String>,
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
        let recovery_note = store.last_recovery().map(describe_recovery);
        Ok(History {
            store,
            pending: std::collections::HashMap::new(),
            flushed_at: now_ms,
            path,
            health: None,
            recovery_note,
        })
    }

    /// Учитывает очередной снимок.
    ///
    /// Только копит: на диск ничего не идёт до сброса. Писать каждый тик
    /// значило бы тысячи мелких записей в час — то самое изнашивание,
    /// за которое Bamboo ругает других.
    pub fn observe(&mut self, snapshot: &Snapshot) {
        // Складываем процессы одной программы. Прежде каждый процесс шёл
        // отдельной точкой в одну корзину, и корзина хранила среднее
        // по процессам: у Chrome с его полусотней процессов это двести
        // мегабайт вместо пяти гигабайт. Утечка одной вкладки тонула
        // в среднем, а в отчёте Chrome выглядел скромнее блокнота.
        #[derive(Default)]
        struct Total {
            memory: u64,
            cpu: f32,
            read: u64,
            write: u64,
            threads: u32,
        }
        let mut totals: std::collections::HashMap<&str, Total> = std::collections::HashMap::new();
        for line in &snapshot.top {
            let total = totals.entry(line.name.as_str()).or_default();
            total.memory = total.memory.saturating_add(line.memory.as_u64());
            total.cpu += line.cpu_percent.max(0.0);
            total.read = total.read.saturating_add(line.read_per_second);
            total.write = total.write.saturating_add(line.write_per_second);
            total.threads = total.threads.saturating_add(line.threads);
        }
        let mut apps: Vec<(&str, Total)> = totals.into_iter().collect();
        apps.sort_by_key(|(_, total)| core::cmp::Reverse(total.memory));

        // Начало 15-минутного интервала — то же выравнивание, что у уровня
        // L2 в хранилище. Время по стенным часам: корзины в базе живут
        // в них, а монотонные часы начинаются с запуска агента.
        let wall_ms = bamboo_core::SampleTime::wall_clock_now();
        let bucket_ms = bamboo_store::bucket_start(wall_ms, bamboo_store::L2_BUCKET_MS);

        for (name, total) in apps.into_iter().take(TOP_APPS) {
            let memory_kib = (total.memory / 1024).min(u64::from(u32::MAX)) as u32;
            let bucket = bamboo_store::Bucket {
                start_ms: bucket_ms,
                samples: 1,
                // Доля процессора в миллисекундах за тик. Тик секундный,
                // поэтому проценты и миллисекунды сходятся один к одному.
                cpu_ms: total.cpu as u64 * 10,
                read_kib: total.read / 1024,
                write_kib: total.write / 1024,
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
                    avg: total.threads,
                    min: total.threads,
                    max: total.threads,
                },
            };
            // Слияние в корзину интервала, а не накопление точек: средние
            // и пределы Bucket::merge взвешивает по числу замеров.
            let empty = bamboo_store::Bucket {
                samples: 0,
                ..bucket
            };
            let slot = self
                .pending
                .entry(name.to_string())
                .or_default()
                .entry(bucket_ms)
                .or_insert(empty);
            *slot = slot.merge(bucket);
        }
    }

    /// Кладёт подвисание на диск.
    ///
    /// Без буфера, сразу: подвисаний за сутки единицы, а не тысячи, и копить
    /// их до сброса раз в четыре часа значило бы потерять при перезапуске
    /// ровно то, ради чего они и записываются.
    pub fn record_freeze(&mut self, entry: &bamboo_store::FreezeEntry) -> Result<(), String> {
        self.write(&mut |store| store.record_freeze(entry))
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
        let batches: Vec<(String, Vec<bamboo_store::Bucket>)> = pending
            .iter()
            .map(|(name, by_interval)| {
                let mut buckets: Vec<bamboo_store::Bucket> =
                    by_interval.values().copied().collect();
                buckets.sort_by_key(|bucket| bucket.start_ms);
                (name.clone(), buckets)
            })
            .collect();

        let written = self.write(&mut |store| {
            for (name, buckets) in &batches {
                let app_id = store.app_id(name, name, now_ms as i64)?;
                store.write_buckets(Level::L2, app_id, buckets)?;
            }
            Ok(batches.len())
        });
        if let Err(error) = written {
            // Накопленное не выбрасываем: оно вернётся в копилку, и следующий
            // сброс попробует снова. Прежде неудачный сброс молча терял
            // до четырёх часов наблюдений.
            self.pending = pending;
            return Err(error);
        }

        // Сворачивание и удаление старого — там же, где запись: иначе база
        // росла бы без предела, а предел в 200 МБ задан разделом 8 ТЗ.
        //
        // Часы настенные: корзины лежат в них. Монотонные — миллисекунды
        // от запуска агента — давали «сейчас минус тридцать суток» далеко
        // в отрицательных значениях, и под чистку не попадало ничего.
        let wall_ms = bamboo_core::SampleTime::wall_clock_now();
        let _ = self.store.roll_up_to_l3(wall_ms);
        let _ = self.store.prune(wall_ms);
        let _ = self.store.prune_freezes(wall_ms);

        // Проверка на ходу, раз в сброс. Третье повреждение не задело запись
        // наблюдений — ошибок не было, и лечение по ошибке молчало неделю,
        // пока записи о подвисаниях уходили в битое дерево.
        if !self.store.is_sound() {
            let _ = self.reopen();
        }

        written
    }

    /// Выполняет запись, а на повреждение отвечает лечением и одной
    /// повторной попыткой.
    ///
    /// Написано после того, как база повредилась во время работы и четыре
    /// дня агент писал в неё, получая отказ за отказом и молча их
    /// выбрасывая. Лечение при запуске есть, но до следующего запуска
    /// могут пройти недели.
    fn write<T>(
        &mut self,
        op: &mut dyn FnMut(&mut Store) -> bamboo_store::Result<T>,
    ) -> Result<T, String> {
        let outcome = match op(&mut self.store) {
            Err(error) if bamboo_store::is_corruption(&error) => {
                self.reopen()?;
                op(&mut self.store)
            }
            other => other,
        };
        match outcome {
            Ok(value) => {
                self.health = None;
                Ok(value)
            }
            Err(error) => {
                let text = error.to_string();
                self.health = Some(text.clone());
                Err(text)
            }
        }
    }

    /// Закрывает базу и открывает заново — с лечением.
    fn reopen(&mut self) -> Result<(), String> {
        // Сначала закрыть: Windows не даёт переименовать открытый файл,
        // и отодвинуть повреждённую базу при живом соединении нельзя.
        let placeholder = Store::in_memory().map_err(|error| error.to_string())?;
        drop(core::mem::replace(&mut self.store, placeholder));
        match Store::open(&self.path) {
            Ok(store) => {
                self.recovery_note = store.last_recovery().map(describe_recovery);
                self.store = store;
                Ok(())
            }
            Err(error) => {
                let text = error.to_string();
                self.health = Some(text.clone());
                Err(text)
            }
        }
    }

    /// Последняя беда с записью. `None` — всё пишется.
    pub fn health(&self) -> Option<&str> {
        self.health.as_deref()
    }

    /// Что пришлось сделать с базой — один раз, чтобы сказать человеку.
    pub fn take_recovery_note(&mut self) -> Option<String> {
        self.recovery_note.take()
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

/// Что пришлось сделать с базой — словами человека.
fn describe_recovery(recovery: &bamboo_store::Recovery) -> String {
    match recovery {
        bamboo_store::Recovery::DroppedJournal => "База наблюдений была повреждена в журнале последних записей. Журнал убран, сама история цела: потерян только хвост, не успевший дойти до базы.".to_string(),
        bamboo_store::Recovery::Rebuilt {
            salvaged_rows,
            lost_parts: 0,
        } => format!("База наблюдений была повреждена и пересобрана. Перенесено всё: {salvaged_rows} записей."),
        bamboo_store::Recovery::Rebuilt {
            salvaged_rows,
            lost_parts,
        } => format!("База наблюдений была повреждена и пересобрана. Перенесено {salvaged_rows} записей, не прочиталось частей: {lost_parts}. Повреждённый файл оставлен рядом, чтобы было по чему разбираться."),
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
            path: std::path::PathBuf::new(),
            health: None,
            recovery_note: None,
        }
    }

    #[test]
    fn hours_of_ticks_collapse_into_quarter_hour_buckets() {
        // Ошибка, найденная замером на живой машине: сырые точки давали
        // 181 тысячу строк за сутки. Час тиков раз в секунду обязан
        // схлопнуться в считанные корзины на приложение, а не в 3600.
        let mut history = history();
        let snapshot = Snapshot {
            top: vec![line("chrome.exe", 4000)],
            ..Default::default()
        };

        for _ in 0..3600 {
            history.observe(&snapshot);
        }

        let buckets: usize = history.pending.values().map(|by| by.len()).sum();
        assert!(
            buckets <= 6,
            "час наблюдения дал {buckets} корзин — точки не слились"
        );
        // И замеры не потерялись: их число сохранено внутри корзин.
        let samples: u32 = history
            .pending
            .values()
            .flat_map(|by| by.values())
            .map(|bucket| bucket.samples)
            .sum();
        assert_eq!(samples, 3600, "слияние потеряло замеры");
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

        for _ in 0..100 {
            history.observe(&snapshot);
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
        history.observe(&Snapshot {
            top: vec![line("chrome.exe", 4000)],
            ..Default::default()
        });
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

        history.observe(&Snapshot {
            top: many,
            ..Default::default()
        });
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

        history.observe(&Snapshot {
            top: many,
            ..Default::default()
        });
        assert_eq!(history.pending_apps(), TOP_APPS);
        assert!(history.pending.contains_key("важный.exe"));
    }

    #[test]
    fn a_flush_writes_and_empties_the_buffer() {
        let mut history = history();
        history.observe(&Snapshot {
            top: vec![line("chrome.exe", 4000), line("code.exe", 2000)],
            ..Default::default()
        });

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
