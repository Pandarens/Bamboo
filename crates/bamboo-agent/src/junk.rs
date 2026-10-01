//! Мусор на диске: только то, что восстанавливается само.
//!
//! Написано по живому случаю. Диск C: был заполнен на 89%, половина
//! подвисаний за две недели приходилась на накопитель, а 85 ГБ из занятого
//! оказались кэшем сборки Rust — `target\debug`, который пересобирается
//! сам и удаляется без единой потери. Знать это человек не обязан.
//!
//! Граница проведена жёстко, и это главное в модуле. Мусор здесь — то,
//! что программа создаёт сама и создаст заново: кэши сборки, скачанные
//! пакеты, временные файлы, дампы сбоев. Документы, фотографии, загрузки,
//! «большие файлы» Bamboo не ищет и не предлагает: решать, нужны ли они,
//! может только человек, а утилита, которая советует удалить чужой архив,
//! опаснее мусора.
//!
//! Чистильщики вроде «удалим кэш браузера» здесь не водятся по той же
//! причине, что и «очистка памяти» (раздел 11.5 ТЗ): браузер наберёт кэш
//! обратно за день и будет медленнее, пока набирает.

#![forbid(unsafe_code)]

use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Что за мусор.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `target` рядом с `Cargo.toml`.
    RustBuild,
    /// `node_modules` проекта, к которому не прикасались больше месяца.
    NodeModules,
    /// `bin` и `obj` рядом с проектом .NET.
    DotnetBuild,
    /// `build` и `.gradle` рядом с проектом Gradle.
    GradleBuild,
    /// Кэши сборщиков сайтов: `.next`, `.nuxt`, `.parcel-cache` и родня.
    WebCache,
    /// Папка, которую программа сама пометила как кэш (`CACHEDIR.TAG`).
    TaggedCache,
    /// Кэш скачанных пакетов: npm, pip, cargo, gradle, NuGet.
    PackageCache,
    /// Временные файлы старше недели.
    Temp,
    /// Дампы и отчёты о сбоях.
    CrashDumps,
}

impl Kind {
    /// Что это — словами человека.
    pub fn what(self) -> &'static str {
        match self {
            Kind::RustBuild => bamboo_core::pick("кэш сборки Rust", "Rust build cache"),
            Kind::NodeModules => bamboo_core::pick(
                "пакеты npm проекта, к которому не прикасались больше месяца",
                "npm packages of a project untouched for over a month",
            ),
            Kind::DotnetBuild => bamboo_core::pick("результаты сборки .NET", ".NET build output"),
            Kind::GradleBuild => {
                bamboo_core::pick("результаты сборки Gradle", "Gradle build output")
            }
            Kind::WebCache => bamboo_core::pick("кэш сборщика сайта", "web bundler cache"),
            Kind::TaggedCache => bamboo_core::pick(
                "кэш, который программа сама пометила удаляемым",
                "a cache its program marked as disposable",
            ),
            Kind::PackageCache => {
                bamboo_core::pick("кэш скачанных пакетов", "downloaded package cache")
            }
            Kind::Temp => bamboo_core::pick(
                "временные файлы старше недели",
                "temporary files older than a week",
            ),
            Kind::CrashDumps => {
                bamboo_core::pick("дампы и отчёты о сбоях", "crash dumps and reports")
            }
        }
    }

    /// Как оно вернётся — вместо обратного рецепта, которого у удаления нет.
    pub fn comes_back(self) -> &'static str {
        match self {
            Kind::RustBuild | Kind::DotnetBuild | Kind::GradleBuild => bamboo_core::pick(
                "пересоберётся при следующей сборке, первая будет дольше обычного",
                "rebuilt on the next build; the first one takes longer than usual",
            ),
            Kind::NodeModules => bamboo_core::pick(
                "вернутся командой npm install, если проект понадобится",
                "npm install brings them back if the project is needed again",
            ),
            Kind::WebCache | Kind::TaggedCache => bamboo_core::pick(
                "программа создаст его заново при следующем запуске",
                "its program recreates it on the next run",
            ),
            Kind::PackageCache => bamboo_core::pick(
                "нужное скачается заново при следующей установке",
                "whatever is needed is downloaded again on the next install",
            ),
            Kind::Temp => bamboo_core::pick(
                "программы, которые их создали, давно закрылись; занятые файлы Bamboo пропустит",
                "the programs that made them closed long ago; files in use are skipped",
            ),
            Kind::CrashDumps => bamboo_core::pick(
                "нужны только для разбора сбоев — если вы их никому не отправляете, не нужны вовсе",
                "only useful for analysing crashes — if you do not send them anywhere, not needed at all",
            ),
        }
    }

    /// Мусор внутри проекта. Такой помечается в Проводнике: человек видит
    /// его, открывая свои папки. Кэши в AppData он не открывает.
    pub fn in_project(self) -> bool {
        matches!(
            self,
            Kind::RustBuild
                | Kind::NodeModules
                | Kind::DotnetBuild
                | Kind::GradleBuild
                | Kind::WebCache
                | Kind::TaggedCache
        )
    }
}

