//! Сборка тика: снимок системы, дельты, обновление таблицы процессов.

use core::time::Duration;

use bamboo_core::app::PathNormalizer;
use bamboo_core::{CoreTimes, Result, SampleTime, SystemSample};
use bamboo_sys::{
    clock, cpu::CpuTimesBuffer, memory, power, process::full_image_path, user, ProcessBuffer,
};

use crate::cadence::{Cadence, CadenceController, Conditions};
use crate::table::{ProcessIdentity, ProcessTable, TickChanges};

/// Результат одного тика.
#[derive(Clone, Debug)]
pub struct Tick {
    pub at: SampleTime,
    /// Фактический интервал с прошлого тика. Считать загрузку по номинальному
    /// интервалу нельзя: поток мог проснуться позже из-за timer coalescing
    /// или выхода машины из сна.
    pub interval_ms: u64,
    /// Времена ядер — дельтами за интервал, а не накопительно.
    pub system: SystemSample,
    pub changes: TickChanges,
    pub process_count: usize,
    pub cadence: Cadence,
}

impl Tick {
    /// Суммарная загрузка процессора за интервал, 0..1.
    pub fn cpu_busy(&self) -> f64 {
        self.system.total_busy_ratio()
    }

    /// Доля времени в DPC и прерываниях.
    pub fn driver_time(&self) -> f64 {
        self.system.driver_ratio()
    }
}

/// Коллектор. Живёт в одном потоке и переиспользует все буферы между тиками.
pub struct Collector {
    processes: ProcessBuffer,
    cpu: CpuTimesBuffer,
    table: ProcessTable,
    cadence: CadenceController,

    prev_cores: Vec<CoreTimes>,
    prev_at: Option<SampleTime>,
    widget_open: bool,
}

impl Collector {
    pub fn new() -> Self {
        Collector {
            processes: ProcessBuffer::new(),
            cpu: CpuTimesBuffer::new(),
            table: ProcessTable::new(environment_normalizer()),
            cadence: CadenceController::new(),
            prev_cores: Vec::new(),
            prev_at: None,
            widget_open: false,
        }
    }

    pub fn table(&self) -> &ProcessTable {
        &self.table
    }

    pub fn cadence(&self) -> Cadence {
        self.cadence.current()
    }

    /// Пауза до следующего тика.
    pub fn next_interval(&self) -> Duration {
        self.cadence.interval()
    }

    /// Сообщает коллектору, открыт ли виджет. При открытии частота опроса
    /// поднимается сразу, при закрытии — падает после подтверждений.
    pub fn set_widget_open(&mut self, open: bool) {
        self.widget_open = open;
    }

    /// Снимает один тик.
    pub fn tick(&mut self) -> Result<Tick> {
        let at = clock::now();
        let interval_ms = self
            .prev_at
            .map(|prev| at.elapsed_ms_since(prev))
            .unwrap_or(0);

        self.cpu.refresh()?;
        let cores = core_deltas(&self.prev_cores, self.cpu.cores());
        self.prev_cores = self.cpu.cores().to_vec();

        let memory = memory::memory_stat().unwrap_or_default();

        self.processes.refresh()?;
        self.table.begin_tick();
        for raw in self.processes.iter() {
            let pid = raw.pid();

            // Idle — не процесс, а способ учёта простоя: его «загрузка» равна
            // числу свободных ядер и в топе потребителей выглядит как
            // тысяча процентов. Простой берём из времён ядер.
            if pid == 0 {
                continue;
            }

            let name = || raw.image_name();
            self.table
                .observe(raw.to_sample(), at, interval_ms, || ProcessIdentity {
                    image_name: name(),
                    image_path: full_image_path(pid).ok(),
                });
        }
        let changes = self.table.end_tick();

        let cadence = self.cadence.update(self.conditions());
        self.prev_at = Some(at);

        Ok(Tick {
            at,
            interval_ms,
            system: SystemSample { at, cores, memory },
            changes,
            process_count: self.table.len(),
            cadence,
        })
    }

