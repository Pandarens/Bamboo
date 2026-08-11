//! Настройки Bamboo в реестре.
//!
//! Хранятся в разделе текущего пользователя: настройки — дело личное,
//! и прав администратора для них требовать незачем.
//!
//! Формат простой намеренно: несколько флагов, которые человек может
//! посмотреть и поправить руками через `regedit`, если понадобится.
//! Прятать свои настройки в бинарный файл — недружелюбно.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE,
};

/// Где живут настройки.
const SETTINGS_KEY: &str = "Software\\Bamboo";

/// Показывать ли виджет сразу при запуске.
const SHOW_WIDGET: &str = "ShowWidgetOnStart";

/// Разрешение на самостоятельную оптимизацию.
const AUTOPILOT: &str = "Autopilot";

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Открывает раздел настроек, создавая его при первом обращении.
fn open(access: u32) -> Result<Key> {
    let path = wide(SETTINGS_KEY);
    let mut handle: HKEY = core::ptr::null_mut();
    let mut disposition: u32 = 0;

    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            0,
            core::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            access,
            core::ptr::null(),
            &mut handle,
            &mut disposition,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(Error::Win32 {
            call: "RegCreateKeyExW(настройки)",
            code: status,
        });
    }
    Ok(Key(handle))
}

/// Читает флаг. Значение по умолчанию возвращается, если настройки ещё нет.
fn read_flag(name: &str, default: bool) -> bool {
    let Ok(key) = open(KEY_READ) else {
        return default;
    };
    let wide_name = wide(name);
    let mut value: u32 = 0;
    let mut size = core::mem::size_of::<u32>() as u32;

    let status = unsafe {
        RegQueryValueExW(
            key.0,
            wide_name.as_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return default;
    }
    value != 0
}

/// Записывает строковое значение настройки.
fn write_string(name: &str, value: &str) -> Result<()> {
    use windows_sys::Win32::System::Registry::REG_SZ;

    let key = open(KEY_SET_VALUE)?;
    let wide_name = wide(name);
    let wide_value = wide(value);

    let status = unsafe {
        RegSetValueExW(
            key.0,
            wide_name.as_ptr(),
            0,
            REG_SZ,
            wide_value.as_ptr().cast(),
            // Длина в байтах вместе с завершающим нулём: без него
            // строка в реестре останется без конца.
            (wide_value.len() * 2) as u32,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(Error::Win32 {
            call: "RegSetValueExW(строка настройки)",
            code: status,
        });
    }
    Ok(())
}

fn write_flag(name: &str, enabled: bool) -> Result<()> {
    let key = open(KEY_SET_VALUE)?;
    let wide_name = wide(name);
    let value: u32 = u32::from(enabled);

    let status = unsafe {
        RegSetValueExW(
            key.0,
            wide_name.as_ptr(),
            0,
            REG_DWORD,
            (&value as *const u32).cast(),
            core::mem::size_of::<u32>() as u32,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(Error::Win32 {
            call: "RegSetValueExW(настройки)",
            code: status,
        });
    }
    Ok(())
}

/// Показывать ли виджет при запуске.
///
/// По умолчанию нет: Bamboo — фоновый наблюдатель, и вылезать на экран
/// без спроса ему незачем. Виджет открывается из трея, когда нужен.
pub fn show_widget_on_start() -> bool {
    read_flag(SHOW_WIDGET, false)
}

/// Запоминает, показывать ли виджет при запуске.
pub fn set_show_widget_on_start(enabled: bool) -> Result<()> {
    write_flag(SHOW_WIDGET, enabled)
}

/// Разрешена ли самостоятельная оптимизация.
///
/// По умолчанию нет, и это принципиально: вмешательство без спроса человек
/// должен разрешить сам. Утилита, которая начинает распоряжаться чужим
/// компьютером сразу после установки, — ровно то, чем Bamboo быть не должен.
pub fn autopilot_enabled() -> bool {
    read_flag(AUTOPILOT, false)
}

/// Запоминает разрешение на самостоятельную оптимизацию.
pub fn set_autopilot_enabled(enabled: bool) -> Result<()> {
    write_flag(AUTOPILOT, enabled)
}

/// Читает строковое значение из реестра.
///
/// Общий помощник: им пользуется поиск установленных игр, чтобы спросить
/// у Steam, куда он себя поставил, вместо того чтобы гадать по
/// «Program Files (x86)» — его ставят и на другой диск.
///
/// `None` — ключа нет либо значение не строковое. Это штатный исход:
/// программы, о которой спрашиваем, может не быть на машине вовсе.
pub(crate) fn registry_string(hive: &str, path: &str, value: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        RegOpenKeyExW, RegQueryValueExW, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY,
    };

    let root = match hive {
        "HKLM" => HKEY_LOCAL_MACHINE,
        _ => HKEY_CURRENT_USER,
    };

    let wide_path: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
    let mut handle: HKEY = core::ptr::null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            root,
            wide_path.as_ptr(),
            0,
            KEY_READ | KEY_WOW64_64KEY,
            &mut handle,
        )
    };
    if status != 0 {
        return None;
    }
    let handle = OwnedKey(handle);

    let wide_value: Vec<u16> = value.encode_utf16().chain(core::iter::once(0)).collect();
    let mut kind: u32 = 0;
    let mut size: u32 = 0;
    let status = unsafe {
        RegQueryValueExW(
            handle.0,
            wide_value.as_ptr(),
            core::ptr::null(),
            &mut kind,
            core::ptr::null_mut(),
            &mut size,
        )
    };
    // Пути в реестре бывают длинными, но не такими: предел защищает
    // от попытки выделить нелепый буфер по испорченному значению.
    if status != 0 || size == 0 || size as usize > 64 * 1024 {
        return None;
    }

    let mut buffer = vec![0u8; size as usize];
    let status = unsafe {
        RegQueryValueExW(
            handle.0,
            wide_value.as_ptr(),
            core::ptr::null(),
            &mut kind,
            buffer.as_mut_ptr(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }

    let words: Vec<u16> = buffer
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|word| *word != 0)
        .collect();
    Some(String::from_utf16_lossy(&words)).filter(|text| !text.is_empty())
}

/// Ключ реестра, закрывающийся сам.
struct OwnedKey(HKEY);

impl Drop for OwnedKey {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

/// Читает числовое значение из реестра.
///
/// Спутник `registry_string`: тип и порядок запуска службы записаны
/// числами, а не строками.
pub(crate) fn registry_u32(hive: &str, path: &str, value: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{
        RegOpenKeyExW, RegQueryValueExW, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY,
    };

    let root = match hive {
        "HKLM" => HKEY_LOCAL_MACHINE,
        _ => HKEY_CURRENT_USER,
    };

    let wide_path: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
    let mut handle: HKEY = core::ptr::null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            root,
            wide_path.as_ptr(),
            0,
            KEY_READ | KEY_WOW64_64KEY,
            &mut handle,
        )
    };
    if status != 0 {
        return None;
    }
    let handle = OwnedKey(handle);

    let wide_value: Vec<u16> = value.encode_utf16().chain(core::iter::once(0)).collect();
    let mut kind: u32 = 0;
    let mut number: u32 = 0;
    let mut size = core::mem::size_of::<u32>() as u32;

    let status = unsafe {
        RegQueryValueExW(
            handle.0,
            wide_value.as_ptr(),
            core::ptr::null(),
            &mut kind,
            (&mut number as *mut u32).cast(),
            &mut size,
        )
    };
    (status == 0).then_some(number)
}