/// Найденный мусор.
#[derive(Clone, Debug)]
pub struct Found {
    pub path: PathBuf,
    pub kind: Kind,
    pub bytes: u64,
    pub files: u64,
    /// Что внутри оставить: там лежит запущенная программа. На живой машине
    /// сам Bamboo был запущен из `target\release` — удалить его вместе
    /// с кэшем значило бы удалить работающую программу.
    pub keep: Vec<PathBuf>,
    /// Кто запущен из оставленного — для объяснения.
    pub kept_for: Vec<String>,
    /// Для временных файлов: удалять только то, что старше.
    pub older_than: Option<SystemTime>,
}

/// С какого размера о мусоре стоит говорить. Сто мегабайт: мелочь
/// не стоит ни внимания человека, ни записи на диск ради её удаления.
pub const WORTH: u64 = 100 * 1024 * 1024;

/// Сколько проект должен пролежать нетронутым, чтобы его пакеты npm
/// стали мусором. Месяц: пакеты рабочего проекта удалять незачем — их
/// придётся ставить заново в тот же день.
const STALE_PROJECT: Duration = Duration::from_secs(30 * 24 * 3600);

/// Сколько лет временному файлу, чтобы считать его брошенным.
const OLD_TEMP: Duration = Duration::from_secs(7 * 24 * 3600);

/// Как глубоко спускаться от корня. Проекты не лежат глубже: Desktop\SD\
/// Bamboo — третий уровень от профиля.
const DEPTH: usize = 8;

/// Сколько папок обойти за раз. Предел против диска с миллионом каталогов:
/// обход идёт в фоне, но и фоновый не должен длиться часами.
const FOLDER_BUDGET: u64 = 400_000;

/// Куда не спускаться вовсе: системное, чужое и то, где проектов нет.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "appdata",
    "$recycle.bin",
    "system volume information",
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "recovery",
    "perflogs",
    "$winreagent",
    "config.msi",
    "msocache",
    "onedrivetemp",
    ".venv",
    "venv",
    "vendor",
];

/// Признаки проекта в папке — по ним решается, чем считать соседей.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Markers {
    pub cargo: bool,
    pub node: bool,
    pub dotnet: bool,
    pub gradle: bool,
}

impl Markers {
    /// Признаки по именам файлов папки.
    pub fn from_files<'a>(names: impl IntoIterator<Item = &'a str>) -> Markers {
        let mut markers = Markers::default();
        for name in names {
            let lower = name.to_ascii_lowercase();
            match lower.as_str() {
                "cargo.toml" => markers.cargo = true,
                "package.json" => markers.node = true,
                "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts" => {
                    markers.gradle = true
                }
                _ if lower.ends_with(".csproj")
                    || lower.ends_with(".fsproj")
                    || lower.ends_with(".vbproj") =>
                {
                    markers.dotnet = true
                }
                _ => {}
            }
        }
        markers
    }
}

/// Чем считать папку по её имени и признакам проекта рядом.
///
/// `None` — не мусор. Имя без признака ничего не значит: `target` без
/// `Cargo.toml` рядом может оказаться чем угодно, включая чью-то работу.
pub fn classify(name: &str, beside: &Markers) -> Option<Kind> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "target" if beside.cargo => Some(Kind::RustBuild),
        "node_modules" if beside.node => Some(Kind::NodeModules),
        "bin" | "obj" if beside.dotnet => Some(Kind::DotnetBuild),
        "build" | ".gradle" if beside.gradle => Some(Kind::GradleBuild),
        ".next" | ".nuxt" | ".parcel-cache" | ".turbo" | ".svelte-kit" | ".angular"
            if beside.node =>
        {
            Some(Kind::WebCache)
        }
        _ => None,
    }
}

/// Глобальные кэши и свалки: место известно заранее, искать не надо.
pub fn known_places() -> Vec<(PathBuf, Kind)> {
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let mut places = Vec::new();
    if let Some(local) = env("LOCALAPPDATA") {
        // Только содержимое кэша npm, а не _npx: оттуда запущены серверы,
        // которыми пользуются прямо сейчас.
        places.push((local.join("npm-cache").join("_cacache"), Kind::PackageCache));
        places.push((local.join("pip").join("cache"), Kind::PackageCache));
        places.push((local.join("Yarn").join("Cache"), Kind::PackageCache));
        places.push((local.join("go-build"), Kind::PackageCache));
        places.push((local.join("CrashDumps"), Kind::CrashDumps));
    }
    if let Some(home) = env("USERPROFILE") {
        places.push((
            home.join(".cargo").join("registry").join("cache"),
            Kind::PackageCache,
        ));
        places.push((home.join(".gradle").join("caches"), Kind::PackageCache));
        places.push((home.join(".nuget").join("packages"), Kind::PackageCache));
    }
    if let Some(data) = env("ProgramData") {
        let wer = data.join("Microsoft").join("Windows").join("WER");
        places.push((wer.join("ReportArchive"), Kind::CrashDumps));
        places.push((wer.join("ReportQueue"), Kind::CrashDumps));
    }
    if let Some(windows) = env("SystemRoot") {
        places.push((windows.join("LiveKernelReports"), Kind::CrashDumps));
    }
    places.push((std::env::temp_dir(), Kind::Temp));
    places
}

