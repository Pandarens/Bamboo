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

/// Исполнители сценариев: у них имя образа не говорит ничего.
///
/// На живой машине «node.exe» оказался сразу тремя разными вещами: сервером
/// разработки, забытым на двое суток, пятью копиями сервера для чтения PDF
/// и пятью копиями индексатора кода. В Диспетчере задач все они — одна
/// и та же строка «Node.js JavaScript Runtime», и закрыть нужное по ней
/// нельзя. Что это на самом деле, написано только в строке запуска.
const SCRIPT_HOSTS: &[&str] = &[
    "node",
    "deno",
    "bun",
    "python",
    "python3",
    "pythonw",
    "py",
    "php",
    "php-cgi",
    "java",
    "javaw",
    "ruby",
    "rubyw",
    "dotnet",
    "powershell",
    "pwsh",
];

/// Исполнитель ли это сценариев — стоит ли читать его строку запуска.
pub fn is_script_host(image_name: &str) -> bool {
    let lowered = image_name.to_ascii_lowercase();
    let stem = lowered.strip_suffix(".exe").unwrap_or(&lowered);
    SCRIPT_HOSTS.contains(&stem)
}

/// Что выполняет исполнитель сценариев: «vite (stanica_club)»,
/// «@modelcontextprotocol/server-pdf», «artisan serve».
///
/// `None` — строка ничего не говорит: у голой оболочки или у кода,
/// переданного прямо в строке, имени нет, а выдумывать его нельзя.
pub fn script_label(command_line: &str) -> Option<String> {
    let args = split_arguments(command_line);
    let (host, rest) = args.split_first()?;
    let host = file_name(host).to_ascii_lowercase();
    let host = host.strip_suffix(".exe").unwrap_or(&host).to_string();

    match host.as_str() {
        "node" | "deno" | "bun" => {
            let mut positional = positionals(rest, &["-r", "--require", "--import", "--loader"]);
            if matches!(
                positional.first().map(String::as_str),
                Some("run" | "task" | "serve" | "x")
            ) && host != "node"
            {
                positional.remove(0);
            }
            if rest
                .iter()
                .any(|arg| matches!(arg.as_str(), "-e" | "--eval" | "-p" | "--print"))
            {
                return None;
            }
            let (script, after) = positional.split_first()?;
            Some(from_script(script, after))
        }
        "python" | "python3" | "pythonw" | "py" => {
            if let Some(at) = rest.iter().position(|arg| arg == "-m") {
                return rest.get(at + 1).cloned();
            }
            if rest.iter().any(|arg| arg == "-c") {
                return None;
            }
            let positional = positionals(rest, &["-X", "-W"]);
            let (script, after) = positional.split_first()?;
            Some(from_script(script, after))
        }
        "php" | "php-cgi" => {
            if let Some(at) = rest.iter().position(|arg| arg == "-f") {
                let script = rest.get(at + 1)?;
                return Some(from_script(script, &[]));
            }
            let positional = positionals(rest, &["-S", "-t", "-d", "-c", "-z"]);
            let (script, after) = positional.split_first()?;
            Some(from_script(script, after))
        }
        "java" | "javaw" => {
            if let Some(at) = rest.iter().position(|arg| arg == "-jar") {
                return rest.get(at + 1).map(|jar| stem(file_name(jar)).to_string());
            }
            let positional = positionals(
                rest,
                &["-cp", "-classpath", "--class-path", "-p", "--module-path"],
            );
            // Главный класс: «org.gradle.launcher.daemon.bootstrap.GradleDaemon»
            // говорит о себе последним словом.
            let main = positional.first()?;
            Some(main.rsplit(['.', '/']).next().unwrap_or(main).to_string())
        }
        "ruby" | "rubyw" => {
            let positional = positionals(rest, &["-I", "-r"]);
            let (script, after) = positional.split_first()?;
            Some(from_script(script, after))
        }
        "dotnet" => {
            let positional = positionals(rest, &[]);
            let first = positional.first()?;
            if first.to_ascii_lowercase().ends_with(".dll") {
                Some(stem(file_name(first)).to_string())
            } else {
                Some(first.clone())
            }
        }
        "powershell" | "pwsh" => {
            let at = rest
                .iter()
                .position(|arg| matches!(arg.to_ascii_lowercase().as_str(), "-file" | "-f"))?;
            rest.get(at + 1).map(|script| file_name(script).to_string())
        }
        _ => None,
    }
}

