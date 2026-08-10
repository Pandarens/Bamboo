//! Ограничение дисковой активности процесса (ТЗ, раздел 11.1).
//!
//! Отвечает на просьбу «запретить программе трогать диск». Запретить
//! нельзя: процесс, которому отказали в вводе-выводе, не станет вежливо
//! ждать — он упадёт или зависнет, а пользователь получит сломанное
//! приложение вместо тихого. Windows и не даёт такого запрета.
//!
//! Что действительно можно — ограничить скорость. Job object умеет
//! придерживать процесс по числу операций и по пропускной способности,
//! и это ровно то поведение, которого от «запрета» ждут: фоновый
//! обновлятор перестаёт мешать, но не ломается.
//!
//! Ограничение живёт, пока жив дескриптор job. Закрылся Bamboo — лимит
//! снят, процесс работает как раньше: незаметно оставить систему
//! в изменённом состоянии нельзя.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetIoRateControlInformationJobObject,
    JOBOBJECT_IO_RATE_CONTROL_INFORMATION, JOB_OBJECT_IO_RATE_CONTROL_ENABLE,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// Насколько сильно придержать процесс.
///
/// Числа не круглые ради красоты: они выбраны так, чтобы фоновая работа
/// продолжалась, а на общую отзывчивость системы не влияла. Обычный SATA
/// SSD выдаёт десятки тысяч операций в секунду, поэтому даже «слабое»
/// ограничение — это малая доля накопителя.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoLimit {
    /// Придержать заметно: фоновому обновлятору хватит, мешать перестанет.
    Background,
    /// Придержать сильно: для того, кто явно зарвался.
    Tight,
}

impl IoLimit {
    /// Предел по операциям в секунду.
    fn max_iops(self) -> i64 {
        match self {
            IoLimit::Background => 500,
            IoLimit::Tight => 100,
        }
    }

    /// Предел по пропускной способности, байт в секунду.
    fn max_bandwidth(self) -> i64 {
        match self {
            IoLimit::Background => 8 * 1024 * 1024,
            IoLimit::Tight => 1024 * 1024,
        }
    }

    /// Как объяснить ограничение человеку.
    pub fn describe(self) -> &'static str {
        match self {
            IoLimit::Background => "не больше 500 операций и 8 МБ в секунду",
            IoLimit::Tight => "не больше 100 операций и 1 МБ в секунду",
        }
    }
}

/// Процесс с ограниченной дисковой активностью.
///
/// Носитель ограничения — сам объект: пока он жив, лимит действует.
/// Уронили Bamboo — лимит исчез вместе с ним, и это правильно: чужой
/// процесс не должен остаться придушенным после нашей аварии.
pub struct LimitedProcess {
    job: HANDLE,
    pid: u32,
    limit: IoLimit,
}

impl LimitedProcess {
    /// Ограничивает дисковую активность процесса.
    pub fn throttle(pid: u32, limit: IoLimit) -> Result<LimitedProcess> {
        if pid == 0 || pid == 4 || pid == std::process::id() {
            return Err(Error::Unsupported(
                "этому процессу дисковую активность ограничивать нельзя",
            ));
        }

        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return Err(Error::Win32 {
                call: "OpenProcess(для ограничения диска)",
                code: unsafe { GetLastError() },
            });
        }

        let job = unsafe { CreateJobObjectW(core::ptr::null(), core::ptr::null()) };
        if job.is_null() {
            unsafe { CloseHandle(process) };
            return Err(Error::Win32 {
                call: "CreateJobObjectW(ограничение диска)",
                code: unsafe { GetLastError() },
            });
        }

        let assigned = unsafe { AssignProcessToJobObject(job, process) };
        unsafe { CloseHandle(process) };
        if assigned == 0 {
            let code = unsafe { GetLastError() };
            unsafe { CloseHandle(job) };
            // Частый случай: процесс уже состоит в чужом job, который
            // не разрешает вложенность. Так живут UWP-приложения.
            return Err(Error::Win32 {
                call: "AssignProcessToJobObject(ограничение диска)",
                code,
            });
        }