/// Где искать проекты: профиль и корни дисков.
pub fn project_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE") {
        roots.push(PathBuf::from(home));
    }
    for letter in b'C'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\", letter as char));
        if root.exists() {
            roots.push(root);
        }
    }
    roots
}

/// Ссылка, точка монтирования или облачная заглушка. Внутрь не ходим:
/// ссылка уводит в чужое место или по кругу, а заглушку OneDrive обход
/// заставил бы скачать.
fn is_special(metadata: &std::fs::Metadata) -> bool {
    const REPARSE_POINT: u32 = 0x400;
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x40000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x400000;
    metadata.file_attributes() & (REPARSE_POINT | OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS)
        != 0
}

/// Сколько весит папка: байты и файлы.
fn measure(dir: &Path, older_than: Option<SystemTime>, stop: &AtomicBool) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut stack = vec![(dir.to_path_buf(), older_than.is_some())];
    while let Some((current, top)) = stack.pop() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            // У временных файлов считаем только старые — на верхнем уровне:
            // их и будем удалять.
            if top {
                if let (Some(limit), Ok(modified)) = (older_than, metadata.modified()) {
                    if modified > limit {
                        continue;
                    }
                }
            }
            if is_special(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                stack.push((entry.path(), false));
            } else {
                bytes += metadata.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// Когда проект трогали последний раз: самое свежее изменение среди того,
/// что лежит прямо в нём, кроме самих пакетов.
fn project_touched(project: &Path) -> Option<SystemTime> {
    std::fs::read_dir(project)
        .ok()?
        .flatten()
        .filter(|entry| !entry.file_name().eq_ignore_ascii_case("node_modules"))
        .filter_map(|entry| entry.metadata().ok()?.modified().ok())
        .max()
}

/// Что внутри мусора оставить: папки верхнего уровня, откуда запущены
/// программы.
fn keep_running(dir: &Path, running: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let lower = |part: std::path::Component<'_>| part.as_os_str().to_string_lossy().to_lowercase();
    let dir_parts: Vec<String> = dir.components().map(lower).collect();
    let mut keep: Vec<PathBuf> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for exe in running {
        let parts: Vec<std::path::Component<'_>> = exe.components().collect();
        if parts.len() <= dir_parts.len()
            || !parts
                .iter()
                .zip(&dir_parts)
                .all(|(part, want)| lower(*part) == *want)
        {
            continue;
        }
        let child = dir.join(parts[dir_parts.len()].as_os_str());
        if !keep.iter().any(|kept| same_path(kept, &child)) {
            keep.push(child);
        }
        if let Some(name) = exe.file_name() {
            let name = name.to_string_lossy().to_string();
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    (keep, names)
}

/// Один ли это путь. Регистр в Windows не различается, а путь
/// к запущенной программе и путь из обхода могут записать его по-разному.
fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// Готовит находку: меряет и отмечает, что оставить.
fn found(
    path: PathBuf,
    kind: Kind,
    running: &[PathBuf],
    worth: u64,
    stop: &AtomicBool,
) -> Option<Found> {
    let older_than = (kind == Kind::Temp).then(|| SystemTime::now() - OLD_TEMP);
    let (keep, kept_for) = keep_running(&path, running);
    let (mut bytes, mut files) = measure(&path, older_than, stop);
    // Оставляемое не освобождается — и в размер не входит.
    for kept in &keep {
        let (kept_bytes, kept_files) = measure(kept, None, stop);
        bytes = bytes.saturating_sub(kept_bytes);
        files = files.saturating_sub(kept_files);
    }
    (bytes >= worth).then_some(Found {
        path,
        kind,
        bytes,
        files,
        keep,
        kept_for,
        older_than,
    })
}

/// Ищет мусор. Долго и в фоне: вызывать из своего потока.
///
/// `seen` — сколько папок просмотрено, для строки хода в окне.
/// `worth` — с какого размера находка стоит упоминания; в работе это
/// [`WORTH`], в проверках — байт.
pub fn scan(
    roots: &[PathBuf],
    places: &[(PathBuf, Kind)],
    running: &[PathBuf],
    worth: u64,
    stop: &AtomicBool,
    seen: &AtomicU64,
) -> Vec<Found> {
    let mut result: Vec<Found> = Vec::new();
    let now = SystemTime::now();

    for (path, kind) in places {
        if stop.load(Ordering::Relaxed) {
            return result;
        }
        if path.is_dir() {
            if let Some(item) = found(path.clone(), *kind, running, worth, stop) {
                result.push(item);
            }
        }
    }

    let mut visited: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut stack: Vec<(PathBuf, usize)> = roots.iter().map(|root| (root.clone(), 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        if stop.load(Ordering::Relaxed) || seen.load(Ordering::Relaxed) >= FOLDER_BUDGET {
            break;
        }
        // Профиль лежит и внутри корня диска: второй раз его не обходим.
        if !visited.insert(dir.clone()) {
            continue;
        }
        seen.fetch_add(1, Ordering::Relaxed);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };

        let mut files: Vec<String> = Vec::new();
        let mut dirs: Vec<(String, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();
            if metadata.is_dir() {
                if !is_special(&metadata) {
                    dirs.push((name, entry.path()));
                }
            } else {
                files.push(name);
            }
        }

        // Папка сама себя назвала кэшем — стандартной меткой CACHEDIR.TAG.
        // Корни не в счёт: удалять диск или профиль целиком не предложим
        // никогда, что бы в них ни лежало.
        if depth > 0
            && files
                .iter()
                .any(|name| name.eq_ignore_ascii_case("CACHEDIR.TAG"))
        {
            if let Some(item) = found(dir.clone(), Kind::TaggedCache, running, worth, stop) {
                result.push(item);
            }
            continue;
        }

        let markers = Markers::from_files(files.iter().map(String::as_str));
        for (name, path) in dirs {
            let lower = name.to_ascii_lowercase();
            if let Some(kind) = classify(&name, &markers) {
                let fresh = kind == Kind::NodeModules
                    && project_touched(&dir)
                        .and_then(|touched| now.duration_since(touched).ok())
                        .is_none_or(|age| age < STALE_PROJECT);
                if !fresh {
                    if let Some(item) = found(path, kind, running, worth, stop) {
                        result.push(item);
                    }
                }
                continue;
            }
            // Пакеты рабочего проекта не мусор, но и внутри искать нечего:
            // там сотни тысяч папок чужого кода.
            //
            // Скрытые папки с точкой — хозяйство инструментов: .cache,
            // .vscode, .cursor. На живой машине в .cache\kilo лежал
            // node_modules установленного расширения, и по признакам он
            // выглядел заброшенным проектом. Удалить его значило сломать
            // инструмент, а не освободить место.
            if lower == "node_modules"
                || lower.starts_with('.')
                || SKIP_DIRS.contains(&lower.as_str())
            {
                continue;
            }
            if depth + 1 < DEPTH {
                stack.push((path, depth + 1));
            }
        }
    }

    let mut result = without_nested(result);
    result.sort_by_key(|item| core::cmp::Reverse(item.bytes));
    result
}

/// Убирает повторы: одна и та же папка, найденная дважды, или папка
/// внутри уже найденной. Кэш Gradle нашёлся и как известное место,
/// и по своей метке CACHEDIR.TAG — дважды посчитанные гигабайты
/// были бы враньём в сумме.
fn without_nested(mut items: Vec<Found>) -> Vec<Found> {
    let key = |item: &Found| item.path.to_string_lossy().to_lowercase();
    // Короткие пути первыми: внешняя папка встречается раньше вложенных.
    items.sort_by_key(|item| key(item).len());
    let mut kept: Vec<Found> = Vec::new();
    for item in items {
        let path = key(&item);
        let inside = kept.iter().any(|outer| {
            let outer = key(outer);
            path == outer || path.starts_with(&format!("{outer}\\"))
        });
        if !inside {
            kept.push(item);
        }
    }
    kept
}

/// Чем кончилось удаление.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cleanup {
    pub freed: u64,
    /// Сколько файлов не удалилось: заняты или нет прав.
    pub failed: u64,
}

/// Удаляет найденное, оставляя то, откуда запущены программы.
///
/// Перед удалением находка проверяется заново: между поиском и нажатием
/// могли пройти часы, и папка за это время могла стать чем-то другим.
pub fn delete(item: &Found, running: &[PathBuf]) -> Result<Cleanup, String> {
    if !still_junk(item) {
        return Err(bamboo_core::pick(
            "папка изменилась с момента поиска — поищите заново",
            "the folder changed since the search — search again",
        )
        .to_string());
    }
    let (mut keep, _) = keep_running(&item.path, running);
    keep.extend(item.keep.iter().cloned());

    let mut cleanup = Cleanup::default();
    let Ok(entries) = std::fs::read_dir(&item.path) else {
        return Err(bamboo_core::pick("папку не открыть", "cannot open the folder").to_string());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.iter().any(|kept| same_path(kept, &path)) {
            continue;
        }
        // Своя пометка снимается отдельно, до удаления.
        if entry.file_name().eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        if let (Some(limit), Ok(modified)) = (
            item.older_than,
            entry.metadata().and_then(|metadata| metadata.modified()),
        ) {
            if modified > limit {
                continue;
            }
        }
        remove_tree(&path, &mut cleanup);
    }

    // Проектный мусор уходит вместе с папкой, если в ней ничего не осталось.
    // Кэши и временные — остаются пустыми папками: их ждут программы.
    if item.kind.in_project() && keep.is_empty() {
        let _ = std::fs::remove_dir(&item.path);
    }
    Ok(cleanup)
}

/// Удаляет файл или дерево, считая освобождённое. Занятое пропускает.
fn remove_tree(path: &Path, cleanup: &mut Cleanup) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        // Ссылку удаляем как ссылку: то, на что она указывает, — не наше.
        if !is_special(&metadata) {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    remove_tree(&entry.path(), cleanup);
                }
            }
        }
        let _ = std::fs::remove_dir(path);
        return;
    }
    let size = metadata.len();
    let mut removed = std::fs::remove_file(path).is_ok();
    if !removed && metadata.permissions().readonly() {
        let mut writable = metadata.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        writable.set_readonly(false);
        removed =
            std::fs::set_permissions(path, writable).is_ok() && std::fs::remove_file(path).is_ok();
    }
    if removed {
        cleanup.freed += size;
    } else {
        cleanup.failed += 1;
    }
}

