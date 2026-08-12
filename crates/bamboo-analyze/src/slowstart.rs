//! Что из автозагрузки замедляет включение компьютера (ТЗ, разделы 9.6 и 10).
//!
//! Самая честная оптимизация из возможных, и вот почему. «Оптимизаторы»
//! предлагают чистить автозагрузку наугад: список записей без единого
//! числа, решайте сами. А Windows после каждой загрузки записывает
//! в журнал диагностики, какой компонент сколько её задержал, — то есть
//! цена каждой записи автозагрузки **измерена**, просто её никто
//! не показывает рядом с выключателем.
//!
//! Этот модуль сводит две уже собираемые вещи: виновников долгой загрузки
//! из журнала диагностики и записи автозагрузки пользователя. Совпало —
//! получается предложение с числом: «Telegram задержал загрузку на 10 с
//! и стоит в автозагрузке». Выключение обратимо и меняет один байт.
//!
//! Чистая логика: о Windows здесь не знают, входы — плоские списки,
//! и всё проверяется тестами целиком.

use bamboo_core::say;

/// Виновник долгой загрузки, как его назвал журнал диагностики.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootCost {
    /// Имя компонента: обычно исполняемый файл, «Telegram.exe».
    pub name: String,
    /// Сколько он занял при загрузке.
    pub total_ms: u64,
}

/// Запись автозагрузки.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupEntry {
    /// Имя записи — по нему же она выключается.
    pub name: String,
    /// Команда запуска: путь к файлу и ключи.
    pub command: String,
    pub enabled: bool,
}

/// Предложение: эта запись автозагрузки стоит вам столько-то секунд
/// при каждом включении.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlowStarter {
    /// Имя записи автозагрузки — то, что передаётся выключателю.
    pub startup_name: String,
    /// Объяснение с числом.
    pub reason: String,
    /// Измеренная цена, для сортировки.
    pub cost_ms: u64,
}

/// Дешевле этого не предлагаем.
///
/// Две секунды. Выключать то, что стоит полсекунды, — беспокойство ради
/// беспокойства: человек потеряет удобство автозапуска и не заметит
/// выигрыша. Утилита, которая находит десять «проблем» на ровном месте,
/// набивает себе цену (ТЗ 11.5).
const WORTH_MENTIONING_MS: u64 = 2000;

/// Сводит виновников загрузки с записями автозагрузки.
///
/// Пустой список — обычный и хороший исход: значит автозагрузка
/// не при чём либо уже вычищена.
pub fn slow_starters(costs: &[BootCost], startup: &[StartupEntry]) -> Vec<SlowStarter> {
    let mut found: Vec<SlowStarter> = Vec::new();

    for cost in costs {
        if cost.total_ms < WORTH_MENTIONING_MS {
            continue;
        }

        let key = squash(cost.name.trim_end_matches(".exe").trim_end_matches(".EXE"));
        if key.is_empty() {
            continue;
        }

        for entry in startup {
            // Выключенные не предлагаем выключить ещё раз: предложение,
            // которое нечего выполнять, — шум.
            if !entry.enabled {
                continue;
            }
            if !matches(&key, entry) {
                continue;
            }
            // Одна запись — одно предложение, даже если журнал назвал
            // компонент в нескольких загрузках.
            if found.iter().any(|known| known.startup_name == entry.name) {
                continue;
            }

            found.push(SlowStarter {
                startup_name: entry.name.clone(),
                reason: say(
                    "{app} задержал загрузку на {cost} и стоит в автозагрузке.                      Это измерено журналом диагностики Windows, а не предположено.                      Выключение обратимо: программа останется установленной                      и запустится вручную.",
                    "{app} delayed startup by {cost} and sits in autostart.                      This is measured by the Windows diagnostics log, not guessed.                      Turning it off is reversible: the program stays installed                      and starts manually.",
                    &[
                        ("app", entry.name.as_str()),
                        ("cost", &spell_cost(cost.total_ms)),
                    ],
                ),
                cost_ms: cost.total_ms,
            });
        }
    }

    // Самые дорогие — первыми: если человек выключит одно, пусть это
    // будет то, что даст больше всего.
    found.sort_by_key(|starter| core::cmp::Reverse(starter.cost_ms));
    found
}

/// Совпадает ли виновник с записью автозагрузки.
///
/// Сравнивается имя файла, а не подстрока команды, и это не педантизм.
/// Первая редакция искала подстроку и спаривала «slow.exe» с командой,
/// содержащей «slower.exe», — то есть предлагала выключить не того.
/// Ложное предложение здесь хуже пропущенного: человек выключит невинную
/// запись и не получит обещанных секунд.
fn matches(culprit_key: &str, entry: &StartupEntry) -> bool {
    if let Some(stem) = executable_stem(&entry.command) {
        if stem == culprit_key {
            return true;
        }
    }
    // Запасной путь — точное совпадение имени записи: у части записей
    // в команде лежит не сам файл, а его обёртка.
    squash(&entry.name) == culprit_key
}

/// Имя исполняемого файла из команды запуска, приведённое для сравнения.
///
/// «"C:\Users\я\AppData\Telegram\Telegram.exe" -autostart» → «telegram».
fn executable_stem(command: &str) -> Option<String> {
    let lowered = command.to_lowercase();
    let end = lowered.find(".exe")?;
    let head = &command[..end];
    let start = head.rfind(['\\', '/', '"']).map(|at| at + 1).unwrap_or(0);
    let stem = squash(&head[start..]);
    (!stem.is_empty()).then_some(stem)
}

