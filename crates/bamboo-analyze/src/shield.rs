//! Защита переднего плана от нехватки памяти (ТЗ, разделы 10.3 и 11.2).
//!
//! Главная оптимизация, которую подсказали данные, а не догадка. Девять
//! суток наблюдения за живой машиной показали: подвисания там не от утечек
//! и не от диска, а от постоянного перебора памяти. Занято в среднем 88%,
//! из подкачки читается девять мегабайт в секунду по медиане, а жалоба —
//! набранный текст появляется с задержкой. Ровно так и бывает, когда
//! страницы программы переднего плана вытеснены и поднимаются обратно
//! с диска.
//!
//! Windows при нехватке памяти отбирает страницы не у всех поровну, а по
//! приоритету памяти: страницы с пониженным приоритетом отдаются первыми.
//! Значит, если фоновым программам без окна — встроенному браузеру Steam,
//! помощнику BlueStacks, службе VPN — понизить приоритет, то при нехватке
//! Windows будет забирать их страницы, а страницы программы, в которой
//! человек работает, останутся на месте.
//!
//! Это не «чистка памяти» из раздела 11.5 и ничего не освобождает: памяти
//! остаётся ровно столько же. Меняется только очередь на вытеснение —
//! кто отдаёт первым. Поэтому и хвастаться здесь нечем в мегабайтах.
//!
//! Прежняя автоматика делала то же самое наоборот. Приоритет понижался,
//! пока человека нет, и возвращался, как только он тронул мышь: журнал
//! живой машины насчитал двести таких возвратов. А защищать передний план
//! нужно ровно тогда, когда человек за компьютером и печатает.
//!
//! Главная опасность — задеть то, с чем человек работает. Поэтому
//! трогаются только программы, у которых во всей семье процессов нет
//! ни одного видимого окна: вкладка Chrome без окна — часть браузера
//! с окном, и её не трогаем; сборка, запущенная из терминала на переднем
//! плане, — часть терминала, и её тоже не трогаем.
//!
//! Чистая логика: о Windows здесь не знают, всё проверяется тестами.

use core::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use bamboo_core::Bytes;

/// Занятость памяти, с которой защита включается.
///
/// Восемьдесят процентов — та же граница, с которой чтение из подкачки
/// становится толкотней, а не загрузкой программ (см. детектор подвисаний).
const ENGAGE_AT: f64 = 0.80;

/// Занятость, ниже которой защита снимается.
///
/// Зазор против дребезга: на машине, где память колеблется около
/// восьмидесяти, без него защита включалась бы и снималась каждую минуту,
/// и каждый раз — запись в журнал.
const RELEASE_BELOW: f64 = 0.72;

/// С какого размера процесс стоит трогать.
///
/// Двести мегабайт. Опора — те, кто держал память на живой машине:
/// встроенный браузер Steam от 480 МБ до полутора гигабайт, помощник
/// BlueStacks 645 МБ, служба VPN до 540 МБ. Мелочь в десятки мегабайт
/// очереди на вытеснение не меняет, а запись в журнал стоит каждая.
const WORTH_SHIELDING: u64 = 200 * 1024 * 1024;

/// Сколько процессов держим разом.
const AT_MOST: usize = 12;

/// Процессы-оболочки: через них семьи не связываются.
///
/// Почти всё, что человек запустил сам, — потомок проводника, а у проводника
/// окно есть всегда. Свяжи семьи через него — и любая программа оказалась бы
/// «частью программы с окном», защищать было бы некого.
const SHELLS: &[&str] = &[
    "explorer.exe",
    "svchost.exe",
    "services.exe",
    "wininit.exe",
    "winlogon.exe",
    "userinit.exe",
    "sihost.exe",
    "taskhostw.exe",
    "runtimebroker.exe",
    "dllhost.exe",
    "smss.exe",
    "csrss.exe",
    "system",
];

/// Что известно о процессе.
#[derive(Clone, Debug)]
pub struct ShieldFacts<'a> {
    pub pid: u32,
    /// Номер родителя: по нему процесс относится к семье.
    pub parent_pid: u32,
    pub name: &'a str,
    pub memory: Bytes,
    /// Есть ли у процесса видимое окно с заголовком.
    pub has_window: bool,
    /// Трогать нельзя: неизменяемый список или уже держит другая автоматика.
    pub protected: bool,
    /// Похоже на утечку — тогда размер не важен: утечка вырастет.
    pub leaking: bool,
}

/// Состояние защиты между тиками.
#[derive(Clone, Debug, Default)]
pub struct Shield {
    engaged: bool,
}

impl Shield {
    pub fn new() -> Shield {
        Shield::default()
    }

    /// Включена ли защита сейчас.
    pub fn engaged(&self) -> bool {
        self.engaged
    }

