//! Устойчивый ключ приложения.
//!
//! Временной ряд нельзя вести по PID: он переиспользуется, а приложение
//! перезапускается. Ключом служит нормализованный путь образа плюс издатель
//! (ТЗ, раздел 8.3):
//!
//! ```text
//! app_key = normalize(image_path) + ":" + publisher_or_hash
//! ```

use core::fmt;

/// Замена абсолютного префикса пути на устойчивый токен.
///
/// Нужна, потому что один и тот же браузер лежит в `C:\Users\vasya\AppData\...`
/// и `C:\Users\petya\AppData\...`, а это одно и то же приложение.
/// Значения путей передаёт вызывающий: `bamboo-core` ничего не знает
/// про переменные окружения Windows.
#[derive(Clone, Debug, Default)]
pub struct PathNormalizer {
    /// Пары «префикс в нижнем регистре» → «токен», от длинных к коротким.
    prefixes: Vec<(String, String)>,
}

impl PathNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Добавляет замену. Пустые пути игнорируются — переменной окружения
    /// может не быть.
    pub fn with_prefix(mut self, prefix: &str, token: &str) -> Self {
        if prefix.is_empty() {
            return self;
        }
        let prefix = prefix.trim_end_matches(['\\', '/']).to_lowercase();
        self.prefixes.push((prefix, token.to_string()));
        // Длинные префиксы должны срабатывать первыми: %localappdata%
        // вложен в %userprofile%.
        self.prefixes.sort_by_key(|(p, _)| core::cmp::Reverse(p.len()));
        self
    }

    /// Приводит путь к каноническому виду: нижний регистр, прямые слэши
    /// заменены на обратные, известные префиксы свёрнуты в токены,
    /// версии в именах каталогов схлопнуты.
    pub fn normalize(&self, path: &str) -> String {
        let mut result = path.replace('/', "\\").to_lowercase();

        for (prefix, token) in &self.prefixes {
            if result.starts_with(prefix.as_str()) {
                result.replace_range(..prefix.len(), token);
                break;
            }
        }

        collapse_versions(&result)
    }
}

/// Схлопывает номера версий в путях.
///
/// Приложения вроде Discord ставятся в каталог `app-1.0.9034`, который меняется
/// при каждом обновлении. Без схлопывания история приложения обнуляется
/// после каждого апдейта.
pub fn collapse_versions(path: &str) -> String {
    path.split('\\')
        .map(collapse_segment)
        .collect::<Vec<_>>()
        .join("\\")
}

fn collapse_segment(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = String::with_capacity(segment.len());
    let mut i = 0;

    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }

        // Нашли начало числа — жадно набираем последовательность вида 1.0.9034
        let start = i;
        let mut groups = 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        while i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
            groups += 1;
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }

        // Одиночное число — часть имени (`vcruntime140`, `python3`), не версия.
        if groups >= 2 {
            out.push_str("{v}");
        } else {
            out.push_str(&segment[start..i]);
        }
    }

    out
}

/// Издатель образа.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Publisher {
    /// Имя из валидной цифровой подписи.
    Signed(String),
    /// Подписи нет — идентифицируем по SHA-256 файла образа.
    Hash(String),
    /// Проверка ещё не выполнялась. Проверка подписи дорогая и в цикле опроса
    /// не делается, поэтому это нормальное состояние для только что увиденного
    /// процесса.
    Unknown,
}

impl fmt::Display for Publisher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Publisher::Signed(name) => write!(f, "{name}"),
            Publisher::Hash(hash) => write!(f, "sha256:{hash}"),
            Publisher::Unknown => write!(f, "?"),
        }
    }
}

/// Ключ приложения.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AppKey(String);

impl AppKey {
    pub fn new(normalized_path: &str, publisher: &Publisher) -> Self {
        AppKey(format!("{normalized_path}:{publisher}"))
    }

    /// Ключ по одному пути, без сведений об издателе.
    pub fn from_path(normalized_path: &str) -> Self {
        AppKey::new(normalized_path, &Publisher::Unknown)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AppKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalizer() -> PathNormalizer {
        PathNormalizer::new()
            .with_prefix("C:\\Users\\vasya", "%userprofile%")
            .with_prefix("C:\\Users\\vasya\\AppData\\Local", "%localappdata%")
            .with_prefix("C:\\Windows", "%windir%")
    }

    #[test]
    fn case_and_slashes_do_not_matter() {
        let n = PathNormalizer::new();
        assert_eq!(n.normalize("C:/Windows/System32/SVCHOST.EXE"), "c:\\windows\\system32\\svchost.exe");
    }

    #[test]
    fn longest_prefix_wins() {
        let n = normalizer();
        assert_eq!(
            n.normalize("C:\\Users\\vasya\\AppData\\Local\\Discord\\Update.exe"),
            "%localappdata%\\discord\\update.exe"
        );
    }

    #[test]
    fn user_profile_is_replaced() {
        let n = normalizer();
        assert_eq!(
            n.normalize("C:\\Users\\Vasya\\Downloads\\tool.exe"),
            "%userprofile%\\downloads\\tool.exe"
        );
    }

    #[test]
    fn versioned_directory_collapses() {
        assert_eq!(
            collapse_versions("%localappdata%\\discord\\app-1.0.9034\\discord.exe"),
            "%localappdata%\\discord\\app-{v}\\discord.exe"
        );
    }

    #[test]
    fn single_number_in_a_name_is_not_a_version() {
        // vcruntime140 и python3 — часть имени, схлопывать нельзя.
        assert_eq!(
            collapse_versions("c:\\windows\\system32\\vcruntime140.dll"),
            "c:\\windows\\system32\\vcruntime140.dll"
        );
        assert_eq!(collapse_versions("c:\\python3\\python.exe"), "c:\\python3\\python.exe");
    }

    #[test]
    fn app_key_survives_an_update() {
        let n = normalizer();
        let before = n.normalize("C:\\Users\\vasya\\AppData\\Local\\Discord\\app-1.0.9034\\Discord.exe");
        let after = n.normalize("C:\\Users\\vasya\\AppData\\Local\\Discord\\app-1.0.9041\\Discord.exe");
        assert_eq!(AppKey::from_path(&before), AppKey::from_path(&after));
    }

    #[test]
    fn different_publishers_are_different_apps() {
        let signed = AppKey::new("c:\\tools\\updater.exe", &Publisher::Signed("Acme".into()));
        let unsigned = AppKey::new("c:\\tools\\updater.exe", &Publisher::Hash("deadbeef".into()));
        assert_ne!(signed, unsigned);
    }
}
