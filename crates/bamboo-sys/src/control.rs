//! Управление чужими процессами (ТЗ, раздел 5.4).
//!
//! Пока только действия уровня риска 1: EcoQoS и приоритет памяти.
//! Оба мгновенно и полностью обратимы, побочных эффектов не имеют
//! и не требуют прав администратора для процессов того же пользователя.
//!
//! Каждая функция изменения имеет пару для чтения текущего состояния:
//! без снятого «до» действие нельзя записать в журнал, а значит нельзя
//! и выполнить.

use core::mem::size_of;

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::{
    GetProcessInformation, OpenProcess, ProcessMemoryPriority, ProcessPowerThrottling,
    SetProcessInformation, MEMORY_PRIORITY_INFORMATION, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
};

/// Приоритет памяти процесса, 0..5.
///
/// 5 — обычный, ниже означает, что страницы вытесняются раньше.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MemoryPriority(pub u32);

impl MemoryPriority {
    pub const NORMAL: MemoryPriority = MemoryPriority(5);
    pub const BELOW_NORMAL: MemoryPriority = MemoryPriority(4);
    pub const MEDIUM: MemoryPriority = MemoryPriority(3);
    pub const LOW: MemoryPriority = MemoryPriority(2);
    pub const VERY_LOW: MemoryPriority = MemoryPriority(1);

    pub fn is_valid(self) -> bool {
        (1..=5).contains(&self.0)
    }
}

/// Дескриптор чужого процесса, открытый ровно с теми правами, что нужны.
struct ProcessHandle(HANDLE);

impl ProcessHandle {
    fn open(pid: u32, access: u32) -> Result<ProcessHandle> {
        if pid == 0 || pid == 4 {
            return Err(Error::Unsupported(
                "процессы ядра не поддаются управлению извне",
            ));
        }
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            return Err(Error::Win32 {
                call: "OpenProcess",
                code: unsafe { GetLastError() },
            });
        }
        Ok(ProcessHandle(handle))
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Читает, включён ли у процесса EcoQoS.
///
/// Нужно для снятия состояния «до»: без него откат превратится
/// в угадывание.
pub fn eco_qos(pid: u32) -> Result<bool> {
    let handle = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;

    let mut state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    };