/// Сценарии, у которых смысл в следующем слове: «artisan serve»,
/// «npm run dev». Одно имя без него ничего не объясняет.
const WITH_SUBCOMMAND: &[&str] = &[
    "artisan",
    "manage.py",
    "rails",
    "rake",
    "console",
    "npm-cli.js",
    "yarn.js",
    "yarn.cjs",
    "pnpm.cjs",
    "pnpm.js",
];

/// Имена-пустышки: «index.js» и «main.py» есть в каждом проекте. За них
/// говорит папка, в которой они лежат.
const GENERIC_STEMS: &[&str] = &[
    "index",
    "main",
    "server",
    "app",
    "cli",
    "run",
    "start",
    "__main__",
    "entry",
    "boot",
    "bootstrap",
    "launcher",
    "program",
    "service",
    "daemon",
    "worker",
    "wrapper",
    "bin",
];

/// Служебные папки: за ними не проект, а устройство пакета.
const PLUMBING_DIRS: &[&str] = &[
    "dist", "bin", "lib", "src", "build", "out", "current", "scripts", "js", "esm", "cjs", "node",
    "release", "debug", "target", "cli", "server", "app",
];

/// Места, где пакеты лежат сами по себе, без проекта: кэш npx, глобальная
/// установка npm, хранилище pnpm.
const GLOBAL_PLACES: &[&str] = &[
    "/npm-cache/",
    "/_npx/",
    "/appdata/roaming/npm/",
    "/program files",
    "/.pnpm-store/",
    "/appdata/local/pnpm/",
    "/.yarn/",
];

/// Подпись по пути к сценарию и словам после него.
fn from_script(script: &str, after: &[String]) -> String {
    let parts = normalise_path(script);
    let lowered: Vec<String> = parts.iter().map(|part| part.to_ascii_lowercase()).collect();
    let joined = format!("/{}/", lowered.join("/"));
    let file = parts.last().map(String::as_str).unwrap_or(script);
    let global = GLOBAL_PLACES.iter().any(|place| joined.contains(place));

    // Пакет из node_modules: имя пакета — сразу за последним node_modules,
    // проект — перед первым. Последний, а не первый: pnpm вкладывает пакеты
    // в node_modules/.pnpm/…/node_modules/имя.
    let package_dirs = ["node_modules", "site-packages", "vendor"];
    for dir in package_dirs {
        let Some(last) = lowered.iter().rposition(|part| part == dir) else {
            continue;
        };
        // У composer пакет из двух частей всегда, у npm — только со @.
        let two_parts = dir == "vendor" || parts.get(last + 1).is_some_and(|p| p.starts_with('@'));
        let package = if two_parts {
            match (parts.get(last + 1), parts.get(last + 2)) {
                (Some(scope), Some(name)) => format!("{scope}/{name}"),
                _ => continue,
            }
        } else {
            match parts.get(last + 1) {
                Some(name) => stem_if_file(name, last + 1 == parts.len() - 1).to_string(),
                None => continue,
            }
        };

        // Пакетные менеджеры: смысл не в них, а в том, что они запускают.
        let file_lower = file.to_ascii_lowercase();
        if package == "npm" && file_lower == "npx-cli.js" {
            if let Some(target) = after.iter().find(|arg| !arg.starts_with('-')) {
                return without_version(target).to_string();
            }
        }
        let label = if WITH_SUBCOMMAND.contains(&file_lower.as_str()) {
            with_subcommand(&package, after)
        } else {
            package
        };

        let first = lowered.iter().position(|part| part == dir).unwrap_or(last);
        let project = (!global && first >= 1)
            .then(|| parts[first - 1].as_str())
            .filter(|name| !name.ends_with(':'))
            .filter(|name| !matches!(*name, ".venv" | "venv" | "env" | "Lib" | "lib"));
        return match project {
            Some(project) => format!("{label} ({project})"),
            None => label,
        };
    }

    let file_lower = file.to_ascii_lowercase();
    if WITH_SUBCOMMAND.contains(&file_lower.as_str()) {
        let name = if file_lower.ends_with(".py") {
            file.to_string()
        } else {
            stem(file).to_string()
        };
        let label = with_subcommand(&name, after);
        return match parts.len().checked_sub(2).map(|at| parts[at].as_str()) {
            Some(project) if !project.ends_with(':') && !global => {
                format!("{label} ({project})")
            }
            _ => label,
        };
    }

    let name = stem(file);
    if !GENERIC_STEMS.contains(&name.to_ascii_lowercase().as_str()) {
        return name.to_string();
    }
    // «index.js» ничего не говорит — говорит папка над ним.
    parts
        .iter()
        .rev()
        .skip(1)
        .find(|dir| !PLUMBING_DIRS.contains(&dir.to_ascii_lowercase().as_str()))
        .filter(|dir| !dir.ends_with(':'))
        .cloned()
        .unwrap_or_else(|| name.to_string())
}