/// Язык интерфейса.
const LANGUAGE: &str = "Language";

/// Выбранный язык интерфейса: «ru» либо «en».
///
/// По умолчанию берём у системы. Человеку, у которого Windows на русском,
/// показывать английский незачем, и наоборот — заставлять его лезть
/// в настройки при первом запуске тоже.
pub fn language() -> String {
    if let Some(chosen) = registry_string("HKCU", SETTINGS_KEY, LANGUAGE) {
        if chosen == "ru" || chosen == "en" {
            return chosen;
        }
    }
    if system_prefers_russian() {
        "ru".to_string()
    } else {
        "en".to_string()
    }
}

/// Запоминает выбранный язык.
pub fn set_language(code: &str) -> Result<()> {
    if code != "ru" && code != "en" {
        return Err(Error::Unsupported("такого языка интерфейса нет"));
    }
    write_string(LANGUAGE, code)
}

/// Русский ли язык у системы.
fn system_prefers_russian() -> bool {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

    let mut buffer = [0u16; 85];
    let length = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    if length <= 0 {
        return false;
    }

    let name = String::from_utf16_lossy(&buffer[..(length - 1).max(0) as usize]);
    name.to_lowercase().starts_with("ru")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Тесты правят один и тот же раздел реестра — сериализуем.
    static LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn a_flag_survives_a_round_trip() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let was = show_widget_on_start();

        set_show_widget_on_start(true).expect("запись настройки не удалась");
        assert!(show_widget_on_start());

        set_show_widget_on_start(false).expect("запись настройки не удалась");
        assert!(!show_widget_on_start());

        // Возвращаем как было: тест не должен менять настройки машины.
        set_show_widget_on_start(was).ok();
    }

    #[test]
    fn an_unknown_flag_falls_back_to_its_default() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Настройки с таким именем нет и не будет — берётся умолчание.
        assert!(read_flag("НетТакойНастройки", true));
        assert!(!read_flag("НетТакойНастройки", false));
    }
}

#[cfg(test)]
mod language_tests {
    use super::*;
    use std::sync::Mutex;

    /// Настройка одна на процесс: тесты, меняющие её, нельзя пускать разом.
    static LANGUAGE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn the_language_defaults_to_the_system_one() {
        // Человеку с русской Windows показывать английский незачем,
        // и заставлять его лезть в настройки при первом запуске тоже.
        let _guard = LANGUAGE_LOCK.lock().unwrap();
        let was = registry_string("HKCU", SETTINGS_KEY, LANGUAGE);

        // Убираем выбор и смотрим, что подставится.
        let _ = write_string(LANGUAGE, "");
        let chosen = language();
        assert!(chosen == "ru" || chosen == "en", "{chosen}");

        if let Some(was) = was {
            let _ = write_string(LANGUAGE, &was);
        }
    }

    #[test]
    fn a_chosen_language_survives_a_restart() {
        let _guard = LANGUAGE_LOCK.lock().unwrap();
        let was = language();

        set_language("en").expect("выбор языка сохраняется");
        assert_eq!(language(), "en");
        set_language("ru").expect("выбор языка сохраняется");
        assert_eq!(language(), "ru");

        let _ = set_language(&was);
    }

    #[test]
    fn an_unknown_language_is_refused() {
        // Молча подставить английский вместо непонятного значения
        // значило бы сменить человеку язык без его ведома.
        let error = set_language("эльфийский").expect_err("такого языка нет");
        assert!(error.to_string().contains("нет"), "{error}");
    }

    #[test]
    fn a_broken_value_in_the_registry_does_not_break_the_language() {
        // Значение правит человек, и опечатка в нём не повод показать
        // пустой интерфейс.
        let _guard = LANGUAGE_LOCK.lock().unwrap();
        let was = language();

        let _ = write_string(LANGUAGE, "мусор");
        let chosen = language();
        assert!(chosen == "ru" || chosen == "en", "{chosen}");

        let _ = set_language(&was);
    }
}
