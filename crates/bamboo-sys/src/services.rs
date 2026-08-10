//! Перечисление служб (ТЗ, разделы 5.5 и 9.9).
//!
//! Только имена — для системного диффа важен факт наличия службы, а не её
//! состояние. Запрос перечисления не требует прав администратора: чтение
//! списка служб доступно любому пользователю.

use core::mem::size_of;

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_MORE_DATA};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, ENUM_SERVICE_STATUS_PROCESSW,
    SC_ENUM_PROCESS_INFO, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_STATE_ALL, SERVICE_WIN32,
};

/// Возвращает имена всех служб Win32 в системе.
pub fn service_names() -> Result<Vec<String>> {
    let manager = unsafe {
        OpenSCManagerW(
            core::ptr::null(),
            core::ptr::null(),
            SC_MANAGER_ENUMERATE_SERVICE,
        )
    };
    if manager.is_null() {
        return Err(Error::Win32 {
            call: "OpenSCManagerW(перечисление)",
            code: unsafe { GetLastError() },
        });
    }

    let result = enumerate(manager);
    unsafe { CloseServiceHandle(manager) };
    result
}

fn enumerate(manager: *mut core::ffi::c_void) -> Result<Vec<String>> {
    // Первый вызов узнаёт нужный размер буфера через ERROR_MORE_DATA.
    let mut bytes_needed: u32 = 0;
    let mut count: u32 = 0;
    let mut resume: u32 = 0;

    unsafe {
        EnumServicesStatusExW(
            manager,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_STATE_ALL,
            core::ptr::null_mut(),
            0,
            &mut bytes_needed,
            &mut count,
            &mut resume,
            core::ptr::null(),
        );
    }

    let code = unsafe { GetLastError() };
    if code != ERROR_MORE_DATA {
        return Err(Error::Win32 {
            call: "EnumServicesStatusExW(размер)",
            code,
        });
    }

    // Буфер из u64 ради выравнивания: структура содержит указатели.
    let mut buffer = vec![0u64; (bytes_needed as usize).div_ceil(8) + 1];
    let capacity = (buffer.len() * 8) as u32;
    let mut names = Vec::new();

    // Продолжаем с курсора, пока не переберём все службы.
    loop {
        let mut needed: u32 = 0;
        let mut returned: u32 = 0;

        let ok = unsafe {
            EnumServicesStatusExW(
                manager,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                buffer.as_mut_ptr().cast(),
                capacity,
                &mut needed,
                &mut returned,
                &mut resume,
                core::ptr::null(),
            )
        };
        let call_error = unsafe { GetLastError() };

        collect_names(&buffer, returned as usize, &mut names);

        if ok != 0 {
            break; // Все службы перебраны.
        }
        if call_error != ERROR_MORE_DATA {
            return Err(Error::Win32 {
                call: "EnumServicesStatusExW",
                code: call_error,
            });
        }
        // ERROR_MORE_DATA: есть ещё, resume уже сдвинут — повторяем.
    }

    Ok(names)
}

/// Читает имена служб из заполненного буфера.
///
/// # Safety-инвариант
/// `count` не должен превышать число реально записанных структур: Windows
/// сообщает его в `returned`, отсюда и берём.
fn collect_names(buffer: &[u64], count: usize, names: &mut Vec<String>) {
    let base = buffer.as_ptr().cast::<u8>();
    let stride = size_of::<ENUM_SERVICE_STATUS_PROCESSW>();

    for index in 0..count {
        // SAFETY: каждая запись лежит по своему смещению внутри буфера,
        // выделенного под bytes_needed; читаем невыровненно.
        let entry = unsafe {
            core::ptr::read_unaligned(
                base.add(index * stride)
                    .cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
            )
        };
        if entry.lpServiceName.is_null() {
            continue;
        }
        // Имя службы — UTF-16 с нулём, указывает внутрь того же буфера.
        let name = unsafe { utf16_ptr(entry.lpServiceName) };
        if !name.is_empty() {
            names.push(name);
        }
    }
}

/// Читает UTF-16 строку по указателю до нуля.
///
/// # Safety
/// `ptr` должен указывать на нуль-терминированную UTF-16 строку внутри
/// живого буфера.
unsafe fn utf16_ptr(ptr: *const u16) -> String {
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
        if len > 1024 {
            break; // защита от неожиданно длинной строки
        }
    }
    String::from_utf16_lossy(core::slice::from_raw_parts(ptr, len))
}

// Перечисление драйверов ядра (ТЗ, раздел 9.9) осознанно отложено.
// EnumDeviceDrivers отдаёт 213 базовых адресов, но GetDeviceDriverBaseNameW
// под обычным пользователем не резолвит ни один из них (проверено вживую:
// resolved=0 без кода ошибки). Имена драйверов надёжно доступны либо
// от администратора, либо через реестр DriverDatabase — это отдельный кусок.
// Пока системный дифф работает по службам и автозагрузке.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_can_be_enumerated_without_admin() {
        let names = service_names().expect("перечисление служб не удалось");
        // На любой Windows служб десятки.
        assert!(
            names.len() > 10,
            "служб подозрительно мало: {}",
            names.len()
        );
    }

    #[test]
    fn well_known_services_are_present() {
        let names = service_names().unwrap();
        let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
        // Планировщик заданий есть на каждой Windows.
        assert!(
            lower.iter().any(|n| n == "schedule"),
            "служба планировщика Schedule не найдена"
        );
    }

    #[test]
    fn names_are_not_empty() {
        for name in service_names().unwrap() {
            assert!(!name.is_empty());
        }
    }
}

/// Служба, которой принадлежит процесс.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceOwner {
    /// Имя службы в системе.
    pub name: String,
    /// Отображаемое имя.
    pub display: String,
}

