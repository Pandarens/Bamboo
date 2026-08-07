//! Таблица наблюдаемых процессов и вычисление дельт.

use std::collections::HashMap;

use bamboo_core::app::{PathNormalizer, Publisher};
use bamboo_core::{AppKey, Bytes, Nanos, Pid, ProcessId, ProcessSample, SampleTime};

use crate::ring::RingBuffer;

/// Глубина буфера L0 в точках: 600 сэмплов, то есть 10 минут при опросе
/// раз в секунду и почти час при обычных пяти.
pub const L0_CAPACITY: usize = 600;

/// Одна точка истории процесса.
///
/// Компактность здесь не микрооптимизация, а требование бюджета: точка
/// хранится 600 раз на каждый из ~300 процессов. При 20 байтах это 3.4 МБ,
/// при «удобных» 64 байтах было бы 11 МБ — весь бюджет памяти брокера
/// и сверху.
///
/// Накопительные метрики хранятся дельтами за интервал, мгновенные — как есть.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MetricPoint {
    /// Процессорное время за интервал, миллисекунды.
    pub cpu_ms: u32,
    /// Закоммиченные приватные страницы на момент замера.
    pub private_kib: u32,
    /// Приватная часть рабочего набора на момент замера.
    pub working_set_private_kib: u32,
    /// Прочитано за интервал.
    pub read_kib: u32,
    /// Записано за интервал.
    pub write_kib: u32,
}

// Размер точки — часть контракта с бюджетом памяти, а не деталь реализации.
const _: () = assert!(core::mem::size_of::<MetricPoint>() == 20);

/// Как процесс называется и откуда запущен.
#[derive(Clone, Debug)]
pub struct ProcessIdentity {
    /// Имя образа из снимка, например `chrome.exe`.
    pub image_name: String,
    /// Полный путь, если его удалось получить. Для процессов ядра
    /// и защищённых процессов будет `None` — и это нормально.
    pub image_path: Option<String>,
}

/// Наблюдаемый процесс.
#[derive(Clone, Debug)]
pub struct TrackedProcess {
    pub id: ProcessId,
    pub parent_pid: Pid,
    pub session_id: u32,
    pub image_name: Box<str>,
    pub app_key: AppKey,
    pub first_seen: SampleTime,
    pub last_seen: SampleTime,

    pub threads: u32,
    pub handles: u32,
    /// Доля одного ядра за последний интервал: 1.0 — ядро занято полностью.
    pub cpu_share: f32,
    /// Процессорное время за всё время наблюдения.
    pub cpu_total: Nanos,
    /// Записано за всё время наблюдения.
    pub written: Bytes,

    pub history: RingBuffer<MetricPoint>,

    /// Предыдущий сырой сэмпл — база для дельт.
    prev: ProcessSample,
    /// На каком тике процесс видели в последний раз.
    seen_at_generation: u64,
}

impl TrackedProcess {
    pub fn pid(&self) -> Pid {
        self.id.pid
    }

    /// Сколько процесс прожил под наблюдением.
    pub fn observed_ms(&self) -> u64 {
        self.last_seen.elapsed_ms_since(self.first_seen)
    }

    /// Последняя точка истории.
    pub fn last_point(&self) -> MetricPoint {
        self.history.last().copied().unwrap_or_default()
    }
}

/// Что изменилось за тик.
#[derive(Clone, Debug, Default)]
pub struct TickChanges {
    pub started: Vec<ProcessId>,
    /// Завершившиеся процессы вместе с именами: после удаления из таблицы
    /// узнать имя будет неоткуда.
    pub exited: Vec<(ProcessId, Box<str>)>,
}

/// Таблица процессов.
///
/// Ограничение метода, которое надо помнить: опрос раз в 5 секунд не видит
/// процессы, живущие 2 секунды, а именно короткоживущие процессы вызывают
/// внезапные фризы. Это лечится ETW-сессией на `Kernel-Process` (этап 0.3),
/// а не учащением опроса.
#[derive(Debug)]
pub struct ProcessTable {
    processes: HashMap<ProcessId, TrackedProcess>,
    normalizer: PathNormalizer,
    generation: u64,
}

