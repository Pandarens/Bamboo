//! Обучение на отклонениях (ТЗ, раздел 10.3).
//!
//! Правило простое: то, от чего человек отказался дважды, Bamboo больше
//! не предлагает никогда. Утилита, повторяющая одно и то же предложение
//! после двух отказов, ведёт себя как навязчивый продавец, а не как
//! помощник, — и человек перестаёт читать её предложения вовсе.
//!
//! Всё это чистая логика: ни одного обращения к Windows. Хранится решение
//! человека в обычном текстовом файле, который он может прочитать и
//! поправить руками. Реестр или база сделали бы список непрозрачным,
//! а это как раз то, что человек вправе видеть: чем именно Bamboo решил
//! больше не заниматься.

use std::collections::HashMap;

/// Отказы, накопленные за время работы, вместе с их идемпотентностью.
///
/// Идемпотентность здесь не украшение, а необходимость. Порог всего два,
/// а показать одно предложение может и виджет, и окно; если оба доложат
/// об отказе, приложение замолчит навсегда после одного настоящего отказа
/// человека. Поэтому повторный отказ по той же паре в течение короткого
/// времени считается тем же самым отказом.
#[derive(Debug, Default)]
pub struct Rejections {
    /// Сколько раз отклонили: ключ — приложение и действие вместе.
    counts: HashMap<String, u8>,
    /// Когда отказ засчитан в последний раз — для идемпотентности.
    last_ms: HashMap<String, i64>,
}

/// Насколько близкие по времени отказы считаются одним.
///
/// Две секунды: человек не успевает отклонить одно и то же осмысленно
/// дважды за такой срок, а вот два места интерфейса — успевают.
const SAME_REJECTION_MS: i64 = 2000;

/// После скольких отказов предложение замолкает навсегда.
pub const REJECTIONS_TO_SILENCE: u8 = 2;

/// Что произошло после отказа.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Learned {
    /// Отказ учтён, предложение ещё может появиться.
    Counted { count: u8 },
    /// Второй отказ: больше не предложим никогда.
    Silenced,
    /// Тот же самый отказ, пришедший дважды. Ничего не изменилось.
    Duplicate,
}

impl Rejections {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ключ пары «приложение + действие».
    ///
    /// Именно пары, а не одного приложения: отказ придержать диск браузеру
    /// не означает отказа от экономичного режима для него же. Молчать
    /// про всё сразу после отказа от одного — перебор.
    fn key(app_key: &str, action: &str) -> String {
        format!("{}\u{1}{}", app_key.to_lowercase(), action)
    }

    /// Учитывает отказ человека.
    pub fn reject(&mut self, app_key: &str, action: &str, now_ms: i64) -> Learned {
        let key = Self::key(app_key, action);

        if let Some(when) = self.last_ms.get(&key) {
            if now_ms.saturating_sub(*when) < SAME_REJECTION_MS {
                return Learned::Duplicate;
            }
        }
        self.last_ms.insert(key.clone(), now_ms);

        let count = self.counts.entry(key).or_insert(0);
        *count = count.saturating_add(1);

        if *count >= REJECTIONS_TO_SILENCE {
            Learned::Silenced
        } else {
            Learned::Counted { count: *count }
        }
    }

    /// Замолчало ли это предложение навсегда.
    pub fn is_silenced(&self, app_key: &str, action: &str) -> bool {
        self.counts
            .get(&Self::key(app_key, action))
            .is_some_and(|count| *count >= REJECTIONS_TO_SILENCE)
    }

    /// Сколько раз отклоняли.
    pub fn count(&self, app_key: &str, action: &str) -> u8 {
        self.counts
            .get(&Self::key(app_key, action))
            .copied()
            .unwrap_or(0)
    }

    /// Сколько пар замолчало.
    pub fn silenced_count(&self) -> usize {
        self.counts
            .values()
            .filter(|count| **count >= REJECTIONS_TO_SILENCE)
            .count()
    }

    /// Забывает отказ: человек передумал.
    ///
    /// Нужно обязательно. Список, из которого нельзя выйти, — ловушка:
    /// один случайный отказ навсегда лишил бы человека предложения,
    /// которое ему потом понадобилось.
    pub fn forget(&mut self, app_key: &str, action: &str) {
        let key = Self::key(app_key, action);
        self.counts.remove(&key);
        self.last_ms.remove(&key);
    }

    /// Записывает в текст: по строке на пару.
    ///
    /// Формат нарочно простой и читаемый глазами: человек вправе видеть,
    /// чем именно Bamboo решил больше не заниматься, и поправить это
    /// обычным блокнотом.
    pub fn to_text(&self) -> String {
        let mut lines: Vec<String> = self
            .counts
            .iter()
            .map(|(key, count)| {
                let (app, action) = key.split_once('\u{1}').unwrap_or((key.as_str(), ""));
                format!("{count}\t{app}\t{action}")
            })
            .collect();
        // Порядок устойчив, чтобы файл не менялся от перезапуска
        // к перезапуску без причины.
        lines.sort();
        lines.join("\n")
    }

