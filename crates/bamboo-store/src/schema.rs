//! Схема базы и миграции.

/// Версия схемы. Хранится в `PRAGMA user_version`.
pub const SCHEMA_VERSION: i32 = 1;

/// Длительность интервала уровня L2 — 15 минут.
pub const L2_BUCKET_MS: i64 = 15 * 60 * 1000;
/// Длительность интервала уровня L3 — час.
pub const L3_BUCKET_MS: i64 = 60 * 60 * 1000;

/// Глубина L2 — 30 суток.
pub const L2_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Глубина L3 — 12 месяцев.
pub const L3_RETENTION_MS: i64 = 365 * 24 * 60 * 60 * 1000;

/// Начальная схема.
///
/// Таблицы сэмплов объявлены `WITHOUT ROWID`: ключ у них составной
/// и осмысленный, а лишний скрытый rowid — это ещё один индекс на каждую
/// из миллионов строк.
pub const V1: &str = r#"
CREATE TABLE apps (
    id            INTEGER PRIMARY KEY,
    app_key       TEXT NOT NULL UNIQUE,
    image_name    TEXT NOT NULL,
    first_seen_ms INTEGER NOT NULL,
    last_seen_ms  INTEGER NOT NULL
);

CREATE TABLE samples_l2 (
    app_id      INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    bucket_ms   INTEGER NOT NULL,
    samples     INTEGER NOT NULL,
    cpu_ms      INTEGER NOT NULL,
    read_kib    INTEGER NOT NULL,
    write_kib   INTEGER NOT NULL,
    private_avg INTEGER NOT NULL,
    private_min INTEGER NOT NULL,
    private_max INTEGER NOT NULL,
    ws_avg      INTEGER NOT NULL,
    ws_min      INTEGER NOT NULL,
    ws_max      INTEGER NOT NULL,
    handles_avg INTEGER NOT NULL,
    handles_min INTEGER NOT NULL,
    handles_max INTEGER NOT NULL,
    threads_avg INTEGER NOT NULL,
    threads_min INTEGER NOT NULL,
    threads_max INTEGER NOT NULL,
    PRIMARY KEY (app_id, bucket_ms)
) WITHOUT ROWID;

CREATE TABLE samples_l3 (
    app_id      INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    bucket_ms   INTEGER NOT NULL,
    samples     INTEGER NOT NULL,
    cpu_ms      INTEGER NOT NULL,
    read_kib    INTEGER NOT NULL,
    write_kib   INTEGER NOT NULL,
    private_avg INTEGER NOT NULL,
    private_min INTEGER NOT NULL,
    private_max INTEGER NOT NULL,
    ws_avg      INTEGER NOT NULL,
    ws_min      INTEGER NOT NULL,
    ws_max      INTEGER NOT NULL,
    handles_avg INTEGER NOT NULL,
    handles_min INTEGER NOT NULL,
    handles_max INTEGER NOT NULL,
    threads_avg INTEGER NOT NULL,
    threads_min INTEGER NOT NULL,
    threads_max INTEGER NOT NULL,
    PRIMARY KEY (app_id, bucket_ms)
) WITHOUT ROWID;

-- Ключ — серийный номер, а не номер PhysicalDrive: номера
-- переприсваиваются при переподключении устройств.
CREATE TABLE smart_history (
    drive_key          TEXT NOT NULL,
    at_ms              INTEGER NOT NULL,
    data_written_bytes INTEGER,
    data_read_bytes    INTEGER,
    percentage_used    INTEGER,
    available_spare    INTEGER,
    media_errors       INTEGER,
    temperature_c      INTEGER,
    power_on_hours     INTEGER,
    critical_warning   INTEGER,
    PRIMARY KEY (drive_key, at_ms)
) WITHOUT ROWID;

CREATE TABLE boot_history (
    at_ms           INTEGER PRIMARY KEY,
    total_ms        INTEGER NOT NULL,
    main_path_ms    INTEGER NOT NULL,
    post_boot_ms    INTEGER NOT NULL,
    degradation_ms  INTEGER
);

CREATE TABLE user_prefs (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;
