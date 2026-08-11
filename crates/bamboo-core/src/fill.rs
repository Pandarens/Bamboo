//! Подстановка в переведённый шаблон.
//!
//! `format!` требует шаблон литералом, а переведённый шаблон приходит
//! переменной — выбранным из пары «русский, английский». Отсюда и нужда
//! в подстановке во время работы.
//!
//! Ключи именованные, а не по порядку, и это не украшение. Порядок слов
//! в языках разный: «диск придержан у chrome.exe» и «chrome.exe: disk held
//! back» ставят имя в разные места. С позиционными ключами перевод пришлось
//! бы подгонять под порядок оригинала, а это ровно тот случай, когда
//! получается корявый английский, выдающий машинный перевод.

/// Подставляет значения в шаблон.
///
/// Ключи в шаблоне пишутся в фигурных скобках: `{app}`. Незнакомый ключ
/// остаётся в тексте как есть — это заметно глазами и чинится, а молчаливая
/// пустота на его месте выглядела бы как обрывок фразы.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;

    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open + 1..];

        let Some(close) = rest.find('}') else {
            // Скобка без пары. Возвращаем её на место: терять кусок текста
            // хуже, чем показать кривую скобку.
            out.push('{');
            break;
        };

        let key = &rest[..close];
        match values.iter().find(|(name, _)| *name == key) {
            Some((_, value)) => out.push_str(value),
            None => {
                out.push('{');
                out.push_str(key);
                out.push('}');
            }
        }
        rest = &rest[close + 1..];
    }

    out.push_str(rest);
    out
}

/// Выбирает шаблон по языку и подставляет значения.
///
/// Основной способ вызова: обе строки видны рядом, и расхождение между
/// ними заметно глазами.
pub fn say(russian: &'static str, english: &'static str, values: &[(&str, &str)]) -> String {
    fill(crate::lang::pick(russian, english), values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::{set_language, Language};
    use std::sync::Mutex;

    static LANGUAGE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn values_land_where_the_keys_are() {
        assert_eq!(
            fill("{app}: диск придержан", &[("app", "chrome.exe")]),
            "chrome.exe: диск придержан"
        );
    }

    #[test]
    fn the_same_value_can_appear_twice() {
        assert_eq!(
            fill("{app} и ещё раз {app}", &[("app", "код")]),
            "код и ещё раз код"
        );
    }

    #[test]
    fn a_translation_may_reorder_the_keys() {
        // Ради этого ключи именованные. Порядок слов в языках разный,
        // и подгонять перевод под порядок оригинала — верный способ
        // получить корявый английский.
        let values = [("app", "chrome.exe"), ("limit", "8 МБ/с")];
        assert_eq!(
            fill("{app}: придержан до {limit}", &values),
            "chrome.exe: придержан до 8 МБ/с"
        );
        assert_eq!(
            fill("held {app} back to {limit}", &values),
            "held chrome.exe back to 8 МБ/с"
        );
        // И наоборот — ключи в другом порядке.
        assert_eq!(
            fill("{limit} — предел для {app}", &values),
            "8 МБ/с — предел для chrome.exe"
        );
    }

    #[test]
    fn an_unknown_key_stays_visible_instead_of_vanishing() {
        // Молчаливая пустота на месте ключа выглядит как обрывок фразы
        // и не подсказывает, что чинить.
        assert_eq!(
            fill("{app}: {чего-то-нет}", &[("app", "код")]),
            "код: {чего-то-нет}"
        );
    }

    #[test]
    fn a_template_without_keys_is_returned_as_is() {
        assert_eq!(fill("просто текст", &[]), "просто текст");
        assert_eq!(fill("", &[("a", "b")]), "");
    }

    #[test]
    fn an_unclosed_brace_does_not_eat_the_rest() {
        // Терять кусок текста хуже, чем показать кривую скобку.
        assert_eq!(
            fill("начало {ключ без конца", &[]),
            "начало {ключ без конца"
        );
    }

    #[test]
    fn saying_follows_the_chosen_language() {
        let _guard = LANGUAGE_LOCK.lock().unwrap();
        let values = [("app", "chrome.exe")];

        set_language(Language::Russian);
        assert_eq!(
            say("{app}: придержан", "{app}: held back", &values),
            "chrome.exe: придержан"
        );

        set_language(Language::English);
        assert_eq!(
            say("{app}: придержан", "{app}: held back", &values),
            "chrome.exe: held back"
        );

        set_language(Language::Russian);
    }
}