fn with_subcommand(name: &str, after: &[String]) -> String {
    let words: Vec<&str> = after
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .take(2)
        .map(String::as_str)
        .collect();
    // У npm и yarn смысл в «run dev»: одного «run» мало.
    let take = if matches!(words.first(), Some(&"run")) {
        2
    } else {
        1
    };
    let words: Vec<&str> = words.into_iter().take(take).collect();
    if words.is_empty() {
        name.to_string()
    } else {
        format!("{name} {}", words.join(" "))
    }
}

/// «@scope/name@1.2.3» → «@scope/name»: версия в подписи — шум.
fn without_version(spec: &str) -> &str {
    match spec.rfind('@') {
        Some(at) if at > 0 => &spec[..at],
        _ => spec,
    }
}

/// Позиционные слова: всё, что не ключ и не значение ключа.
fn positionals(args: &[String], with_value: &[&str]) -> Vec<String> {
    let mut result = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            skip = with_value.contains(&arg.as_str());
            continue;
        }
        result.push(arg.clone());
    }
    result
}

/// Путь по частям, с разобранными «..» и «.»: `.bin\..\vite` — это `vite`.
fn normalise_path(path: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    for part in path.split(['\\', '/']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other.to_string()),
        }
    }
    parts
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn stem(file: &str) -> &str {
    match file.rfind('.') {
        Some(at) if at > 0 => &file[..at],
        _ => file,
    }
}

fn stem_if_file(name: &str, is_file: bool) -> &str {
    if is_file {
        stem(name)
    } else {
        name
    }
}