/// Только буквы и цифры, в нижнем регистре: «Telegram Desktop»
/// и «telegram.exe» так сходятся.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn spell_cost(ms: u64) -> String {
    // Единица тоже переводится: «с» в английском предложении — это
    // кириллица посреди фразы, и сторожевой тест прав, что ловит её.
    let unit = bamboo_core::pick("с", "s");
    if ms < 10_000 {
        format!("{:.1} {unit}", ms as f64 / 1000.0)
    } else {
        format!("{} {unit}", ms / 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(name: &str, ms: u64) -> BootCost {
        BootCost {
            name: name.to_string(),
            total_ms: ms,
        }
    }

    fn entry(name: &str, command: &str) -> StartupEntry {
        StartupEntry {
            name: name.to_string(),
            command: command.to_string(),
            enabled: true,
        }
    }

    #[test]
    fn a_slow_autostart_is_matched_through_its_command() {
        // Имя записи вольное, а в команде лежит путь к тому самому файлу —
        // по нему и сводим. Язык фиксируем: он глобален на процесс,
        // и соседний тест мог оставить английский.
        let _guard = crate::LANGUAGE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        bamboo_core::set_language(bamboo_core::Language::Russian);

        let found = slow_starters(
            &[cost("Telegram.exe", 9973)],
            &[entry(
                "Telegram Desktop",
                r"C:\Users\я\AppData\Telegram\Telegram.exe -autostart",
            )],
        );

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].startup_name, "Telegram Desktop");
        assert!(found[0].reason.contains("10.0 с"), "{}", found[0].reason);
        // Обязательные части честного предложения: измерено и обратимо.
        assert!(found[0].reason.contains("измерено"), "{}", found[0].reason);
        assert!(found[0].reason.contains("обратимо"), "{}", found[0].reason);
    }

    #[test]
    fn a_similar_name_is_not_mistaken_for_a_match() {
        // Первая редакция сопоставляла подстрокой и спаривала slow.exe
        // с командой slower.exe — предлагала выключить не того. Ложное
        // предложение хуже пропущенного.
        let found = slow_starters(
            &[cost("slow.exe", 4000)],
            &[entry("Slower", r"C:\b\slower.exe")],
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn system_components_never_match_user_autostart() {
        // explorer.exe и dwm.exe — виновники почти каждой загрузки,
        // но в автозагрузке пользователя их нет, и предлагать нечего.
        let found = slow_starters(
            &[cost("explorer.exe", 3246), cost("dwm.exe", 4174)],
            &[entry("Telegram Desktop", r"C:\Telegram\Telegram.exe")],
        );
        assert!(found.is_empty());
    }

    #[test]
    fn cheap_delays_are_not_worth_the_bother() {
        // Выключать то, что стоит полсекунды, — беспокойство ради
        // беспокойства. Утилита, находящая десять проблем на ровном
        // месте, набивает себе цену.
        let found = slow_starters(
            &[cost("Skrinshoter.exe", 400)],
            &[entry("Skrinshoter", r"C:\Skrinshoter\Skrinshoter.exe")],
        );
        assert!(found.is_empty());
    }

    #[test]
    fn an_already_disabled_entry_is_not_suggested_again() {
        let mut off = entry("Telegram Desktop", r"C:\Telegram\Telegram.exe");
        off.enabled = false;

        let found = slow_starters(&[cost("Telegram.exe", 9973)], &[off]);
        assert!(
            found.is_empty(),
            "предложение, которое нечего выполнять, — шум"
        );
    }

    #[test]
    fn the_most_expensive_comes_first() {
        let found = slow_starters(
            &[cost("slow.exe", 4000), cost("slower.exe", 12_000)],
            &[
                entry("Slow", r"C:\a\slow.exe"),
                entry("Slower", r"C:\b\slower.exe"),
            ],
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].startup_name, "Slower");
    }

    #[test]
    fn one_entry_yields_one_suggestion() {
        // Журнал называет компонент в нескольких загрузках — предложение
        // всё равно одно.
        let found = slow_starters(
            &[cost("Telegram.exe", 9973), cost("telegram.exe", 8100)],
            &[entry("Telegram Desktop", r"C:\Telegram\Telegram.exe")],
        );
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn empty_inputs_yield_an_empty_answer() {
        assert!(slow_starters(&[], &[]).is_empty());
        assert!(slow_starters(&[cost("a.exe", 9000)], &[]).is_empty());
        assert!(slow_starters(&[], &[entry("A", "a.exe")]).is_empty());
    }

    #[test]
    fn the_suggestion_speaks_both_languages() {
        use bamboo_core::{set_language, Language};
        // Замок общий на весь крейт: язык глобален, и локальный замок
        // не защитил бы от тестов из соседних модулей того же бинарника.
        let _guard = crate::LANGUAGE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let inputs = (
            [cost("Telegram.exe", 9973)],
            [entry("Telegram Desktop", r"C:\Telegram\Telegram.exe")],
        );

        set_language(Language::English);
        let english = slow_starters(&inputs.0, &inputs.1)[0].reason.clone();
        set_language(Language::Russian);
        let russian = slow_starters(&inputs.0, &inputs.1)[0].reason.clone();

        assert_ne!(english, russian);
        assert!(english.contains("measured"), "{english}");
        assert!(
            english.contains("10.0 s"),
            "единица не переведена: {english}"
        );
        assert!(
            !english
                .chars()
                .any(|c| ('\u{0410}'..='\u{044f}').contains(&c)),
            "в английском тексте кириллица: {english}"
        );
    }
}