/// Остаётся ли находка мусором: признак проекта на месте.
fn still_junk(item: &Found) -> bool {
    if !item.path.is_dir() {
        return false;
    }
    // Корни и профиль не удаляются никогда — страховка от ошибки в поиске.
    if item.path.parent().is_none() || item.path.components().count() < 3 {
        return false;
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        if item.path == Path::new(&home) {
            return false;
        }
    }
    if !item.kind.in_project() {
        return true;
    }
    if item.kind == Kind::TaggedCache {
        return item.path.join("CACHEDIR.TAG").exists();
    }
    let Some(parent) = item.path.parent() else {
        return false;
    };
    let names: Vec<String> = std::fs::read_dir(parent)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    let markers = Markers::from_files(names.iter().map(String::as_str));
    item.path
        .file_name()
        .and_then(|name| classify(&name.to_string_lossy(), &markers))
        == Some(item.kind)
}

/// Что знает окно о мусоре.
#[derive(Debug, Default)]
pub struct State {
    pub scanning: bool,
    pub found: Vec<Found>,
    /// Когда закончился последний поиск.
    pub finished_at: Option<SystemTime>,
    /// Итоги удаления по папкам.
    pub outcomes: Vec<(PathBuf, String)>,
    /// Что удаляется прямо сейчас.
    pub deleting: Option<PathBuf>,
}

