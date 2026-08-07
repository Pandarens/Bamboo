//! Действия и их уровни риска (ТЗ, раздел 11.1).

use core::fmt;

/// Кто может выполнить действие без спроса.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Autonomy {
    /// Можно применять автономно.
    Automatic,
    /// Нужно подтверждение пользователя.
    WithConfirmation,
    /// Только вручную, по прямому запросу.
    ManualOnly,
}

/// Атомарное изменение состояния системы.
///
/// Каждое действие имеет обратный рецепт — иначе его не существует.
/// Порядок вариантов соответствует возрастанию риска.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    /// EcoQoS: планировщик уводит потоки на энергоэффективные ядра.
    EnableEcoQos,
    /// Понижение приоритета памяти.
    LowerMemoryPriority,
    /// Отложенный автозапуск службы.
    DelayServiceStart,
    /// Отключение элемента автозагрузки.
    DisableStartupItem,
    /// Отключение таймера пробуждения у задачи.
    DisableWakeTimer,
    /// Отключение задачи планировщика.
    DisableScheduledTask,
    /// Заморозка процесса через job object.
    FreezeProcess,
    /// Остановка службы.
    StopService,
    /// Перевод службы в состояние «отключена».
    DisableService,
}

impl Action {
    /// Уровень риска, 1..6.
    pub fn risk(self) -> u8 {
        match self {
            Action::EnableEcoQos | Action::LowerMemoryPriority => 1,
            Action::DelayServiceStart | Action::DisableStartupItem => 2,
            Action::DisableWakeTimer | Action::DisableScheduledTask => 3,
            Action::FreezeProcess => 4,
            Action::StopService => 5,
            Action::DisableService => 6,
        }
    }

    pub fn autonomy(self) -> Autonomy {
        match self {
            // Мгновенно и полностью обратимо, побочных эффектов нет.
            Action::EnableEcoQos | Action::LowerMemoryPriority | Action::DelayServiceStart => {
                Autonomy::Automatic
            }
            Action::DisableStartupItem
            | Action::DisableWakeTimer
            | Action::DisableScheduledTask => Autonomy::WithConfirmation,
            // Заморозка способна вызвать каскадный отказ, остановка
            // и отключение службы ломают то, что от неё зависит.
            Action::FreezeProcess | Action::StopService | Action::DisableService => {
                Autonomy::ManualOnly
            }
        }
    }

    /// Требуется ли точка восстановления системы перед применением.
    ///
    /// По ТЗ (раздел 12.2) — перед действиями уровня 5 и выше.
    pub fn needs_restore_point(self) -> bool {
        self.risk() >= 5
    }

    /// Обратное действие. У каждого действия оно есть по определению:
    /// без обратного рецепта действие в журнал не пишется и не выполняется.
    pub fn undo(self) -> Action {
        match self {
            Action::EnableEcoQos => Action::EnableEcoQos,
            Action::LowerMemoryPriority => Action::LowerMemoryPriority,
            Action::DelayServiceStart => Action::DelayServiceStart,
            Action::DisableStartupItem => Action::DisableStartupItem,
            Action::DisableWakeTimer => Action::DisableWakeTimer,
            Action::DisableScheduledTask => Action::DisableScheduledTask,
            Action::FreezeProcess => Action::FreezeProcess,
            Action::StopService => Action::StopService,
            Action::DisableService => Action::DisableService,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Action::EnableEcoQos => "включить экономичный режим",
            Action::LowerMemoryPriority => "понизить приоритет памяти",
            Action::DelayServiceStart => "перевести службу на отложенный запуск",
            Action::DisableStartupItem => "убрать из автозагрузки",
            Action::DisableWakeTimer => "запретить будить компьютер",
            Action::DisableScheduledTask => "отключить задачу планировщика",
            Action::FreezeProcess => "заморозить процесс",
            Action::StopService => "остановить службу",
            Action::DisableService => "отключить службу",
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Режим автономности, выставленный пользователем.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AutonomyMode {
    /// Bamboo не делает ничего сам, только предлагает.
    #[default]
    Observe,
    /// Автономно применяются уровни 1–2.
    Assist,
}

/// Можно ли применить действие без спроса.
///
/// Обратите внимание: уровни 3 и выше требуют подтверждения **всегда**,
/// независимо от режима. Это не настройка, а свойство действия.
pub fn may_apply_autonomously(action: Action, mode: AutonomyMode, learning: bool) -> bool {
    // Период обучения: первые 14 суток Bamboo только наблюдает.
    if learning {
        return false;
    }
    if mode != AutonomyMode::Assist {
        return false;
    }
    action.autonomy() == Autonomy::Automatic && action.risk() <= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Action; 9] = [
        Action::EnableEcoQos,
        Action::LowerMemoryPriority,
        Action::DelayServiceStart,
        Action::DisableStartupItem,
        Action::DisableWakeTimer,
        Action::DisableScheduledTask,
        Action::FreezeProcess,
        Action::StopService,
        Action::DisableService,
    ];

    #[test]
    fn risk_levels_match_the_spec() {
        assert_eq!(Action::EnableEcoQos.risk(), 1);
        assert_eq!(Action::DisableStartupItem.risk(), 2);
        assert_eq!(Action::DisableScheduledTask.risk(), 3);
        assert_eq!(Action::FreezeProcess.risk(), 4);
        assert_eq!(Action::StopService.risk(), 5);
        assert_eq!(Action::DisableService.risk(), 6);
    }

    #[test]
    fn only_the_two_lowest_levels_can_be_automatic() {
        for action in ALL {
            if action.autonomy() == Autonomy::Automatic {
                assert!(
                    action.risk() <= 2,
                    "{action} уровня {} не может быть автономным",
                    action.risk()
                );
            }
        }
    }

    #[test]
    fn dangerous_actions_are_manual_only() {
        for action in ALL {
            if action.risk() >= 4 {
                assert_eq!(action.autonomy(), Autonomy::ManualOnly, "{action}");
            }
        }
    }

    #[test]
    fn restore_point_is_required_from_level_five() {
        assert!(!Action::FreezeProcess.needs_restore_point());
        assert!(Action::StopService.needs_restore_point());
        assert!(Action::DisableService.needs_restore_point());
    }

    #[test]
    fn observing_mode_never_acts_on_its_own() {
        for action in ALL {
            assert!(!may_apply_autonomously(
                action,
                AutonomyMode::Observe,
                false
            ));
        }
    }

    #[test]
    fn during_learning_nothing_is_automatic() {
        // Первые 14 суток базовая линия ещё не сформирована,
        // и решать по ней нельзя.
        for action in ALL {
            assert!(!may_apply_autonomously(action, AutonomyMode::Assist, true));
        }
    }

    #[test]
    fn assist_mode_handles_only_levels_one_and_two() {
        assert!(may_apply_autonomously(
            Action::EnableEcoQos,
            AutonomyMode::Assist,
            false
        ));
        assert!(may_apply_autonomously(
            Action::DelayServiceStart,
            AutonomyMode::Assist,
            false
        ));

        // Отключение автозагрузки — тоже уровень 2, но требует
        // подтверждения: пользователь может не понять, куда делась иконка.
        assert!(!may_apply_autonomously(
            Action::DisableStartupItem,
            AutonomyMode::Assist,
            false
        ));
        assert!(!may_apply_autonomously(
            Action::FreezeProcess,
            AutonomyMode::Assist,
            false
        ));
    }

    #[test]
    fn every_action_has_an_inverse() {
        for action in ALL {
            // Не тождество: важно, что обратный рецепт вообще определён
            // для каждого действия без исключений.
            let _ = action.undo();
        }
    }
}