    fn conditions(&self) -> Conditions {
        let power = power::power_status().ok();
        let state = user::notification_state();

        Conditions {
            widget_open: self.widget_open,
            fullscreen: state.is_fullscreen(),
            on_battery: power.is_some_and(|p| p.on_battery()),
            battery_low: power.is_some_and(|p| p.battery_low()),
            idle_ms: user::idle_ms().unwrap_or(0),
        }
    }
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

/// Разница времён ядер между тиками.
///
/// На первом тике предыдущих значений нет, и накопительные с загрузки
/// величины пришлось бы показать как загрузку за интервал. Возвращаем нули:
/// лучше пустой первый тик, чем «загрузка 100%» на старте.
fn core_deltas(prev: &[CoreTimes], now: &[CoreTimes]) -> Vec<CoreTimes> {
    if prev.len() != now.len() {
        return vec![CoreTimes::default(); now.len()];
    }
    now.iter()
        .zip(prev)
        .map(|(now, prev)| now.delta(prev))
        .collect()
}

/// Нормализатор путей, настроенный по переменным окружения текущей машины.
fn environment_normalizer() -> PathNormalizer {
    let var = |name: &str| std::env::var(name).unwrap_or_default();

    // Порядок добавления не важен: длинные префиксы сортируются вперёд.
    PathNormalizer::new()
        .with_prefix(&var("LOCALAPPDATA"), "%localappdata%")
        .with_prefix(&var("APPDATA"), "%appdata%")
        .with_prefix(&var("USERPROFILE"), "%userprofile%")
        .with_prefix(&var("ProgramFiles(x86)"), "%programfiles(x86)%")
        .with_prefix(&var("ProgramFiles"), "%programfiles%")
        .with_prefix(&var("ProgramData"), "%programdata%")
        .with_prefix(&var("SystemRoot"), "%windir%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_tick_reports_no_load() {
        let mut collector = Collector::new();
        let tick = collector.tick().unwrap();

        assert_eq!(tick.interval_ms, 0);
        assert_eq!(tick.cpu_busy(), 0.0, "на первом тике не с чем сравнивать");
        assert!(tick.process_count > 10);
        assert!(!tick.changes.started.is_empty());
    }

    #[test]
    fn second_tick_has_a_plausible_load() {
        let mut collector = Collector::new();
        collector.tick().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let tick = collector.tick().unwrap();

        assert!(tick.interval_ms >= 250, "интервал {} мс", tick.interval_ms);
        let busy = tick.cpu_busy();
        assert!((0.0..=1.0).contains(&busy), "загрузка {busy}");
        assert!((0.0..=1.0).contains(&tick.driver_time()));
    }

    #[test]
    fn own_process_is_tracked_with_a_real_path() {
        let mut collector = Collector::new();
        collector.tick().unwrap();

        let me = std::process::id();
        let tracked = collector
            .table()
            .iter()
            .find(|p| p.pid() == me)
            .expect("собственный процесс не попал в таблицу");

        assert!(tracked.image_name.to_lowercase().contains("bamboo"));
        assert!(
            !tracked.app_key.as_str().starts_with("?\\"),
            "для своего процесса путь обязан разрешиться: {}",
            tracked.app_key
        );
    }

    #[test]
    fn idle_is_not_tracked_as_a_process() {
        let mut collector = Collector::new();
        collector.tick().unwrap();
        assert!(
            collector.table().iter().all(|p| p.pid() != 0),
            "Idle попал в таблицу процессов"
        );
    }

    #[test]
    fn per_process_cpu_does_not_exceed_the_machine() {
        let mut collector = Collector::new();
        collector.tick().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        collector.tick().unwrap();

        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f32;
        for process in collector.table().iter() {
            assert!(
                process.cpu_share <= cores + 0.5,
                "{} показывает {} ядер",
                process.image_name,
                process.cpu_share
            );
        }
    }

    #[test]
    fn history_fits_the_memory_budget() {
        let mut collector = Collector::new();
        for _ in 0..3 {
            collector.tick().unwrap();
        }

        // Полный буфер L0 — 600 точек по 20 байт на процесс. Проверяем,
        // что оценка на реальном числе процессов остаётся в пределах
        // бюджета брокера с запасом.
        let now = collector.table().estimated_memory_bytes();
        let full = collector.table().len() * (600 * 20 + 200);
        assert!(now < full, "оценка {now} уже равна полному буферу {full}");
        assert!(full < 8 * 1024 * 1024, "полный L0 занял бы {full} байт");
    }
}