/// Состояние, общее для окна и фоновых потоков.
pub type Shared = std::sync::Arc<std::sync::Mutex<State>>;

/// Сколько папок просмотрено текущим поиском.
pub type Seen = std::sync::Arc<AtomicU64>;

/// Пути к запущенным программам: их папки не удаляются.
fn running_images() -> Vec<PathBuf> {
    let mut buffer = bamboo_sys::ProcessBuffer::new();
    if buffer.refresh().is_err() {
        return Vec::new();
    }
    buffer
        .iter()
        .filter_map(|process| bamboo_sys::process::full_image_path(process.pid()).ok())
        .map(PathBuf::from)
        .collect()
}

/// Ищет мусор в этом потоке и обновляет состояние. Долго.
pub fn run_scan(shared: &Shared, seen: &AtomicU64) {
    {
        let Ok(mut state) = shared.lock() else {
            return;
        };
        if state.scanning {
            return;
        }
        state.scanning = true;
    }
    // Обход читает десятки тысяч каталогов: пусть уступает всем.
    bamboo_sys::foldermark::background_thread();
    seen.store(0, Ordering::Relaxed);
    let running = running_images();
    let found = scan(
        &project_roots(),
        &known_places(),
        &running,
        WORTH,
        &AtomicBool::new(false),
        seen,
    );
    if bamboo_sys::mark_junk_enabled() {
        sync_marks(&found, true);
    }
    if let Ok(mut state) = shared.lock() {
        state.found = found;
        state.finished_at = Some(SystemTime::now());
        state.outcomes.clear();
        state.scanning = false;
    }
}

/// Запускает поиск в своём потоке, если он ещё не идёт.
pub fn start_scan(shared: &Shared, seen: &Seen) {
    if shared.lock().map(|state| state.scanning).unwrap_or(true) {
        return;
    }
    let shared = shared.clone();
    let seen = seen.clone();
    let _ = std::thread::Builder::new()
        .name("bamboo-junk".into())
        .spawn(move || run_scan(&shared, &seen));
}

/// Удаляет найденное по пути в своём потоке.
pub fn start_delete(shared: &Shared, path: PathBuf) {
    let item = {
        let Ok(mut state) = shared.lock() else {
            return;
        };
        if state.deleting.is_some() {
            return;
        }
        let Some(item) = state.found.iter().find(|item| item.path == path).cloned() else {
            return;
        };
        state.deleting = Some(path.clone());
        item
    };
    let shared = shared.clone();
    let _ = std::thread::Builder::new()
        .name("bamboo-junk-delete".into())
        .spawn(move || {
            bamboo_sys::foldermark::background_thread();
            let _ = bamboo_sys::foldermark::unmark(&item.path);
            forget_mark(&item.path);
            let outcome = match delete(&item, &running_images()) {
                Ok(cleanup) if cleanup.failed == 0 => bamboo_core::say(
                    "Удалено, освобождено {freed}.",
                    "Deleted, {freed} freed.",
                    &[("freed", &bamboo_core::Bytes(cleanup.freed).to_string())],
                ),
                Ok(cleanup) => bamboo_core::say(
                    "Освобождено {freed}. Файлов осталось: {left} — они заняты программами или защищены.",
                    "{freed} freed. {left} files remain — in use or protected.",
                    &[
                        ("freed", &bamboo_core::Bytes(cleanup.freed).to_string()),
                        ("left", &cleanup.failed.to_string()),
                    ],
                ),
                Err(why) => why,
            };
            if let Ok(mut state) = shared.lock() {
                state.deleting = None;
                state.outcomes.retain(|(done, _)| *done != item.path);
                state.outcomes.push((item.path.clone(), outcome));
            }
        });
}

