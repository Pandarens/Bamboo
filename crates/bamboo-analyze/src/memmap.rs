//! Куда ушла память — разбор занятой памяти по частям.
//!
//! Вопрос человека с живой машины: «используется 12 ГБ, а я не вижу
//! процессов, которые это занимают». Он прав: Диспетчер задач показывает
//! у процессов только их личную память в ОЗУ, и там было 7,3 ГБ из 12,8 ГБ.
//! Остальные пять с половиной строки в списке процессов не имеют вовсе:
//!
//! - ядро и драйверы — 1,3 ГБ;
//! - файлы, с которыми система работает, — 0,9 ГБ;
//! - видеокарта: у встроенной Intel UHD 730 своей памяти нет, она берёт
//!   её из ОЗУ — 0,8 ГБ, и это видно только на вкладке GPU;
//! - общие библиотеки, таблицы страниц, защищённое ядро — остаток,
//!   который встроенными средствами Windows точнее не разложить.
//!
//! Этот модуль складывает части и называет остаток остатком, не выдавая
//! его за измерение. Чистая логика: входы — числа, всё проверяется тестами.

use std::collections::HashMap;

use bamboo_core::{pick, Bytes};

/// Часть памяти.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    /// Личная память программ в ОЗУ.
    Programs,
    /// Сжатая память Windows.
    Compression,
    /// Память, которую видеокарта взяла из ОЗУ.
    Graphics,
    /// Ядро и драйверы.
    Kernel,
    /// Файлы в работе и изменения, ждущие записи.
    FileCache,
    /// Всё, что занято, но по частям не измеряется.
    Other,
    /// Кэш ожидания: занят, но отдаётся по первому требованию.
    Standby,
    /// Совсем пустые страницы.
    Free,
}

impl PartKind {
    pub fn label(self) -> &'static str {
        match self {
            PartKind::Programs => pick("Программы", "Programs"),
            PartKind::Compression => pick("Сжатая память", "Compressed memory"),
            PartKind::Graphics => pick("Видеокарта", "Graphics"),
            PartKind::Kernel => pick("Ядро и драйверы", "Kernel and drivers"),
            PartKind::FileCache => pick("Файлы в работе", "Files in use"),
            PartKind::Other => pick("Прочее системное", "Other system memory"),
            PartKind::Standby => pick("Кэш — считается свободным", "Cache — counts as free"),
            PartKind::Free => pick("Совсем свободно", "Completely free"),
        }
    }

    /// Что это такое — одной фразой, языком человека.
    pub fn hint(self) -> &'static str {
        match self {
            PartKind::Programs => pick(
                "Личная память программ в ОЗУ — ровно то, что Диспетчер задач показывает у процессов.",
                "Private memory of programs in RAM — exactly what Task Manager shows for processes.",
            ),
            PartKind::Compression => pick(
                "Windows сжимает редкие страницы, чтобы не писать их на диск. Это экономия, а не потеря.",
                "Windows compresses rarely used pages instead of writing them to disk. This saves memory rather than wasting it.",
            ),
            PartKind::Graphics => pick(
                "У встроенной видеокарты своей памяти нет — она берёт её из ОЗУ. У процессов в Диспетчере задач этого не видно.",
                "Integrated graphics has no memory of its own and borrows it from RAM. Task Manager does not show this for processes.",
            ),
            PartKind::Kernel => pick(
                "Сама Windows и драйверы устройств. Если растёт день ото дня — течёт драйвер.",
                "Windows itself and device drivers. If it grows day after day, a driver is leaking.",
            ),
            PartKind::FileCache => pick(
                "Файлы, с которыми система работает прямо сейчас, и изменения, ждущие записи на диск.",
                "Files the system is working with right now, and changes waiting to be written to disk.",
            ),
            PartKind::Other => pick(
                "Общие библиотеки, которыми пользуются сразу многие программы, таблицы страниц, защищённое ядро. Точнее встроенными средствами Windows не разложить.",
                "Libraries shared by many programs at once, page tables, the secure kernel. Windows offers no finer breakdown by its own means.",
            ),
            PartKind::Standby => pick(
                "Недавние файлы про запас. Windows отдаёт эту память программам по первому требованию — это не потеря.",
                "Recent files kept in reserve. Windows hands this memory to programs on first demand, so nothing is lost.",
            ),
            PartKind::Free => pick("Пустые страницы.", "Empty pages."),
        }
    }

    /// Видна ли часть в списке процессов Диспетчера задач.
    pub fn in_task_manager(self) -> bool {
        matches!(self, PartKind::Programs | PartKind::Compression)
    }

    /// Занята ли по-настоящему — или отдаётся по первому требованию.
    pub fn in_use(self) -> bool {
        !matches!(self, PartKind::Standby | PartKind::Free)
    }
}

