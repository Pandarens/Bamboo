//! База данных: открытие, запись, чтение, обслуживание.

use std::path::Path;

use bamboo_core::Bytes;
use rusqlite::{params, Connection, OptionalExtension};

use crate::aggregate::{roll_up, Bucket, Stat};
use crate::schema::{
    L2_BUCKET_MS, L2_RETENTION_MS, L3_BUCKET_MS, L3_RETENTION_MS, SCHEMA_VERSION, V1,
};

pub type Result<T> = rusqlite::Result<T>;

/// Уровень хранения.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// 15 минут, 30 суток.
    L2,
    /// Час, 12 месяцев.
    L3,
}

impl Level {
    fn table(self) -> &'static str {
        match self {
            Level::L2 => "samples_l2",
            Level::L3 => "samples_l3",
        }
    }

    pub fn bucket_ms(self) -> i64 {
        match self {
            Level::L2 => L2_BUCKET_MS,
            Level::L3 => L3_BUCKET_MS,
        }
    }

    pub fn retention_ms(self) -> i64 {
        match self {
            Level::L2 => L2_RETENTION_MS,
            Level::L3 => L3_RETENTION_MS,
        }
    }
}

/// Снимок здоровья накопителя на момент времени.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SmartSnapshot {
    pub at_unix_ms: i64,
    pub data_written: Option<Bytes>,
    pub data_read: Option<Bytes>,
    pub percentage_used: Option<u8>,
    pub available_spare: Option<u8>,
    pub media_errors: Option<u64>,
    pub temperature_c: Option<i16>,
    pub power_on_hours: Option<u64>,
    pub critical_warning: Option<u8>,
}

/// Запись о загрузке системы.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootEntry {
    pub at_unix_ms: i64,
    pub total_ms: u64,
    pub main_path_ms: u64,
    pub post_boot_ms: u64,
    pub degradation_ms: Option<u64>,
}

pub struct Store {
    conn: Connection,
}

/// Размер страницы базы, байт.
///
/// Меньше принятой по умолчанию четвёрки килобайт. SQLite переписывает
/// страницу целиком ради любой изменённой в ней записи, а записи одного
/// сброса разбросаны по страницам первичным ключом. На маленькой странице
/// впустую переписывается меньше — а это прямо тот расход, который
/// ограничивает раздел 4 ТЗ.
const PAGE_SIZE: i64 = 1024;

/// Похожа ли ошибка на повреждение файла.
///
/// Именно повреждение, а не любая беда: занятую базу надо подождать,
/// а не выбрасывать, и нехватку прав тоже. Отодвинуть файл в сторону —
/// действие необратимое, и на сомнительном признаке его делать нельзя.
fn looks_corrupt(error: &rusqlite::Error) -> bool {
    use rusqlite::ffi::ErrorCode;
    match error {
        rusqlite::Error::SqliteFailure(code, _) => matches!(
            code.code,
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
        ),
        _ => false,
    }
}

/// Убирает журнал WAL и разделяемый индекс.
///
/// Теряется то, что записано после последней контрольной точки, — у нас
/// это хвост наблюдений, не больше одного сброса. База при этом остаётся
/// в состоянии последней контрольной точки, то есть целой и связной.
///
/// Молча: сказать об этом всё равно некому и нечего — данных на диске
/// ровно столько, сколько успело дойти до базы.
fn drop_wal(path: &Path) {
    for tail in ["-wal", "-shm"] {
        let mut companion = path.as_os_str().to_os_string();
        companion.push(tail);
        let _ = std::fs::remove_file(std::path::PathBuf::from(companion));
    }
}

/// Отодвигает повреждённую базу вместе с её журналом.
///
/// Файл не удаляется, а переименовывается: повреждение — это повод
/// разобраться, а по удалённому файлу разбираться не по чему. Хранится
/// одна копия, прежняя затирается — иначе повторные поломки съели бы диск,
/// который раздел 4 ТЗ ограничивает двумястами мегабайтами.
///
/// Журнал WAL и разделяемый индекс уносятся тоже. Оставить их — значит
/// дать SQLite наложить старый журнал на новый пустой файл и получить
/// вторую повреждённую базу вместо чистой.
fn quarantine(path: &Path) -> Result<()> {
    let broken = path.with_extension("повреждена.db");
    let _ = std::fs::remove_file(&broken);
    if let Err(error) = std::fs::rename(path, &broken) {
        // Переименовать не вышло — файл держат. Тогда честнее вернуть
        // ошибку, чем работать поверх повреждённого.
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some(format!("повреждённую базу не удалось отодвинуть: {error}")),
        ));
    }
    drop_wal(path);
    Ok(())
}

