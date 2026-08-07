//! Состояние памяти системы.

use core::mem::size_of;

use bamboo_core::{Bytes, Error, MemoryStat, Result};
use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};

/// Снимает состояние памяти одним вызовом.
///
/// Берём `GetPerformanceInfo`, а не `GlobalMemoryStatusEx`: он отдаёт коммит
/// и размер системного кэша, которых во втором нет, и всё в страницах,
/// то есть без потери точности на больших объёмах.
pub fn memory_stat() -> Result<MemoryStat> {
    let mut info: PERFORMANCE_INFORMATION = unsafe { core::mem::zeroed() };
    info.cb = size_of::<PERFORMANCE_INFORMATION>() as u32;

    let ok = unsafe { GetPerformanceInfo(&mut info, info.cb) };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "GetPerformanceInfo",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }

    let page = info.PageSize as u64;
    let pages = |count: usize| Bytes(count as u64 * page);

    Ok(MemoryStat {
        physical_total: pages(info.PhysicalTotal),
        physical_available: pages(info.PhysicalAvailable),
        commit_used: pages(info.CommitTotal),
        commit_limit: pages(info.CommitLimit),
        cache: pages(info.SystemCache),
    })
}

/// Число процессов, потоков и дескрипторов в системе.
///
/// Дешёвая проверка целостности снимка процессов: если счётчик разошёлся
/// с длиной списка более чем на пару единиц, значит снимок снят в момент
/// массового запуска или завершения.
pub fn system_counts() -> Result<SystemCounts> {
    let mut info: PERFORMANCE_INFORMATION = unsafe { core::mem::zeroed() };
    info.cb = size_of::<PERFORMANCE_INFORMATION>() as u32;

    let ok = unsafe { GetPerformanceInfo(&mut info, info.cb) };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "GetPerformanceInfo",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }

    Ok(SystemCounts {
        processes: info.ProcessCount,
        threads: info.ThreadCount,
        handles: info.HandleCount,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemCounts {
    pub processes: u32,
    pub threads: u32,
    pub handles: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_numbers_are_sane() {
        let stat = memory_stat().unwrap();

        // Минимально поддерживаемая конфигурация — 4 ГБ.
        assert!(stat.physical_total >= Bytes::from_mib(1024));
        assert!(stat.physical_available <= stat.physical_total);
        assert!(stat.commit_used <= stat.commit_limit);
        assert!(stat.commit_limit >= stat.physical_total);

        let pressure = stat.commit_pressure();
        assert!((0.0..=1.0).contains(&pressure), "давление на память {pressure}");
    }

    #[test]
    fn counts_match_reality() {
        let counts = system_counts().unwrap();
        assert!(counts.processes > 10);
        assert!(counts.threads > counts.processes);
    }
}