/// Что измерено. `None` — счётчика в системе нет, и тогда часть
/// не показывается, а её память уходит в остаток.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryFacts {
    pub total: Bytes,
    pub available: Bytes,
    /// Личная память всех программ в ОЗУ, кроме сжатой памяти.
    pub programs: Bytes,
    pub compression: Bytes,
    pub graphics: Option<Bytes>,
    pub pool_nonpaged: Option<Bytes>,
    pub pool_paged: Option<Bytes>,
    pub kernel_code: Option<Bytes>,
    pub file_cache: Option<Bytes>,
    pub modified: Option<Bytes>,
    pub free_zero: Option<Bytes>,
}

/// Одна часть раскладки.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryPart {
    pub kind: PartKind,
    pub bytes: Bytes,
    /// Доля от всей памяти, 0..1.
    pub share: f64,
}

/// Раскладывает память по частям.
///
/// Занятые части складываются ровно в «занято» Диспетчера задач: что
/// не измерено по частям, уходит в остаток. Остаток не бывает меньше нуля —
/// счётчики снимаются не в один миг, и измеренное может чуть перевалить
/// за «занято»; выдумывать отрицательную память хуже, чем промолчать.
pub fn breakdown(facts: &MemoryFacts) -> Vec<MemoryPart> {
    let total = facts.total.as_u64();
    if total == 0 {
        return Vec::new();
    }
    let available = facts.available.as_u64().min(total);
    let in_use = total - available;

    let sum = |values: &[Option<Bytes>]| -> Option<u64> {
        let known: Vec<u64> = values
            .iter()
            .flatten()
            .map(|bytes| bytes.as_u64())
            .collect();
        (!known.is_empty()).then(|| known.iter().sum())
    };

    let mut parts: Vec<(PartKind, u64)> = vec![
        (PartKind::Programs, facts.programs.as_u64()),
        (PartKind::Compression, facts.compression.as_u64()),
    ];
    if let Some(graphics) = facts.graphics {
        parts.push((PartKind::Graphics, graphics.as_u64()));
    }
    if let Some(kernel) = sum(&[facts.pool_nonpaged, facts.pool_paged, facts.kernel_code]) {
        parts.push((PartKind::Kernel, kernel));
    }
    if let Some(files) = sum(&[facts.file_cache, facts.modified]) {
        parts.push((PartKind::FileCache, files));
    }
    let measured: u64 = parts.iter().map(|(_, bytes)| bytes).sum();
    parts.push((PartKind::Other, in_use.saturating_sub(measured)));

    match facts.free_zero {
        Some(free) => {
            let free = free.as_u64().min(available);
            parts.push((PartKind::Standby, available - free));
            parts.push((PartKind::Free, free));
        }
        // Без счётчика пустых страниц кэш от свободного не отличить —
        // показываем одной частью, подсказка у кэша объясняет обе.
        None => parts.push((PartKind::Standby, available)),
    }

    parts
        .into_iter()
        .filter(|(kind, bytes)| *bytes > 0 || *kind == PartKind::Programs)
        .map(|(kind, bytes)| MemoryPart {
            kind,
            bytes: Bytes(bytes),
            share: bytes as f64 / total as f64,
        })
        .collect()
}

/// Программа, сложенная из своих процессов.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppTotal {
    pub name: String,
    pub processes: usize,
    pub bytes: Bytes,
}

/// Складывает процессы в программы по имени, крупные первыми.
///
/// Ради этого и нужно: у Chrome на живой машине был 41 процесс, и в списке
/// он размазан по сорока одной строке, каждая из которых невелика. Сложенный,
/// он оказался главным потребителем — три гигабайта.
pub fn top_apps<'a>(
    processes: impl IntoIterator<Item = (&'a str, Bytes)>,
    count: usize,
) -> Vec<AppTotal> {
    let mut totals: HashMap<String, AppTotal> = HashMap::new();
    for (name, bytes) in processes {
        let total = totals
            .entry(name.to_lowercase())
            .or_insert_with(|| AppTotal {
                name: name.to_string(),
                processes: 0,
                bytes: Bytes::ZERO,
            });
        total.processes += 1;
        total.bytes = Bytes(total.bytes.as_u64() + bytes.as_u64());
    }
    let mut apps: Vec<AppTotal> = totals
        .into_values()
        .filter(|app| app.bytes.as_u64() > 0)
        .collect();
    apps.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    apps.truncate(count);
    apps
}

/// С какого размера память видеокарты у программы стоит обсуждать.
///
/// Двести мегабайт. На живой машине Word держал 259 МБ — для текстового
/// редактора много, а Chrome со всеми вкладками обходился 154 МБ.
pub const HEAVY_GRAPHICS: Bytes = Bytes(200 * 1024 * 1024);

/// Системные части, у которых аппаратное ускорение не выключить: отрисовка
/// окон, подсистема консоли, проводник. Советовать им нечего.
const GRAPHICS_SYSTEM: &[&str] = &["dwm.exe", "csrss.exe", "explorer.exe"];

