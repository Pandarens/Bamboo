//! Идентификация процессов и метрики на процесс.

use crate::units::{Bytes, Nanos};

pub type Pid = u32;

/// Устойчивый идентификатор экземпляра процесса.
///
/// PID переиспользуются, поэтому одного PID недостаточно: после завершения
/// процесса тот же номер через минуту может принадлежать чему угодно.
/// Пара «PID + время создания» уникальна (ТЗ, раздел 8.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId {
    pub pid: Pid,
    /// Время создания процесса в интервалах по 100 нс от 1601 года.
    pub create_time: i64,
}

impl ProcessId {
    pub const fn new(pid: Pid, create_time: i64) -> Self {
        ProcessId { pid, create_time }
    }
}

/// Сырой сэмпл процесса — то, что отдал `NtQuerySystemInformation`.
///
/// Накопительные метрики (процессорное время, ввод-вывод) хранятся как есть,
/// без вычитания предыдущего значения: дельту считает `bamboo-collect`, потому
/// что только он знает интервал между тиками.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessSample {
    pub id: ProcessId,
    pub parent_pid: Pid,
    pub session_id: u32,
    pub base_priority: i32,

    pub threads: u32,
    pub handles: u32,

    /// Накопительное время в пользовательском режиме.
    pub cpu_user: Nanos,
    /// Накопительное время в режиме ядра.
    pub cpu_kernel: Nanos,

    /// Приватная часть рабочего набора — память, которую процесс реально
    /// занимает в физической памяти и ни с кем не делит.
    pub working_set_private: Bytes,
    /// Закоммиченные приватные страницы. Основная метрика для детекта роста:
    /// в отличие от рабочего набора не проседает при вытеснении в подкачку.
    pub private_pages: Bytes,
    pub virtual_size: Bytes,

    /// Логический ввод-вывод, накопительно. Про ограничения — см. `io_counters_note`.
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub other_bytes: u64,
    pub read_ops: u64,
    pub write_ops: u64,
    pub other_ops: u64,
}

impl ProcessSample {
    /// Суммарное процессорное время процесса.
    pub fn cpu_total(&self) -> Nanos {
        self.cpu_user + self.cpu_kernel
    }
}

/// Оговорка про счётчики ввода-вывода, которую обязан показывать интерфейс
/// (ТЗ, раздел 6.3). Держим текст рядом с типом, чтобы он не разошёлся с кодом.
pub const IO_COUNTERS_NOTE: &str = "\
Счётчики отражают логический ввод-вывод, а не обращения к диску. \
Попадания в кэш файловой системы считаются как I/O, хотя диск не трогали. \
Отложенная запись выполняется системным потоком, поэтому реальный сброс \
на диск приписывается процессу System — приложения с memory-mapped файлами \
(СУБД, Docker, виртуализация) здесь систематически недооценены. \
Точная атрибуция физических записей возможна только через ETW.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_pid_after_reuse_is_a_different_process() {
        let first = ProcessId::new(4242, 133_000_000_000_000_000);
        let reused = ProcessId::new(4242, 133_000_000_500_000_000);
        assert_ne!(first, reused);
    }

    #[test]
    fn cpu_total_sums_both_modes() {
        let sample = ProcessSample {
            cpu_user: Nanos::from_millis(300),
            cpu_kernel: Nanos::from_millis(700),
            ..Default::default()
        };
        assert_eq!(sample.cpu_total(), Nanos::from_millis(1000));
    }
}
