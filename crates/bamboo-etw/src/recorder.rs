//! Видеорегистратор (ТЗ, раздел 7.4).
//!
//! Кольцевой буфер событий за последние минуты, который никуда не пишется,
//! пока не сработает триггер. При срабатывании буфер целиком сбрасывается —
//! и получается ретроспектива за минуты **до** события.
//!
//! Обычные мониторы этого не дают: пока пользователь заметит фриз и откроет
//! окно, всплеск уже закончился и смотреть не на что.

use std::collections::VecDeque;

use crate::event::ProcessEvent;

/// Глубина буфера по времени.
pub const WINDOW_MS: i64 = 10 * 60 * 1000;
/// Потолок по памяти. Соответствует обещанным в ТЗ 4–8 МБ.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
/// Сколько сбросов храним, прежде чем ротировать старые.
pub const MAX_DUMPS: usize = 10;
/// Не чаще одного сброса в пять минут: иначе затяжная нагрузка
/// вытеснит из истории всё остальное.
pub const MIN_INTERVAL_BETWEEN_DUMPS_MS: i64 = 5 * 60 * 1000;

/// Почему сработал сброс.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TriggerReason {
    /// Процессор загружен, а пользователя нет.
    CpuWhileIdle { percent: u32 },
    /// Идёт активная запись на диск, а пользователя нет.
    DiskWriteWhileIdle { megabytes: u64 },
    /// Один процесс держит ядро, не имея фокуса.
    ProcessHogging { image_name: String, percent: u32 },
    /// Пользователь нажал «зафиксировать».
    Manual,
}

impl TriggerReason {
    pub fn describe(&self) -> String {
        match self {
            TriggerReason::CpuWhileIdle { percent } => {
                format!("процессор был занят на {percent}%, пока вас не было")
            }
            TriggerReason::DiskWriteWhileIdle { megabytes } => {
                format!("на диск записано {megabytes} МБ за минуту, пока вас не было")
            }
            TriggerReason::ProcessHogging {
                image_name,
                percent,
            } => format!("{image_name} держал {percent}% ядра без фокуса"),
            TriggerReason::Manual => "зафиксировано вручную".to_string(),
        }
    }
}

/// Сброшенный на диск фрагмент истории.
#[derive(Clone, Debug)]
pub struct Dump {
    pub at_unix_ms: i64,
    pub reason: TriggerReason,
    pub events: Vec<ProcessEvent>,
}

/// Состояние системы для проверки триггеров.
#[derive(Clone, Debug, Default)]
pub struct Conditions<'a> {
    pub at_unix_ms: i64,
    /// Суммарная загрузка процессора, 0..1.
    pub cpu_busy: f64,
    /// Сколько времени человек не трогал компьютер.
    pub user_idle_ms: u64,
    /// Записано на диск за последнюю минуту.
    pub written_last_minute_bytes: u64,
    /// Процесс, дольше всех держащий ядро без фокуса: имя и доля ядра.
    pub hogging_process: Option<(&'a str, f32)>,
    /// Сколько он это делает.
    pub hogging_for_ms: u64,
}

/// Пороги срабатывания.
const IDLE_MS: u64 = 3 * 60 * 1000;
const CPU_WHILE_IDLE: f64 = 0.25;
const WRITE_WHILE_IDLE_BYTES: u64 = 100 * 1024 * 1024;
const HOG_SHARE: f32 = 0.50;
const HOG_DURATION_MS: u64 = 30_000;

/// Кольцевой буфер событий со сбросом по триггеру.
#[derive(Debug)]
pub struct Recorder {
    events: VecDeque<ProcessEvent>,
    bytes: usize,
    dumps: VecDeque<Dump>,
    last_dump_at_ms: Option<i64>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

impl Recorder {
    pub fn new() -> Self {
        Recorder {
            events: VecDeque::new(),
            bytes: 0,
            dumps: VecDeque::new(),
            last_dump_at_ms: None,
        }
    }

    /// Кладёт событие в буфер, вытесняя устаревшие.
    pub fn record(&mut self, event: ProcessEvent) {
        let now = event.at_unix_ms();
        self.bytes += event.size_bytes();
        self.events.push_back(event);
        self.evict(now);
    }

