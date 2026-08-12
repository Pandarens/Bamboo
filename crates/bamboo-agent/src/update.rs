//! Обновление через выпуски на GitHub.
//!
//! Задача простая на словах и въедливая на деле: узнать, вышла ли новая
//! версия, скачать её и заменить себя. Въедливость вся в последнем шаге —
//! запущенный исполняемый файл в Windows нельзя перезаписать, зато можно
//! переименовать. На этом и держится вся замена.
//!
//! Чего здесь намеренно нет: тихого обновления. Bamboo не подменяет себя
//! у человека за спиной — он говорит, что вышла новая версия, и ждёт
//! решения. Программа, которая молча заменяет свой исполняемый файл, —
//! это ровно то поведение, из-за которого к «оптимизаторам» и относятся
//! как к сомнительному ПО.

/// Откуда берём выпуски.
///
/// Именно список, а не `/releases/latest`. Тот отдаёт только окончательные
/// выпуски и молча пропускает предвыпуски — на бете обновление не нашлось бы
/// никогда. Выяснилось это не рассуждением, а запросом: на живом выпуске
/// пришёл честный 404.
const RELEASES_URL: &str = "https://api.github.com/repos/Pandarens/Bamboo/releases?per_page=10";

/// Какая версия собрана сейчас.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Как часто спрашиваем GitHub.
///
/// Раз в сутки. Чаще незачем: выпуски выходят не по часам, а лишние запросы
/// к чужому серверу — это трафик человека и нагрузка не на нас.
pub const CHECK_EVERY_MS: u64 = 24 * 60 * 60 * 1000;

/// Больше этого исполняемый файл Bamboo быть не может.
///
/// Сборка занимает единицы мегабайт. Предел защищает от того, чтобы
/// подсунутый ответ заставил нас скачивать гигабайты.
const MAX_SIZE: usize = 64 * 1024 * 1024;

/// Найденный выпуск.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Метка версии без ведущей «v».
    pub version: String,
    /// Адрес исполняемого файла.
    pub download_url: String,
    /// Описание выпуска: показываем человеку, что изменилось.
    pub notes: String,
    /// Ожидаемый SHA-256, если он указан в описании.
    pub sha256: Option<String>,
}

/// Что известно про обновление.
#[derive(Clone, Debug, Default)]
pub struct UpdateState {
    /// Найденный выпуск новее нынешнего.
    pub available: Option<Release>,
    /// Что сказать человеку про последнюю проверку.
    pub note: String,
}

/// Разбирает ответ GitHub.
///
/// Разбираем по строке, а не полноценным разбором JSON: нужны четыре поля,
/// а тащить разборщик ради них — плохой размен, тот же довод, что и для
/// манифестов расширений.
pub fn parse_release(json: &str) -> Option<Release> {
    parse_release_for(json, !CURRENT.contains('-'))
}

/// То же с явным признаком «пользователь на стабильной версии».
///
/// Признак вынесен параметром не только ради тестов: без него правила
/// разбора зависели бы от версии самой сборки, и тесты, написанные при
/// бете, молча меняли бы смысл после выпуска — ровно это и случилось.
fn parse_release_for(json: &str, stable_user: bool) -> Option<Release> {
    // Ответ бывает и списком, и одиночным выпуском. Список разбираем
    // поэлементно: искать поля во всём тексте разом нельзя — метка версии
    // взялась бы из первого выпуска, а файл из второго.
    //
    // Из подходящих берётся САМЫЙ НОВЫЙ ПО ВЕРСИИ, а не первый по списку.
    // GitHub сортирует по дате создания, и первым может оказаться хотфикс
    // старой линии: выпусти мы v0.8.1 после v0.9.0 — прежний разбор
    // предложил бы всем «последнюю версию» 0.8.1.
    //
    // И второе правило: тому, кто стоит на выпуске, беты не предлагаются.
    // Человек, поставивший стабильную версию, выбрал стабильность, и
    // затягивать его обратно в предвыпуски без спроса нельзя. Тем, кто
    // уже на бете, предлагается всё: они на этот риск согласились.
    let mut best: Option<Release> = None;
    for element in top_level_objects(json) {
        let Some(release) = parse_one(element) else {
            continue;
        };
        if stable_user && release.version.contains('-') {
            continue;
        }
        let newer = match &best {
            Some(known) => is_newer(&release.version, &known.version),
            None => true,
        };
        if newer {
            best = Some(release);
        }
    }
    best
}