    /// Номера процессов, которым сейчас нужен пониженный приоритет памяти.
    ///
    /// Пустой список — обычный исход: памяти хватает, или прикрывать
    /// передний план не от кого.
    pub fn wanted(
        &mut self,
        processes: &[ShieldFacts<'_>],
        memory_used_share: f64,
        foreground_pid: u32,
        own_pid: u32,
    ) -> Vec<u32> {
        if self.engaged {
            if memory_used_share < RELEASE_BELOW {
                self.engaged = false;
            }
        } else if memory_used_share >= ENGAGE_AT {
            self.engaged = true;
        }
        if !self.engaged {
            return Vec::new();
        }

        let index: HashMap<u32, usize> = processes
            .iter()
            .enumerate()
            .map(|(at, process)| (process.pid, at))
            .collect();

        // Кто на виду: у кого есть окно, кто на переднем плане, и сам Bamboo.
        // Вместе с ними на виду и их предки до оболочки: родитель, чей
        // интерфейс сейчас перед человеком, тоже часть того, с чем он работает.
        let mut seen = vec![false; processes.len()];
        for (at, process) in processes.iter().enumerate() {
            if process.has_window || process.pid == foreground_pid || process.pid == own_pid {
                for member in lineage(processes, &index, at) {
                    seen[member] = true;
                }
            }
        }
        // И все одноимённые: процессы одной программы делят имя,
        // а родство между ними бывает через процесс, которого в снимке нет.
        let seen_names: HashSet<String> = processes
            .iter()
            .enumerate()
            .filter(|(at, _)| seen[*at])
            .map(|(_, process)| process.name.to_lowercase())
            .collect();
        let visible =
            |at: usize| seen[at] || seen_names.contains(&processes[at].name.to_lowercase());

        let mut picked: Vec<&ShieldFacts<'_>> = processes
            .iter()
            .enumerate()
            .filter(|(at, process)| {
                !process.protected
                    && (process.memory.as_u64() >= WORTH_SHIELDING || process.leaking)
                    && !lineage(processes, &index, *at).into_iter().any(visible)
            })
            .map(|(_, process)| process)
            .collect();

        // Больший — первым: если места в списке мало, пусть достанется тем,
        // у кого отнимать есть что.
        picked.sort_by_key(|process| Reverse(process.memory.as_u64()));
        picked
            .into_iter()
            .take(AT_MOST)
            .map(|process| process.pid)
            .collect()
    }
}

/// Процесс и его предки вплоть до оболочки — номерами в списке.
fn lineage(processes: &[ShieldFacts<'_>], index: &HashMap<u32, usize>, start: usize) -> Vec<usize> {
    let mut chain = vec![start];
    let mut current = start;
    // Глубина ограничена, и не зря: номера процессов переиспользуются,
    // и «родителем» бывает чужой процесс, получивший тот же номер, —
    // вплоть до цикла.
    for _ in 0..32 {
        let parent_pid = processes[current].parent_pid;
        if parent_pid == 0 || parent_pid == processes[current].pid {
            break;
        }
        let Some(&parent) = index.get(&parent_pid) else {
            break;
        };
        if is_shell(processes[parent].name) || chain.contains(&parent) {
            break;
        }
        chain.push(parent);
        current = parent;
    }
    chain
}

fn is_shell(name: &str) -> bool {
    SHELLS.iter().any(|shell| name.eq_ignore_ascii_case(shell))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1024 * 1024;
    const TIGHT: f64 = 0.88;
    const OWN: u32 = 999;

    fn process(
        pid: u32,
        parent_pid: u32,
        name: &'static str,
        mb: u64,
        has_window: bool,
    ) -> ShieldFacts<'static> {
        ShieldFacts {
            pid,
            parent_pid,
            name,
            memory: Bytes(mb * MB),
            has_window,
            protected: false,
            leaking: false,
        }
    }

    /// Обстановка с живой машины: Steam свёрнут в трей, его встроенный
    /// браузер держит больше гигабайта, человек работает в Telegram.
    fn steam_in_tray() -> Vec<ShieldFacts<'static>> {
        vec![
            process(10, 1, "explorer.exe", 600, true),
            process(20, 10, "steam.exe", 150, false),
            process(21, 20, "steamwebhelper.exe", 1100, false),
            process(22, 21, "steamwebhelper.exe", 300, false),
            process(23, 21, "steamwebhelper.exe", 80, false),
            process(30, 10, "Telegram.exe", 900, true),
        ]
    }

    #[test]
    fn a_windowless_background_family_is_shielded() {
        let wanted = Shield::new().wanted(&steam_in_tray(), TIGHT, 30, OWN);
        assert_eq!(wanted, vec![21, 22]);
    }

    #[test]
    fn nothing_is_touched_while_memory_is_plentiful() {
        let mut shield = Shield::new();
        assert!(shield.wanted(&steam_in_tray(), 0.60, 30, OWN).is_empty());
        assert!(!shield.engaged());
    }