    fn evict(&mut self, now_ms: i64) {
        // Сначала по времени: держим окно в десять минут.
        while let Some(front) = self.events.front() {
            if now_ms - front.at_unix_ms() > WINDOW_MS {
                self.bytes -= front.size_bytes();
                self.events.pop_front();
            } else {
                break;
            }
        }
        // Затем по памяти: на машине с бурным запуском процессов окно
        // в десять минут может не влезть в бюджет.
        while self.bytes > MAX_BYTES {
            let Some(front) = self.events.pop_front() else {
                break;
            };
            self.bytes -= front.size_bytes();
        }
    }

    pub fn buffered_events(&self) -> usize {
        self.events.len()
    }

    pub fn buffered_bytes(&self) -> usize {
        self.bytes
    }

    pub fn dumps(&self) -> &VecDeque<Dump> {
        &self.dumps
    }

    /// Проверяет триггеры и при срабатывании сбрасывает буфер.
    pub fn check(&mut self, conditions: &Conditions<'_>) -> Option<&Dump> {
        let reason = trigger(conditions)?;
        self.dump(conditions.at_unix_ms, reason)
    }

    /// Сброс по явному запросу пользователя.
    ///
    /// Ограничение по частоте здесь не применяется: если человек нажал
    /// кнопку, он хочет получить снимок сейчас, а не через пять минут.
    pub fn capture_manually(&mut self, at_unix_ms: i64) -> &Dump {
        self.push_dump(at_unix_ms, TriggerReason::Manual);
        self.dumps.back().expect("сброс только что добавлен")
    }

    fn dump(&mut self, at_unix_ms: i64, reason: TriggerReason) -> Option<&Dump> {
        if let Some(last) = self.last_dump_at_ms {
            if at_unix_ms - last < MIN_INTERVAL_BETWEEN_DUMPS_MS {
                return None;
            }
        }
        self.push_dump(at_unix_ms, reason);
        self.dumps.back()
    }