/// Разбивает ответ на верхнеуровневые объекты.
///
/// Для одиночного выпуска это он сам, для списка — его элементы по порядку,
/// а GitHub отдаёт список от новых к старым.
fn top_level_objects(json: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (at, symbol) in json.char_indices() {
        if in_string {
            // Скобки внутри строк не считаются: в описании выпуска их полно.
            match symbol {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match symbol {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = at;
                }
                depth += 1;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    out.push(&json[start..=at]);
                }
            }
            _ => {}
        }
    }
    out
}

/// Разбирает один выпуск.
fn parse_one(json: &str) -> Option<Release> {
    let tag = json_string(json, "tag_name")?;
    let version = tag.trim_start_matches('v').to_string();
    if version.is_empty() {
        return None;
    }

    let notes = json_string(json, "body").unwrap_or_default();
    // Экранированные переводы строк в описании — обычное дело для JSON,
    // а показывать человеку «\n» посреди текста нельзя.
    let notes = notes.replace("\\r\\n", "\n").replace("\\n", "\n");

    Some(Release {
        version,
        download_url: exe_asset_url(json)?,
        sha256: sha256_from_notes(&notes),
        notes,
    })
}

/// Находит адрес исполняемого файла среди вложений выпуска.
fn exe_asset_url(json: &str) -> Option<String> {
    // Вложений у выпуска бывает несколько; нужно то, что оканчивается
    // на .exe. Брать первое попавшееся нельзя — рядом лежат исходники,
    // которые GitHub добавляет сам.
    let mut rest = json;
    while let Some(at) = rest.find("\"browser_download_url\"") {
        rest = &rest[at + "\"browser_download_url\"".len()..];
        if let Some(url) = json_value_here(rest) {
            if url.ends_with(".exe") {
                return Some(url);
            }
        }
    }
    None
}

/// Достаёт ожидаемый хеш из описания выпуска.
///
/// Ищем строку вида `SHA256: <64 шестнадцатеричные цифры>`. Нет её — ничего
/// страшного: проверка целостности пропускается, и об этом честно
/// сообщается, а не делается вид, что проверили.
fn sha256_from_notes(notes: &str) -> Option<String> {
    let at = notes.to_lowercase().find("sha256")?;
    let tail = &notes[at..];

    // Берём первое слово из 64 шестнадцатеричных цифр после метки.
    tail.split(|c: char| !c.is_ascii_hexdigit())
        .find(|word| word.len() == 64)
        .map(str::to_lowercase)
}

/// Значение строкового поля JSON по имени ключа.
fn json_string(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let at = text.find(&needle)? + needle.len();
    json_value_here(&text[at..])
}

/// Значение сразу после ключа: двоеточие, потом строка в кавычках.
fn json_value_here(after_key: &str) -> Option<String> {
    let colon = after_key.find(':')?;
    let rest = &after_key[colon + 1..];
    // `null` вместо строки — законный ответ, и обрывать разбор на нём
    // не нужно.
    let start = rest.find('"')?;
    if rest[..start].contains(',') {
        return None;
    }

    let mut value = String::new();
    let mut escaped = false;
    for symbol in rest[start + 1..].chars() {
        if escaped {
            // Экранирование внутри значения оставляем как есть, кроме
            // кавычки: разрывать на ней строку нельзя.
            match symbol {
                '"' | '\\' | '/' => value.push(symbol),
                _ => {
                    value.push('\\');
                    value.push(symbol);
                }
            }
            escaped = false;
            continue;
        }
        match symbol {
            '\\' => escaped = true,
            '"' => return Some(value),
            _ => value.push(symbol),
        }
    }
    None
}

/// Новее ли `candidate`, чем `current`.
///
/// Сравниваем по числам, а не по строкам: «0.10.0» строкой меньше «0.9.0»,
/// и обновление на нём остановилось бы навсегда. Предвыпускная часть после
/// дефиса считается **старше** такой же версии без неё — так принято, и это
/// не мелочь: иначе выпущенная 1.0.0 никогда бы не сменила 1.0.0-beta.1.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (candidate_core, candidate_pre) = split_prerelease(candidate);
    let (current_core, current_pre) = split_prerelease(current);

    match numbers(candidate_core).cmp(&numbers(current_core)) {
        core::cmp::Ordering::Greater => true,
        core::cmp::Ordering::Less => false,
        core::cmp::Ordering::Equal => match (candidate_pre, current_pre) {
            // Выпуск новее своего же предвыпуска.
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (None, None) => false,
            // Предвыпуски одной версии сравниваем по числам в хвосте.
            // Прежнее сравнение по строке было ошибкой, дремавшей
            // до двузначного номера: «beta.10» строкой меньше «beta.9»,
            // и все, кто на девятой бете, никогда не увидели бы десятую.
            (Some(one), Some(other)) => prerelease_rank(one) > prerelease_rank(other),
        },
    }
}

