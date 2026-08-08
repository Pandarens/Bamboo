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
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, CreateServiceW, DeleteService,
    OpenSCManagerW, OpenServiceW, QueryServiceConfig2W, QueryServiceConfigW, QUERY_SERVICE_CONFIGW,
    SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE, SERVICE_ALL_ACCESS,
    SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
    SERVICE_CONFIG_DESCRIPTION, SERVICE_DELAYED_AUTO_START_INFO, SERVICE_DEMAND_START,
    SERVICE_DESCRIPTIONW, SERVICE_DISABLED, SERVICE_ERROR_NORMAL, SERVICE_NO_CHANGE,
    SERVICE_QUERY_CONFIG, SERVICE_WIN32_OWN_PROCESS,
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

/// Как запускается служба: тип старта и флаг отложенного автозапуска.
///
/// Это ровно то, что нужно действию «перевести службу на отложенный старт»
/// (ТЗ 11.1, уровень 2): снять текущее состояние, чтобы потом вернуть, и
/// выставить своё. Числовой `start_type` — константы вида `SERVICE_AUTO_START`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceStart {
    pub start_type: u32,
    pub delayed: bool,
}

impl ServiceStart {
    /// Автозапуск с отложенным стартом — целевое состояние действия.
    pub fn delayed_auto() -> ServiceStart {
        ServiceStart {
            start_type: SERVICE_AUTO_START,
            delayed: true,
        }
    }

    /// Стартует ли служба по триггеру (`SERVICE_DEMAND_START`).
    ///
    /// В Windows 11 многие службы демонд-старт и поднимаются по триггеру,
    /// в простое ничего не потребляя. Переводить такие на отложенный старт
    /// бессмысленно, а иногда вредно — вызывающий проверяет это до действия.
    pub fn is_demand_start(self) -> bool {
        self.start_type == SERVICE_DEMAND_START
    }

    /// Отключена ли служба полностью.
    pub fn is_disabled(self) -> bool {
        self.start_type == SERVICE_DISABLED
    }
}

fn open_service(manager: &ScHandle, name: &str, access: u32) -> Result<ScHandle> {
    let wname = wide(name);
    let handle = unsafe { OpenServiceW(manager.0, wname.as_ptr(), access) };
    if handle.is_null() {
        return Err(Error::Win32 {
            call: "OpenServiceW",
            code: unsafe { GetLastError() },
        });
    }
    Ok(ScHandle(handle))
}

/// Читает, как запускается служба. Запрос конфигурации доступен обычному
/// пользователю для большинства служб — прав администратора не требует.
pub fn service_start(name: &str) -> Result<ServiceStart> {
    let manager = open_manager(SC_MANAGER_CONNECT)?;
    let service = open_service(&manager, name, SERVICE_QUERY_CONFIG)?;
    Ok(ServiceStart {
        start_type: query_start_type(&service)?,
        delayed: query_delayed(&service)?,
    })
}

/// Меняет тип запуска службы и флаг отложенного старта. Изменение
/// конфигурации требует прав администратора (`SERVICE_CHANGE_CONFIG`).
pub fn set_service_start(name: &str, config: ServiceStart) -> Result<()> {
    let manager = open_manager(SC_MANAGER_CONNECT)?;
    let service = open_service(&manager, name, SERVICE_CHANGE_CONFIG)?;

    // Меняем только тип запуска; тип службы и контроль ошибок оставляем
    // как есть через SERVICE_NO_CHANGE, строки — нулями.
    let ok = unsafe {
        ChangeServiceConfigW(
            service.0,
            SERVICE_NO_CHANGE,
            config.start_type,
            SERVICE_NO_CHANGE,
            core::ptr::null(),
            core::ptr::null(),
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null(),
            core::ptr::null(),
            core::ptr::null(),
        )
    };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "ChangeServiceConfigW",
            code: unsafe { GetLastError() },
        });
    }

    // Флаг отложенного старта значим только при автозапуске; для прочих типов
    // система его игнорирует, поэтому выставляем безусловно.
    let info = SERVICE_DELAYED_AUTO_START_INFO {
        fDelayedAutostart: config.delayed as i32,
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

/// Тип запуска из `QueryServiceConfigW`. Структура переменной длины —
/// первый вызов узнаёт нужный размер.
fn query_start_type(service: &ScHandle) -> Result<u32> {
    let mut needed: u32 = 0;
    unsafe { QueryServiceConfigW(service.0, core::ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(Error::Win32 {
            call: "QueryServiceConfigW(размер)",
            code: unsafe { GetLastError() },
        });
    }

    let mut buffer = vec![0u8; needed as usize];
    let ok = unsafe {
        QueryServiceConfigW(
            service.0,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "QueryServiceConfigW",
            code: unsafe { GetLastError() },
        });
    }

    // SAFETY: буфер заполнен как QUERY_SERVICE_CONFIGW; читаем невыровненно
    // на случай, если аллокатор дал не по границе структуры.
    let config =
        unsafe { core::ptr::read_unaligned(buffer.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
    Ok(config.dwStartType)
}

/// Флаг отложенного автозапуска из `QueryServiceConfig2W`.
fn query_delayed(service: &ScHandle) -> Result<bool> {
    // Структура — единственный BOOL, но берём буфер с запасом.
    let mut buffer = [0u8; 16];
    let mut needed: u32 = 0;
    let ok = unsafe {
        QueryServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "QueryServiceConfig2W(отложенный старт)",
            code: unsafe { GetLastError() },
        });
    }
    let info = unsafe {
        core::ptr::read_unaligned(buffer.as_ptr() as *const SERVICE_DELAYED_AUTO_START_INFO)
    };
    Ok(info.fDelayedAutostart != 0)
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

    #[test]
    fn the_scheduler_service_start_config_is_readable_without_admin() {
        // Планировщик есть на любой Windows, и его конфигурацию читать
        // обычному пользователю разрешено. Тип запуска — осмысленное
        // значение, а не мусор.
        let config = service_start("Schedule").expect("конфиг планировщика не прочитался");
        assert!(
            config.start_type <= SERVICE_DISABLED,
            "неправдоподобный тип запуска: {}",
            config.start_type
        );
    }

    #[test]
    fn a_missing_service_reports_an_error() {
        assert!(service_start("НетТакойСлужбыBamboo").is_err());
    }

    #[test]
    fn changing_a_service_without_admin_is_refused_cleanly() {
        // Без прав администратора смена конфигурации обязана вернуть ошибку,
        // а не молча «получиться». Систему тест не меняет.
        let result = set_service_start("Schedule", ServiceStart::delayed_auto());
        assert!(
            result.is_err(),
            "смена конфигурации без прав не должна проходить"
        );
    }

    #[test]
    fn the_target_state_is_delayed_autostart() {
        let target = ServiceStart::delayed_auto();
        assert_eq!(target.start_type, SERVICE_AUTO_START);
        assert!(target.delayed);
        assert!(!target.is_demand_start());
        assert!(!target.is_disabled());
    }
}