    fn push_dump(&mut self, at_unix_ms: i64, reason: TriggerReason) {
        self.dumps.push_back(Dump {
            at_unix_ms,
            reason,
            events: self.events.iter().cloned().collect(),
        });
        while self.dumps.len() > MAX_DUMPS {
            self.dumps.pop_front();
        }
        self.last_dump_at_ms = Some(at_unix_ms);
    }
}

/// Проверяет условия срабатывания. Порядок — от более общего к частному.
fn trigger(conditions: &Conditions<'_>) -> Option<TriggerReason> {
    let idle = conditions.user_idle_ms >= IDLE_MS;

    if idle && conditions.cpu_busy >= CPU_WHILE_IDLE {
        return Some(TriggerReason::CpuWhileIdle {
            percent: (conditions.cpu_busy * 100.0) as u32,
        });
    }

    if idle && conditions.written_last_minute_bytes >= WRITE_WHILE_IDLE_BYTES {
        return Some(TriggerReason::DiskWriteWhileIdle {
            megabytes: conditions.written_last_minute_bytes / (1024 * 1024),
        });
    }

    if let Some((name, share)) = conditions.hogging_process {
        if share >= HOG_SHARE && conditions.hogging_for_ms >= HOG_DURATION_MS {
            return Some(TriggerReason::ProcessHogging {
                image_name: name.to_string(),
                percent: (share * 100.0) as u32,
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60_000;

    fn event(at: i64, pid: u32) -> ProcessEvent {
        ProcessEvent::Started {
            at_unix_ms: at,
            pid,
            parent_pid: 4,
            image_name: "svchost.exe".into(),
            session_id: 0,
        }
    }

    fn idle_conditions(at: i64) -> Conditions<'static> {
        Conditions {
            at_unix_ms: at,
            user_idle_ms: 30 * 60 * 1000,
            ..Default::default()
        }
    }

    #[test]
    fn events_older_than_the_window_are_dropped() {
        let mut recorder = Recorder::new();
        for minute in 0..20 {
            recorder.record(event(minute * MINUTE, minute as u32));
        }
        // Окно — десять минут, значит хвоста быть не должно.
        assert!(recorder.buffered_events() <= 11);
    }

    #[test]
    fn the_buffer_respects_the_memory_budget() {
        let mut recorder = Recorder::new();
        // Много событий в пределах окна: вытеснять придётся по памяти.
        for index in 0..300_000u32 {
            recorder.record(event(index as i64, index));
        }
        assert!(
            recorder.buffered_bytes() <= MAX_BYTES,
            "буфер занял {} байт",
            recorder.buffered_bytes()
        );
    }

    #[test]
    fn a_night_time_cpu_spike_triggers_a_dump() {
        let mut recorder = Recorder::new();
        recorder.record(event(0, 1));

        let mut conditions = idle_conditions(MINUTE);
        conditions.cpu_busy = 0.40;

        let dump = recorder.check(&conditions).expect("триггер не сработал");
        assert_eq!(dump.reason, TriggerReason::CpuWhileIdle { percent: 40 });
        assert_eq!(
            dump.events.len(),
            1,
            "в сброс должна попасть история до события"
        );
    }

    #[test]
    fn the_dump_contains_what_happened_before_the_trigger() {
        // Ровно та ценность, ради которой всё затевалось: ретроспектива
        // за минуты до срабатывания.
        let mut recorder = Recorder::new();
        for index in 0..50u32 {
            recorder.record(event(index as i64 * 1000, index));
        }

        let mut conditions = idle_conditions(60_000);
        conditions.cpu_busy = 0.9;
        let dump = recorder.check(&conditions).unwrap();

        assert_eq!(dump.events.len(), 50);
        assert_eq!(dump.events[0].pid(), 0);
    }

    #[test]
    fn work_while_the_user_is_present_does_not_trigger() {
        let mut recorder = Recorder::new();
        let conditions = Conditions {
            at_unix_ms: MINUTE,
            cpu_busy: 0.95,
            user_idle_ms: 1000,
            ..Default::default()
        };
        assert!(recorder.check(&conditions).is_none());
    }

    #[test]
    fn heavy_writes_while_idle_trigger_a_dump() {
        let mut recorder = Recorder::new();
        let mut conditions = idle_conditions(MINUTE);
        conditions.written_last_minute_bytes = 300 * 1024 * 1024;

        let dump = recorder.check(&conditions).unwrap();
        assert_eq!(
            dump.reason,
            TriggerReason::DiskWriteWhileIdle { megabytes: 300 }
        );
    }

    #[test]
    fn a_hogging_process_needs_to_hold_the_core_long_enough() {
        let mut recorder = Recorder::new();

        let brief = Conditions {
            at_unix_ms: MINUTE,
            hogging_process: Some(("апдейтер.exe", 0.9)),
            hogging_for_ms: 5_000,
            ..Default::default()
        };
        assert!(recorder.check(&brief).is_none(), "пять секунд — не повод");

        let sustained = Conditions {
            at_unix_ms: MINUTE,
            hogging_process: Some(("апдейтер.exe", 0.9)),
            hogging_for_ms: 45_000,
            ..Default::default()
        };
        let dump = recorder.check(&sustained).unwrap();
        assert!(matches!(dump.reason, TriggerReason::ProcessHogging { .. }));
    }

    #[test]
    fn dumps_are_rate_limited() {
        let mut recorder = Recorder::new();
        let mut conditions = idle_conditions(MINUTE);
        conditions.cpu_busy = 0.9;

        assert!(recorder.check(&conditions).is_some());

        // Затяжная нагрузка не должна вытеснить из истории всё остальное.
        conditions.at_unix_ms = MINUTE + 30_000;
        assert!(recorder.check(&conditions).is_none());

        conditions.at_unix_ms = MINUTE + 6 * MINUTE;
        assert!(recorder.check(&conditions).is_some());
    }

    #[test]
    fn a_manual_capture_ignores_the_rate_limit() {
        let mut recorder = Recorder::new();
        let mut conditions = idle_conditions(MINUTE);
        conditions.cpu_busy = 0.9;
        recorder.check(&conditions);

        // Человек нажал кнопку — он хочет снимок сейчас.
        let dump = recorder.capture_manually(MINUTE + 1000);
        assert_eq!(dump.reason, TriggerReason::Manual);
        assert_eq!(recorder.dumps().len(), 2);
    }

    #[test]
    fn only_the_last_ten_dumps_are_kept() {
        let mut recorder = Recorder::new();
        for index in 0..25 {
            recorder.capture_manually(index * MINUTE);
        }
        assert_eq!(recorder.dumps().len(), MAX_DUMPS);
        // Остались самые свежие.
        assert_eq!(recorder.dumps().back().unwrap().at_unix_ms, 24 * MINUTE);
    }

    #[test]
    fn an_empty_recorder_dumps_nothing_but_does_not_panic() {
        let mut recorder = Recorder::new();
        let dump = recorder.capture_manually(0);
        assert!(dump.events.is_empty());
    }
}
