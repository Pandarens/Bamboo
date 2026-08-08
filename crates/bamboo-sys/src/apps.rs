//! Перечисление установленных приложений (ТЗ, раздел 9.9).
//!
//! Через ключи `Uninstall` в реестре — тот же источник, что у «Установки
//! и удаления программ». Читается без прав администратора. Для системного
//! диффа берём отображаемые имена: появление нового приложения за неделю
//! пользователю полезно знать.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
};

/// Куст и путь, где живут записи об установленных приложениях.
struct Source {
    root: HKEY,
    path: &'static str,
    /// Дополнительный флаг доступа: 32- или 64-битное представление реестра.
    wow: u32,
}

const UNINSTALL: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall";

fn sources() -> Vec<Source> {
    vec![
        // 64-битные приложения для всех пользователей.
        Source {
            root: HKEY_LOCAL_MACHINE,
            path: UNINSTALL,
            wow: KEY_WOW64_64KEY,
        },
        // 32-битные приложения для всех пользователей (WOW6432Node).
        Source {
            root: HKEY_LOCAL_MACHINE,
            path: UNINSTALL,
            wow: KEY_WOW64_32KEY,
        },
        // Приложения текущего пользователя.
        Source {
            root: HKEY_CURRENT_USER,
            path: UNINSTALL,
            wow: 0,
        },
    ]
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

fn open(root: HKEY, path: &str, extra_access: u32) -> Option<Key> {
    let wide_path = wide(path);
    let mut handle: HKEY = core::ptr::null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            root,
            wide_path.as_ptr(),
            0,
            KEY_READ | extra_access,
            &mut handle,
        )
    };
    (status == ERROR_SUCCESS).then_some(Key(handle))
}

/// Возвращает отображаемые имена установленных приложений.
///
/// Имена отдаём как есть (для показа пользователю) и дедуплицируем через
/// `BTreeSet`: одно приложение может встречаться в нескольких кустах —
/// например, 64-битный установщик кладёт запись и в общий, и в WOW-раздел.
pub fn installed_applications() -> Result<Vec<String>> {
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut opened_any = false;

    for source in sources() {
        let Some(key) = open(source.root, source.path, source.wow) else {
            continue;
        };
        opened_any = true;

        for subkey_name in subkeys(&key) {
            if let Some(display) = read_display_name(&key, &subkey_name, source.wow) {
                names.insert(display);
            }
        }
    }

    if !opened_any {
        return Err(Error::Unsupported("ни один куст Uninstall не открылся"));
    }
    Ok(names.into_iter().collect())
}

/// Перечисляет имена подключей.
fn subkeys(key: &Key) -> Vec<String> {
    let mut names = Vec::new();
    for index in 0.. {
        let mut buffer = [0u16; 256];
        let mut len = buffer.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key.0,
                index,
                buffer.as_mut_ptr(),
                &mut len,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&buffer[..len as usize]));
    }
    names
}

/// Читает значение `DisplayName` из подключа приложения.
///
/// Пропускает записи без имени: обновления и системные компоненты часто
/// не имеют `DisplayName` и в списке приложений не нужны.
fn read_display_name(parent: &Key, subkey: &str, wow: u32) -> Option<String> {
    // Открываем подключ прямо относительно родителя: тот же куст и права.
    let sub = open_relative(parent, subkey, wow)?;

    let value = wide("DisplayName");
    let mut buffer = [0u16; 512];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegQueryValueExW(
            sub.0,
            value.as_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };

    if status != ERROR_SUCCESS || size < 2 {
        return None;
    }
    // size в байтах, включая завершающий ноль.
    let chars = (size as usize / 2).saturating_sub(1);
    let name = String::from_utf16_lossy(&buffer[..chars]);
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Открывает подключ относительно уже открытого ключа.
fn open_relative(parent: &Key, subkey: &str, extra_access: u32) -> Option<Key> {
    let wide_sub = wide(subkey);
    let mut handle: HKEY = core::ptr::null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            parent.0,
            wide_sub.as_ptr(),
            0,
            KEY_READ | extra_access,
            &mut handle,
        )
    };
    (status == ERROR_SUCCESS).then_some(Key(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applications_can_be_listed_without_admin() {
        let apps = installed_applications().expect("список приложений не прочитался");
        // На любой рабочей Windows установлено хоть что-то.
        assert!(!apps.is_empty(), "список приложений пуст");
    }

    #[test]
    fn names_are_non_empty_and_deduplicated() {
        let apps = installed_applications().unwrap();
        for app in &apps {
            assert!(!app.trim().is_empty());
        }
        // BTreeSet на входе гарантирует уникальность и порядок.
        let mut sorted = apps.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), apps.len(), "в списке есть дубликаты");
    }
}
