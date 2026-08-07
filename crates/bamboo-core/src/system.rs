//! Метрики системы в целом.

use crate::time::SampleTime;
use crate::units::{Bytes, Nanos};

/// Процессорное время одного ядра, накопительно с момента загрузки.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreTimes {
    pub idle: Nanos,
    /// Время в режиме ядра. Внимание: Windows включает сюда `idle`,
    /// вычитание выполняется при разборе в `bamboo-sys`.
    pub kernel: Nanos,
    pub user: Nanos,
    /// Время в отложенных вызовах процедур. Ключ к диагностике драйверов.
    pub dpc: Nanos,
    pub interrupt: Nanos,
    pub interrupt_count: u32,
}

impl CoreTimes {
    /// Всё время ядра за период, включая простой.
    pub fn total(&self) -> Nanos {
        self.idle + self.kernel + self.user
    }

    /// Доля занятости ядра, 0..1.
    pub fn busy_ratio(&self) -> f64 {
        let total = self.total();
        if total.as_u64() == 0 {
            return 0.0;
        }
        (self.kernel + self.user).as_u64() as f64 / total.as_u64() as f64
    }

    /// Дельта относительно предыдущего замера.
    pub fn delta(&self, prev: &CoreTimes) -> CoreTimes {
        CoreTimes {
            idle: self.idle - prev.idle,
            kernel: self.kernel - prev.kernel,
            user: self.user - prev.user,
            dpc: self.dpc - prev.dpc,
            interrupt: self.interrupt - prev.interrupt,
            interrupt_count: self.interrupt_count.wrapping_sub(prev.interrupt_count),
        }
    }
}

/// Состояние памяти системы.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryStat {
    pub physical_total: Bytes,
    pub physical_available: Bytes,
    /// Сколько памяти система обязалась предоставить.
    pub commit_used: Bytes,
    /// Предел коммита: физическая память плюс файл подкачки.
    pub commit_limit: Bytes,
    pub cache: Bytes,
}

impl MemoryStat {
    pub fn physical_used(&self) -> Bytes {
        self.physical_total - self.physical_available
    }

    /// Давление на память, 0..1. Считается по коммиту, а не по свободной
    /// физической памяти: свободная память в Windows стремится к нулю
    /// не потому, что её не хватает, а потому что она занята кэшем.
    pub fn commit_pressure(&self) -> f64 {
        if self.commit_limit.as_u64() == 0 {
            return 0.0;
        }
        self.commit_used.as_u64() as f64 / self.commit_limit.as_u64() as f64
    }
}

/// Снимок системы на один тик.
#[derive(Clone, Debug, Default)]
pub struct SystemSample {
    pub at: SampleTime,
    pub cores: Vec<CoreTimes>,
    pub memory: MemoryStat,
}

impl SystemSample {
    /// Суммарная загрузка процессора за период, где 1.0 — все ядра заняты
    /// полностью. На вход подаются дельты, а не накопительные значения.
    pub fn total_busy_ratio(&self) -> f64 {
        if self.cores.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.cores.iter().map(CoreTimes::busy_ratio).sum();
        sum / self.cores.len() as f64
    }

    /// Доля времени, проведённая в DPC и прерываниях. Если она велика,
    /// а сумма по процессам мала — виновник на уровне драйверов,
    /// и ни один процессный монитор этого не покажет (ТЗ, раздел 9.3).
    pub fn driver_ratio(&self) -> f64 {
        let total: u64 = self.cores.iter().map(|c| c.total().as_u64()).sum();
        if total == 0 {
            return 0.0;
        }
        let driver: u64 = self
            .cores
            .iter()
            .map(|c| (c.dpc + c.interrupt).as_u64())
            .sum();
        driver as f64 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(idle_ms: u64, kernel_ms: u64, user_ms: u64) -> CoreTimes {
        CoreTimes {
            idle: Nanos::from_millis(idle_ms),
            kernel: Nanos::from_millis(kernel_ms),
            user: Nanos::from_millis(user_ms),
            ..Default::default()
        }
    }

    #[test]
    fn busy_ratio_counts_kernel_and_user() {
        assert!((core(750, 100, 150).busy_ratio() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn idle_core_is_not_busy() {
        assert_eq!(core(1000, 0, 0).busy_ratio(), 0.0);
        assert_eq!(CoreTimes::default().busy_ratio(), 0.0);
    }

    #[test]
    fn driver_ratio_sees_dpc_time() {
        let sample = SystemSample {
            cores: vec![CoreTimes {
                idle: Nanos::from_millis(700),
                kernel: Nanos::from_millis(200),
                user: Nanos::from_millis(100),
                dpc: Nanos::from_millis(150),
                interrupt: Nanos::from_millis(50),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!((sample.driver_ratio() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn commit_pressure_is_safe_on_empty_data() {
        assert_eq!(MemoryStat::default().commit_pressure(), 0.0);
    }
}