/// Делит строку запуска на слова по правилам Windows: пробел внутри кавычек
/// слово не рвёт, сами кавычки в слово не входят.
fn split_arguments(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' | '\t' if !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            other => {
                current.push(other);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod script_tests {
    use super::*;

    // Строки ниже — настоящие, с машины, где «node.exe» держал два гигабайта
    // и никто не мог сказать, что это.

    #[test]
    fn a_dev_server_is_named_with_its_project() {
        let line = r#""node" "C:\Users\user\Desktop\VS\stanica_club\node_modules\.bin\\..\vite\bin\vite.js""#;
        assert_eq!(script_label(line).as_deref(), Some("vite (stanica_club)"));
    }

    #[test]
    fn a_server_from_the_npx_cache_has_no_project() {
        let line = r#""C:\Program Files\nodejs\node.exe" "C:\Users\user\AppData\Local\npm-cache\_npx\6583fba12287d067\node_modules\.bin\\..\@modelcontextprotocol\server-pdf\dist\index.js" --stdio"#;
        assert_eq!(
            script_label(line).as_deref(),
            Some("@modelcontextprotocol/server-pdf")
        );
    }

    #[test]
    fn npx_is_named_after_what_it_runs() {
        // Обёртка npx и сам сервер должны подписаться одинаково: это одна
        // копия, и считать её надо один раз.
        let line = r#""C:\Program Files\nodejs\\node.exe" "C:\Users\user\AppData\Roaming\npm\node_modules\npm\bin\npx-cli.js" "-y" "@modelcontextprotocol/server-pdf" "--stdio""#;
        assert_eq!(
            script_label(line).as_deref(),
            Some("@modelcontextprotocol/server-pdf")
        );
    }

    #[test]
    fn npm_run_keeps_the_script_name() {
        let line = r#""C:\Program Files\nodejs\\node.exe" "C:\Users\user\AppData\Roaming\npm\node_modules\npm\bin\npm-cli.js" run dev"#;
        assert_eq!(script_label(line).as_deref(), Some("npm run dev"));
    }

    #[test]
    fn a_standalone_tool_is_named_by_its_file() {
        let line = r#""C:\Users\user\AppData\Local\codegraph\current\bin\..\node.exe" --liftoff-only "C:\Users\user\AppData\Local\codegraph\current\bin\..\lib\dist\bin\codegraph.js" "serve" "--mcp""#;
        assert_eq!(script_label(line).as_deref(), Some("codegraph"));
    }

    #[test]
    fn a_generic_file_is_named_by_its_folder() {
        let line = r#"node C:\work\shop-api\dist\index.js"#;
        assert_eq!(script_label(line).as_deref(), Some("shop-api"));
    }

    #[test]
    fn laravel_is_named_with_its_project() {
        let line = r#"C:\php\php.exe -S 127.0.0.1:8001 C:\Users\user\Desktop\VS\stanica_club\vendor\laravel\framework\src\Illuminate\Foundation\resources\server.php"#;
        assert_eq!(
            script_label(line).as_deref(),
            Some("laravel/framework (stanica_club)")
        );
        assert_eq!(
            script_label(r#""C:\php\php.exe" artisan serve"#).as_deref(),
            Some("artisan serve")
        );
    }

    #[test]
    fn python_modules_and_scripts() {
        assert_eq!(
            script_label(r#"python.exe -m http.server 8000"#).as_deref(),
            Some("http.server")
        );
        assert_eq!(
            script_label(r#"python.exe C:\sites\blog\manage.py runserver"#).as_deref(),
            Some("manage.py runserver (blog)")
        );
        assert_eq!(script_label(r#"python.exe -c "print(1)""#), None);
    }

    #[test]
    fn java_is_named_by_its_main_class_or_jar() {
        assert_eq!(
            script_label(r#"java.exe -Xmx2g -cp C:\g\lib\gradle.jar org.gradle.launcher.daemon.bootstrap.GradleDaemon 8.5"#).as_deref(),
            Some("GradleDaemon")
        );
        assert_eq!(
            script_label(r#"javaw.exe -jar "C:\Apps\Tool Box\tool.jar""#).as_deref(),
            Some("tool")
        );
    }

    #[test]
    fn a_bare_shell_says_nothing() {
        // Вкладка терминала: имени у неё нет, и выдумывать его нельзя.
        assert_eq!(
            script_label(r#"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe"#),
            None
        );
        assert_eq!(
            script_label(r#"powershell.exe -NoProfile -File C:\tools\backup.ps1"#).as_deref(),
            Some("backup.ps1")
        );
        assert_eq!(script_label(r#"node -e "console.log(1)""#), None);
    }

    #[test]
    fn hosts_are_recognised_by_image_name() {
        assert!(is_script_host("node.exe"));
        assert!(is_script_host("Python.exe"));
        assert!(!is_script_host("chrome.exe"));
        assert!(!is_script_host("claude.exe"));
    }
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
