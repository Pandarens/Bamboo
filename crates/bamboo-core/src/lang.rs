//! Язык, на котором Bamboo говорит с человеком (ТЗ, раздел 19).
//!
//! Механизм нарочно устроен просто: выбранный язык лежит в одном месте,
//! а каждая функция, возвращающая текст человеку, сама выбирает нужную
//! строку. Каталога с ключами здесь нет, и это осознанно.
//!
//! Каталог по ключам — привычный способ, но для этого проекта он плох.
//! Тексты здесь не подписи кнопок, а связные объяснения на несколько
//! предложений, и ключ вроде `bottleneck.gpu.advice` не говорит ничего:
//! чтобы понять, что правишь, надо всё равно идти в каталог. Когда обе
//! строки лежат рядом в одном `match`, расхождение видно глазами, а
//! забыть перевести новую ветку не даёт компилятор.
//!
//! Цена такого решения — строки живут в коде, а не в файле, который можно
//! отдать переводчику. Для двух языков это приемлемо; для десяти пришлось бы
//! менять подход.
//!
//! Строки интерфейса переводятся отдельно, средствами Slint: там как раз
//! подписи, и каталог им к лицу.

use core::sync::atomic::{AtomicU8, Ordering};

/// На каком языке говорим.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    Russian,
    English,
}

impl Language {
    /// Код языка: «ru» либо «en».
    pub fn code(self) -> &'static str {
        match self {
            Language::Russian => "ru",
            Language::English => "en",
        }
    }

    /// Разбирает код. Незнакомый — русский: это язык, на котором написан
    /// оригинал, и подставлять вместо непонятного значения английский
    /// значило бы сменить человеку язык без его ведома.
    pub fn parse(code: &str) -> Language {
        match code {
            "en" => Language::English,
            _ => Language::Russian,
        }
    }
}

/// Выбранный язык. Одно значение на процесс: он выбирается при запуске
/// и не меняется на ходу — тексты берутся в момент показа, и половина
/// окна осталась бы на прежнем языке.
static CHOSEN: AtomicU8 = AtomicU8::new(0);

/// Устанавливает язык.
pub fn set_language(language: Language) {
    CHOSEN.store(
        match language {
            Language::Russian => 0,
            Language::English => 1,
        },
        Ordering::Relaxed,
    );
}

/// Какой язык выбран.
pub fn language() -> Language {
    match CHOSEN.load(Ordering::Relaxed) {
        1 => Language::English,
        _ => Language::Russian,
    }
}

/// Выбирает строку по языку.
///
/// Читается как пара: слева русский оригинал, справа перевод. Оба видны
/// в одном месте, и расхождение между ними заметно глазами — ради этого
/// всё и затевалось.
pub fn pick(russian: &'static str, english: &'static str) -> &'static str {
    match language() {
        Language::Russian => russian,
        Language::English => english,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Язык один на процесс: тесты, меняющие его, нельзя пускать разом.
    static LANGUAGE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn the_default_is_the_language_the_original_is_written_in() {
        assert_eq!(Language::default(), Language::Russian);
    }

    #[test]
    fn an_unknown_code_falls_back_to_russian() {
        // Подставить английский вместо непонятного значения значило бы
        // сменить человеку язык без его ведома.
        assert_eq!(Language::parse("эльфийский"), Language::Russian);
        assert_eq!(Language::parse(""), Language::Russian);
        assert_eq!(Language::parse("en"), Language::English);
    }

    #[test]
    fn a_code_survives_a_round_trip() {
        for language in [Language::Russian, Language::English] {
            assert_eq!(Language::parse(language.code()), language);
        }
    }

    #[test]
    fn picking_follows_the_chosen_language() {
        let _guard = LANGUAGE_LOCK.lock().unwrap();
        let was = language();

        set_language(Language::Russian);
        assert_eq!(pick("по-русски", "in English"), "по-русски");

        set_language(Language::English);
        assert_eq!(pick("по-русски", "in English"), "in English");

        set_language(was);
    }
}