    /// Читает из текста. Испорченные строки пропускаются молча: файл
    /// правит человек, и опечатка в нём не повод терять весь список.
    pub fn from_text(text: &str) -> Rejections {
        let mut out = Rejections::new();
        for line in text.lines() {
            let mut parts = line.split('\t');
            let (Some(count), Some(app)) = (parts.next(), parts.next()) else {
                continue;
            };
            let action = parts.next().unwrap_or_default();
            let Ok(count) = count.trim().parse::<u8>() else {
                continue;
            };
            if count == 0 || app.trim().is_empty() {
                continue;
            }
            out.counts.insert(Self::key(app, action), count);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_second_refusal_silences_the_suggestion_forever() {
        let mut learned = Rejections::new();
        assert_eq!(
            learned.reject("chrome.exe", "эконом", 0),
            Learned::Counted { count: 1 }
        );
        assert!(!learned.is_silenced("chrome.exe", "эконом"));

        assert_eq!(
            learned.reject("chrome.exe", "эконом", 10_000),
            Learned::Silenced
        );
        assert!(learned.is_silenced("chrome.exe", "эконом"));
    }

    #[test]
    fn two_places_reporting_one_refusal_do_not_count_twice() {
        // Ловушка, из-за которой правило сломалось бы в тот же день:
        // порог всего два, и лишнее срабатывание заставляет приложение
        // замолчать навсегда после ОДНОГО настоящего отказа.
        let mut learned = Rejections::new();
        learned.reject("chrome.exe", "эконом", 1000);
        assert_eq!(
            learned.reject("chrome.exe", "эконом", 1200),
            Learned::Duplicate
        );

        assert_eq!(learned.count("chrome.exe", "эконом"), 1);
        assert!(!learned.is_silenced("chrome.exe", "эконом"));
    }

    #[test]
    fn refusing_one_action_does_not_silence_the_others() {
        // Отказ придержать диск браузеру не означает отказа от экономичного
        // режима для него же. Молчать про всё сразу — перебор.
        let mut learned = Rejections::new();
        learned.reject("chrome.exe", "диск", 0);
        learned.reject("chrome.exe", "диск", 10_000);

        assert!(learned.is_silenced("chrome.exe", "диск"));
        assert!(!learned.is_silenced("chrome.exe", "эконом"));
    }

    #[test]
    fn the_name_is_matched_regardless_of_case() {
        let mut learned = Rejections::new();
        learned.reject("Chrome.exe", "эконом", 0);
        learned.reject("CHROME.EXE", "эконом", 10_000);
        assert!(learned.is_silenced("chrome.exe", "эконом"));
    }

    #[test]
    fn a_person_can_change_their_mind() {
        // Список, из которого нельзя выйти, — ловушка: один случайный отказ
        // навсегда лишил бы человека нужного предложения.
        let mut learned = Rejections::new();
        learned.reject("chrome.exe", "эконом", 0);
        learned.reject("chrome.exe", "эконом", 10_000);
        assert!(learned.is_silenced("chrome.exe", "эконом"));

        learned.forget("chrome.exe", "эконом");
        assert!(!learned.is_silenced("chrome.exe", "эконом"));
        assert_eq!(learned.count("chrome.exe", "эконом"), 0);
    }

    #[test]
    fn the_list_survives_a_restart() {
        // Без этого «навсегда» жило бы до выхода из программы, и человек
        // получал бы то же предложение после каждой перезагрузки.
        let mut learned = Rejections::new();
        learned.reject("chrome.exe", "эконом", 0);
        learned.reject("chrome.exe", "эконом", 10_000);
        learned.reject("updater.exe", "диск", 0);

        let restored = Rejections::from_text(&learned.to_text());
        assert!(restored.is_silenced("chrome.exe", "эконом"));
        assert_eq!(restored.count("updater.exe", "диск"), 1);
        assert_eq!(restored.silenced_count(), 1);
    }

    #[test]
    fn the_file_is_readable_by_a_person() {
        // Человек вправе видеть, чем именно Bamboo решил больше
        // не заниматься, и поправить это блокнотом.
        let mut learned = Rejections::new();
        learned.reject("chrome.exe", "эконом", 0);

        let text = learned.to_text();
        assert!(text.contains("chrome.exe"), "{text}");
        assert!(text.contains("эконом"), "{text}");
        assert!(text.starts_with('1'), "{text}");
    }

    #[test]
    fn a_broken_line_does_not_lose_the_rest() {
        let text = "мусор\n\n2\tchrome.exe\tэконом\nx\tплохо\tсовсем\n1\tupdater.exe\tдиск";
        let restored = Rejections::from_text(text);
        assert!(restored.is_silenced("chrome.exe", "эконом"));
        assert_eq!(restored.count("updater.exe", "диск"), 1);
    }

    #[test]
    fn an_empty_file_is_an_empty_list() {
        let restored = Rejections::from_text("");
        assert_eq!(restored.silenced_count(), 0);
        assert!(!restored.is_silenced("что угодно", "эконом"));
    }

    #[test]
    fn a_saved_list_does_not_churn_between_restarts() {
        // Файл, меняющийся без причины, мешает и человеку, и любому
        // средству, которое его сравнивает.
        let mut learned = Rejections::new();
        learned.reject("б.exe", "эконом", 0);
        learned.reject("а.exe", "диск", 0);

        assert_eq!(
            learned.to_text(),
            Rejections::from_text(&learned.to_text()).to_text()
        );
    }
}
