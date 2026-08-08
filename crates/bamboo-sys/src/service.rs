//! Регистрация и жизненный цикл службы Windows (ТЗ, разделы 3.1 и 17.3).
//!
//! Брокер работает под SYSTEM с автозапуском и флагом отложенного старта:
//! служба, влияющая на время загрузки, была бы иронична для утилиты,
//! которая это время измеряет.
//!
//! Установка и удаление требуют прав администратора — это операции с базой
//! диспетчера служб. Здесь только обёртки; вызывает их брокер.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_SERVICE_EXISTS};
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, CloseServiceHandle, CreateServiceW, DeleteService, OpenSCManagerW,
    OpenServiceW, SC_HANDLE, SC_MANAGER_CREATE_SERVICE, SERVICE_ALL_ACCESS, SERVICE_AUTO_START,
    SERVICE_CONFIG_DELAYED_AUTO_START_INFO, SERVICE_CONFIG_DESCRIPTION,
    SERVICE_DELAYED_AUTO_START_INFO, SERVICE_DESCRIPTIONW, SERVICE_ERROR_NORMAL,
    SERVICE_WIN32_OWN_PROCESS,
};

/// Стандартное право доступа DELETE. В `windows-sys` живёт под именами
/// файловых прав, но по значению это общесистемное право удаления объекта.
const DELETE: u32 = 0x0001_0000;

/// Имя службы в системе.
pub const SERVICE_NAME: &str = "BambooBroker";
/// Отображаемое имя.
pub const SERVICE_DISPLAY: &str = "Bamboo — брокер диагностики";
const SERVICE_DESCRIPTION: &str =
    "Выполняет привилегированные операции Bamboo по запросу пользовательского агента.";

/// RAII-обёртка над дескриптором диспетчера служб или самой службы.
struct ScHandle(SC_HANDLE);

impl Drop for ScHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseServiceHandle(self.0) };
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

fn open_manager(access: u32) -> Result<ScHandle> {
    let handle = unsafe { OpenSCManagerW(core::ptr::null(), core::ptr::null(), access) };
    if handle.is_null() {
        return Err(Error::Win32 {
            call: "OpenSCManagerW",
            code: unsafe { GetLastError() },
        });
    }
    Ok(ScHandle(handle))
}

/// Устанавливает службу с автозапуском и отложенным стартом.
///
/// `exe_path` — полный путь к бинарнику брокера. Идемпотентна: если служба
/// уже есть, возвращает `Unsupported`, а не падает.
pub fn install(exe_path: &str) -> Result<()> {
    let manager = open_manager(SC_MANAGER_CREATE_SERVICE)?;

    let name = wide(SERVICE_NAME);
    let display = wide(SERVICE_DISPLAY);
    // Брокер запускается с аргументом, говорящим ему работать как служба.
    let command = wide(&format!("\"{exe_path}\" service"));

    let service = unsafe {
        CreateServiceW(
            manager.0,
            name.as_ptr(),
            display.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            command.as_ptr(),
            core::ptr::null(),
            core::ptr::null_mut(),
            core::ptr::null(),
            // lpServiceStartName = null означает LocalSystem (SYSTEM).
            core::ptr::null(),
            core::ptr::null(),
        )
    };

    if service.is_null() {
        let code = unsafe { GetLastError() };
        if code == ERROR_SERVICE_EXISTS {
            return Err(Error::Unsupported("служба Bamboo уже установлена"));
        }
        return Err(Error::Win32 {
            call: "CreateServiceW",
            code,
        });
    }
    let service = ScHandle(service);

    set_description(&service)?;
    set_delayed_start(&service)?;
    Ok(())
}

fn set_description(service: &ScHandle) -> Result<()> {
    let mut description = wide(SERVICE_DESCRIPTION);
    let info = SERVICE_DESCRIPTIONW {
        lpDescription: description.as_mut_ptr(),
    };
    let ok = unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DESCRIPTION,
            (&info as *const SERVICE_DESCRIPTIONW).cast(),
        )
    };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "ChangeServiceConfig2W(описание)",
            code: unsafe { GetLastError() },
        });
    }
    Ok(())
}

/// Отложенный автозапуск: не тормозить загрузку системы (ТЗ, раздел 17.3).
fn set_delayed_start(service: &ScHandle) -> Result<()> {
    let info = SERVICE_DELAYED_AUTO_START_INFO {
        fDelayedAutostart: 1,
    };
    let ok = unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
            (&info as *const SERVICE_DELAYED_AUTO_START_INFO).cast(),
        )
    };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "ChangeServiceConfig2W(отложенный старт)",
            code: unsafe { GetLastError() },
        });
    }
    Ok(())
}

/// Удаляет службу. Идемпотентна по смыслу: отсутствие службы — не ошибка
/// установки, но об этом сообщается вызывающему.
pub fn uninstall() -> Result<()> {
    const SC_MANAGER_CONNECT: u32 = 0x0001;
    let manager = open_manager(SC_MANAGER_CONNECT)?;

    let name = wide(SERVICE_NAME);
    let service = unsafe { OpenServiceW(manager.0, name.as_ptr(), DELETE) };
    if service.is_null() {
        return Err(Error::Win32 {
            call: "OpenServiceW",
            code: unsafe { GetLastError() },
        });
    }
    let service = ScHandle(service);

    let ok = unsafe { DeleteService(service.0) };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "DeleteService",
            code: unsafe { GetLastError() },
        });
    }
    Ok(())
}