/// Программы, которым стоит посоветовать выключить аппаратное ускорение.
pub fn heavy_graphics(apps: &[AppTotal]) -> Vec<&AppTotal> {
    apps.iter()
        .filter(|app| {
            app.bytes >= HEAVY_GRAPHICS
                && !GRAPHICS_SYSTEM
                    .iter()
                    .any(|system| app.name.eq_ignore_ascii_case(system))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1024 * 1024;

    fn mb(value: u64) -> Bytes {
        Bytes(value * MB)
    }

    /// Настоящий замер с машины, где «не видно, кто занимает 12 ГБ».
    fn the_real_machine() -> MemoryFacts {
        MemoryFacts {
            total: mb(16141),
            available: mb(3353),
            programs: mb(7284 - 510),
            compression: mb(510),
            graphics: Some(mb(780)),
            pool_nonpaged: Some(mb(606)),
            pool_paged: Some(mb(607)),
            kernel_code: Some(mb(63)),
            file_cache: Some(mb(903)),
            modified: Some(mb(22)),
            free_zero: Some(mb(389)),
        }
    }

    fn part(parts: &[MemoryPart], kind: PartKind) -> Option<u64> {
        parts
            .iter()
            .find(|part| part.kind == kind)
            .map(|part| part.bytes.as_u64() / MB)
    }

    #[test]
    fn the_parts_in_use_add_up_to_what_task_manager_calls_used() {
        let parts = breakdown(&the_real_machine());
        let in_use: u64 = parts
            .iter()
            .filter(|part| part.kind.in_use())
            .map(|part| part.bytes.as_u64())
            .sum();
        assert_eq!(
            in_use,
            mb(16141 - 3353).as_u64(),
            "части не сложились в «занято»"
        );
        // Остаток — то, что по частям не измерить: 2,5 ГБ на той машине.
        assert_eq!(part(&parts, PartKind::Other), Some(2523));
        assert_eq!(part(&parts, PartKind::Kernel), Some(606 + 607 + 63));
        assert_eq!(part(&parts, PartKind::Standby), Some(3353 - 389));
    }

    #[test]
    fn only_programs_and_compression_are_visible_in_task_manager() {
        // Ровно поэтому человек и не нашёл пять гигабайт: остальное строки
        // в списке процессов не имеет.
        let hidden: u64 = breakdown(&the_real_machine())
            .iter()
            .filter(|part| part.kind.in_use() && !part.kind.in_task_manager())
            .map(|part| part.bytes.as_u64() / MB)
            .sum();
        assert_eq!(hidden, 12788 - 7284);
    }

    #[test]
    fn the_remainder_never_goes_negative() {
        // Счётчики снимаются не в один миг, и измеренное может перевалить
        // за «занято». Отрицательной памяти не бывает.
        let mut facts = the_real_machine();
        facts.programs = mb(20_000);
        let parts = breakdown(&facts);
        assert_eq!(
            part(&parts, PartKind::Other),
            None,
            "остаток не должен уходить в минус"
        );
    }

    #[test]
    fn a_missing_counter_is_silence_not_a_zero() {
        // Нет счётчиков видеокарты — части нет вовсе, а её память честно
        // уходит в остаток.
        let mut facts = the_real_machine();
        facts.graphics = None;
        let parts = breakdown(&facts);
        assert_eq!(part(&parts, PartKind::Graphics), None);
        assert_eq!(part(&parts, PartKind::Other), Some(2523 + 780));
    }

    #[test]
    fn processes_of_one_program_are_added_together() {
        let mut processes: Vec<(&str, Bytes)> = vec![("chrome.exe", mb(74)); 41];
        processes.push(("Telegram.exe", mb(360)));
        processes.push(("CHROME.EXE", mb(10)));
        let apps = top_apps(processes, 5);
        assert_eq!(apps[0].name, "chrome.exe");
        assert_eq!(apps[0].processes, 42, "одноимённые процессы не сложились");
        assert_eq!(apps[0].bytes, mb(74 * 41 + 10));
        assert_eq!(apps[1].name, "Telegram.exe");
    }

    #[test]
    fn heavy_graphics_advice_skips_parts_of_windows_itself() {
        let apps = vec![
            AppTotal {
                name: "WINWORD.EXE".into(),
                processes: 1,
                bytes: mb(259),
            },
            AppTotal {
                name: "dwm.exe".into(),
                processes: 1,
                bytes: mb(253),
            },
            AppTotal {
                name: "chrome.exe".into(),
                processes: 41,
                bytes: mb(154),
            },
        ];
        let heavy: Vec<&str> = heavy_graphics(&apps)
            .iter()
            .map(|app| app.name.as_str())
            .collect();
        assert_eq!(
            heavy,
            vec!["WINWORD.EXE"],
            "советовать надо Word, а не отрисовке окон"
        );
    }
}