    #[test]
    fn the_shield_does_not_flap_around_the_threshold() {
        let mut shield = Shield::new();
        assert!(
            shield.wanted(&steam_in_tray(), 0.79, 30, OWN).is_empty(),
            "включилась раньше порога"
        );
        assert!(
            !shield.wanted(&steam_in_tray(), 0.81, 30, OWN).is_empty(),
            "не включилась на пороге"
        );
        assert!(
            !shield.wanted(&steam_in_tray(), 0.75, 30, OWN).is_empty(),
            "снялась в зазоре — будет дребезг"
        );
        assert!(
            shield.wanted(&steam_in_tray(), 0.70, 30, OWN).is_empty(),
            "не снялась, хотя нехватка прошла"
        );
    }

    #[test]
    fn a_background_program_with_an_open_window_is_left_alone() {
        // Steam открыт — к нему человек может вернуться в любую секунду,
        // и страницы его браузера должны быть на месте.
        let mut processes = steam_in_tray();
        processes[1].has_window = true;
        assert!(Shield::new().wanted(&processes, TIGHT, 30, OWN).is_empty());
    }

    #[test]
    fn tabs_of_a_browser_with_a_window_are_part_of_the_browser() {
        // У вкладок Chrome своего окна нет — окно одно на весь браузер.
        // Понизь им приоритет — и при возврате в браузер он полез бы
        // за страницами на диск.
        let processes = vec![
            process(10, 1, "explorer.exe", 600, true),
            process(40, 10, "chrome.exe", 500, true),
            process(41, 40, "chrome.exe", 450, false),
            process(42, 40, "chrome.exe", 380, false),
            process(30, 10, "Telegram.exe", 900, true),
        ];
        assert!(Shield::new().wanted(&processes, TIGHT, 30, OWN).is_empty());
    }

    #[test]
    fn a_build_started_from_the_foreground_terminal_is_not_slowed() {
        // Из журнала подвисаний живой машины: link.exe держал больше
        // гигабайта, rustc — 740 МБ. Это сборка, которую человек ждёт,
        // и она потомок терминала на переднем плане.
        let processes = vec![
            process(10, 1, "explorer.exe", 600, true),
            process(50, 10, "WindowsTerminal.exe", 100, true),
            process(51, 50, "pwsh.exe", 80, false),
            process(52, 51, "cargo.exe", 60, false),
            process(53, 52, "link.exe", 1200, false),
            process(54, 52, "rustc.exe", 740, false),
        ];
        assert!(Shield::new().wanted(&processes, TIGHT, 50, OWN).is_empty());
    }

    #[test]
    fn the_shell_does_not_glue_every_program_into_one_family() {
        // Почти всё — потомки проводника, а у проводника окно есть всегда.
        // Помощник без окна прямо под проводником обязан защищаться.
        let processes = vec![
            process(10, 1, "explorer.exe", 600, true),
            process(60, 10, "BlueStacksAI.exe", 645, false),
        ];
        assert_eq!(Shield::new().wanted(&processes, TIGHT, 10, OWN), vec![60]);
    }

    #[test]
    fn protected_processes_and_bamboo_itself_are_never_touched() {
        let mut processes = vec![
            process(10, 1, "explorer.exe", 600, true),
            process(70, 10, "service-like.exe", 400, false),
            process(OWN, 10, "bamboo-agent.exe", 300, false),
        ];
        processes[1].protected = true;
        assert!(Shield::new().wanted(&processes, TIGHT, 10, OWN).is_empty());
    }

    #[test]
    fn a_small_leaking_background_process_is_shielded_before_it_grows() {
        let mut processes = vec![
            process(10, 1, "explorer.exe", 600, true),
            process(80, 10, "leaky-helper.exe", 60, false),
        ];
        processes[1].leaking = true;
        assert_eq!(Shield::new().wanted(&processes, TIGHT, 10, OWN), vec![80]);
    }

    #[test]
    fn a_parent_cycle_from_reused_ids_does_not_hang() {
        let processes = vec![
            process(90, 91, "a.exe", 400, false),
            process(91, 90, "b.exe", 300, false),
        ];
        assert_eq!(
            Shield::new().wanted(&processes, TIGHT, 0, OWN),
            vec![90, 91]
        );
    }

    #[test]
    fn at_most_a_dozen_are_held_and_the_biggest_first() {
        let names: Vec<&'static str> = (0..20)
            .map(|i| &*Box::leak(format!("helper{i}.exe").into_boxed_str()))
            .collect();
        let mut processes = vec![process(10, 1, "explorer.exe", 600, true)];
        for (i, name) in names.iter().enumerate() {
            processes.push(process(
                100 + i as u32,
                10,
                name,
                250 + i as u64 * 10,
                false,
            ));
        }
        let wanted = Shield::new().wanted(&processes, TIGHT, 10, OWN);
        assert_eq!(wanted.len(), AT_MOST);
        assert_eq!(wanted[0], 119, "первым должен идти самый большой");
    }
}
