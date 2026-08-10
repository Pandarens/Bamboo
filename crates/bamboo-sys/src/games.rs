//! Установленные игры.
//!
//! Нужны ради одного: чтобы в разделе «Анализ» не приходилось вписывать
//! имя исполняемого файла руками. Человек знает игру по названию, а не по
//! тому, что её файл зовётся `BitCraft.exe`, и заставлять его это выяснять —
//! перекладывание своей работы на него.
//!
//! Берём из того, что лежит на диске у самих магазинов: Steam держит
//! манифесты установленных игр в `steamapps`, Epic — в `ProgramData`.
//! Никаких обращений в сеть и никаких списков «известных игр», зашитых
//! в программу: такой список устарел бы в день выпуска.

use bamboo_core::Result;

/// Установленная игра.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Game {
    /// Название, как его показывает магазин.
    pub name: String,
    /// Имя исполняемого файла — то, под которым игра видна в списке
    /// процессов. Пусто, если найти его не удалось: тогда игру всё равно
    /// показываем, но выбрать не даём.
    pub exe: String,
    /// Откуда узнали: «Steam», «Epic Games».
    pub source: String,
}

/// Ищет установленные игры.
///
/// Пустой список — обычное дело: игр может не быть вовсе либо магазин
/// установлен не туда, куда мы смотрим.
pub fn installed() -> Result<Vec<Game>> {
    let mut found = Vec::new();
    collect_steam(&mut found);
    collect_epic(&mut found);

    found.sort_by_key(|game| game.name.to_lowercase());
    found.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    Ok(found)
}

// --- Steam ---

/// Собирает игры Steam.
fn collect_steam(found: &mut Vec<Game>) {
    let Some(root) = steam_root() else {
        return;
    };

    for library in steam_libraries(&root) {
        let apps = std::path::Path::new(&library).join("steamapps");
        let Ok(entries) = std::fs::read_dir(&apps) else {
            continue;
        };

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };

            let (Some(title), Some(dir)) =
                (vdf_value(&text, "name"), vdf_value(&text, "installdir"))
            else {
                continue;
            };

            let folder = apps.join("common").join(&dir);
            found.push(Game {
                exe: main_executable(&folder, &title).unwrap_or_default(),
                name: title,
                source: "Steam".to_string(),
            });
        }
    }
}

/// Где стоит Steam.
fn steam_root() -> Option<String> {
    // Спрашиваем сам Steam, а не гадаем по «Program Files (x86)»: его
    // ставят и на другой диск, и путь тогда совсем иной.
    crate::settings::registry_string("HKCU", r"Software\Valve\Steam", "SteamPath")
        .or_else(|| {
            crate::settings::registry_string(
                "HKLM",
                r"SOFTWARE\WOW6432Node\Valve\Steam",
                "InstallPath",
            )
        })
        .or_else(|| {
            crate::settings::registry_string("HKLM", r"SOFTWARE\Valve\Steam", "InstallPath")
        })
}

/// Все папки библиотек Steam.
///
/// Игры лежат не только там, где стоит сам Steam: библиотеку заводят
/// на любом диске, и без этого списка нашлась бы только часть.
fn steam_libraries(root: &str) -> Vec<String> {
    let mut out = vec![root.to_string()];

    let vdf = std::path::Path::new(root)
        .join("steamapps")
        .join("libraryfolders.vdf");
    let Ok(text) = std::fs::read_to_string(vdf) else {
        return out;
    };

    for path in vdf_values(&text, "path") {
        // В файле пути записаны с удвоенными обратными косыми — это
        // экранирование формата, а не часть пути.
        let path = path.replace("\\\\", "\\");
        if !out.iter().any(|known| known.eq_ignore_ascii_case(&path)) {
            out.push(path);
        }
    }
    out
}

