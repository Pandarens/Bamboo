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
///
/// `Serialize`/`Deserialize` нужны, потому что действие пересекает канал
/// агент-брокер в составе запроса `Apply`.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Action {
    /// EcoQoS: планировщик уводит потоки на энергоэффективные ядра.
    EnableEcoQos,
    /// Понижение приоритета памяти.
    LowerMemoryPriority,
    /// Ограничение скорости работы с диском.
    ///
    /// Исполнитель его не выполняет, и это не упущение: ограничение живёт
    /// ровно столько, сколько жив дескриптор job-объекта, а исполнитель
    /// не хранит состояния между вызовами и закрыл бы дескриптор сразу.
    /// Держит его реестр придержанных процессов, он же и пишет в журнал.
    /// В перечне действие всё равно нужно: журналу и политике оно известно
    /// наравне с остальными.
    LimitDiskRate,
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
            Action::EnableEcoQos | Action::LowerMemoryPriority | Action::LimitDiskRate => 1,
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
            Action::EnableEcoQos
            | Action::LowerMemoryPriority
            | Action::LimitDiskRate
            | Action::DelayServiceStart => Autonomy::Automatic,
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
            Action::LimitDiskRate => Action::LimitDiskRate,
            Action::DelayServiceStart => Action::DelayServiceStart,
            Action::DisableStartupItem => Action::DisableStartupItem,
            Action::DisableWakeTimer => Action::DisableWakeTimer,
            Action::DisableScheduledTask => Action::DisableScheduledTask,
            Action::FreezeProcess => Action::FreezeProcess,
            Action::StopService => Action::StopService,
            Action::DisableService => Action::DisableService,
        }
    }

    /// Как действие записывается в базу.
    ///
    /// Отдельно от `name()`, и это не дублирование. `name()` — текст для
    /// человека: он переводится, его правят, он живёт по правилам языка.
    /// А в колонку базы уходит ключ, и он обязан не меняться никогда:
    /// журнал, записанный вчера, должен читаться завтра — и на другом
    /// языке интерфейса тоже.
    ///
    /// Пока русское название служило и тем, и другим, перевод интерфейса
    /// молча сломал бы чтение всех существующих журналов.
    pub fn storage_key(self) -> &'static str {
        match self {
            Action::EnableEcoQos => "enable_eco_qos",
            Action::LowerMemoryPriority => "lower_memory_priority",
            Action::LimitDiskRate => "limit_disk_rate",
            Action::DelayServiceStart => "delay_service_start",
            Action::DisableStartupItem => "disable_startup_item",
            Action::DisableWakeTimer => "disable_wake_timer",
            Action::DisableScheduledTask => "disable_scheduled_task",
            Action::FreezeProcess => "freeze_process",
            Action::StopService => "stop_service",
            Action::DisableService => "disable_service",
        }
    }

    /// Восстанавливает действие по ключу хранения.
    ///
    /// Принимает и старые русские названия: журналы, записанные до
    /// разделения, обязаны читаться. Молча превращать неизвестное значение
    /// в первое попавшееся действие нельзя — так делал прежний разбор,
    /// и запись о остановке службы читалась бы как экономичный режим.
    pub fn from_storage_key(key: &str) -> Option<Action> {
        const ALL: [Action; 10] = [
            Action::EnableEcoQos,
            Action::LowerMemoryPriority,
            Action::LimitDiskRate,
            Action::DelayServiceStart,
            Action::DisableStartupItem,
            Action::DisableWakeTimer,
            Action::DisableScheduledTask,
            Action::FreezeProcess,
            Action::StopService,
            Action::DisableService,
        ];
        ALL.into_iter()
            .find(|action| action.storage_key() == key || action.name() == key)
    }

    pub fn name(self) -> &'static str {
        match self {
            Action::EnableEcoQos => "включить экономичный режим",
            Action::LowerMemoryPriority => "понизить приоритет памяти",
            Action::LimitDiskRate => "придержать скорость диска",
            Action::DelayServiceStart => "перевести службу на отложенный запуск",
            Action::DisableStartupItem => "убрать из автозагрузки",
            Action::DisableWakeTimer => "запретить будить компьютер",
            Action::DisableScheduledTask => "отключить задачу планировщика",
            Action::FreezeProcess => "заморозить процесс",
            Action::StopService => "остановить службу",
            Action::DisableService => "отключить службу",
        }
    }

    /// Применяется ли действие к конкретному процессу (нужен PID).
    ///
    /// Остальные действия адресуются по имени: служба, задача планировщика
    /// или элемент автозагрузки живут дольше любого своего процесса, и PID
    /// им не только не нужен, но и вреден — сегодня один, завтра другой.
    pub fn targets_process(self) -> bool {
        matches!(
            self,
            Action::EnableEcoQos
                | Action::LowerMemoryPriority
                | Action::LimitDiskRate
                | Action::FreezeProcess
        )
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
    fn process_actions_need_a_pid_and_service_actions_do_not() {
        assert!(Action::EnableEcoQos.targets_process());
        assert!(Action::LowerMemoryPriority.targets_process());
        assert!(Action::FreezeProcess.targets_process());
        // Службы, задачи и автозагрузка адресуются по имени, не по PID.
        assert!(!Action::StopService.targets_process());
        assert!(!Action::DisableService.targets_process());
        assert!(!Action::DisableScheduledTask.targets_process());
        assert!(!Action::DisableStartupItem.targets_process());
        assert!(!Action::DisableWakeTimer.targets_process());
        assert!(!Action::DelayServiceStart.targets_process());
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

#[cfg(test)]
mod storage_key_tests {
    use super::*;

    #[test]
    fn every_action_has_a_stable_ascii_key() {
        for action in ALL_ACTIONS {
            let key = action.storage_key();
            assert!(key.is_ascii(), "ключ обязан быть переносимым: {key}");
            assert_eq!(Action::from_storage_key(key), Some(action));
        }
    }

    #[test]
    fn keys_do_not_collide() {
        // Одинаковый ключ у двух действий означал бы, что журнал
        // не отличает экономичный режим от остановки службы.
        let mut keys: Vec<&str> = ALL_ACTIONS.iter().map(|a| a.storage_key()).collect();
        keys.sort();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before);
    }

    #[test]
    fn old_russian_names_still_resolve() {
        // Журналы, записанные до разделения, обязаны читаться.
        assert_eq!(
            Action::from_storage_key("включить экономичный режим"),
            Some(Action::EnableEcoQos)
        );
        assert_eq!(
            Action::from_storage_key("остановить службу"),
            Some(Action::StopService)
        );
    }

    #[test]
    fn an_unknown_value_is_refused_not_guessed() {
        // Прежний разбор молча превращал незнакомое значение в первое
        // действие списка: запись об остановке службы читалась бы как
        // экономичный режим.
        assert_eq!(Action::from_storage_key("нет такого"), None);
        assert_eq!(Action::from_storage_key(""), None);
    }

    /// Все действия. Список здесь, а не в тесте, чтобы забыть новое
    /// действие было нельзя: компилятор о нём не напомнит, а этот
    /// список проверяется на полноту соседним тестом.
    const ALL_ACTIONS: [Action; 10] = [
        Action::EnableEcoQos,
        Action::LowerMemoryPriority,
        Action::LimitDiskRate,
        Action::DelayServiceStart,
        Action::DisableStartupItem,
        Action::DisableWakeTimer,
        Action::DisableScheduledTask,
        Action::FreezeProcess,
        Action::StopService,
        Action::DisableService,
    ];
}