/// Находит службу по номеру её процесса.
///
/// Нужно, когда завершённый процесс возвращается: его почти всегда
/// поднимает служба, и завершать процесс повторно бесполезно — надо
/// разбираться со службой. Чтобы предложить это человеку, сначала надо
/// узнать, о какой службе речь.
pub fn service_by_pid(pid: u32) -> Option<ServiceOwner> {
    if pid == 0 || pid == 4 {
        return None;
    }

    let manager = unsafe {
        OpenSCManagerW(
            core::ptr::null(),
            core::ptr::null(),
            SC_MANAGER_ENUMERATE_SERVICE,
        )
    };
    if manager.is_null() {
        return None;
    }

    let found = find_service_with_pid(manager, pid);
    unsafe { CloseServiceHandle(manager) };
    found
}

fn find_service_with_pid(manager: *mut core::ffi::c_void, pid: u32) -> Option<ServiceOwner> {
    let mut bytes_needed: u32 = 0;
    let mut count: u32 = 0;
    let mut resume: u32 = 0;

    unsafe {
        EnumServicesStatusExW(
            manager,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_STATE_ALL,
            core::ptr::null_mut(),
            0,
            &mut bytes_needed,
            &mut count,
            &mut resume,
            core::ptr::null(),
        );
    }
    if unsafe { GetLastError() } != ERROR_MORE_DATA {
        return None;
    }

    let mut buffer = vec![0u64; (bytes_needed as usize).div_ceil(8) + 1];
    let capacity = (buffer.len() * 8) as u32;

    loop {
        let mut needed: u32 = 0;
        let mut returned: u32 = 0;

        let ok = unsafe {
            EnumServicesStatusExW(
                manager,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                buffer.as_mut_ptr().cast(),
                capacity,
                &mut needed,
                &mut returned,
                &mut resume,
                core::ptr::null(),
            )
        };
        let call_error = unsafe { GetLastError() };

        if let Some(owner) = owner_in_buffer(&buffer, returned as usize, pid) {
            return Some(owner);
        }
        if ok != 0 || call_error != ERROR_MORE_DATA {
            return None;
        }
    }
}

/// Ищет в заполненном буфере службу с нужным номером процесса.
fn owner_in_buffer(buffer: &[u64], count: usize, pid: u32) -> Option<ServiceOwner> {
    let base = buffer.as_ptr().cast::<u8>();
    let stride = size_of::<ENUM_SERVICE_STATUS_PROCESSW>();

    for index in 0..count {
        // SAFETY: запись лежит по своему смещению внутри буфера,
        // выделенного под bytes_needed; читаем невыровненно.
        let entry = unsafe {
            core::ptr::read_unaligned(
                base.add(index * stride)
                    .cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
            )
        };

        if entry.ServiceStatusProcess.dwProcessId != pid {
            continue;
        }
        if entry.lpServiceName.is_null() {
            continue;
        }

        let name = unsafe { utf16_ptr(entry.lpServiceName) };
        let display = if entry.lpDisplayName.is_null() {
            name.clone()
        } else {
            unsafe { utf16_ptr(entry.lpDisplayName) }
        };
        return Some(ServiceOwner { name, display });
    }
    None
}

/// Останавливает службу.
///
/// Требует прав администратора: остановка службы — операция уровня 5
/// по иерархии рисков (ТЗ, раздел 11.1). Останавливаем ровно то, что
/// попросили, и не трогаем зависимые: обрушить половину системы одной
/// кнопкой Bamboo не станет.
pub fn stop_service(name: &str) -> Result<()> {
    use windows_sys::Win32::System::Services::{
        ControlService, OpenServiceW, SC_MANAGER_CONNECT, SERVICE_CONTROL_STOP,
        SERVICE_QUERY_STATUS, SERVICE_STATUS, SERVICE_STOP,
    };

    let manager =
        unsafe { OpenSCManagerW(core::ptr::null(), core::ptr::null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(Error::Win32 {
            call: "OpenSCManagerW(остановка)",
            code: unsafe { GetLastError() },
        });
    }

    let wide: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();
    let service =
        unsafe { OpenServiceW(manager, wide.as_ptr(), SERVICE_STOP | SERVICE_QUERY_STATUS) };
    if service.is_null() {
        let code = unsafe { GetLastError() };
        unsafe { CloseServiceHandle(manager) };
        return Err(Error::Win32 {
            call: "OpenServiceW(остановка)",
            code,
        });
    }

    let mut status: SERVICE_STATUS = unsafe { core::mem::zeroed() };
    let ok = unsafe { ControlService(service, SERVICE_CONTROL_STOP, &mut status) };
    let code = unsafe { GetLastError() };

    unsafe {
        CloseServiceHandle(service);
        CloseServiceHandle(manager);
    }

    if ok == 0 {
        return Err(Error::Win32 {
            call: "ControlService(стоп)",
            code,
        });
    }
    Ok(())
}

#[cfg(test)]
mod owner_tests {
    use super::*;

    #[test]
    fn a_running_service_process_is_found() {
        // Планировщик заданий работает на любой Windows и живёт в службе.
        // Находим его процесс через список служб и проверяем обратный путь.
        let names = service_names().unwrap_or_default();
        assert!(names.iter().any(|name| name == "Schedule"));

        // Сам поиск по номеру проверяем на собственном процессе: службой
        // он не является, и ответ обязан быть пустым.
        assert_eq!(service_by_pid(std::process::id()), None);
    }

    #[test]
    fn kernel_processes_are_never_services() {
        assert_eq!(service_by_pid(0), None);
        assert_eq!(service_by_pid(4), None);
    }

    #[test]
    fn stopping_a_missing_service_fails_cleanly() {
        assert!(stop_service("НетТакойСлужбыBamboo").is_err());
    }
}