/// Включает или выключает пометки в Проводнике и приводит их в порядок.
pub fn start_sync_marks(shared: &Shared, enabled: bool) {
    let found = shared
        .lock()
        .map(|state| state.found.clone())
        .unwrap_or_default();
    let _ = std::thread::Builder::new()
        .name("bamboo-junk-marks".into())
        .spawn(move || {
            sync_marks(&found, enabled);
        });
}

fn bamboo_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Bamboo")
}

/// Где Bamboo помнит свои пометки: снять их надо и тогда, когда мусор
/// уже не находится, — иначе красные папки остались бы навсегда.
fn marks_file() -> PathBuf {
    bamboo_dir().join("помеченный-мусор.txt")
}

fn load_marks() -> Vec<PathBuf> {
    std::fs::read_to_string(marks_file())
        .map(|text| {
            text.lines()
                .filter(|l| !l.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

fn save_marks(marks: &[PathBuf]) {
    let text: String = marks
        .iter()
        .map(|path| format!("{}\n", path.display()))
        .collect();
    let _ = std::fs::write(marks_file(), text);
}

fn forget_mark(path: &Path) {
    let mut marks = load_marks();
    marks.retain(|marked| !same_path(marked, path));
    save_marks(&marks);
}

/// Значок пометки. Лежит у Bamboo, а не внутри программы: обновление
/// заменяет исполняемый файл, и значок из него на время пропадал бы.
fn icon_file() -> PathBuf {
    let path = bamboo_dir().join("junk.ico");
    const ICON: &[u8] = include_bytes!("../assets/junk.ico");
    if std::fs::read(&path)
        .map(|bytes| bytes != ICON)
        .unwrap_or(true)
    {
        let _ = std::fs::create_dir_all(bamboo_dir());
        let _ = std::fs::write(&path, ICON);
    }
    path
}

/// Подсказка при наведении на помеченную папку.
fn tip(item: &Found) -> String {
    bamboo_core::say(
        "Bamboo: {what}, {size}. Удаляется без потерь — {back}. Удалить можно в Bamboo, раздел «Мусор».",
        "Bamboo: {what}, {size}. Safe to delete — {back}. Delete it in Bamboo, the Junk section.",
        &[
            ("what", item.kind.what()),
            ("size", &bamboo_core::Bytes(item.bytes).to_string()),
            ("back", item.kind.comes_back()),
        ],
    )
}

/// Приводит пометки в Проводнике к найденному: мусор в проектах помечен,
/// всё прочее, помеченное прежде, — снято. Возвращает, сколько помечено.
pub fn sync_marks(found: &[Found], enabled: bool) -> usize {
    let previous = load_marks();
    let mut marked: Vec<PathBuf> = Vec::new();
    if enabled {
        let icon = icon_file();
        for item in found.iter().filter(|item| item.kind.in_project()) {
            if bamboo_sys::foldermark::mark(&item.path, &icon, &tip(item)).is_ok() {
                marked.push(item.path.clone());
            }
        }
    }
    for old in &previous {
        if !marked.iter().any(|now| same_path(now, old)) {
            let _ = bamboo_sys::foldermark::unmark(old);
        }
    }
    save_marks(&marked);
    marked.len()
}

/// Строка найденного для окна.
pub struct Row {
    pub path: String,
    pub what: String,
    pub size: String,
    pub note: String,
    pub done: bool,
}

/// Строки для окна: найденное с итогами удаления.
pub fn rows(state: &State) -> Vec<Row> {
    state
        .found
        .iter()
        .map(|item| {
            let outcome = state
                .outcomes
                .iter()
                .find(|(path, _)| *path == item.path)
                .map(|(_, text)| text.clone());
            let busy = state.deleting.as_ref() == Some(&item.path);
            let note = match (&outcome, busy) {
                (Some(text), _) => text.clone(),
                (None, true) => bamboo_core::pick("Удаляю…", "Deleting…").to_string(),
                (None, false) if !item.kept_for.is_empty() => bamboo_core::say(
                    "Папку, откуда запущена {programs}, Bamboo оставит — она в размер не входит.",
                    "The folder {programs} runs from is kept and not counted.",
                    &[("programs", &item.kept_for.join(", "))],
                ),
                (None, false) => String::new(),
            };
            Row {
                path: item.path.display().to_string(),
                what: format!(
                    "{} · {} · {}",
                    item.kind.what(),
                    file_count(item.files),
                    item.kind.comes_back()
                ),
                size: bamboo_core::Bytes(item.bytes).to_string(),
                note,
                done: outcome.is_some() || busy,
            }
        })
        .collect()
}

/// «1 файл», «3 файла», «47244 файлов».
fn file_count(count: u64) -> String {
    let russian = match (count % 10, count % 100) {
        (1, rest) if rest != 11 => "файл",
        (2..=4, rest) if !(12..=14).contains(&rest) => "файла",
        _ => "файлов",
    };
    let english = if count == 1 { "file" } else { "files" };
    format!("{count} {}", bamboo_core::pick(russian, english))
}

/// Строка хода поиска.
pub fn status(state: &State, seen: u64) -> String {
    if state.scanning {
        return bamboo_core::say(
            "Ищу… просмотрено папок: {seen}. Поиск идёт в фоне и уступает диск всем остальным.",
            "Searching… folders seen: {seen}. It runs in the background and yields the disk to everything else.",
            &[("seen", &seen.to_string())],
        );
    }
    let Some(finished) = state.finished_at else {
        return bamboo_core::pick(
            "Поиск идёт сам через десять минут после запуска Bamboo и раз в сутки. Можно и сейчас — кнопкой.",
            "The search runs by itself ten minutes after Bamboo starts and once a day. Or now, with the button.",
        )
        .to_string();
    };
    let checked = checked_ago(
        SystemTime::now()
            .duration_since(finished)
            .unwrap_or_default(),
    );
    let left: Vec<&Found> = state
        .found
        .iter()
        .filter(|item| !state.outcomes.iter().any(|(path, _)| *path == item.path))
        .collect();
    if left.is_empty() {
        return bamboo_core::say(
            "Мусора больше 100 МБ не нашлось. {checked}",
            "No junk over 100 MB found. {checked}",
            &[("checked", &checked)],
        );
    }
    let total: u64 = left.iter().map(|item| item.bytes).sum();
    bamboo_core::say(
        "Найдено мест: {count}, вместе {total}. {checked}",
        "Places found: {count}, {total} in all. {checked}",
        &[
            ("count", &left.len().to_string()),
            ("total", &bamboo_core::Bytes(total).to_string()),
            ("checked", &checked),
        ],
    )
}

/// «Проверено только что», «Проверено 3 ч назад».
fn checked_ago(age: Duration) -> String {
    let minutes = age.as_secs() / 60;
    let (value, ru, en) = match minutes {
        0 => return bamboo_core::pick("Проверено только что.", "Checked just now.").to_string(),
        1..=59 => (minutes, "мин", "min"),
        60..=2879 => (minutes / 60, "ч", "h"),
        _ => (minutes / 1440, "дн", "d"),
    };
    bamboo_core::say(
        "Проверено {value} {unit} назад.",
        "Checked {value} {unit} ago.",
        &[
            ("value", &value.to_string()),
            ("unit", bamboo_core::pick(ru, en)),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bamboo-junk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("папка для проверки");
        dir
    }

    fn file(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    fn age(path: &Path, days: u64) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(days * 24 * 3600))
            .unwrap();
    }

    /// Настоящий обход этой машины: сколько длится и что находит.
    /// Ничего не удаляет. Запуск: cargo test -p bamboo-agent probe_this_machine -- --ignored --nocapture
    #[test]
    #[ignore]
    fn probe_this_machine() {
        let started = std::time::Instant::now();
        let seen = AtomicU64::new(0);
        let found = scan(
            &project_roots(),
            &known_places(),
            &[],
            WORTH,
            &AtomicBool::new(false),
            &seen,
        );
        println!(
            "папок просмотрено: {}, за {:.1} с",
            seen.load(Ordering::Relaxed),
            started.elapsed().as_secs_f64()
        );
        for item in &found {
            println!(
                "{:>10} {:>8} файлов  {:?}  {}",
                bamboo_core::Bytes(item.bytes).to_string(),
                item.files,
                item.kind,
                item.path.display()
            );
        }
    }

    #[test]
    fn the_same_place_is_counted_once() {
        let item = |path: &str, bytes| Found {
            path: PathBuf::from(path),
            kind: Kind::PackageCache,
            bytes,
            files: 1,
            keep: Vec::new(),
            kept_for: Vec::new(),
            older_than: None,
        };
        let kept = without_nested(vec![
            item(r"C:\Users\u\.gradle\caches", 800),
            item(r"C:\Users\u\.gradle\caches", 800),
            item(r"C:\Users\u\.cargo\registry\cache", 170),
            item(r"C:\Users\u\.cargo\registry", 960),
            item(r"C:\Users\u\.cargo\registry-old", 10),
        ]);
        let paths: Vec<String> = kept.iter().map(|i| i.path.display().to_string()).collect();
        assert_eq!(kept.len(), 3, "{paths:?}");
        assert!(paths.contains(&r"C:\Users\u\.cargo\registry-old".to_string()));
    }

    #[test]
    fn tool_folders_are_not_projects() {
        // Установленное расширение в .cache выглядит как заброшенный проект.
        let root = scratch("tools");
        let tool = root.join(".cache").join("kilo").join("packages").join("x");
        file(&tool.join("package.json"), 10);
        file(&tool.join("node_modules").join("a").join("i.js"), 500);
        age(&tool.join("package.json"), 90);
        assert!(scan_small(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_name_without_its_project_is_not_junk() {
        // «target» без Cargo.toml рядом может быть чем угодно.
        let nothing = Markers::default();
        assert_eq!(classify("target", &nothing), None);
        assert_eq!(classify("node_modules", &nothing), None);
        assert_eq!(classify("bin", &nothing), None);

        let rust = Markers::from_files(["Cargo.toml", "README.md"]);
        assert_eq!(classify("target", &rust), Some(Kind::RustBuild));
        let dotnet = Markers::from_files(["Shop.csproj"]);
        assert_eq!(classify("obj", &dotnet), Some(Kind::DotnetBuild));
        let web = Markers::from_files(["package.json"]);
        assert_eq!(classify(".next", &web), Some(Kind::WebCache));
        assert_eq!(classify("src", &web), None);
    }

    #[test]
    fn a_rust_build_cache_is_found_and_measured() {
        let root = scratch("rust");
        file(&root.join("proj").join("Cargo.toml"), 10);
        file(&root.join("proj").join("src").join("main.rs"), 10);
        file(
            &root
                .join("proj")
                .join("target")
                .join("debug")
                .join("big.rlib"),
            1000,
        );
        let found = scan_small(&root);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, Kind::RustBuild);
        assert_eq!(found[0].bytes, 1000);
        assert!(found[0].path.ends_with("target"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Обход с пределом в один байт вместо ста мегабайт: гигабайты
    /// на проверку не пишем.
    fn scan_small(root: &Path) -> Vec<Found> {
        let stop = AtomicBool::new(false);
        let seen = AtomicU64::new(0);
        scan(&[root.to_path_buf()], &[], &[], 1, &stop, &seen)
    }

    #[test]
    fn a_running_program_is_kept_with_its_folder() {
        // Живой случай: Bamboo запущен из target\release того самого
        // проекта, чей кэш сборки занимал 85 ГБ.
        let root = scratch("keep");
        let target = root.join("target");
        file(&target.join("debug").join("incremental").join("a.bin"), 700);
        file(&target.join("release").join("bamboo-agent.exe"), 300);
        let running = vec![target.join("release").join("bamboo-agent.exe")];
        let (keep, names) = keep_running(&target, &running);
        assert_eq!(keep, vec![target.join("release")]);
        assert_eq!(names, vec!["bamboo-agent.exe".to_string()]);

        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        let item = Found {
            path: target.clone(),
            kind: Kind::RustBuild,
            bytes: 700,
            files: 1,
            keep,
            kept_for: names,
            older_than: None,
        };
        let cleanup = delete(&item, &running).expect("удаление");
        assert_eq!(cleanup.freed, 700);
        assert!(!target.join("debug").exists());
        assert!(target.join("release").join("bamboo-agent.exe").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_fresh_project_keeps_its_packages() {
        let root = scratch("npm");
        file(&root.join("fresh").join("package.json"), 10);
        file(
            &root
                .join("fresh")
                .join("node_modules")
                .join("x")
                .join("i.js"),
            500,
        );
        file(&root.join("old").join("package.json"), 10);
        file(
            &root.join("old").join("node_modules").join("x").join("i.js"),
            500,
        );
        age(&root.join("old").join("package.json"), 60);
        let found = scan_small(&root);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].path.starts_with(root.join("old")));
        assert_eq!(found[0].kind, Kind::NodeModules);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_old_temporary_files_go() {
        let root = scratch("temp");
        file(&root.join("old.tmp"), 400);
        file(&root.join("new.tmp"), 300);
        age(&root.join("old.tmp"), 30);
        let limit = SystemTime::now() - OLD_TEMP;
        let (bytes, _) = measure(&root, Some(limit), &AtomicBool::new(false));
        assert_eq!(bytes, 400, "в размер вошли свежие файлы");

        let item = Found {
            path: root.clone(),
            kind: Kind::Temp,
            bytes,
            files: 1,
            keep: Vec::new(),
            kept_for: Vec::new(),
            older_than: Some(limit),
        };
        let cleanup = delete(&item, &[]).expect("удаление");
        assert_eq!(cleanup.freed, 400);
        assert!(
            root.join("new.tmp").exists(),
            "свежий временный файл удалён"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_that_stopped_being_junk_is_not_deleted() {
        // Между поиском и нажатием Cargo.toml убрали — это уже не кэш.
        let root = scratch("changed");
        file(&root.join("target").join("keep.txt"), 100);
        let item = Found {
            path: root.join("target"),
            kind: Kind::RustBuild,
            bytes: 100,
            files: 1,
            keep: Vec::new(),
            kept_for: Vec::new(),
            older_than: None,
        };
        assert!(delete(&item, &[]).is_err());
        assert!(root.join("target").join("keep.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
