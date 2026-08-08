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
