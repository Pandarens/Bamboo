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

/// Реестр процессов, которым мы придержали диск.
///
/// Ограничение держится job-объектом и живёт ровно столько, сколько живёт
/// его дескриптор. Поэтому дескрипторы надо где-то хранить: выпустим —
/// лимит снимется сам. Это же и защита от забытых изменений — закрылся
/// Bamboo, и чужой процесс работает как раньше.
#[derive(Default)]
pub struct IoLimits {
    limited: std::collections::HashMap<u32, bamboo_sys::LimitedProcess>,
}

impl IoLimits {
    pub fn new() -> Self {
        Self::default()
    }

    /// Придержан ли процесс прямо сейчас.
    pub fn is_limited(&self, pid: u32) -> bool {
        self.limited.contains_key(&pid)
    }

    /// Переключает ограничение и объясняет результат словами.
    ///
    /// Отдельно оговорка про «запретить». Запрета ввода-вывода в Windows
    /// нет, и он был бы вреден: процесс, которому отказали в чтении файла,
    /// не подождёт вежливо — он упадёт. Поэтому придерживаем скорость.
    pub fn toggle(&mut self, pid: u32, image_name: &str) -> String {
        use bamboo_policy::ProcessFacts;

        if self.limited.remove(&pid).is_some() {
            return format!("{image_name}: ограничение диска снято.");
        }

        let facts = ProcessFacts {
            image_name,
            session_id: 1,
            ..Default::default()
        };
        if let Some(reason) = bamboo_policy::immutable_reason(&facts) {
            return format!("{image_name}: придерживать нельзя — {reason}");
        }

        let limit = bamboo_sys::IoLimit::Background;
        match bamboo_sys::LimitedProcess::throttle(pid, limit) {
            Ok(limited) => {
                self.limited.insert(pid, limited);
                let held = self.count();
                let others = if held > 1 {
                    format!(" Сейчас придержано процессов: {held}.")
                } else {
                    String::new()
                };
                format!(
                    "{image_name}: диск придержан — {}. Полного запрета не бывает:                      процесс, которому отказали в чтении, просто упал бы.                      Ограничение снимется само, когда Bamboo закроется.{others}",
                    limit.describe()
                )
            }
            Err(error) => format!("{image_name}: придержать не удалось — {error}"),
        }
    }

    /// Сколько процессов сейчас придержано.
    pub fn count(&self) -> usize {
        self.limited.len()
    }
}

/// Завершает процесс по требованию пользователя.
///
/// Стоит особняком от `apply` и намеренно не идёт через журнал действий:
/// журнал существует ради отката, а поднять завершённый процесс нельзя.
/// Это единственная необратимая операция в Bamboo, и подаётся она именно
/// так — с подтверждением в интерфейсе и предупреждением о потере
/// несохранённых данных.
///
/// Системные процессы защищены тем же неизменяемым списком, что и все
/// остальные действия: убить `lsass.exe` через Bamboo не выйдет.
pub fn terminate(pid: u32, image_name: &str) -> String {
    let facts = ProcessFacts {
        image_name,
        session_id: 1,
        ..Default::default()
    };
    if let Some(reason) = bamboo_policy::immutable_reason(&facts) {
        return format!("{image_name}: завершать нельзя — {reason}");
    }

    match bamboo_sys::terminate_process(pid) {
        Ok(()) => format!("{image_name} (PID {pid}) завершён. Это действие необратимо."),
        Err(error) => format!("{image_name}: завершить не удалось — {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiting_a_system_process_is_refused() {
        let mut limits = IoLimits::new();
        let note = limits.toggle(4, "lsass.exe");
        assert!(note.contains("нельзя"), "получили: {note}");
        assert_eq!(limits.count(), 0);
    }

    #[test]
    fn toggling_a_missing_process_reports_failure() {
        let mut limits = IoLimits::new();
        let note = limits.toggle(0xFFFF_FFF0, "нет-такого.exe");
        assert!(note.contains("не удалось"), "получили: {note}");
        assert!(!limits.is_limited(0xFFFF_FFF0));
    }

    #[test]
    fn a_limit_can_be_applied_and_lifted() {
        let child = std::process::Command::new("cmd.exe")
            .args(["/c", "ping -n 5 127.0.0.1 > nul"])
            .spawn();
        let Ok(mut child) = child else { return };

        let mut limits = IoLimits::new();
        let note = limits.toggle(child.id(), "cmd.exe");

        if limits.is_limited(child.id()) {
            assert!(note.contains("придержан"), "{note}");
            // Второе нажатие снимает ограничение.
            let off = limits.toggle(child.id(), "cmd.exe");
            assert!(off.contains("снято"), "{off}");
            assert_eq!(limits.count(), 0);
        }

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_system_process_cannot_be_terminated() {
        // Неизменяемый список обязан отсечь запрос до системного вызова.
        let note = terminate(4, "lsass.exe");
        assert!(note.contains("завершать нельзя"), "получили: {note}");
    }

    #[test]
    fn terminating_reports_failure_instead_of_pretending() {
        // Процесса с таким PID нет — операция обязана честно не удаться.
        let note = terminate(0xFFFF_FFF0, "нет-такого.exe");
        assert!(note.contains("не удалось"), "получили: {note}");
    }

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
