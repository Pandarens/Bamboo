//! Исполнение действий на живой системе.

use bamboo_journal::Target;
use bamboo_policy::Action;
use bamboo_sys::control::{self, MemoryPriority};

use crate::executor::Backend;
use crate::state::{yes_no, PriorState};

/// Приоритет памяти, до которого понижаем фоновые приложения.
///
/// Не самый низкий: слишком агрессивное понижение приводит к тому,
/// что приложение при возврате пользователя долго считывает страницы
/// обратно, и человек видит подтормаживание вместо экономии.
const BACKGROUND_PRIORITY: MemoryPriority = MemoryPriority::MEDIUM;

pub struct SystemBackend;

impl SystemBackend {
    fn pid(target: &Target) -> Result<u32, String> {
        target
            .pid
            .ok_or_else(|| "действие применяется к процессу, но PID не задан".to_string())
    }
}

impl Backend for SystemBackend {
    fn capture(&self, action: Action, target: &Target) -> Result<PriorState, String> {
        match action {
            Action::EnableEcoQos => {
                let pid = Self::pid(target)?;
                let current = control::eco_qos(pid).map_err(|error| error.to_string())?;
                Ok(PriorState::new().with("eco_qos", yes_no(current)))
            }
            Action::LowerMemoryPriority => {
                let pid = Self::pid(target)?;
                let current = control::memory_priority(pid).map_err(|error| error.to_string())?;
                Ok(PriorState::new().with("memory_priority", current.0))
            }
            other => Err(format!("{} ещё не реализовано", other.name())),
        }
    }

    fn apply(&self, action: Action, target: &Target) -> Result<(), String> {
        match action {
            Action::EnableEcoQos => {
                control::set_eco_qos(Self::pid(target)?, true).map_err(|error| error.to_string())
            }
            Action::LowerMemoryPriority => {
                control::set_memory_priority(Self::pid(target)?, BACKGROUND_PRIORITY)
                    .map_err(|error| error.to_string())
            }
            other => Err(format!("{} ещё не реализовано", other.name())),
        }
    }

    fn revert(&self, action: Action, target: &Target, prior: &PriorState) -> Result<(), String> {
        let pid = Self::pid(target)?;

        match action {
            Action::EnableEcoQos => {
                match prior.get_bool("eco_qos") {
                    // До нас режим был включён — оставляем как было.
                    Some(true) => control::set_eco_qos(pid, true),
                    // До нас режимом управляла система. Возвращаем ей
                    // управление, а не выключаем принудительно: это разные
                    // состояния, и второе — не откат, а новое изменение.
                    Some(false) | None => control::clear_eco_qos(pid),
                }
                .map_err(|error| error.to_string())
            }

            Action::LowerMemoryPriority => {
                let previous = prior
                    .get_u32("memory_priority")
                    .map(MemoryPriority)
                    .filter(|priority| priority.is_valid())
                    // Записи о прошлом состоянии нет — возвращаем обычный
                    // приоритет: он заведомо не хуже того, что мы выставили.
                    .unwrap_or(MemoryPriority::NORMAL);
                control::set_memory_priority(pid, previous).map_err(|error| error.to_string())
            }

            other => Err(format!("{} ещё не реализовано", other.name())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Target {
        Target {
            app_key: "bamboo-cli".into(),
            pid: Some(std::process::id()),
            ..Default::default()
        }
    }

    #[test]
    fn eco_qos_applies_and_reverts_on_a_live_process() {
        let backend = SystemBackend;

        let prior = backend.capture(Action::EnableEcoQos, &me()).unwrap();
        backend.apply(Action::EnableEcoQos, &me()).unwrap();
        assert!(control::eco_qos(std::process::id()).unwrap());

        backend.revert(Action::EnableEcoQos, &me(), &prior).unwrap();
        assert_eq!(
            control::eco_qos(std::process::id()).unwrap(),
            prior.get_bool("eco_qos").unwrap(),
            "состояние после отката не совпало с состоянием до"
        );
    }

    #[test]
    fn memory_priority_applies_and_reverts_on_a_live_process() {
        let backend = SystemBackend;
        let before = control::memory_priority(std::process::id()).unwrap();

        let prior = backend.capture(Action::LowerMemoryPriority, &me()).unwrap();
        backend.apply(Action::LowerMemoryPriority, &me()).unwrap();
        assert_eq!(
            control::memory_priority(std::process::id()).unwrap(),
            BACKGROUND_PRIORITY
        );

        backend
            .revert(Action::LowerMemoryPriority, &me(), &prior)
            .unwrap();
        assert_eq!(
            control::memory_priority(std::process::id()).unwrap(),
            before
        );
    }

    #[test]
    fn an_action_without_a_pid_is_refused() {
        let backend = SystemBackend;
        let target = Target::app("без-pid");
        assert!(backend.capture(Action::EnableEcoQos, &target).is_err());
        assert!(backend.apply(Action::EnableEcoQos, &target).is_err());
    }

    #[test]
    fn unimplemented_actions_say_so_instead_of_pretending() {
        let backend = SystemBackend;
        let error = backend.apply(Action::StopService, &me()).unwrap_err();
        assert!(error.contains("ещё не реализовано"));
    }

    #[test]
    fn a_corrupted_prior_state_falls_back_to_something_safe() {
        let backend = SystemBackend;
        let before = control::memory_priority(std::process::id()).unwrap();

        backend.apply(Action::LowerMemoryPriority, &me()).unwrap();
        // Запись повреждена, прошлого значения нет.
        backend
            .revert(
                Action::LowerMemoryPriority,
                &me(),
                &PriorState::parse("мусор"),
            )
            .unwrap();

        assert_eq!(
            control::memory_priority(std::process::id()).unwrap(),
            MemoryPriority::NORMAL
        );

        control::set_memory_priority(std::process::id(), before).unwrap();
    }
}
