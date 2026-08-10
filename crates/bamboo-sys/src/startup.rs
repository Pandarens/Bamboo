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
    HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_SZ,
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

/// Имя, под которым Bamboo прописывается в автозапуск.
pub const STARTUP_NAME: &str = "Bamboo";

/// Строка в кодировке Windows с завершающим нулём.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Стоит ли Bamboo в автозапуске.
pub fn is_in_startup() -> bool {
    let Ok(run) = Key::open(RUN_KEY, KEY_READ) else {
        return false;
    };
    read_value(&run, STARTUP_NAME).is_some()
}

/// Добавляет Bamboo в автозапуск текущего пользователя.
///
/// Пишем в раздел пользователя, а не машины: для машинного нужны права
/// администратора, а наблюдатель за системой должен ставиться без них.
/// Путь берём собственный — тот, откуда программа запущена сейчас.
pub fn add_to_startup() -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|_| Error::Unsupported("не удалось определить путь к себе"))?;
    // Путь в кавычках: без них пробел в пути превратит команду в две.
    let command = format!("\"{}\"", exe.to_string_lossy());

    let run = Key::open(RUN_KEY, KEY_SET_VALUE)?;
    let name = wide(STARTUP_NAME);
    let value = wide(&command);

    let status = unsafe {
        RegSetValueExW(
            run.0,
            name.as_ptr(),
            0,
            REG_SZ,
            value.as_ptr().cast(),
            (value.len() * 2) as u32,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(Error::Win32 {
            call: "RegSetValueExW(автозапуск)",
            code: status,
        });
    }
    Ok(())
}

/// Убирает Bamboo из автозапуска.
pub fn remove_from_startup() -> Result<()> {
    use windows_sys::Win32::System::Registry::RegDeleteValueW;

    let run = Key::open(RUN_KEY, KEY_SET_VALUE)?;
    let name = wide(STARTUP_NAME);

    let status = unsafe { RegDeleteValueW(run.0, name.as_ptr()) };
    // Значения не было — цель достигнута, это не ошибка.
    if status != ERROR_SUCCESS && status != 2 {
        return Err(Error::Win32 {
            call: "RegDeleteValueW(автозапуск)",
            code: status,
        });
    }
    Ok(())
}

/// Читает строковое значение из ключа.
fn read_value(key: &Key, name: &str) -> Option<String> {
    let wide_name = wide(name);
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;

    let status = unsafe {
        RegQueryValueExW(
            key.0,
            wide_name.as_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS || size < 2 {
        return None;
    }
    let chars = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buffer[..chars]))
}

#[cfg(test)]
mod autostart_tests {
    use super::*;
    use std::sync::Mutex;

    /// Оба теста правят один и тот же ключ реестра. Параллельно они
    /// гонялись бы за него и мешали друг другу, поэтому сериализуем.
    static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn adding_and_removing_autostart_works() {
        let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Проверяем на живом реестре: раздел пользователя, прав не нужно.
        // Прибираем за собой в любом случае.
        let was_there = is_in_startup();

        add_to_startup().expect("добавление в автозапуск не удалось");
        assert!(is_in_startup(), "после добавления запись должна быть");

        remove_from_startup().expect("удаление из автозапуска не удалось");
        assert!(!is_in_startup(), "после удаления записи быть не должно");

        // Возвращаем как было, чтобы тест не менял настройки машины.
        if was_there {
            add_to_startup().ok();
        }
    }

    #[test]
    fn removing_what_is_not_there_is_not_an_error() {
        let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let was_there = is_in_startup();
        remove_from_startup().expect("первое удаление");
        // Повторное удаление — цель уже достигнута.
        remove_from_startup().expect("повторное удаление не должно падать");

        if was_there {
            add_to_startup().ok();
        }
    }
}