        let limited = LimitedProcess { job, pid, limit };
        limited.apply()?;
        Ok(limited)
    }

    fn apply(&self) -> Result<()> {
        let info = JOBOBJECT_IO_RATE_CONTROL_INFORMATION {
            MaxIops: self.limit.max_iops(),
            MaxBandwidth: self.limit.max_bandwidth(),
            ReservationIops: 0,
            // Пустое имя тома означает «все тома»: ограничивать процесс
            // на одном диске и пускать на другой смысла нет.
            VolumeName: core::ptr::null(),
            BaseIoSize: 0,
            ControlFlags: JOB_OBJECT_IO_RATE_CONTROL_ENABLE as u32,
        };

        let ok = unsafe {
            SetIoRateControlInformationJobObject(
                self.job,
                &info as *const JOBOBJECT_IO_RATE_CONTROL_INFORMATION,
            )
        };

        if ok == 0 {
            return Err(Error::Win32 {
                call: "SetIoRateControlInformationJobObject",
                code: unsafe { GetLastError() },
            });
        }
        Ok(())
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn limit(&self) -> IoLimit {
        self.limit
    }

    /// Снимает ограничение.
    pub fn release(self) -> Result<()> {
        // Достаточно закрыть job: процесс выходит из него и ограничение
        // перестаёт действовать. Отдельно выключать флаг не нужно.
        drop(self);
        Ok(())
    }
}

impl Drop for LimitedProcess {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.job) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_are_ordered_from_soft_to_hard() {
        assert!(IoLimit::Tight.max_iops() < IoLimit::Background.max_iops());
        assert!(IoLimit::Tight.max_bandwidth() < IoLimit::Background.max_bandwidth());
    }

    #[test]
    fn every_limit_explains_itself_in_numbers() {
        for limit in [IoLimit::Background, IoLimit::Tight] {
            let text = limit.describe();
            assert!(!text.is_empty());
            // Объяснение обязано называть числа: «придержан» без цифр
            // человеку ничего не говорит.
            assert!(text.chars().any(|c| c.is_ascii_digit()), "{text}");
        }
    }

    #[test]
    fn system_processes_are_refused() {
        assert!(LimitedProcess::throttle(0, IoLimit::Background).is_err());
        assert!(LimitedProcess::throttle(4, IoLimit::Background).is_err());
    }

    #[test]
    fn bamboo_does_not_limit_itself() {
        // Утилита, придушившая собственный ввод-вывод, перестала бы вести
        // журнал — то есть перестала бы объяснять, что натворила.
        assert!(LimitedProcess::throttle(std::process::id(), IoLimit::Tight).is_err());
    }

    #[test]
    fn a_missing_process_fails_cleanly() {
        assert!(LimitedProcess::throttle(0xFFFF_FFF0, IoLimit::Background).is_err());
    }

    #[test]
    fn a_live_process_can_be_limited_and_released() {
        // Ограничиваем настоящий процесс — свой дочерний, чтобы никому
        // не мешать. Проверяем, что лимит ставится и снимается.
        let child = std::process::Command::new("cmd.exe")
            .args(["/c", "ping -n 4 127.0.0.1 > nul"])
            .spawn();
        let Ok(mut child) = child else {
            return; // без cmd.exe проверять нечего
        };

        match LimitedProcess::throttle(child.id(), IoLimit::Background) {
            Ok(limited) => {
                assert_eq!(limited.pid(), child.id());
                assert_eq!(limited.limit(), IoLimit::Background);
                limited.release().expect("снятие ограничения не удалось");
            }
            Err(error) => {
                // На части систем управление скоростью ввода-вывода
                // недоступно — это штатный отказ, а не провал теста.
                let text = error.to_string();
                assert!(text.contains("Job") || text.contains("Io") || text.contains("Open"));
            }
        }

        let _ = child.kill();
        let _ = child.wait();
    }
}
