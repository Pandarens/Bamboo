//! Установленные расширения браузеров.
//!
//! Оговорка, с которой начинается вся эта работа: сопоставить процесс
//! расширения с конкретным расширением снаружи **нельзя**. В командной
//! строке процесса стоит только пометка `--extension-process` и внутренний
//! счётчик браузера — идентификатора там нет. Проверено на живых процессах,
//! а не предположено.
//!
//! Что можно — прочитать список установленных расширений из профиля и
//! показать его человеку: какие вообще стоят и сколько их. Это отвечает
//! на вопрос «что за шесть процессов расширений» настолько, насколько
//! на него вообще можно ответить снаружи.

use bamboo_core::Result;

/// Расширение браузера.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extension {
    /// Имя из манифеста.
    pub name: String,
    /// Идентификатор — он же имя папки.
    pub id: String,
}

/// Читает установленные расширения Chrome и Edge.
///
/// Пустой список — обычное дело: браузер может быть не установлен либо
/// расширений нет вовсе.
pub fn installed() -> Result<Vec<Extension>> {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    if local.is_empty() {
        return Ok(Vec::new());
    }

    // Профилей у браузера бывает несколько; смотрим самые обычные.
    let roots = [
        format!("{local}\\Google\\Chrome\\User Data"),
        format!("{local}\\Microsoft\\Edge\\User Data"),
    ];
    let profiles = ["Default", "Profile 1", "Profile 2", "Profile 3"];

    let mut found: Vec<Extension> = Vec::new();
    for root in roots {
        for profile in profiles {
            let path = std::path::Path::new(&root).join(profile).join("Extensions");
            collect_from(&path, &mut found);
        }
    }

    // Схлопываем по имени, а не по идентификатору: одно и то же
    // расширение в Chrome и в Edge имеет разные идентификаторы, а человеку
    // оно видится одним — и в списке должно быть одним.
    found.sort_by_key(|extension| extension.name.to_lowercase());
    found.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    Ok(found)
}

/// Собирает расширения из папки `Extensions`.
fn collect_from(path: &std::path::Path, found: &mut Vec<Extension>) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };

    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().to_string();
        // Идентификатор расширения — ровно 32 буквы от a до p.
        if id.len() != 32 || !id.chars().all(|c| ('a'..='p').contains(&c)) {
            continue;
        }

        // Внутри — папка версии, и уже в ней манифест. Версий бывает
        // несколько; берём любую, имя расширения в них одно.
        let Ok(versions) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        for version in versions.flatten() {
            let manifest = version.path().join("manifest.json");
            if let Some(name) = read_name(&manifest, &version.path()) {
                found.push(Extension { name, id });
                break;
            }
        }
    }
}

/// Достаёт имя расширения из манифеста.
///
/// Разбираем поиском по строке, а не полноценным разбором JSON: нужен
/// ровно один ключ, а тащить в проект целый разборщик ради него — плохой
/// размен. Формат манифеста при этом стабилен уже полтора десятка лет.
fn read_name(manifest: &std::path::Path, version_dir: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let raw = json_string_value(&text, "name")?;

    // Имя может быть ссылкой на перевод: `__MSG_appName__`. Тогда само
    // имя лежит в файле локализации.
    let Some(key) = raw
        .strip_prefix("__MSG_")
        .and_then(|r| r.strip_suffix("__"))
    else {
        return Some(raw);
    };

    let locales = version_dir.join("_locales");
    // Русский, затем английский, затем язык по умолчанию из манифеста.
    let mut candidates = vec!["ru".to_string(), "en".to_string(), "en_US".to_string()];
    if let Some(default) = json_string_value(&text, "default_locale") {
        candidates.insert(0, default);
    }

    for locale in candidates {
        let messages = locales.join(&locale).join("messages.json");
        let Ok(text) = std::fs::read_to_string(&messages) else {
            continue;
        };
        // В файле переводов имя лежит под своим ключом, а внутри — «message».
        if let Some(at) = text.find(&format!("\"{key}\"")) {
            if let Some(name) = json_string_value(&text[at..], "message") {
                return Some(name);
            }
        }
    }

    // Перевода не нашли — показывать `__MSG_appName__` бессмысленно.
    None
}

/// Значение строкового поля JSON по имени ключа.
fn json_string_value(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let at = text.find(&needle)? + needle.len();
    let rest = &text[at..];

    // После ключа идёт двоеточие, потом строка в кавычках.
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let start = after.find('"')? + 1;

    let mut value = String::new();
    let mut escaped = false;
    for symbol in after[start..].chars() {
        if escaped {
            // Экранированные кавычки внутри имени встречаются редко,
            // но обрывать строку на них нельзя.
            value.push(symbol);
            escaped = false;
            continue;
        }
        match symbol {
            '\\' => escaped = true,
            '"' => return Some(value.trim().to_string()).filter(|v| !v.is_empty()),
            _ => value.push(symbol),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_name_is_extracted() {
        let manifest = r#"{ "manifest_version": 3, "name": "uBlock Origin", "version": "1.5" }"#;
        assert_eq!(
            json_string_value(manifest, "name"),
            Some("uBlock Origin".to_string())
        );
    }

    #[test]
    fn escaped_quotes_do_not_cut_the_name_short() {
        let manifest = r#"{ "name": "Кавычки \"внутри\" имени" }"#;
        let name = json_string_value(manifest, "name").unwrap();
        assert!(name.contains("внутри"), "{name}");
    }

    #[test]
    fn a_missing_key_yields_nothing() {
        assert_eq!(json_string_value(r#"{ "version": "1.0" }"#, "name"), None);
        assert_eq!(json_string_value("", "name"), None);
    }

    #[test]
    fn an_empty_value_is_not_a_name() {
        assert_eq!(json_string_value(r#"{ "name": "" }"#, "name"), None);
    }

    #[test]
    fn installed_extensions_are_listed_without_errors() {
        // Список может быть пуст — браузера может не быть. Проверяем,
        // что чтение не падает и не выдумывает записей.
        let list = installed().expect("список расширений");
        for extension in &list {
            assert_eq!(extension.id.len(), 32, "неверный идентификатор");
            assert!(!extension.name.trim().is_empty());
            // Непереведённое имя показывать нельзя.
            assert!(
                !extension.name.starts_with("__MSG_"),
                "непереведённое имя: {}",
                extension.name
            );
        }
    }
}
