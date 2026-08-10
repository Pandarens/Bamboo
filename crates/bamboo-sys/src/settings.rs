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
