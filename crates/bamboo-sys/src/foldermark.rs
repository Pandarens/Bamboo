//! Пометка папки в Проводнике: свой значок и подсказка при наведении.
//!
//! Способ штатный — тот же, что «Свойства → Настройка → Сменить значок»:
//! файл `desktop.ini` внутри папки и признак «только чтение» у самой папки,
//! по которому Проводник понимает, что desktop.ini надо читать. Для папок
//! этот признак ничего не запрещает: писать в неё можно как прежде.
//!
//! Раскрашивать строки в Проводнике по-другому можно только своей
//! библиотекой, загруженной в сам Проводник. Ошибка в ней роняет рабочий
//! стол целиком — такой ценой пометка мусора не нужна.

use std::path::Path;

use bamboo_core::{Error, Result};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileAttributesW, SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_SYSTEM, INVALID_FILE_ATTRIBUTES,
};
use windows_sys::Win32::UI::Shell::{
    PathMakeSystemFolderW, PathUnmakeSystemFolderW, SHChangeNotify, SHCNE_UPDATEDIR,
    SHCNE_UPDATEITEM, SHCNF_PATHW,
};

/// Первая строка нашего desktop.ini. По ней пометка узнаётся как своя:
/// чужой desktop.ini — чужие настройки папки, и трогать их нельзя.
const SIGNATURE: &str = "; Bamboo:";

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(core::iter::once(0))
        .collect()
}

fn attributes(path: &Path) -> Option<u32> {
    let path = wide(path);
    let value = unsafe { GetFileAttributesW(path.as_ptr()) };
    (value != INVALID_FILE_ATTRIBUTES).then_some(value)
}