    let ok = unsafe {
        GetProcessInformation(
            handle.0,
            ProcessPowerThrottling,
            (&mut state as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };

    if ok == 0 {
        return Err(Error::Win32 {
            call: "GetProcessInformation(ProcessPowerThrottling)",
            code: unsafe { GetLastError() },
        });
    }

    // Режим включён, только если он и под управлением, и выставлен.
    let controlled = state.ControlMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0;
    let enabled = state.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0;
    Ok(controlled && enabled)
}

/// Включает или выключает EcoQoS.
pub fn set_eco_qos(pid: u32, enabled: bool) -> Result<()> {
    let handle = ProcessHandle::open(pid, PROCESS_SET_INFORMATION)?;

    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: if enabled {
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED
        } else {
            0
        },
    };

    let ok = unsafe {
        SetProcessInformation(
            handle.0,
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

/// Возвращает процесс под управление системы по умолчанию.
///
/// Это не то же самое, что выключить EcoQoS: система сама решает, когда
/// его применять, и после сброса вернётся к своему решению. Именно так
/// выглядит настоящий откат.
pub fn clear_eco_qos(pid: u32) -> Result<()> {
    let handle = ProcessHandle::open(pid, PROCESS_SET_INFORMATION)?;

    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    };

    let ok = unsafe {
        SetProcessInformation(
            handle.0,
            ProcessPowerThrottling,
            (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };

    if ok == 0 {
        return Err(Error::Win32 {
            call: "SetProcessInformation(сброс ProcessPowerThrottling)",
            code: unsafe { GetLastError() },
        });
    }
    Ok(())
}

/// Читает приоритет памяти процесса.
pub fn memory_priority(pid: u32) -> Result<MemoryPriority> {
    let handle = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;

    let mut info = MEMORY_PRIORITY_INFORMATION { MemoryPriority: 0 };
    let ok = unsafe {
        GetProcessInformation(
            handle.0,
            ProcessMemoryPriority,
            (&mut info as *mut MEMORY_PRIORITY_INFORMATION).cast(),
            size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
    };

    if ok == 0 {
        return Err(Error::Win32 {
            call: "GetProcessInformation(ProcessMemoryPriority)",
            code: unsafe { GetLastError() },
        });
    }
    Ok(MemoryPriority(info.MemoryPriority))
}

/// Задаёт приоритет памяти.
pub fn set_memory_priority(pid: u32, priority: MemoryPriority) -> Result<()> {
    if !priority.is_valid() {
        return Err(Error::Unsupported(
            "приоритет памяти бывает только от 1 до 5",
        ));
    }

    let handle = ProcessHandle::open(pid, PROCESS_SET_INFORMATION)?;
    let info = MEMORY_PRIORITY_INFORMATION {
        MemoryPriority: priority.0,
    };

    let ok = unsafe {
        SetProcessInformation(
            handle.0,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Все проверки идут на собственном процессе: он гарантированно наш,
    /// его состояние можно менять и возвращать, и никому от этого не хуже.
    fn me() -> u32 {
        std::process::id()
    }

    #[test]
    fn eco_qos_can_be_read_set_and_rolled_back() {
        let before = eco_qos(me()).expect("состояние EcoQoS не прочиталось");

        set_eco_qos(me(), true).unwrap();
        assert!(eco_qos(me()).unwrap(), "EcoQoS не включился");

        set_eco_qos(me(), false).unwrap();
        assert!(!eco_qos(me()).unwrap(), "EcoQoS не выключился");

        // Возвращаем ровно то, что было.
        if before {
            set_eco_qos(me(), true).unwrap();
        } else {
            clear_eco_qos(me()).unwrap();
        }
    }

    #[test]
    fn memory_priority_can_be_read_set_and_rolled_back() {
        let before = memory_priority(me()).expect("приоритет памяти не прочитался");
        assert!(before.is_valid(), "получили {before:?}");

        set_memory_priority(me(), MemoryPriority::LOW).unwrap();
        assert_eq!(memory_priority(me()).unwrap(), MemoryPriority::LOW);

        set_memory_priority(me(), before).unwrap();
        assert_eq!(memory_priority(me()).unwrap(), before);
    }

    #[test]
    fn an_invalid_priority_is_refused_before_touching_the_process() {
        assert!(set_memory_priority(me(), MemoryPriority(0)).is_err());
        assert!(set_memory_priority(me(), MemoryPriority(9)).is_err());
    }

    #[test]
    fn kernel_processes_are_refused_outright() {
        assert!(set_eco_qos(4, true).is_err());
        assert!(eco_qos(0).is_err());
    }

    #[test]
    fn a_dead_pid_reports_an_error() {
        // Заведомо несуществующий PID: номера кратны четырём,
        // и такой большой в системе не появится.
        assert!(eco_qos(0x7FFF_FFF0).is_err());
    }
}

/// Класс приоритета процесса.
///
/// Это грубая ручка: она говорит планировщику, кому отдавать процессорное
/// время при нехватке. Поднимать выше «выше обычного» не станем никогда:
/// класс реального времени вытесняет системные потоки, включая обработку
/// ввода, и машина перестаёт слушаться мыши.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PriorityClass(pub u32);

impl PriorityClass {
    /// Ниже обычного: фоновые программы во время игры.
    pub const BELOW_NORMAL: PriorityClass = PriorityClass(0x0000_4000);
    pub const NORMAL: PriorityClass = PriorityClass(0x0000_0020);
    /// Выше обычного: игра и то, что должно оставаться отзывчивым.
    pub const ABOVE_NORMAL: PriorityClass = PriorityClass(0x0000_8000);

    /// Разрешён ли класс к применению.
    ///
    /// Высокий и реального времени сюда не входят намеренно: ими легко
    /// подвесить машину, а выигрыш в играх недоказуем.
    pub fn is_allowed(self) -> bool {
        self == Self::BELOW_NORMAL || self == Self::NORMAL || self == Self::ABOVE_NORMAL
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::BELOW_NORMAL => "ниже обычного",
            Self::NORMAL => "обычный",
            Self::ABOVE_NORMAL => "выше обычного",
            _ => "неизвестный",
        }
    }
}

/// Читает класс приоритета процесса.
pub fn priority_class(pid: u32) -> Result<PriorityClass> {
    use windows_sys::Win32::System::Threading::GetPriorityClass;

    let handle = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let class = unsafe { GetPriorityClass(handle.0) };
    if class == 0 {
        return Err(Error::Win32 {
            call: "GetPriorityClass",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }
    Ok(PriorityClass(class))
}

/// Меняет класс приоритета процесса.
pub fn set_priority_class(pid: u32, class: PriorityClass) -> Result<()> {
    use windows_sys::Win32::System::Threading::SetPriorityClass;

    if !class.is_allowed() {
        return Err(Error::Unsupported(
            "такой класс приоритета Bamboo не выставляет: им можно подвесить машину",
        ));
    }

    let handle = ProcessHandle::open(pid, PROCESS_SET_INFORMATION)?;
    let ok = unsafe { SetPriorityClass(handle.0, class.0) };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "SetPriorityClass",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }
    Ok(())
}

#[cfg(test)]
mod priority_tests {
    use super::*;

    #[test]
    fn only_safe_classes_are_allowed() {
        assert!(PriorityClass::BELOW_NORMAL.is_allowed());
        assert!(PriorityClass::NORMAL.is_allowed());
        assert!(PriorityClass::ABOVE_NORMAL.is_allowed());

        // Высокий (0x80) и реального времени (0x100) — нет. Ими вытесняются
        // системные потоки, включая обработку ввода: машина перестаёт
        // слушаться мыши, а выигрыш в играх недоказуем.
        assert!(!PriorityClass(0x0000_0080).is_allowed());
        assert!(!PriorityClass(0x0000_0100).is_allowed());
    }

    #[test]
    fn a_forbidden_class_is_refused_before_touching_the_process() {
        let error = set_priority_class(std::process::id(), PriorityClass(0x0000_0100));
        assert!(error.is_err());
    }

    #[test]
    fn our_own_priority_is_readable_and_restorable() {
        let me = std::process::id();
        let before = priority_class(me).expect("свой приоритет должен читаться");

        set_priority_class(me, PriorityClass::BELOW_NORMAL).expect("понижение не удалось");
        assert_eq!(priority_class(me).unwrap(), PriorityClass::BELOW_NORMAL);

        set_priority_class(me, before).expect("возврат не удался");
        assert_eq!(priority_class(me).unwrap(), before);
    }

    #[test]
    fn every_allowed_class_has_a_name() {
        for class in [
            PriorityClass::BELOW_NORMAL,
            PriorityClass::NORMAL,
            PriorityClass::ABOVE_NORMAL,
        ] {
            assert!(!class.name().is_empty());
            assert_ne!(class.name(), "неизвестный");
        }
    }
}
