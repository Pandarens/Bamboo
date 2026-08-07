//! Процессорное время системы, по ядрам.
//!
//! Зачем отдельно от процессов: если система нагружена, а сумма по процессам
//! мала — виновник на уровне драйверов. Это диагноз, который не может
//! поставить ни один процессный монитор (ТЗ, раздел 9.3).

use core::mem::size_of;

use bamboo_core::{CoreTimes, Error, Nanos, Result};

use crate::nt::{
    nt_success, NtQuerySystemInformation, CLASS_PROCESSOR_PERFORMANCE, STATUS_INFO_LENGTH_MISMATCH,
    SYSTEM_PROCESSOR_PERFORMANCE_INFO,
};

/// Переиспользуемый буфер под времена ядер.
pub struct CpuTimesBuffer {
    raw: Vec<SYSTEM_PROCESSOR_PERFORMANCE_INFO>,
    cores: Vec<CoreTimes>,
}

impl Default for CpuTimesBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuTimesBuffer {
    pub fn new() -> Self {
        // 64 записи — потолок одной процессорной группы Windows.
        CpuTimesBuffer {
            raw: vec![SYSTEM_PROCESSOR_PERFORMANCE_INFO::default(); 64],
            cores: Vec::new(),
        }
    }

    /// Снимает накопительные времена всех ядер.
    pub fn refresh(&mut self) -> Result<()> {
        for _ in 0..4 {
            let capacity_bytes = self.raw.len() * size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFO>();
            let mut returned: u32 = 0;
            let status = unsafe {
                NtQuerySystemInformation(
                    CLASS_PROCESSOR_PERFORMANCE,
                    self.raw.as_mut_ptr().cast(),
                    capacity_bytes as u32,
                    &mut returned,
                )
            };

            if nt_success(status) {
                let count = returned as usize / size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFO>();
                self.rebuild(count);
                return Ok(());
            }

            if status != STATUS_INFO_LENGTH_MISMATCH {
                self.cores.clear();
                return Err(Error::Nt {
                    call: "NtQuerySystemInformation(SystemProcessorPerformanceInformation)",
                    status,
                });
            }

            self.raw.resize(
                self.raw.len() * 2,
                SYSTEM_PROCESSOR_PERFORMANCE_INFO::default(),
            );
        }

        self.cores.clear();
        Err(Error::Nt {
            call: "NtQuerySystemInformation(SystemProcessorPerformanceInformation)",
            status: STATUS_INFO_LENGTH_MISMATCH,
        })
    }

    fn rebuild(&mut self, count: usize) {
        self.cores.clear();
        self.cores.reserve(count);
        for raw in &self.raw[..count.min(self.raw.len())] {
            // Windows кладёт время простоя внутрь KernelTime. Если этого
            // не вычесть, простаивающая система выглядит загруженной на 100%.
            let kernel_without_idle = raw.KernelTime.saturating_sub(raw.IdleTime);
            self.cores.push(CoreTimes {
                idle: Nanos::from_100ns(raw.IdleTime),
                kernel: Nanos::from_100ns(kernel_without_idle),
                user: Nanos::from_100ns(raw.UserTime),
                dpc: Nanos::from_100ns(raw.DpcTime),
                interrupt: Nanos::from_100ns(raw.InterruptTime),
                interrupt_count: raw.InterruptCount,
            });
        }
    }

    /// Времена ядер из последнего снимка, накопительно с момента загрузки.
    pub fn cores(&self) -> &[CoreTimes] {
        &self.cores
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_every_logical_core() {
        let mut buffer = CpuTimesBuffer::new();
        buffer.refresh().unwrap();

        let expected = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        assert_eq!(buffer.cores().len(), expected);
    }

    #[test]
    fn idle_is_excluded_from_kernel_time() {
        let mut buffer = CpuTimesBuffer::new();
        buffer.refresh().unwrap();

        // На простаивающей машине основная часть времени — простой.
        // Если бы idle остался внутри kernel, занятость была бы около единицы.
        for core in buffer.cores() {
            assert!(
                core.busy_ratio() < 0.95,
                "занятость {:.2} похожа на невычтенный idle",
                core.busy_ratio()
            );
        }
    }

    #[test]
    fn counters_only_grow() {
        let mut buffer = CpuTimesBuffer::new();
        buffer.refresh().unwrap();
        let before: Vec<_> = buffer.cores().to_vec();

        std::thread::sleep(std::time::Duration::from_millis(120));
        buffer.refresh().unwrap();

        let mut moved = false;
        for (now, prev) in buffer.cores().iter().zip(&before) {
            assert!(now.total() >= prev.total());
            if now.total() > prev.total() {
                moved = true;
            }
        }
        assert!(moved, "за 120 мс время ядер не изменилось");
    }
}