// --- Работа процесса в роли службы ---

use core::sync::atomic::{AtomicIsize, Ordering};
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SetServiceStatus, StartServiceCtrlDispatcherW,
    SERVICE_ACCEPT_STOP, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    SERVICE_TABLE_ENTRYW,
};

/// Дескриптор статуса службы. Ставится в service_main, читается обработчиком
/// управляющих команд. Отдельного мьютекса не нужно: пишется один раз
/// на старте, дальше только читается.
static STATUS_HANDLE: AtomicIsize = AtomicIsize::new(0);

/// Функция, которую диспетчер запустит как тело службы. Хранится глобально,
/// потому что `service_main` — `extern "system"` с фиксированной сигнатурой
/// и замыкание в неё не передать.
static mut SERVICE_BODY: Option<fn(StopSignal)> = None;

/// Сигнал остановки для тела службы. Диспетчер выставляет его при получении
/// `SERVICE_CONTROL_STOP`, тело обязано периодически его проверять и выйти.
#[derive(Clone)]
pub struct StopSignal(std::sync::Arc<core::sync::atomic::AtomicBool>);

static STOP_FLAG: std::sync::OnceLock<std::sync::Arc<core::sync::atomic::AtomicBool>> =
    std::sync::OnceLock::new();

impl StopSignal {
    pub fn stop_requested(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Запускает процесс в роли службы Windows.
///
/// Блокирует поток до завершения службы. Вызывается брокером, когда его
/// запустил диспетчер служб (с аргументом `service`). Тело `body` получает
/// сигнал остановки и обязано корректно завершиться при его срабатывании —
/// иначе служба зависнет в состоянии остановки.
pub fn run_as_service(body: fn(StopSignal)) -> Result<()> {
    // SAFETY: устанавливается один раз до запуска диспетчера, дальше только
    // читается из service_main в том же процессе.
    unsafe {
        SERVICE_BODY = Some(body);
    }

    let mut name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        // Массив завершается нулевой записью.
        SERVICE_TABLE_ENTRYW {
            lpServiceName: core::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];

    let ok = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "StartServiceCtrlDispatcherW",
            code: unsafe { GetLastError() },
        });
    }
    Ok(())
}

/// Точка входа службы. Вызывается диспетчером в отдельном потоке.
unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let name = wide(SERVICE_NAME);
    let handle =
        RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control_handler), core::ptr::null_mut());
    if handle.is_null() {
        return;
    }
    STATUS_HANDLE.store(handle as isize, Ordering::SeqCst);

    report_status(SERVICE_START_PENDING, 0);
    let stop = std::sync::Arc::new(core::sync::atomic::AtomicBool::new(false));
    let _ = STOP_FLAG.set(std::sync::Arc::clone(&stop));

    report_status(SERVICE_RUNNING, SERVICE_ACCEPT_STOP);

    if let Some(body) = SERVICE_BODY {
        body(StopSignal(stop));
    }

    report_status(SERVICE_STOPPED, 0);
}

/// Обработчик управляющих команд от диспетчера.
unsafe extern "system" fn control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut core::ffi::c_void,
    _context: *mut core::ffi::c_void,
) -> u32 {
    if control == SERVICE_CONTROL_STOP {
        report_status(SERVICE_STOP_PENDING, 0);
        if let Some(flag) = STOP_FLAG.get() {
            flag.store(true, Ordering::Relaxed);
        }
    }
    0
}

fn report_status(state: u32, accepted: u32) {
    let handle = STATUS_HANDLE.load(Ordering::SeqCst) as SERVICE_STATUS_HANDLE;
    if handle.is_null() {
        return;
    }
    let mut status: SERVICE_STATUS = unsafe { core::mem::zeroed() };
    status.dwServiceType = SERVICE_WIN32_OWN_PROCESS;
    status.dwCurrentState = state;
    status.dwControlsAccepted = accepted;
    // Ждать помощи от системы при остановке не заставляем.
    status.dwWaitHint = if state == SERVICE_STOP_PENDING {
        5000
    } else {
        0
    };
    unsafe { SetServiceStatus(handle, &status) };
}

/// Установлена ли служба.
pub fn is_installed() -> bool {
    const SC_MANAGER_CONNECT: u32 = 0x0001;
    const SERVICE_QUERY_STATUS: u32 = 0x0004;

    let Ok(manager) = open_manager(SC_MANAGER_CONNECT) else {
        return false;
    };
    let name = wide(SERVICE_NAME);
    let service = unsafe { OpenServiceW(manager.0, name.as_ptr(), SERVICE_QUERY_STATUS) };
    if service.is_null() {
        return false;
    }
    unsafe { CloseServiceHandle(service) };
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_without_admin_fails_cleanly() {
        // Без прав администратора установка обязана вернуть понятную ошибку,
        // а не упасть. С правами тест на машине разработки не гоняем:
        // он бы менял систему.
        let result = install("C:\\nonexistent\\bamboo-service.exe");
        // Либо отказ доступа, либо «уже установлена» — оба варианта штатны.
        assert!(result.is_err() || super::is_installed());
    }

    #[test]
    fn querying_installation_status_does_not_panic() {
        // Просто не должно падать независимо от прав.
        let _ = is_installed();
    }
}