impl Store {
    /// Открывает или создаёт базу.
    ///
    /// Повреждённая база не роняет Bamboo и не остаётся молча лежать.
    /// Сначала убирается журнал WAL — этого хватает почти всегда, и тогда
    /// история остаётся при себе; и лишь если база повреждена сама, она
    /// отодвигается в сторону, а наблюдения начинают копиться заново.
    ///
    /// Это не выдуманная предосторожность. На машине, где Bamboo проработал
    /// неделю, база оказалась повреждена — `integrity_check` нашёл битые
    /// страницы дерева, — и всё это время история молча не собиралась.
    /// Разделы истории показывали пустоту, отличить её от «ещё не накопилось»
    /// было нечем, и о поломке никто не узнал. Диск при этом был здоров:
    /// ни одной ошибки в системном журнале, SMART чистый.
    ///
    /// Терять историю неприятно, но она телеметрия, а не журнал действий.
    /// Журнал действий лежит отдельным файлом ровно для этого (ТЗ 12.2)
    /// и с `synchronous = FULL`, а здесь потеря допустима — молчаливая
    /// неработоспособность недопустима.
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let path = path.as_ref();
        match Self::open_sound(path) {
            Ok(store) => Ok(store),
            Err(error) if looks_corrupt(&error) => {
                // Сначала убираем журнал WAL, и только потом — саму базу.
                // Порядок стоил истории за неделю, когда был обратным.
                //
                // На живой машине повреждённая база после удаления журнала
                // прочиталась целиком: все 181 600 записей оказались
                // на месте, битым был хвост журнала. Первая редакция
                // уносила базу вместе с ним и теряла всё, хотя терять
                // требовалось только несбросившийся хвост.
                drop_wal(path);
                match Self::open_sound(path) {
                    Ok(store) => Ok(store),
                    Err(again) if looks_corrupt(&again) => {
                        quarantine(path)?;
                        Self::open_sound(path)
                    }
                    Err(other) => Err(other),
                }
            }
            Err(other) => Err(other),
        }
    }

    /// Открывает базу и убеждается, что она цела.
    fn open_sound(path: &Path) -> Result<Store> {
        let store = Self::prepare(Connection::open(path)?)?;
        // `quick_check` вместо `integrity_check`: он не проверяет
        // согласованность индексов с таблицами, зато не читает их дважды.
        // Для нашей задачи этого хватает — ловим битые страницы, а не
        // расхождение индекса.
        //
        // Проверка стоит времени при запуске, и вот сколько: замер на базе
        // ровно в 200 МБ — потолок из раздела 4 ТЗ — дал 1054 мс, то есть
        // около 5 мс на мегабайт. На настоящей базе, где наблюдения сложены
        // в четвертьчасовые корзины, это порядка сотни миллисекунд. Плата
        // принимается: неделя молча несобираемой истории стоила дороже.
        let verdict: String = store
            .conn
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
        if verdict != "ok" {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                Some(verdict),
            ));
        }
        Ok(store)
    }

    /// База в памяти. Нужна тестам.
    pub fn in_memory() -> Result<Store> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Store> {
        // Подождать занятую базу, а не падать сразу — тот же довод, что
        // в журнале действий: к файлу обращаются независимые участники,
        // и без времени ожидания второй получает «database is locked».
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Размер страницы — ПЕРВЫМ делом, до включения WAL. После включения
        // журнала эта настройка молча ничего не делает, и база останется
        // со страницей по умолчанию.
        //
        // Зачем менять. Замер показал, что обещанные разделом 8.2 ТЗ
        // «20 МБ записи в сутки при часовом сбросе» арифметически
        // не сходятся: при сотне приложений выходит около пятидесяти
        // мегабайт. Причина в том, что первичный ключ разбрасывает записи
        // одного сброса по разным страницам, и SQLite переписывает
        // страницу целиком ради каждой. Меньшая страница означает меньше
        // переписанного впустую.
        //
        // Строку таблицы бюджета в ТЗ это не отменяет: настоящее решение —
        // более редкий сброс, и оно принимается тем, кто заводит хранилище.
        conn.pragma_update(None, "page_size", PAGE_SIZE)?;
        // WAL: читатели не блокируют писателя. Для утилиты, которая пишет
        // раз в час батчем, а читает при каждом открытии окна, это важно.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // NORMAL вместо FULL: при потере питания можно потерять последнюю
        // транзакцию телеметрии. Это допустимо — данные не банковские,
        // а FULL означает fsync на каждый коммит и лишнюю запись на SSD.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Без ограничения журнал WAL растёт неограниченно.
        conn.pragma_update(None, "journal_size_limit", 8 * 1024 * 1024)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let mut store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;

        if version >= SCHEMA_VERSION {
            return Ok(());
        }

        let tx = self.conn.transaction()?;
        if version < 1 {
            tx.execute_batch(V1)?;
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()
    }

    pub fn schema_version(&self) -> Result<i32> {
        self.conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
    }

    /// Возвращает идентификатор приложения, создавая запись при первой встрече.
    pub fn app_id(&self, app_key: &str, image_name: &str, now_ms: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO apps (app_key, image_name, first_seen_ms, last_seen_ms)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(app_key) DO UPDATE SET last_seen_ms = ?3",
            params![app_key, image_name, now_ms],
        )?;
        self.conn.query_row(
            "SELECT id FROM apps WHERE app_key = ?1",
            params![app_key],
            |row| row.get(0),
        )
    }

    /// Записывает агрегаты одним батчем в одной транзакции.
    ///
    /// Именно батчем, а не по строке: цель из раздела 4 ТЗ — не больше
    /// 20 МБ записи в сутки, и отдельная транзакция на каждую строку
    /// съест этот бюджет журналом WAL.
    pub fn write_buckets(&mut self, level: Level, app_id: i64, buckets: &[Bucket]) -> Result<()> {
        if buckets.is_empty() {
            return Ok(());
        }

        let sql = format!(
            "INSERT INTO {} (app_id, bucket_ms, samples, cpu_ms, read_kib, write_kib,
                private_avg, private_min, private_max,
                ws_avg, ws_min, ws_max,
                handles_avg, handles_min, handles_max,
                threads_avg, threads_min, threads_max)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(app_id, bucket_ms) DO UPDATE SET
                samples = samples + excluded.samples,
                cpu_ms = cpu_ms + excluded.cpu_ms,
                read_kib = read_kib + excluded.read_kib,
                write_kib = write_kib + excluded.write_kib,
                private_min = min(private_min, excluded.private_min),
                private_max = max(private_max, excluded.private_max),
                ws_min = min(ws_min, excluded.ws_min),
                ws_max = max(ws_max, excluded.ws_max),
                handles_min = min(handles_min, excluded.handles_min),
                handles_max = max(handles_max, excluded.handles_max),
                threads_min = min(threads_min, excluded.threads_min),
                threads_max = max(threads_max, excluded.threads_max)",
            level.table()
        );

        let tx = self.conn.transaction()?;
        {
            let mut statement = tx.prepare(&sql)?;
            for bucket in buckets {
                statement.execute(params![
                    app_id,
                    bucket.start_ms,
                    bucket.samples,
                    bucket.cpu_ms as i64,
                    bucket.read_kib as i64,
                    bucket.write_kib as i64,
                    bucket.private_kib.avg,
                    bucket.private_kib.min,
                    bucket.private_kib.max,
                    bucket.working_set_kib.avg,
                    bucket.working_set_kib.min,
                    bucket.working_set_kib.max,
                    bucket.handles.avg,
                    bucket.handles.min,
                    bucket.handles.max,
                    bucket.threads.avg,
                    bucket.threads.min,
                    bucket.threads.max,
                ])?;
            }
        }
        tx.commit()
    }

    /// Читает агрегаты за период, от старых к свежим.
    pub fn read_buckets(
        &self,
        level: Level,
        app_id: i64,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<Bucket>> {
        let sql = format!(
            "SELECT bucket_ms, samples, cpu_ms, read_kib, write_kib,
                    private_avg, private_min, private_max,
                    ws_avg, ws_min, ws_max,
                    handles_avg, handles_min, handles_max,
                    threads_avg, threads_min, threads_max
             FROM {} WHERE app_id = ?1 AND bucket_ms >= ?2 AND bucket_ms < ?3
             ORDER BY bucket_ms",
            level.table()
        );

        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params![app_id, from_ms, to_ms], |row| {
            Ok(Bucket {
                start_ms: row.get(0)?,
                samples: row.get(1)?,
                cpu_ms: row.get::<_, i64>(2)? as u64,
                read_kib: row.get::<_, i64>(3)? as u64,
                write_kib: row.get::<_, i64>(4)? as u64,
                private_kib: Stat {
                    avg: row.get(5)?,
                    min: row.get(6)?,
                    max: row.get(7)?,
                },
                working_set_kib: Stat {
                    avg: row.get(8)?,
                    min: row.get(9)?,
                    max: row.get(10)?,
                },
                handles: Stat {
                    avg: row.get(11)?,
                    min: row.get(12)?,
                    max: row.get(13)?,
                },
                threads: Stat {
                    avg: row.get(14)?,
                    min: row.get(15)?,
                    max: row.get(16)?,
                },
            })
        })?;

        rows.collect()
    }

    /// Поднимает L2 на уровень L3 для данных старше глубины L2.
    ///
    /// Возвращает число созданных часовых интервалов.
    pub fn roll_up_to_l3(&mut self, now_ms: i64) -> Result<usize> {
        let cutoff = now_ms - Level::L2.retention_ms();

        let app_ids: Vec<i64> = {
            let mut statement = self
                .conn
                .prepare("SELECT DISTINCT app_id FROM samples_l2 WHERE bucket_ms < ?1")?;
            let rows = statement.query_map(params![cutoff], |row| row.get(0))?;
            rows.collect::<Result<Vec<i64>>>()?
        };

        let mut written = 0;
        for app_id in app_ids {
            let old = self.read_buckets(Level::L2, app_id, i64::MIN, cutoff)?;
            let hourly = roll_up(&old, L3_BUCKET_MS);
            written += hourly.len();
            self.write_buckets(Level::L3, app_id, &hourly)?;
        }

        Ok(written)
    }

    /// Удаляет данные старше глубины каждого уровня.
    pub fn prune(&self, now_ms: i64) -> Result<usize> {
        let mut removed = 0;
        removed += self.conn.execute(
            "DELETE FROM samples_l2 WHERE bucket_ms < ?1",
            params![now_ms - Level::L2.retention_ms()],
        )?;
        removed += self.conn.execute(
            "DELETE FROM samples_l3 WHERE bucket_ms < ?1",
            params![now_ms - Level::L3.retention_ms()],
        )?;
        removed += self.conn.execute(
            "DELETE FROM smart_history WHERE at_ms < ?1",
            params![now_ms - Level::L3.retention_ms()],
        )?;
        Ok(removed)
    }

    /// Записывает снимок SMART.
    pub fn record_smart(&self, drive_key: &str, snapshot: &SmartSnapshot) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO smart_history
                (drive_key, at_ms, data_written_bytes, data_read_bytes, percentage_used,
                 available_spare, media_errors, temperature_c, power_on_hours, critical_warning)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                drive_key,
                snapshot.at_unix_ms,
                snapshot.data_written.map(|b| b.as_u64() as i64),
                snapshot.data_read.map(|b| b.as_u64() as i64),
                snapshot.percentage_used,
                snapshot.available_spare,
                snapshot.media_errors.map(|v| v as i64),
                snapshot.temperature_c,
                snapshot.power_on_hours.map(|v| v as i64),
                snapshot.critical_warning,
            ],
        )?;
        Ok(())
    }

    /// Снимки SMART за период, от старых к свежим.
    pub fn smart_history(&self, drive_key: &str, since_ms: i64) -> Result<Vec<SmartSnapshot>> {
        let mut statement = self.conn.prepare(
            "SELECT at_ms, data_written_bytes, data_read_bytes, percentage_used,
                    available_spare, media_errors, temperature_c, power_on_hours, critical_warning
             FROM smart_history WHERE drive_key = ?1 AND at_ms >= ?2 ORDER BY at_ms",
        )?;
        let rows = statement.query_map(params![drive_key, since_ms], |row| {
            Ok(SmartSnapshot {
                at_unix_ms: row.get(0)?,
                data_written: row.get::<_, Option<i64>>(1)?.map(|v| Bytes(v as u64)),
                data_read: row.get::<_, Option<i64>>(2)?.map(|v| Bytes(v as u64)),
                percentage_used: row.get(3)?,
                available_spare: row.get(4)?,
                media_errors: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                temperature_c: row.get(6)?,
                power_on_hours: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
                critical_warning: row.get(8)?,
            })
        })?;
        rows.collect()
    }

    /// Среднесуточный объём записи за период — то, чего не хватало
    /// анализатору расхода ресурса SSD (ТЗ, раздел 9.4).
    ///
    /// Возвращает `None`, пока в истории нет двух замеров на расстоянии
    /// хотя бы часа: по одной точке темп не считается, а по двум подряд
    /// снятым — считается неверно.
    pub fn daily_write_average(&self, drive_key: &str, since_ms: i64) -> Result<Option<Bytes>> {
        let history = self.smart_history(drive_key, since_ms)?;

        let first = history.iter().find(|s| s.data_written.is_some());
        let last = history.iter().rev().find(|s| s.data_written.is_some());

        let (Some(first), Some(last)) = (first, last) else {
            return Ok(None);
        };

        let span_ms = last.at_unix_ms - first.at_unix_ms;
        if span_ms < 3_600_000 {
            return Ok(None);
        }

        let written = last
            .data_written
            .unwrap_or(Bytes::ZERO)
            .saturating_sub(first.data_written.unwrap_or(Bytes::ZERO));

        let days = span_ms as f64 / 86_400_000.0;
        Ok(Some(Bytes((written.as_u64() as f64 / days) as u64)))
    }

    /// Выросло ли число ошибок носителя с прошлого замера.
    pub fn media_errors_grew(&self, drive_key: &str, since_ms: i64) -> Result<bool> {
        let history = self.smart_history(drive_key, since_ms)?;
        let values: Vec<u64> = history.iter().filter_map(|s| s.media_errors).collect();
        match (values.first(), values.last()) {
            (Some(first), Some(last)) => Ok(last > first),
            _ => Ok(false),
        }
    }

    pub fn record_boot(&self, entry: &BootEntry) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO boot_history
                (at_ms, total_ms, main_path_ms, post_boot_ms, degradation_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                entry.at_unix_ms,
                entry.total_ms as i64,
                entry.main_path_ms as i64,
                entry.post_boot_ms as i64,
                entry.degradation_ms.map(|v| v as i64),
            ],
        )?;
        Ok(())
    }

    /// Загрузки от свежих к старым.
    pub fn boot_history(&self, limit: usize) -> Result<Vec<BootEntry>> {
        let mut statement = self.conn.prepare(
            "SELECT at_ms, total_ms, main_path_ms, post_boot_ms, degradation_ms
             FROM boot_history ORDER BY at_ms DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok(BootEntry {
                at_unix_ms: row.get(0)?,
                total_ms: row.get::<_, i64>(1)? as u64,
                main_path_ms: row.get::<_, i64>(2)? as u64,
                post_boot_ms: row.get::<_, i64>(3)? as u64,
                degradation_ms: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
            })
        })?;
        rows.collect()
    }

    pub fn set_pref(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO user_prefs (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn pref(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM user_prefs WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
    }

    /// Размер базы. Бюджет из раздела 4 ТЗ — не больше 200 МБ.
    pub fn size_bytes(&self) -> Result<Bytes> {
        let pages: i64 = self.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let page_size: i64 = self.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        Ok(Bytes((pages * page_size).max(0) as u64))
    }

    /// Уплотняет базу после массового удаления.
    pub fn vacuum(&self) -> Result<()> {
        self.conn.execute_batch("VACUUM")
    }
}