/// Значение ключа в файле формата VDF.
///
/// Формат простой до примитивности: `"ключ"<пробелы>"значение"`. Полного
/// разборщика он не стоит — нужен один ключ, а вложенность нам безразлична.
fn vdf_value(text: &str, key: &str) -> Option<String> {
    vdf_values(text, key).into_iter().next()
}

/// Все значения ключа: у `path` их столько, сколько библиотек.
fn vdf_values(text: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\"");
    let mut out = Vec::new();
    let mut rest = text;

    while let Some(at) = rest.find(&needle) {
        rest = &rest[at + needle.len()..];
        // Дальше идёт значение в кавычках. Если до конца строки кавычек
        // нет, значит это ключ раздела, а не пары — пропускаем.
        let line_end = rest.find('\n').unwrap_or(rest.len());
        let line = &rest[..line_end];

        if let Some(start) = line.find('"') {
            if let Some(end) = line[start + 1..].find('"') {
                let value = &line[start + 1..start + 1 + end];
                if !value.is_empty() {
                    out.push(value.to_string());
                }
            }
        }
    }
    out
}

// --- Epic Games ---

/// Собирает игры Epic Games.
fn collect_epic(found: &mut Vec<Game>) {
    let programdata = std::env::var("PROGRAMDATA").unwrap_or_default();
    if programdata.is_empty() {
        return;
    }

    let manifests = std::path::Path::new(&programdata)
        .join("Epic")
        .join("EpicGamesLauncher")
        .join("Data")
        .join("Manifests");
    let Ok(entries) = std::fs::read_dir(manifests) else {
        return;
    };

    for entry in entries.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) != Some("item") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };

        let Some(title) = json_value(&text, "DisplayName") else {
            continue;
        };
        // У Epic путь к файлу лежит прямо в манифесте — искать не нужно.
        let exe = json_value(&text, "LaunchExecutable")
            .map(|path| {
                path.rsplit(['\\', '/'])
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
            .unwrap_or_default();

        found.push(Game {
            name: title,
            exe,
            source: "Epic Games".to_string(),
        });
    }
}

/// Значение строкового поля JSON.
fn json_value(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let at = text.find(&needle)? + needle.len();
    let rest = &text[at..];
    let colon = rest.find(':')?;
    let start = rest[colon + 1..].find('"')? + colon + 2;

    let mut value = String::new();
    let mut escaped = false;
    for symbol in rest[start..].chars() {
        if escaped {
            // В путях Epic косые удвоены — это экранирование JSON.
            value.push(symbol);
            escaped = false;
            continue;
        }
        match symbol {
            '\\' => escaped = true,
            '"' => return Some(value).filter(|v| !v.is_empty()),
            _ => value.push(symbol),
        }
    }
    None
}

// --- Поиск исполняемого файла ---

/// Файлы, которые лежат рядом с игрой, но игрой не являются.
///
/// Список именно такой формы — по подстроке, — потому что имена у них
/// разнятся: `UnityCrashHandler64.exe`, `UnityCrashHandler32.exe`.
const NOT_A_GAME: [&str; 8] = [
    "crashhandler",
    "crashreport",
    "unitycrash",
    "vcredist",
    "dxsetup",
    "dotnetfx",
    "uninstall",
    "installer",
];

/// Находит главный исполняемый файл игры в её папке.
///
/// Смотрим только корень папки: там он и лежит почти всегда, а обход всего
/// дерева на игре в сорок гигабайт стоил бы секунд и порядочно дискового
/// чтения — ровно того, чего Bamboo избегает у других.
fn main_executable(folder: &std::path::Path, title: &str) -> Option<String> {
    let entries = std::fs::read_dir(folder).ok()?;

    let mut candidates: Vec<(String, u64)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let lowered = name.to_lowercase();
        if !lowered.ends_with(".exe") {
            continue;
        }
        if NOT_A_GAME.iter().any(|bad| lowered.contains(bad)) {
            continue;
        }
        let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        candidates.push((name, size));
    }

    if candidates.is_empty() {
        return None;
    }

    // Сначала пробуем по имени: файл игры почти всегда назван похоже
    // на неё саму. Это надёжнее размера — у игр на Unity главный файл
    // как раз маленький, а тяжёлое лежит в данных.
    let key = squash(title);
    if let Some((name, _)) = candidates
        .iter()
        .find(|(name, _)| squash(name.trim_end_matches(".exe")) == key)
    {
        return Some(name.clone());
    }
    if let Some((name, _)) = candidates.iter().find(|(name, _)| {
        let file = squash(name.trim_end_matches(".exe"));
        !file.is_empty() && (key.starts_with(&file) || file.starts_with(&key))
    }) {
        return Some(name.clone());
    }

    // Не совпало — берём самый крупный: у игр не на Unity это обычно он.
    candidates.sort_by_key(|(_, size)| core::cmp::Reverse(*size));
    candidates.first().map(|(name, _)| name.clone())
}