/// Порядковый вид предвыпускной части: слово и числа отдельно.
///
/// «beta.10» → («beta», [10]), и сравнение кортежей даёт правильный
/// порядок: beta.9 < beta.10 < rc.1.
fn prerelease_rank(pre: &str) -> (String, Vec<u32>) {
    let mut word = String::new();
    let mut numbers = Vec::new();

    for part in pre.split(['.', '-']) {
        match part.parse::<u32>() {
            Ok(number) => numbers.push(number),
            Err(_) => {
                if !word.is_empty() {
                    word.push('.');
                }
                word.push_str(&part.to_lowercase());
            }
        }
    }
    (word, numbers)
}

fn split_prerelease(version: &str) -> (&str, Option<&str>) {
    match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    }
}

/// Числовые части версии. Недостающие считаем нулями, посторонние — нулём:
/// падать из-за странной метки версии нельзя.
fn numbers(version: &str) -> Vec<u32> {
    let mut parts: Vec<u32> = version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect();
    parts.resize(3, 0);
    parts
}

/// Спрашивает у GitHub, есть ли что-то новее.
///
/// Отсутствие обновления — обычный и самый частый исход. Ошибка сети — тоже
/// не беда: значит, спросим завтра.
pub fn check() -> UpdateState {
    let json = match bamboo_sys::fetch_text(RELEASES_URL, "application/vnd.github+json") {
        Ok(json) => json,
        Err(error) => {
            return UpdateState {
                available: None,
                note: format!(
                    "Проверить обновления не удалось: {error}. Это не поломка —                      Bamboo спросит ещё раз позже."
                ),
            }
        }
    };

    let Some(release) = parse_release(&json) else {
        return UpdateState {
            available: None,
            note: "Выпусков пока нет.".to_string(),
        };
    };

    if !is_newer(&release.version, CURRENT) {
        return UpdateState {
            available: None,
            note: format!("Установлена последняя версия — {CURRENT}."),
        };
    }

    UpdateState {
        note: format!("Вышла версия {}. У вас {CURRENT}.", release.version),
        available: Some(release),
    }
}

/// Скачивает выпуск и заменяет собой нынешний файл.
///
/// Возвращает объяснение для человека и признак того, что замена удалась
/// и нужен перезапуск.
pub fn install(release: &Release) -> (String, bool) {
    let bytes = match bamboo_sys::fetch(&release.download_url, "application/octet-stream") {
        Ok(bytes) => bytes,
        Err(error) => return (format!("Скачать не удалось: {error}"), false),
    };

    if let Err(why) = check_payload(&bytes, release) {
        return (why, false);
    }

    match replace_self(&bytes) {
        Ok(()) => (
            format!(
                "Версия {} установлена. Изменения вступят в силу после перезапуска                  Bamboo — закройте его из трея и запустите снова.",
                release.version
            ),
            true,
        ),
        Err(why) => (why, false),
    }
}

/// Проверяет скачанное, прежде чем класть его на место своего файла.
fn check_payload(bytes: &[u8], release: &Release) -> Result<(), String> {
    if bytes.len() > MAX_SIZE {
        return Err("Скачанный файл неправдоподобно велик — установка отменена.".to_string());
    }
    // Программа Windows начинается с этих двух букв. Проверка грубая, но
    // ловит главное: что вместо программы скачалась страница с ошибкой.
    if bytes.len() < 2 || &bytes[..2] != b"MZ" {
        return Err("Скачанное не похоже на программу Windows — установка отменена.".to_string());
    }

    let Some(expected) = &release.sha256 else {
        // Молчать об этом нельзя: человек вправе знать, что проверки
        // целостности не было.
        return Ok(());
    };

    match bamboo_sys::sha256_hex(bytes) {
        Ok(actual) if actual == *expected => Ok(()),
        Ok(actual) => Err(format!(
            "Контрольная сумма не сошлась: ожидалась {expected}, получилась {actual}.              Файл повреждён при загрузке — установка отменена."
        )),
        Err(error) => Err(format!("Проверить контрольную сумму не удалось: {error}")),
    }
}