impl ProcessTable {
    pub fn new(normalizer: PathNormalizer) -> Self {
        ProcessTable {
            processes: HashMap::new(),
            normalizer,
            generation: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.processes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }

    pub fn get(&self, id: ProcessId) -> Option<&TrackedProcess> {
        self.processes.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &TrackedProcess> + '_ {
        self.processes.values()
    }

    /// Начинает новый тик. Всё, что не будет отмечено `observe`,
    /// считается завершившимся.
    pub fn begin_tick(&mut self) {
        self.generation += 1;
    }

    /// Учитывает один процесс из снимка.
    ///
    /// `identity` вызывается только для впервые увиденного процесса: получение
    /// полного пути открывает дескриптор, и делать это каждый тик нельзя.
    pub fn observe<F>(
        &mut self,
        sample: ProcessSample,
        at: SampleTime,
        interval_ms: u64,
        identity: F,
    ) where
        F: FnOnce() -> ProcessIdentity,
    {
        let generation = self.generation;

        if !self.processes.contains_key(&sample.id) {
            let identity = identity();
            let app_key = self.app_key_for(&identity);
            self.processes.insert(
                sample.id,
                TrackedProcess {
                    id: sample.id,
                    parent_pid: sample.parent_pid,
                    session_id: sample.session_id,
                    image_name: identity.image_name.into_boxed_str(),
                    app_key,
                    first_seen: at,
                    last_seen: at,
                    threads: sample.threads,
                    handles: sample.handles,
                    cpu_share: 0.0,
                    cpu_total: Nanos::ZERO,
                    written: Bytes::ZERO,
                    // Первый сэмпл даёт только базу для дельт: сколько CPU
                    // процесс съел до того, как мы его увидели, нам неизвестно,
                    // и приписывать это одному интервалу нельзя.
                    history: RingBuffer::with_capacity(L0_CAPACITY),
                    prev: sample,
                    seen_at_generation: generation,
                },
            );
            return;
        }

        let tracked = self
            .processes
            .get_mut(&sample.id)
            .expect("процесс только что проверен на наличие");

        let cpu_delta = sample.cpu_total().saturating_sub(tracked.prev.cpu_total());
        let read_delta = sample.read_bytes.saturating_sub(tracked.prev.read_bytes);
        let write_delta = sample.write_bytes.saturating_sub(tracked.prev.write_bytes);

        tracked.history.push(MetricPoint {
            cpu_ms: cpu_delta.as_millis().min(u32::MAX as u64) as u32,
            private_kib: sample.private_pages.as_kib().min(u32::MAX as u64) as u32,
            working_set_private_kib: sample
                .working_set_private
                .as_kib()
                .min(u32::MAX as u64) as u32,
            read_kib: (read_delta / 1024).min(u32::MAX as u64) as u32,
            write_kib: (write_delta / 1024).min(u32::MAX as u64) as u32,
        });

        tracked.cpu_share = if interval_ms == 0 {
            0.0
        } else {
            (cpu_delta.as_u64() as f64 / (interval_ms as f64 * 1_000_000.0)) as f32
        };
        tracked.cpu_total += cpu_delta;
        tracked.written += Bytes(write_delta);
        tracked.threads = sample.threads;
        tracked.handles = sample.handles;
        tracked.last_seen = at;
        tracked.prev = sample;
        tracked.seen_at_generation = generation;
    }

    /// Завершает тик: убирает процессы, которых не было в снимке.
    pub fn end_tick(&mut self) -> TickChanges {
        let generation = self.generation;
        let mut changes = TickChanges::default();

        for tracked in self.processes.values() {
            if tracked.seen_at_generation == generation && tracked.history.is_empty() {
                changes.started.push(tracked.id);
            }
        }

        self.processes.retain(|id, tracked| {
            if tracked.seen_at_generation == generation {
                true
            } else {
                changes.exited.push((*id, tracked.image_name.clone()));
                false
            }
        });

        changes
    }

    fn app_key_for(&self, identity: &ProcessIdentity) -> AppKey {
        match &identity.image_path {
            Some(path) => AppKey::new(&self.normalizer.normalize(path), &Publisher::Unknown),
            // Пути нет — ключ по имени образа. Он слабее (два разных
            // updater.exe склеятся), поэтому помечаем это явно.
            None => AppKey::new(
                &format!("?\\{}", identity.image_name.to_lowercase()),
                &Publisher::Unknown,
            ),
        }
    }

    /// Топ процессов по загрузке процессора за последний интервал.
    pub fn top_by_cpu(&self, limit: usize) -> Vec<&TrackedProcess> {
        let mut all: Vec<_> = self.processes.values().collect();
        all.sort_by(|a, b| b.cpu_share.total_cmp(&a.cpu_share));
        all.truncate(limit);
        all
    }

    /// Топ процессов по записи за последний интервал.
    pub fn top_by_write(&self, limit: usize) -> Vec<&TrackedProcess> {
        let mut all: Vec<_> = self.processes.values().collect();
        all.sort_by_key(|p| core::cmp::Reverse(p.last_point().write_kib));
        all.truncate(limit);
        all
    }

    /// Топ процессов по приватной памяти.
    pub fn top_by_memory(&self, limit: usize) -> Vec<&TrackedProcess> {
        let mut all: Vec<_> = self.processes.values().collect();
        all.sort_by_key(|p| core::cmp::Reverse(p.last_point().private_kib));
        all.truncate(limit);
        all
    }

    /// Оценка занятой под историю памяти. Бюджет из раздела 4 ТЗ должен
    /// измеряться, а не декларироваться.
    pub fn estimated_memory_bytes(&self) -> usize {
        let per_entry = core::mem::size_of::<TrackedProcess>() + core::mem::size_of::<ProcessId>();
        self.processes
            .values()
            .map(|p| per_entry + p.history.allocated_bytes() + p.image_name.len())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str) -> ProcessIdentity {
        ProcessIdentity {
            image_name: name.to_string(),
            image_path: Some(format!("C:\\Program Files\\{name}")),
        }
    }

    fn sample(pid: u32, cpu_ms: u64, write_bytes: u64) -> ProcessSample {
        ProcessSample {
            id: ProcessId::new(pid, 1),
            cpu_user: Nanos::from_millis(cpu_ms),
            private_pages: Bytes::from_mib(100),
            working_set_private: Bytes::from_mib(80),
            write_bytes,
            threads: 4,
            handles: 200,
            ..Default::default()
        }
    }

    fn at(ms: u64) -> SampleTime {
        SampleTime::new(ms, 0)
    }

    fn table() -> ProcessTable {
        ProcessTable::new(PathNormalizer::new())
    }

    #[test]
    fn first_sample_only_sets_the_baseline() {
        let mut table = table();
        table.begin_tick();
        table.observe(sample(100, 5_000, 0), at(0), 1000, || identity("app.exe"));
        table.end_tick();

        let tracked = table.get(ProcessId::new(100, 1)).unwrap();
        // Пять секунд CPU процесс потратил до того, как мы его увидели.
        // Приписать их первому интервалу означало бы показать 500% загрузки.
        assert_eq!(tracked.cpu_share, 0.0);
        assert!(tracked.history.is_empty());
    }

    #[test]
    fn cpu_share_is_computed_from_the_delta() {
        let mut table = table();
        table.begin_tick();
        table.observe(sample(100, 1_000, 0), at(0), 1000, || identity("app.exe"));
        table.end_tick();

        table.begin_tick();
        table.observe(sample(100, 1_500, 0), at(1000), 1000, || identity("app.exe"));
        table.end_tick();

        let tracked = table.get(ProcessId::new(100, 1)).unwrap();
        // 500 мс процессорного времени за интервал в 1000 мс — половина ядра.
        assert!((tracked.cpu_share - 0.5).abs() < 1e-6, "доля {}", tracked.cpu_share);
        assert_eq!(tracked.last_point().cpu_ms, 500);
    }

    #[test]
    fn identity_is_resolved_once_per_process() {
        let mut table = table();
        let mut calls = 0;

        for tick in 0..5 {
            table.begin_tick();
            table.observe(sample(100, tick * 100, 0), at(tick * 1000), 1000, || {
                calls += 1;
                identity("app.exe")
            });
            table.end_tick();
        }

        assert_eq!(calls, 1, "полный путь запрашивался {calls} раз вместо одного");
    }

    #[test]
    fn exited_processes_are_reported_and_removed() {
        let mut table = table();
        table.begin_tick();
        table.observe(sample(100, 0, 0), at(0), 1000, || identity("app.exe"));
        table.observe(sample(200, 0, 0), at(0), 1000, || identity("other.exe"));
        table.end_tick();
        assert_eq!(table.len(), 2);

        table.begin_tick();
        table.observe(sample(100, 100, 0), at(1000), 1000, || identity("app.exe"));
        let changes = table.end_tick();

        assert_eq!(table.len(), 1);
        assert_eq!(changes.exited.len(), 1);
        assert_eq!(changes.exited[0].0.pid, 200);
        assert_eq!(&*changes.exited[0].1, "other.exe");
    }

    #[test]
    fn reused_pid_starts_a_new_history() {
        let mut table = table();
        table.begin_tick();
        let mut first = sample(100, 0, 0);
        first.id = ProcessId::new(100, 111);
        table.observe(first, at(0), 1000, || identity("app.exe"));
        table.end_tick();

        table.begin_tick();
        let mut reused = sample(100, 0, 0);
        reused.id = ProcessId::new(100, 222);
        table.observe(reused, at(1000), 1000, || identity("other.exe"));
        let changes = table.end_tick();

        assert_eq!(table.len(), 1);
        assert_eq!(changes.exited.len(), 1, "старый экземпляр должен быть закрыт");
        assert_eq!(&*table.get(ProcessId::new(100, 222)).unwrap().image_name, "other.exe");
    }

    #[test]
    fn counter_reset_does_not_produce_a_negative_delta() {
        let mut table = table();
        table.begin_tick();
        table.observe(sample(100, 10_000, 500_000), at(0), 1000, || identity("app.exe"));
        table.end_tick();

        // Счётчик уехал назад — такое встречается при переполнении.
        table.begin_tick();
        table.observe(sample(100, 1_000, 1_000), at(1000), 1000, || identity("app.exe"));
        table.end_tick();

        let tracked = table.get(ProcessId::new(100, 1)).unwrap();
        assert_eq!(tracked.cpu_share, 0.0);
        assert_eq!(tracked.last_point().write_kib, 0);
    }

    #[test]
    fn history_is_bounded() {
        let mut table = table();
        for tick in 0..(L0_CAPACITY as u64 + 200) {
            table.begin_tick();
            table.observe(sample(100, tick * 10, 0), at(tick * 1000), 1000, || {
                identity("app.exe")
            });
            table.end_tick();
        }
        let tracked = table.get(ProcessId::new(100, 1)).unwrap();
        assert_eq!(tracked.history.len(), L0_CAPACITY);
    }

    #[test]
    fn app_key_ignores_the_pid() {
        let mut table = table();
        table.begin_tick();
        let mut a = sample(100, 0, 0);
        a.id = ProcessId::new(100, 1);
        let mut b = sample(101, 0, 0);
        b.id = ProcessId::new(101, 2);
        table.observe(a, at(0), 1000, || identity("chrome.exe"));
        table.observe(b, at(0), 1000, || identity("chrome.exe"));
        table.end_tick();

        let first = table.get(ProcessId::new(100, 1)).unwrap();
        let second = table.get(ProcessId::new(101, 2)).unwrap();
        assert_eq!(first.app_key, second.app_key);
    }

    #[test]
    fn processes_without_a_path_get_a_marked_key() {
        let mut table = table();
        table.begin_tick();
        table.observe(sample(4, 0, 0), at(0), 1000, || ProcessIdentity {
            image_name: "System".to_string(),
            image_path: None,
        });
        table.end_tick();

        let tracked = table.get(ProcessId::new(4, 1)).unwrap();
        assert!(
            tracked.app_key.as_str().starts_with("?\\"),
            "ключ без пути обязан быть помечен: {}",
            tracked.app_key
        );
    }
}