/// Приводит название к виду, в котором его можно сравнивать: только буквы
/// и цифры, в нижнем регистре. «BitCraft Online» и «bitcraft» так сходятся.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_read_out_of_vdf() {
        let vdf =
            "\"AppState\"\n{\n\t\"appid\"\t\t\"3454650\"\n\t\"name\"\t\t\"BitCraft Online\"\n}";
        assert_eq!(vdf_value(vdf, "name"), Some("BitCraft Online".to_string()));
        assert_eq!(vdf_value(vdf, "appid"), Some("3454650".to_string()));
        assert_eq!(vdf_value(vdf, "нет такого"), None);
    }

    #[test]
    fn a_section_header_is_not_a_value() {
        // «"libraryfolders"» — заголовок раздела, а не пара ключ-значение.
        // Принять его за значение значило бы получить мусорный путь.
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\Games\"\n\t}\n}";
        assert_eq!(vdf_value(vdf, "path"), Some("D:\\\\Games".to_string()));
        assert_eq!(vdf_value(vdf, "libraryfolders"), None);
    }

    #[test]
    fn every_library_path_is_found() {
        // Игры лежат не только там, где стоит Steam. Без этого нашлась бы
        // только часть, а человек решил бы, что Bamboo не видит его игр.
        let vdf = r#""libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
	}
}"#;
        let paths = vdf_values(vdf, "path");
        assert_eq!(paths.len(), 2);
        assert!(paths[1].contains("SteamLibrary"));
    }

    #[test]
    fn a_json_field_is_read_out_of_an_epic_manifest() {
        let json = r#"{"DisplayName":"Fortnite","LaunchExecutable":"FortniteGame\\Binaries\\Win64\\Fortnite.exe"}"#;
        assert_eq!(
            json_value(json, "DisplayName"),
            Some("Fortnite".to_string())
        );
        assert!(json_value(json, "LaunchExecutable")
            .unwrap()
            .ends_with("Fortnite.exe"));
    }

    #[test]
    fn names_are_squashed_for_comparison() {
        assert_eq!(squash("BitCraft Online"), "bitcraftonline");
        assert_eq!(squash("S.T.A.L.K.E.R. 2"), "stalker2");
        assert_eq!(squash(""), "");
    }

    #[test]
    fn the_installed_games_are_listed_without_lying() {
        // Живая проверка. Пустой список — законный исход: игр может
        // не быть. Но если игра нашлась, её поля обязаны быть осмысленны.
        let games = installed().expect("список игр");
        for game in &games {
            assert!(!game.name.trim().is_empty(), "игра без названия");
            assert!(!game.source.is_empty());
            if !game.exe.is_empty() {
                assert!(
                    game.exe.to_lowercase().ends_with(".exe"),
                    "не исполняемый файл: {}",
                    game.exe
                );
                // Вспомогательные файлы игрой не считаются.
                let lowered = game.exe.to_lowercase();
                assert!(
                    !NOT_A_GAME.iter().any(|bad| lowered.contains(bad)),
                    "выбран не тот файл: {}",
                    game.exe
                );
            }
        }
    }
}
