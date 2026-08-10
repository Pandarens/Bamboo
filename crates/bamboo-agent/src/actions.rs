//! Действия над процессами из главного окна (ТЗ, разделы 11.1 и 14.6).
//!
//! Здесь только те действия, которые агент вправе выполнить сам: уровень
//! риска 1, прав администратора не требуют, применяются к процессам своей
//! сессии. Всё остальное — через брокер.
//!
//! Чего здесь нет и не будет: «очистки памяти». `EmptyWorkingSet` не
//! освобождает память, а выталкивает рабочий набор в подкачку, после чего
//! приложение считывает его обратно за секунды — пользователь получает
//! красивую цифру и подтормаживание вместо пользы. Это прямой отказ из
//! раздела 11.5 ТЗ, а не недоделка.

#![forbid(unsafe_code)]

use bamboo_actuate::{Executor, Outcome, SystemBackend};
use bamboo_journal::{Actor, Journal, Target};
use bamboo_policy::{Action, AutonomyMode, Context, ProcessFacts, Profile, UserWhitelist};

/// Что можно сделать с процессом из списка.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    /// Экономичный режим: планировщик уводит потоки на энергоэффективные ядра.
    EcoQos,
    /// Понизить приоритет памяти.
    LowerMemory,
}

impl RowAction {
    /// Действие по коду из интерфейса.
    pub fn from_index(index: i32) -> Option<RowAction> {
        match index {
            0 => Some(RowAction::EcoQos),
            1 => Some(RowAction::LowerMemory),
            _ => None,
        }
    }

    fn action(self) -> Action {
        match self {
            RowAction::EcoQos => Action::EnableEcoQos,
            RowAction::LowerMemory => Action::LowerMemoryPriority,
        }
    }
}

fn journal_path() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(base)
        .join("Bamboo")
        .join("journal.db")
}

/// Применяет действие к процессу и возвращает строку для показа пользователю.
///
/// Ничего не делает молча: и успех, и отказ объясняются словами. Отказ
/// политики — штатный исход, а не ошибка: например, системные процессы
/// защищены неизменяемым белым списком.
pub fn apply(pid: u32, image_name: &str, what: RowAction) -> String {
    let path = journal_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let journal = match Journal::open(&path) {
        Ok(journal) => journal,
        Err(error) => return format!("Журнал недоступен, действие отменено: {error}"),
    };

    let executor = Executor::new(&journal, SystemBackend);
    let whitelist = UserWhitelist::new();
    let action = what.action();

    let context = Context {
        action,
        process: ProcessFacts {
            image_name,
            session_id: 1,
            ..Default::default()
        },
        app_key: image_name,
        app_class: None,
        profile: Profile::Normal,
        // Действие запустил человек, а не автоматика: это его решение.
        mode: AutonomyMode::Assist,
        learning: false,
        whitelist: &whitelist,
    };
    let target = Target {
        app_key: image_name.to_string(),
        pid: Some(pid),
        ..Default::default()
    };

    let now = bamboo_core::SampleTime::wall_clock_now();
    match executor.apply(now, &context, &target, Actor::Manual, false) {
        Outcome::Applied { journal_id } => format!(
            "{}: {} — применено, запись №{journal_id}. Откатить можно в журнале.",
            image_name,
            action.name()
        ),
        Outcome::Simulated { would_do } => format!("{image_name}: {would_do}"),
        Outcome::Refused { reason } => format!("{image_name}: отказ — {reason}"),
        Outcome::Failed { error, .. } => format!("{image_name}: не удалось — {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_codes_map_to_real_actions() {
        assert_eq!(RowAction::from_index(0), Some(RowAction::EcoQos));
        assert_eq!(RowAction::from_index(1), Some(RowAction::LowerMemory));
        // Неизвестный код не превращается молча в какое-нибудь действие.
        assert_eq!(RowAction::from_index(7), None);
    }

    #[test]
    fn both_actions_are_the_lowest_risk_level() {
        // Агент вправе делать сам только уровень 1. Если сюда однажды
        // добавят действие рискованнее, тест это заметит.
        for what in [RowAction::EcoQos, RowAction::LowerMemory] {
            assert_eq!(what.action().risk(), 1, "{:?}", what.action());
        }
    }

    #[test]
    fn a_protected_process_is_refused_with_an_explanation() {
        // lsass.exe в неизменяемом белом списке: политика обязана отказать,
        // и отказ обязан быть объяснён.
        let note = apply(4, "lsass.exe", RowAction::EcoQos);
        assert!(note.contains("отказ"), "получили: {note}");
        assert!(
            note.len() > "lsass.exe: отказ — ".len(),
            "отказ без причины"
        );
    }
}
