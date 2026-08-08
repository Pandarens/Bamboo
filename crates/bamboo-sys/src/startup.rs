//! Автозагрузка пользователя (ТЗ, разделы 5.6 и 11.1, уровень 2).
//!
//! Пока только куст текущего пользователя: HKCU доступен агенту без прав
//! администратора. Машинная автозагрузка в HKLM появится вместе с брокером.
//!
//! Отключение — через `StartupApproved`, как это делает диспетчер задач:
//! сама запись в `Run` не удаляется, а помечается выключенной. Это и есть
//! обратимость: включить обратно — значит поменять один байт, а не
//! восстанавливать удалённую команду по памяти.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_BINARY,
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const APPROVED_KEY: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";

/// Первый байт блоба StartupApproved: чётный — включено, нечётный — выключено.
/// Диспетчер задач пишет 0x02 и 0x03 соответственно.
const ENABLED_MARKER: u8 = 0x02;
const DISABLED_MARKER: u8 = 0x03;

/// Элемент автозагрузки.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupItem {
    pub name: String,
    pub command: String,
    pub enabled: bool,
}

struct Key(HKEY);

impl Key {
    fn open(path: &str, access: u32) -> Result<Key> {
        let wide: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
        let mut handle: HKEY = core::ptr::null_mut();
        let status =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide.as_ptr(), 0, access, &mut handle) };
        if status != ERROR_SUCCESS {
            return Err(Error::Win32 {
                call: "RegOpenKeyExW",
                code: status,
            });
        }
        Ok(Key(handle))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

/// Перечисляет автозагрузку текущего пользователя.
pub fn user_startup_items() -> Result<Vec<StartupItem>> {
    let run = Key::open(RUN_KEY, KEY_READ)?;
    let mut items = Vec::new();

    for index in 0.. {
        let mut name = [0u16; 512];
        let mut name_len = name.len() as u32;
        let mut data = [0u8; 4096];
        let mut data_len = data.len() as u32;

        let status = unsafe {
            RegEnumValueW(
                run.0,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                data.as_mut_ptr(),
                &mut data_len,
            )
        };
        if status != ERROR_SUCCESS {
            break; // ERROR_NO_MORE_ITEMS или что-то серьёзное — список кончился.
        }

        let name = String::from_utf16_lossy(&name[..name_len as usize]);
        // Команда лежит как REG_SZ: UTF-16 с нулём на конце.
        let command: Vec<u16> = data[..data_len as usize]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect();

        items.push(StartupItem {
            enabled: is_enabled(&name)?,
            name,
            command: String::from_utf16_lossy(&command),
        });
    }

    Ok(items)
}

/// Включён ли элемент по данным StartupApproved.
///
/// Отсутствие записи означает «включён»: система создаёт блоб только
/// после первого отключения.
fn is_enabled(name: &str) -> Result<bool> {
    let approved = match Key::open(APPROVED_KEY, KEY_READ) {
        Ok(key) => key,
        // Куста может не быть вовсе — тогда всё включено.
        Err(_) => return Ok(true),
    };

    let wide: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();
    let mut data = [0u8; 32];
    let mut len = data.len() as u32;

    let status = unsafe {
        RegQueryValueExW(
            approved.0,
            wide.as_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            data.as_mut_ptr(),
            &mut len,
        )
    };

    if status != ERROR_SUCCESS || len == 0 {
        return Ok(true);
    }
    Ok(data[0] % 2 == 0)
}

/// Включает или выключает элемент автозагрузки текущего пользователя.
///
/// Возвращает прежнее состояние — оно уходит в журнал как состояние «до».
pub fn set_startup_enabled(name: &str, enabled: bool) -> Result<bool> {
    let before = is_enabled(name)?;

    let approved = Key::open(APPROVED_KEY, KEY_SET_VALUE)?;
    let wide: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();

    // 12 байт, как пишет диспетчер задач: маркер и время отключения.
    // Время оставляем нулевым — оно информационное.
    let mut blob = [0u8; 12];
    blob[0] = if enabled {
        ENABLED_MARKER
    } else {
        DISABLED_MARKER
    };

    let status = unsafe {
        RegSetValueExW(
            approved.0,
            wide.as_ptr(),
            0,
            REG_BINARY,
            blob.as_ptr(),
            blob.len() as u32,
        )
    };

    if status != ERROR_SUCCESS {
        return Err(Error::Win32 {
            call: "RegSetValueExW(StartupApproved)",
            code: status,
        });
    }
    Ok(before)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_startup_list_is_readable_without_elevation() {
        // Список может быть и пустым — важно, что чтение не падает
        // и каждый элемент имеет имя.
        let items = user_startup_items().expect("автозагрузка не прочиталась");
        for item in &items {
            assert!(!item.name.is_empty());
        }
    }

    #[test]
    fn an_absent_approved_entry_means_enabled() {
        // Записи с таким именем заведомо нет.
        assert!(is_enabled("bamboo-теста-такого-нет").unwrap());
    }

    #[test]
    fn toggling_round_trips_and_restores() {
        // Работаем только с собственной, специально созданной записью:
        // чужие элементы автозагрузки тестам трогать нельзя.
        let name = "bamboo-self-test";

        let before = set_startup_enabled(name, false).unwrap();
        assert!(!is_enabled(name).unwrap(), "элемент не выключился");

        set_startup_enabled(name, true).unwrap();
        assert!(is_enabled(name).unwrap(), "элемент не включился обратно");

        // Возвращаем как было (для несуществующей записи before = true).
        set_startup_enabled(name, before).unwrap();
    }
}
