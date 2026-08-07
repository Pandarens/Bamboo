//! Собственная скромность Bamboo.
//!
//! Утилита, которая учит систему экономить ресурсы, обязана начать с себя
//! (ТЗ, разделы 4 и 4.1). Здесь два вида функций: применение ограничений
//! к собственным процессам и измерение собственного потребления —
//! бюджет должен проверяться, а не декларироваться.

use core::mem::size_of;

use bamboo_core::{Bytes, Error, Result};
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Memory::SetProcessWorkingSetSizeEx;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, ProcessMemoryPriority, ProcessPowerThrottling, SetProcessInformation,
    MEMORY_PRIORITY_INFORMATION, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
};

/// Приоритет памяти «средний»: страницы Bamboo вытесняются раньше страниц
/// приложений пользователя, но позже, чем совсем фоновый мусор.
const MEMORY_PRIORITY_MEDIUM: u32 = 3;

/// Включает EcoQoS для текущего процесса.
///
/// Планировщик переводит потоки на энергоэффективные ядра и снижает частоту.
/// Для фоновой телеметрии это ровно то, что нужно: разница в задержке
/// незаметна, а вклад в расход батареи падает в разы.
pub fn enable_eco_qos() -> Result<()> {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    };

    let ok = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };

    if ok == 0 {
        return Err(Error::Win32 {
            call: "SetProcessInformation(ProcessPowerThrottling)",
            code: unsafe { GetLastError() },
        });
    }
    Ok(())
}

/// Понижает приоритет памяти текущего процесса до среднего.
pub fn lower_own_memory_priority() -> Result<()> {
    let info = MEMORY_PRIORITY_INFORMATION {
        MemoryPriority: MEMORY_PRIORITY_MEDIUM,
    };

    let ok = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessMemoryPriority,
            (&info as *const MEMORY_PRIORITY_INFORMATION).cast(),
            size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
    };

    if ok == 0 {
        return Err(Error::Win32 {
            call: "SetProcessInformation(ProcessMemoryPriority)",
            code: unsafe { GetLastError() },
        });
    }
    Ok(())
}

/// Однократно отдаёт системе рабочий набор, раздутый во время инициализации.
///
/// Ровно то, что «оптимизаторы памяти» делают чужим процессам каждые
/// несколько секунд и называют очисткой RAM. Разница принципиальная:
/// здесь это собственный процесс и единственный раз после старта, когда
/// страницы инициализации уже не нужны. Повторять нельзя — приложение
/// считывает вытесненное обратно, и вся «экономия» превращается
/// в дисковый ввод-вывод и износ SSD.
pub fn trim_own_working_set_once() -> Result<()> {
    let ok = unsafe { SetProcessWorkingSetSizeEx(GetCurrentProcess(), usize::MAX, usize::MAX, 0) };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "SetProcessWorkingSetSizeEx",
            code: unsafe { GetLastError() },
        });
    }
    Ok(())
}

/// Применяет все ограничения к текущему процессу. Вызывается один раз
/// после инициализации.
pub fn apply_self_limits() -> Result<()> {
    enable_eco_qos()?;
    lower_own_memory_priority()?;
    trim_own_working_set_once()
}

/// Собственное потребление памяти.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnMemory {
    pub working_set: Bytes,
    pub private_bytes: Bytes,
}

/// Измеряет память текущего процесса.
///
/// Нужно для самоконтроля: превышение бюджета из раздела 4 ТЗ —
/// блокирующий баг, а не тема для оптимизации «когда-нибудь потом».
pub fn own_memory() -> Result<OwnMemory> {
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { core::mem::zeroed() };
    counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;

    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    };

    if ok == 0 {
        return Err(Error::Win32 {
            call: "GetProcessMemoryInfo",
            code: unsafe { GetLastError() },
        });
    }

    Ok(OwnMemory {
        working_set: Bytes(counters.WorkingSetSize as u64),
        private_bytes: Bytes(counters.PrivateUsage as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_limits_apply_without_privileges() {
        // Всё это процесс имеет право делать сам с собой, без прав
        // администратора и без UAC.
        apply_self_limits().unwrap();
    }

    #[test]
    fn own_memory_is_measurable() {
        let mem = own_memory().unwrap();
        assert!(mem.working_set > Bytes::ZERO);
        assert!(mem.private_bytes > Bytes::ZERO);
        // Верхняя граница нарочно щедрая: тестовый бинарник собран
        // без оптимизаций и тащит отладочную информацию.
        assert!(mem.working_set < Bytes::from_mib(512), "рабочий набор {}", mem.working_set);
    }
}