fn set_attributes(path: &Path, value: u32) -> Result<()> {
    let wide_path = wide(path);
    if unsafe { SetFileAttributesW(wide_path.as_ptr(), value) } == 0 {
        return Err(Error::Win32 {
            call: "SetFileAttributesW",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }
    Ok(())
}

/// Говорит Проводнику перечитать папку: иначе новый значок появится
/// только после перезапуска Проводника.
fn refresh(folder: &Path) {
    let path = wide(folder);
    unsafe {
        SHChangeNotify(
            SHCNE_UPDATEITEM as i32,
            SHCNF_PATHW,
            path.as_ptr().cast(),
            core::ptr::null(),
        );
        SHChangeNotify(
            SHCNE_UPDATEDIR as i32,
            SHCNF_PATHW,
            path.as_ptr().cast(),
            core::ptr::null(),
        );
    }
}

/// Наша ли пометка у папки.
pub fn is_marked(folder: &Path) -> bool {
    std::fs::read(folder.join("desktop.ini"))
        .map(|bytes| decode(&bytes).starts_with(SIGNATURE))
        .unwrap_or(false)
}

/// Есть ли у папки чужая настройка вида — её не перезаписываем.
fn has_foreign_settings(folder: &Path) -> bool {
    folder.join("desktop.ini").exists() && !is_marked(folder)
}

/// Помечает папку значком и подсказкой.
///
/// Повторная пометка обновляет подсказку: размер мусора меняется, и старое
/// число в подсказке было бы неправдой.
pub fn mark(folder: &Path, icon: &Path, tip: &str) -> Result<()> {
    if has_foreign_settings(folder) {
        return Err(Error::Unsupported("у папки уже есть свои настройки вида"));
    }
    let before = attributes(folder).ok_or(Error::Unsupported("папки нет"))?;
    if before & (FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_SYSTEM) != 0 && !is_marked(folder) {
        // Папку уже кто-то сделал особой — снимая пометку, мы бы сняли
        // и его признак.
        return Err(Error::Unsupported("папка уже помечена как системная"));
    }

    let text = format!(
        "{SIGNATURE} папку можно удалить без потерь. Удалите этот файл — пометка уйдёт.\r\n\
         [.ShellClassInfo]\r\n\
         IconResource={},0\r\n\
         InfoTip={}\r\n",
        icon.display(),
        tip.replace(['\r', '\n'], " "),
    );
    // UTF-16 с меткой порядка байтов: иначе русская подсказка в Проводнике
    // превратится в кракозябры.
    let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }

    let ini = folder.join("desktop.ini");
    // Скрытый системный файл поверх не перепишешь: сначала снять признаки.
    if ini.exists() {
        set_attributes(&ini, FILE_ATTRIBUTE_NORMAL)?;
    }
    std::fs::write(&ini, &bytes).map_err(|error| io_error("запись desktop.ini", error))?;
    set_attributes(&ini, FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM)?;

    let folder_wide = wide(folder);
    if unsafe { PathMakeSystemFolderW(folder_wide.as_ptr()) } == 0 {
        return Err(Error::Win32 {
            call: "PathMakeSystemFolderW",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }
    refresh(folder);
    Ok(())
}

/// Снимает свою пометку. Чужой desktop.ini не трогает.
pub fn unmark(folder: &Path) -> Result<()> {
    if !is_marked(folder) {
        return Ok(());
    }
    let ini = folder.join("desktop.ini");
    set_attributes(&ini, FILE_ATTRIBUTE_NORMAL)?;
    std::fs::remove_file(&ini).map_err(|error| io_error("удаление desktop.ini", error))?;
    let folder_wide = wide(folder);
    unsafe { PathUnmakeSystemFolderW(folder_wide.as_ptr()) };
    refresh(folder);
    Ok(())
}

fn io_error(call: &'static str, error: std::io::Error) -> Error {
    Error::Win32 {
        call,
        code: error.raw_os_error().unwrap_or(0) as u32,
    }
}

fn decode(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => {
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        other => String::from_utf8_lossy(other).into_owned(),
    }
}

/// Переводит текущий поток в фоновый режим: низкий приоритет процессора,
/// диска и памяти.
///
/// Для обхода папок в поисках мусора. Он читает десятки тысяч каталогов,
/// и на загруженном накопителе без фонового режима сам стал бы тем, что
/// тормозит. Возвращает, удалось ли.
pub fn background_thread() -> bool {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    };
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bamboo-mark-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("папка для проверки");
        dir
    }

    #[test]
    fn a_folder_is_marked_and_unmarked_cleanly() {
        let dir = scratch("roundtrip");
        let icon = dir.join("junk.ico");
        mark(&dir, &icon, "Кэш сборки Rust, 85 ГБ").expect("пометка");
        assert!(is_marked(&dir));
        let text = decode(&std::fs::read(dir.join("desktop.ini")).unwrap());
        assert!(text.contains("InfoTip=Кэш сборки Rust, 85 ГБ"), "{text}");
        assert!(text.contains("IconResource="), "{text}");

        // Повторная пометка обновляет подсказку, а не падает на скрытом файле.
        mark(&dir, &icon, "Кэш сборки Rust, 3 ГБ").expect("повторная пометка");
        let text = decode(&std::fs::read(dir.join("desktop.ini")).unwrap());
        assert!(text.contains("3 ГБ"), "{text}");

        unmark(&dir).expect("снятие");
        assert!(!dir.join("desktop.ini").exists());
        let attrs = attributes(&dir).unwrap();
        assert_eq!(attrs & FILE_ATTRIBUTE_READONLY, 0, "признак папки не снят");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn foreign_folder_settings_are_left_alone() {
        // Человек сам сменил значок папке — это его настройка.
        let dir = scratch("foreign");
        std::fs::write(
            dir.join("desktop.ini"),
            "[.ShellClassInfo]\r\nIconResource=C:\\my.ico,0\r\n",
        )
        .unwrap();
        assert!(mark(&dir, &dir.join("junk.ico"), "мусор").is_err());
        unmark(&dir).expect("чужое снимать нечего");
        assert!(dir.join("desktop.ini").exists(), "чужой desktop.ini удалён");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
