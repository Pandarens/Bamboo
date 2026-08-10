//! Чтение командной строки чужого процесса.
//!
//! Нужно ради одного вопроса, который задают чаще всех прочих: «почему
//! браузер занимает восемь гигабайт». Сказать «это пятьдесят семь
//! процессов» — половина ответа. Настоящий ответ в том, что это за
//! процессы: вкладки, расширения, отрисовка. Браузеры пишут свой тип
//! прямо в командную строку, откуда мы его и берём.
//!
//! Способ стандартный, но окольный: у процесса спрашиваем адрес его
//! блока окружения, оттуда читаем адрес параметров запуска, а из них —
//! саму строку. Три чтения чужой памяти, каждое может не удаться, и это
//! нормально: у защищённых процессов мы ничего не прочтём и не должны.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

use crate::nt::{nt_success, UNICODE_STRING};

// Смещения внутри структур ядра для 64-битных процессов.
//
// Раскладку этих структур Microsoft не документирует, но она не менялась
// с Windows XP: от Vista до Windows 11 смещения одни и те же. Проверять
// их всё равно надо — на неверном смещении мы прочитаем мусор, — поэтому
// результат сверяется с ожидаемым видом строки.

/// Где в блоке окружения лежит указатель на параметры запуска.
const PEB_PROCESS_PARAMETERS: usize = 0x20;
/// Где в параметрах запуска лежит строка запуска.
const PARAMS_COMMAND_LINE: usize = 0x70;

/// Длиннее этого командные строки не бывают даже у браузеров.
/// Ограничение защищает от чтения мусора, если смещение вдруг не сойдётся.
const MAX_COMMAND_LINE: usize = 32 * 1024;

