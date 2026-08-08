//! Заморозка процессов через job object (ТЗ, раздел 5.4).
//!
//! Именно job object, а не `NtSuspendProcess`. `JobObjectFreezeInformation` —
//! механизм, которым Windows усыпляет UWP-приложения: он корректно
//! обрабатывает создание новых потоков в замороженном процессе и не
//! оставляет процесс в невыходимом состоянии при падении Bamboo.
//!
//! Структура не документирована официально и описана в Windows Internals.
//! Раскладка зафиксирована проверкой размера.

use core::mem::size_of;

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// Недокументированный класс информации job object.
const JOB_OBJECT_FREEZE_INFORMATION: i32 = 18;

/// Флаг «применить поле Freeze».
const FREEZE_OPERATION: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct JobObjectFreezeInformation {
    flags: u32,
    freeze: u8,
    swap: u8,
    reserved: [u8; 2],
    wake_filter_high: u32,
    wake_filter_low: u32,
}

const _: () = assert!(size_of::<JobObjectFreezeInformation>() == 16);

/// Замороженный процесс. Дескриптор job object обязан жить, пока процесс
/// заморожен: он и есть носитель заморозки.
pub struct FrozenProcess {
    job: HANDLE,
    pid: u32,
}

impl FrozenProcess {
    /// Замораживает процесс.
    ///
    /// Job object создаётся без `KILL_ON_JOB_CLOSE`: если Bamboo упадёт
    /// и дескриптор закроется, процесс разморозится, а не умрёт.
    pub fn freeze(pid: u32) -> Result<FrozenProcess> {
        if pid == 0 || pid == 4 || pid == std::process::id() {
            return Err(Error::Unsupported("этот процесс замораживать нельзя"));
        }

        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return Err(Error::Win32 {
                call: "OpenProcess(для заморозки)",
                code: unsafe { GetLastError() },
            });
        }

        // Анонимный job: имя в общем пространстве нам не нужно.
        let job = unsafe { CreateJobObjectW(core::ptr::null(), core::ptr::null()) };
        if job.is_null() {
            unsafe { CloseHandle(process) };
            return Err(Error::Win32 {
                call: "CreateJobObjectW",
                code: unsafe { GetLastError() },
            });
        }

        let assigned = unsafe { AssignProcessToJobObject(job, process) };
        unsafe { CloseHandle(process) };
        if assigned == 0 {
            let code = unsafe { GetLastError() };
            unsafe { CloseHandle(job) };
            // Частый случай: процесс уже в чужом job без вложенности.
            // Это открытый вопрос ТЗ (раздел 19) — UWP и контейнеры.
            return Err(Error::Win32 {
                call: "AssignProcessToJobObject",
                code,
            });
        }

        let frozen = FrozenProcess { job, pid };
        frozen.set_freeze(true)?;
        Ok(frozen)
    }

    fn set_freeze(&self, freeze: bool) -> Result<()> {
        let info = JobObjectFreezeInformation {
            flags: FREEZE_OPERATION,
            freeze: u8::from(freeze),
            ..Default::default()
        };

        let ok = unsafe {
            SetInformationJobObject(
                self.job,
                JOB_OBJECT_FREEZE_INFORMATION,
                (&info as *const JobObjectFreezeInformation).cast(),
                size_of::<JobObjectFreezeInformation>() as u32,
            )
        };

        if ok == 0 {
            return Err(Error::Win32 {
                call: "SetInformationJobObject(JobObjectFreezeInformation)",
                code: unsafe { GetLastError() },
            });
        }
        Ok(())
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Размораживает процесс. Забирает self: после разморозки держать
    /// дескриптор незачем.
    pub fn thaw(self) -> Result<()> {
        self.set_freeze(false)
        // Drop закроет дескриптор.
    }
}

impl Drop for FrozenProcess {
    fn drop(&mut self) {
        // Страховка: при любом пути выхода процесс размораживается.
        // Требование ТЗ (раздел 12.2): при остановке службы все job-объекты
        // размораживаются до закрытия дескрипторов.
        let _ = self.set_freeze(false);
        unsafe { CloseHandle(self.job) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};

    /// Подопытный: собственный дочерний процесс. Его можно замораживать
    /// и убивать, не рискуя ничем чужим.
    fn spawn_guinea_pig() -> Child {
        Command::new("cmd")
            .args(["/c", "ping -n 60 127.0.0.1 > nul"])
            .spawn()
            .expect("дочерний процесс не запустился")
    }

    #[test]
    fn a_child_process_freezes_and_thaws() {
        let mut child = spawn_guinea_pig();

        let frozen = FrozenProcess::freeze(child.id()).expect("заморозка не удалась");
        assert_eq!(frozen.pid(), child.id());
        // Процесс жив, просто стоит.
        assert!(
            child.try_wait().unwrap().is_none(),
            "процесс умер от заморозки"
        );

        frozen.thaw().expect("разморозка не удалась");
        assert!(
            child.try_wait().unwrap().is_none(),
            "процесс умер от разморозки"
        );

        child.kill().ok();
    }

    #[test]
    fn dropping_the_handle_thaws_instead_of_killing() {
        // Падение Bamboo не должно ни убить процесс, ни оставить его
        // замороженным навсегда.
        let mut child = spawn_guinea_pig();
        {
            let _frozen = FrozenProcess::freeze(child.id()).unwrap();
            // Дескриптор уходит из области видимости без thaw().
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "закрытие дескриптора убило процесс"
        );
        child.kill().ok();
    }

    #[test]
    fn the_kernel_and_ourselves_are_refused() {
        assert!(FrozenProcess::freeze(4).is_err());
        assert!(FrozenProcess::freeze(0).is_err());
        assert!(FrozenProcess::freeze(std::process::id()).is_err());
    }

    #[test]
    fn a_dead_pid_reports_an_error() {
        assert!(FrozenProcess::freeze(0x7FFF_FFF0).is_err());
    }
}