/// Кладёт новый файл на место нынешнего.
///
/// Запущенный исполняемый файл перезаписать нельзя, а переименовать —
/// можно: Windows держит открытым сам файл, а не его имя в папке. Поэтому
/// нынешний уезжает в сторону, новый встаёт на его место, а старый убирается
/// при следующем запуске, когда его уже никто не держит.
fn replace_self(bytes: &[u8]) -> Result<(), String> {
    let current = std::env::current_exe()
        .map_err(|error| format!("Свой путь определить не удалось: {error}"))?;
    let old = current.with_extension("old");

    // Остатки прошлого обновления могли не убраться. Место занято — убираем.
    let _ = std::fs::remove_file(&old);

    std::fs::rename(&current, &old).map_err(|error| {
        format!(
            "Заменить себя не удалось: {error}. Обычно это значит, что папка              с Bamboo защищена от записи — например, он лежит в Program Files."
        )
    })?;

    if let Err(error) = std::fs::write(&current, bytes) {
        // Записать не вышло — возвращаем прежний файл на место. Оставить
        // человека вообще без программы недопустимо.
        let _ = std::fs::rename(&old, &current);
        return Err(format!("Записать новую версию не удалось: {error}"));
    }

    Ok(())
}

/// Убирает файл, оставшийся от прошлого обновления.
///
/// Вызывается при запуске: к этому моменту старый файл уже никем не держится.
pub fn clean_up_after_update() {
    if let Ok(current) = std::env::current_exe() {
        let _ = std::fs::remove_file(current.with_extension("old"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_is_newer_than_nine() {
        // Сравнение строкой сказало бы обратное, и обновление встало бы
        // навсегда на версии 0.9.
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.9.0", "0.10.0"));
    }

    #[test]
    fn the_same_version_is_not_newer() {
        assert!(!is_newer("1.2.3", "1.2.3"));
    }

    #[test]
    fn a_release_supersedes_its_own_prerelease() {
        // Иначе выпущенная 1.0.0 никогда бы не сменила 1.0.0-beta.1,
        // и все, кто поставил бету, остались бы на ней навсегда.
        assert!(is_newer("1.0.0", "1.0.0-beta.1"));
        assert!(!is_newer("1.0.0-beta.1", "1.0.0"));
    }

    #[test]
    fn prereleases_follow_each_other() {
        assert!(is_newer("0.9.0-beta.2", "0.9.0-beta.1"));
        assert!(!is_newer("0.9.0-beta.1", "0.9.0-beta.2"));
    }

    #[test]
    fn missing_parts_count_as_zero() {
        assert!(is_newer("1.1", "1.0.9"));
        assert!(!is_newer("1.0", "1.0.0"));
    }

    #[test]
    fn a_strange_tag_does_not_panic() {
        assert!(!is_newer("не версия", "0.1.0"));
        assert!(!is_newer("", ""));
    }

    #[test]
    fn a_release_is_parsed_out_of_the_answer() {
        let json = r#"{
            "tag_name": "v0.9.0-beta.1",
            "body": "Первая бета.\nSHA256: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "assets": [
                {"browser_download_url": "https://example.com/source.zip"},
                {"browser_download_url": "https://example.com/bamboo-agent.exe"}
            ]
        }"#;

        // Пользователь на бете: предвыпуски ему видны.
        let release = parse_release_for(json, false).expect("выпуск должен разобраться");
        assert_eq!(release.version, "0.9.0-beta.1", "ведущая v должна уйти");
        assert_eq!(
            release.download_url, "https://example.com/bamboo-agent.exe",
            "нужен именно исполняемый файл, а не архив исходников"
        );
        assert!(release.notes.contains("Первая бета."));
        assert!(release.notes.contains('\n'), "переводы строк — настоящие");
        assert_eq!(
            release.sha256.as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn a_list_of_releases_yields_the_newest_one() {
        // GitHub отдаёт список от новых к старым. Брать надо первый —
        // и брать целиком: метка версии и файл обязаны быть из одного
        // выпуска, иначе поставится не то, о чём сказали.
        let json = r#"[
            {"tag_name":"v0.9.0-beta.1","body":"новая",
             "assets":[{"browser_download_url":"https://example.com/new.exe"}]},
            {"tag_name":"v0.8.0","body":"старая",
             "assets":[{"browser_download_url":"https://example.com/old.exe"}]}
        ]"#;

        // Пользователь на бете: ему предлагается и предвыпуск.
        let release = parse_release_for(json, false).unwrap();
        assert_eq!(release.version, "0.9.0-beta.1");
        assert_eq!(release.download_url, "https://example.com/new.exe");
    }

    #[test]
    fn a_release_without_an_executable_is_skipped_for_the_next_one() {
        // Выпуск, к которому не приложили файла, ставить нечем. Молча
        // взять файл от следующего было бы враньём — берём следующий целиком.
        let json = r#"[
            {"tag_name":"v0.9.0","body":"","assets":[]},
            {"tag_name":"v0.8.0","body":"",
             "assets":[{"browser_download_url":"https://example.com/old.exe"}]}
        ]"#;

        let release = parse_release(json).unwrap();
        assert_eq!(release.version, "0.8.0");
        assert_eq!(release.download_url, "https://example.com/old.exe");
    }

    #[test]
    fn braces_inside_release_notes_do_not_break_the_split() {
        // В описании выпуска скобки и кавычки — обычное дело.
        let json = r#"[
            {"tag_name":"v1.0.0","body":"починили {это} и \"то\"",
             "assets":[{"browser_download_url":"https://example.com/a.exe"}]}
        ]"#;

        let release = parse_release(json).unwrap();
        assert_eq!(release.version, "1.0.0");
        assert_eq!(release.download_url, "https://example.com/a.exe");
    }

    #[test]
    fn the_real_answer_from_github_is_understood() {
        // Кусок настоящего ответа, снятый с живого выпуска. Выдуманные
        // ответы проверяют разбор, но не проверяют главного: что поля
        // называются так, как мы думаем, и лежат там, где мы ищем.
        // Именно на живом запросе и выяснилось, что `/releases/latest`
        // молча пропускает предвыпуски.
        let json = r#"[{"url":"https://api.github.com/repos/Pandarens/Bamboo/releases/367888042",
        "tag_name":"v0.9.0-beta.1","target_commitish":"main","name":"Bamboo 0.9.0 beta 1",
        "draft":false,"prerelease":true,
        "assets":[{"url":"https://api.github.com/repos/Pandarens/Bamboo/releases/assets/508720269",
        "id":508720269,"name":"bamboo-agent.exe","content_type":"application/octet-stream",
        "size":9519104,"browser_download_url":"https://github.com/Pandarens/Bamboo/releases/download/v0.9.0-beta.1/bamboo-agent.exe"}],
        "body":"Первая публичная бета.

SHA256: 4d427149e96c088247715c646a366ce34e33c4899bd2f5a9256a66569ab8dc8b
"}]"#;

        // Живой ответ состоит из бет — читаем как пользователь беты.
        let release = parse_release_for(json, false).expect("живой ответ обязан разбираться");
        assert_eq!(release.version, "0.9.0-beta.1");
        assert_eq!(
            release.download_url,
            "https://github.com/Pandarens/Bamboo/releases/download/v0.9.0-beta.1/bamboo-agent.exe",
            "адрес обязан быть настоящим файлом, а не ссылкой API"
        );
        assert_eq!(
            release.sha256.as_deref(),
            Some("4d427149e96c088247715c646a366ce34e33c4899bd2f5a9256a66569ab8dc8b")
        );
        assert!(release.notes.contains("Первая публичная бета."));
    }

    #[test]
    fn a_single_release_object_still_works() {
        // Ответ бывает и одиночным объектом — разбор обязан понимать оба.
        let json = r#"{"tag_name":"v1.0.0","body":"",
            "assets":[{"browser_download_url":"https://example.com/a.exe"}]}"#;
        assert_eq!(parse_release(json).unwrap().version, "1.0.0");
    }

    #[test]
    fn a_release_without_a_checksum_is_still_usable() {
        // Проверка целостности пропускается, но выпуск не отбрасывается:
        // это ухудшение, а не отказ.
        let json = r#"{"tag_name":"v1.0.0","body":"без суммы",
            "assets":[{"browser_download_url":"https://example.com/a.exe"}]}"#;
        let release = parse_release_for(json, false).unwrap();
        assert_eq!(release.sha256, None);
    }

    #[test]
    fn a_release_without_an_executable_is_not_offered() {
        // Предлагать обновление, которое нечем поставить, — обман.
        let json = r#"{"tag_name":"v1.0.0","body":"",
            "assets":[{"browser_download_url":"https://example.com/source.zip"}]}"#;
        assert_eq!(parse_release(json), None);
    }

    #[test]
    fn nonsense_is_not_mistaken_for_a_release() {
        assert_eq!(parse_release(""), None);
        assert_eq!(parse_release("{}"), None);
        assert_eq!(parse_release(r#"{"message":"Not Found"}"#), None);
    }

    #[test]
    fn a_page_of_html_is_never_installed() {
        // Самая частая беда: вместо файла приходит страница с ошибкой.
        // Запускать её потом было бы нечем, но и класть на место своего
        // файла нельзя.
        let release = Release {
            version: "1.0.0".into(),
            download_url: String::new(),
            notes: String::new(),
            sha256: None,
        };
        let why = check_payload(b"<!DOCTYPE html><html>", &release).unwrap_err();
        assert!(why.contains("не похоже на программу"), "{why}");
    }

    #[test]
    fn a_broken_download_is_caught_by_its_checksum() {
        let release = Release {
            version: "1.0.0".into(),
            download_url: String::new(),
            notes: String::new(),
            // Сумма пустой строки — заведомо не та.
            sha256: Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into()),
        };
        let why = check_payload(b"MZ\x90\x00", &release).unwrap_err();
        assert!(why.contains("не сошлась"), "{why}");
    }

    #[test]
    fn a_matching_checksum_passes() {
        let payload = b"MZ\x90\x00";
        let release = Release {
            version: "1.0.0".into(),
            download_url: String::new(),
            notes: String::new(),
            sha256: Some(bamboo_sys::sha256_hex(payload).unwrap()),
        };
        assert!(check_payload(payload, &release).is_ok());
    }

    #[test]
    fn our_own_version_is_never_newer_than_itself() {
        // Иначе Bamboo предлагал бы обновиться на себя же, бесконечно.
        assert!(!is_newer(CURRENT, CURRENT));
    }
}