#[allow(non_snake_case, non_camel_case_types)]
#[repr(C)]
#[derive(Clone, Copy)]
struct PROCESS_BASIC_INFORMATION {
    Reserved1: *mut core::ffi::c_void,
    PebBaseAddress: *mut core::ffi::c_void,
    Reserved2: [*mut core::ffi::c_void; 2],
    UniqueProcessId: usize,
    Reserved3: *mut core::ffi::c_void,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        handle: windows_sys::Win32::Foundation::HANDLE,
        class: u32,
        info: *mut core::ffi::c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

/// Класс `ProcessBasicInformation`.
const CLASS_BASIC_INFORMATION: u32 = 0;

struct OwnedHandle(windows_sys::Win32::Foundation::HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Читает командную строку процесса.
///
/// Ошибка здесь — обычное дело, а не сбой: у системных и защищённых
/// процессов память чужим не читается, и это правильно.
pub fn command_line(pid: u32) -> Result<String> {
    if pid == 0 || pid == 4 {
        return Err(Error::Unsupported("у процессов ядра нет командной строки"));
    }

    let handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if handle.is_null() {
        return Err(Error::Win32 {
            call: "OpenProcess(командная строка)",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }
    let handle = OwnedHandle(handle);

    // 1. Адрес блока окружения процесса.
    let mut basic: PROCESS_BASIC_INFORMATION = unsafe { core::mem::zeroed() };
    let mut returned: u32 = 0;
    let status = unsafe {
        NtQueryInformationProcess(
            handle.0,
            CLASS_BASIC_INFORMATION,
            (&mut basic as *mut PROCESS_BASIC_INFORMATION).cast(),
            core::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &mut returned,
        )
    };
    if !nt_success(status) || basic.PebBaseAddress.is_null() {
        return Err(Error::Unsupported("блок окружения процесса недоступен"));
    }

    // 2. Адрес параметров запуска внутри блока окружения.
    let params_address: usize = read_value(
        &handle,
        basic.PebBaseAddress as usize + PEB_PROCESS_PARAMETERS,
    )?;
    if params_address == 0 {
        return Err(Error::Malformed("параметры запуска не заполнены"));
    }

    // 3. Сама строка: сначала её описание, затем содержимое.
    let text: UNICODE_STRING = read_value(&handle, params_address + PARAMS_COMMAND_LINE)?;
    if text.Buffer.is_null() || text.Length == 0 {
        return Err(Error::Malformed("командная строка пуста"));
    }
    if text.Length as usize > MAX_COMMAND_LINE {
        // Смещение не сошлось: вместо строки прочитан мусор.
        return Err(Error::Malformed("длина командной строки неправдоподобна"));
    }

    let mut buffer = vec![0u16; text.Length as usize / 2];
    let mut read: usize = 0;
    let ok = unsafe {
        ReadProcessMemory(
            handle.0,
            text.Buffer.cast(),
            buffer.as_mut_ptr().cast(),
            text.Length as usize,
            &mut read,
        )
    };
    if ok == 0 || read != text.Length as usize {
        return Err(Error::Unsupported("командную строку прочитать не удалось"));
    }

    Ok(String::from_utf16_lossy(&buffer))
}

/// Читает значение известного типа по адресу в чужом процессе.
fn read_value<T: Copy>(handle: &OwnedHandle, address: usize) -> Result<T> {
    let mut value: T = unsafe { core::mem::zeroed() };
    let mut read: usize = 0;

    let ok = unsafe {
        ReadProcessMemory(
            handle.0,
            address as *const core::ffi::c_void,
            (&mut value as *mut T).cast(),
            core::mem::size_of::<T>(),
            &mut read,
        )
    };
    if ok == 0 || read != core::mem::size_of::<T>() {
        return Err(Error::Unsupported("память процесса прочитать не удалось"));
    }
    Ok(value)
}

/// Что за процесс браузера.
///
/// Chrome, Edge и всё на их основе пишут назначение процесса в командную
/// строку ключом `--type`. Именно это и превращает «пятьдесят семь
/// процессов» в осмысленный ответ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserRole {
    /// Главный процесс браузера: окна, вкладки, всё остальное под ним.
    Browser,
    /// Вкладка или её часть.
    Tab,
    /// Расширение.
    Extension,
    /// Отрисовка.
    Gpu,
    /// Служебный: сеть, хранилище, звук.
    Utility,
    /// Обработчик аварий.
    Crashpad,
}

impl BrowserRole {
    pub fn name(self) -> &'static str {
        match self {
            BrowserRole::Browser => "главный процесс",
            BrowserRole::Tab => "вкладка",
            BrowserRole::Extension => "расширение",
            BrowserRole::Gpu => "отрисовка",
            BrowserRole::Utility => "служебный",
            BrowserRole::Crashpad => "обработчик сбоев",
        }
    }
}

/// Определяет роль процесса браузера по его командной строке.
///
/// `None` — это не браузер либо строку прочитать не вышло.
pub fn browser_role(command_line: &str) -> Option<BrowserRole> {
    let lowered = command_line.to_lowercase();

    // Обработчик сбоев отличаем первым: у него есть свой ключ, а `--type`
    // может отсутствовать.
    if lowered.contains("crashpad") {
        return Some(BrowserRole::Crashpad);
    }

    let Some(at) = lowered.find("--type=") else {
        // Ключа нет — значит это главный процесс. Но только если строка
        // вообще похожа на браузерную: у случайной программы ключа тоже нет.
        return looks_like_browser(&lowered).then_some(BrowserRole::Browser);
    };

    let rest = &lowered[at + "--type=".len()..];
    let kind = rest
        .split([' ', '"'])
        .next()
        .unwrap_or_default()
        .trim_matches('"');

    Some(match kind {
        // Расширения живут в тех же renderer-процессах, но помечены
        // отдельным ключом.
        "renderer" if lowered.contains("--extension-process") => BrowserRole::Extension,
        "renderer" => BrowserRole::Tab,
        "gpu-process" => BrowserRole::Gpu,
        "crashpad-handler" => BrowserRole::Crashpad,
        _ => BrowserRole::Utility,
    })
}

fn looks_like_browser(lowered: &str) -> bool {
    [
        "chrome.exe",
        "msedge.exe",
        "brave.exe",
        "opera.exe",
        "vivaldi.exe",
        "yandex.exe",
    ]
    .iter()
    .any(|name| lowered.contains(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_command_line_is_readable() {
        let text = command_line(std::process::id()).expect("свою строку читать обязаны");
        assert!(!text.is_empty());
        // В своей же строке обязано быть наше имя.
        assert!(
            text.to_lowercase().contains("bamboo"),
            "прочиталось не то: {text}"
        );
    }

    #[test]
    fn kernel_processes_are_refused() {
        assert!(command_line(0).is_err());
        assert!(command_line(4).is_err());
    }

    #[test]
    fn a_missing_process_fails_cleanly() {
        assert!(command_line(0xFFFF_FFF0).is_err());
    }

    #[test]
    fn a_tab_process_is_recognised() {
        let line = r#""C:\Program Files\Google\Chrome\chrome.exe" --type=renderer --lang=ru"#;
        assert_eq!(browser_role(line), Some(BrowserRole::Tab));
    }

    #[test]
    fn an_extension_is_told_apart_from_a_tab() {
        // Расширение живёт в таком же процессе отрисовки, и отличается
        // только ключом. Без этой проверки все расширения считались бы
        // вкладками, и ответ «у вас 40 вкладок» был бы неправдой.
        let line = r#"chrome.exe --type=renderer --extension-process --lang=ru"#;
        assert_eq!(browser_role(line), Some(BrowserRole::Extension));
    }

    #[test]
    fn gpu_and_utility_are_separated() {
        assert_eq!(
            browser_role("chrome.exe --type=gpu-process"),
            Some(BrowserRole::Gpu)
        );
        assert_eq!(
            browser_role("chrome.exe --type=utility --utility-sub-type=network"),
            Some(BrowserRole::Utility)
        );
    }

    #[test]
    fn the_main_process_has_no_type_key() {
        let line = r#""C:\Program Files\Google\Chrome\chrome.exe" --profile-directory=Default"#;
        assert_eq!(browser_role(line), Some(BrowserRole::Browser));
    }

    #[test]
    fn an_ordinary_program_is_not_a_browser() {
        // У блокнота тоже нет ключа --type, но браузером он от этого
        // не становится.
        assert_eq!(browser_role(r#""C:\Windows\notepad.exe""#), None);
    }

    #[test]
    fn crashpad_is_recognised_even_without_a_type() {
        assert_eq!(
            browser_role(r#"chrome.exe --type=crashpad-handler"#),
            Some(BrowserRole::Crashpad)
        );
    }

    #[test]
    fn every_role_has_a_name() {
        for role in [
            BrowserRole::Browser,
            BrowserRole::Tab,
            BrowserRole::Extension,
            BrowserRole::Gpu,
            BrowserRole::Utility,
            BrowserRole::Crashpad,
        ] {
            assert!(!role.name().is_empty());
        }
    }
}