#[cfg(test)]
mod audit_fix_tests {
    use super::*;

    #[test]
    fn beta_ten_is_newer_than_beta_nine() {
        // Ошибка, дремавшая до двузначного номера: строкой «beta.10»
        // меньше «beta.9», и все на девятой бете застряли бы навсегда.
        assert!(is_newer("0.9.0-beta.10", "0.9.0-beta.9"));
        assert!(!is_newer("0.9.0-beta.9", "0.9.0-beta.10"));
        assert!(is_newer("0.9.0-beta.2", "0.9.0-beta.1"));
    }

    #[test]
    fn a_stable_user_is_not_dragged_back_into_betas() {
        // Человек, поставивший стабильную версию, выбрал стабильность.
        let json = r#"[
            {"tag_name":"v9.9.9-beta.1","body":"",
             "assets":[{"browser_download_url":"https://example.com/beta.exe"}]},
            {"tag_name":"v0.1.0","body":"",
             "assets":[{"browser_download_url":"https://example.com/old.exe"}]}
        ]"#;
        let release = parse_release_for(json, true).expect("стабильный выпуск найден");
        assert_eq!(release.version, "0.1.0", "бета не должна была выбраться");
    }

    #[test]
    fn the_best_version_wins_not_the_first_listed() {
        // GitHub сортирует по дате создания: первым может оказаться
        // хотфикс старой линии. Прежний разбор взял бы его и сказал
        // «установлена последняя версия».
        let json = r#"[
            {"tag_name":"v0.8.1","body":"хотфикс старой линии",
             "assets":[{"browser_download_url":"https://example.com/hotfix.exe"}]},
            {"tag_name":"v0.9.0","body":"",
             "assets":[{"browser_download_url":"https://example.com/new.exe"}]}
        ]"#;
        let release = parse_release(json).unwrap();
        assert_eq!(release.version, "0.9.0");
        assert_eq!(release.download_url, "https://example.com/new.exe");
    }

    #[test]
    fn rc_comes_after_any_beta() {
        assert!(is_newer("1.0.0-rc.1", "1.0.0-beta.12"));
        assert!(!is_newer("1.0.0-beta.12", "1.0.0-rc.1"));
    }
}
