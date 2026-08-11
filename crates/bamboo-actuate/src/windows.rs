//! Исполнение действий на живой системе.

use bamboo_journal::Target;
use bamboo_policy::Action;
use bamboo_sys::control::{self, MemoryPriority};

use crate::executor::{Backend, SafetyNet};
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
            Action::DelayServiceStart => {
                let current = bamboo_sys::service_start(&target.app_key)
                    .map_err(|error| error.to_string())?;
                Ok(PriorState::new()
                    .with("service_start_type", current.start_type)
                    .with("service_delayed", yes_no(current.delayed)))
            }
            Action::StopService | Action::DisableService => {
                let current = bamboo_sys::service_start(&target.app_key)
                    .map_err(|error| error.to_string())?;
                // Триггер запоминаем вместе с типом запуска. Без него откат
                // вернул бы «ручной запуск» и выглядел бы успешным, хотя
                // служба, просыпавшаяся сама, просыпаться перестала бы.
                let trigger = bamboo_sys::has_start_trigger(&target.app_key).unwrap_or(false);
                Ok(PriorState::new()
                    .with("service_start_type", current.start_type)
                    .with("service_delayed", yes_no(current.delayed))
                    .with("service_trigger", yes_no(trigger)))
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
            Action::DelayServiceStart => {
                let current = bamboo_sys::service_start(&target.app_key)
                    .map_err(|error| error.to_string())?;
                // Триггерные и отключённые службы не трогаем: перевод на
                // отложенный старт для них бессмыслен или вреден (ТЗ 5.5).
                if current.is_demand_start() {
                    return Err(
                        "служба стартует по триггеру, отложенный старт ей не нужен".to_string()
                    );
                }
                if current.is_disabled() {
                    return Err("служба отключена, менять тип запуска не нужно".to_string());
                }
                bamboo_sys::set_service_start(
                    &target.app_key,
                    bamboo_sys::ServiceStart::delayed_auto(),
                )
                .map_err(|error| error.to_string())
            }
            Action::StopService => bamboo_sys::services::stop_service(&target.app_key)
                .map_err(|error| error.to_string()),
            Action::DisableService => {
                // Триггерную службу не отключаем совсем. Внешне она такая же
                // «ручная», как обычная, но просыпается сама — по появлению
                // устройства, открытию порта, событию. Отключив её, человек
                // получит отказ чего-то постороннего и связь с причиной
                // не найдёт никогда. Проверено, что угадать это по имени
                // нельзя: триггер есть даже у планировщика заданий.
                if bamboo_sys::has_start_trigger(&target.app_key).unwrap_or(false) {
                    return Err(
                        "служба просыпается по триггеру: отключение сломает то,                          ради чего она есть, и связь с причиной найти будет нечем"
                            .to_string(),
                    );
                }
                bamboo_sys::set_service_start(&target.app_key, bamboo_sys::ServiceStart::disabled())
                    .map_err(|error| error.to_string())
            }
            other => Err(format!("{} ещё не реализовано", other.name())),
        }
    }

    fn revert(&self, action: Action, target: &Target, prior: &PriorState) -> Result<(), String> {
        match action {
            Action::EnableEcoQos => {
                let pid = Self::pid(target)?;
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
                let pid = Self::pid(target)?;
                let previous = prior
                    .get_u32("memory_priority")
                    .map(MemoryPriority)
                    .filter(|priority| priority.is_valid())
                    // Записи о прошлом состоянии нет — возвращаем обычный
                    // приоритет: он заведомо не хуже того, что мы выставили.
                    .unwrap_or(MemoryPriority::NORMAL);
                control::set_memory_priority(pid, previous).map_err(|error| error.to_string())
            }

            Action::DelayServiceStart => {
                // Здесь угадывать нельзя: выставить не тот тип запуска значит
                // либо оставить службу отложенной, либо, того хуже, включить
                // отключённую. Нет записи о прошлом — отказываемся от отката.
                let start_type = prior.get_u32("service_start_type").ok_or_else(|| {
                    "в журнале нет прежнего типа запуска службы, откат невозможен".to_string()
                })?;
                let delayed = prior.get_bool("service_delayed").unwrap_or(false);
                bamboo_sys::set_service_start(
                    &target.app_key,
                    bamboo_sys::ServiceStart {
                        start_type,
                        delayed,
                    },
                )
                .map_err(|error| error.to_string())
            }

            Action::StopService => {
                // Запустить обратно. Тип запуска не трогаем: остановка его
                // не меняла, и «восстановить» его значило бы изменить то,
                // чего мы не касались.
                bamboo_sys::services::start_service(&target.app_key)
                    .map_err(|error| error.to_string())
            }

            Action::DisableService => {
                // Тот же довод, что и у отложенного старта: угадывать
                // прежний тип запуска нельзя. Без записи откат невозможен,
                // и сказать об этом честнее, чем выставить «ручной»
                // и объявить дело сделанным.
                let start_type = prior.get_u32("service_start_type").ok_or_else(|| {
                    "в журнале нет прежнего типа запуска службы, откат невозможен".to_string()
                })?;
                let delayed = prior.get_bool("service_delayed").unwrap_or(false);
                bamboo_sys::set_service_start(
                    &target.app_key,
                    bamboo_sys::ServiceStart::from_parts(start_type, delayed),
                )
                .map_err(|error| error.to_string())
            }

            other => Err(format!("{} ещё не реализовано", other.name())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Тесты приоритета памяти меняют его у своего же процесса. Параллельный
    /// запуск нескольких таких тестов гонялся бы за одно значение, поэтому
    /// сериализуем их между собой.
    static MEMORY_PRIORITY_LOCK: Mutex<()> = Mutex::new(());

    fn me() -> Target {
        Target {
            app_key: "bamboo-cli".into(),
            pid: Some(std::process::id()),
            ..Default::default()
        }
    }

    #[test]
    fn capturing_a_service_start_state_works_without_admin() {
        // Снять состояние «до» для действия над службой можно и без прав:
        // это запрос конфигурации. Планировщик есть на любой Windows.
        let backend = SystemBackend;
        let target = Target {
            app_key: "Schedule".into(),
            pid: None,
            ..Default::default()
        };
        let prior = backend
            .capture(Action::DelayServiceStart, &target)
            .expect("состояние службы не снялось");
        assert!(
            prior.get_u32("service_start_type").is_some(),
            "в состоянии нет типа запуска службы"
        );
    }

    #[test]
    fn changing_a_service_start_without_admin_fails_cleanly() {
        // Само действие требует прав администратора: без них — понятная
        // ошибка, а не тихий успех. Систему тест не меняет.
        let backend = SystemBackend;
        let target = Target {
            app_key: "Schedule".into(),
            pid: None,
            ..Default::default()
        };
        assert!(backend.apply(Action::DelayServiceStart, &target).is_err());
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
        let _guard = MEMORY_PRIORITY_LOCK.lock().unwrap();
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
        // Список нарочно перечислен здесь целиком: он и есть то, чего
        // исполнитель ещё не умеет. Реализуете действие — тест упадёт
        // и напомнит убрать его отсюда. Прежняя редакция сторожила так
        // остановку службы и честно упала, когда та появилась.
        //
        // Придержание диска здесь особый случай: оно идёт мимо
        // исполнителя намеренно — ограничение живёт ровно столько,
        // сколько жив дескриптор job-объекта.
        let backend = SystemBackend;
        for action in [
            Action::DisableStartupItem,
            Action::DisableWakeTimer,
            Action::DisableScheduledTask,
            Action::FreezeProcess,
            Action::LimitDiskRate,
        ] {
            let error = backend
                .apply(action, &me())
                .expect_err("нереализованное действие обязано отказать");
            assert!(
                error.contains("ещё не реализовано"),
                "{}: {error}",
                action.name()
            );
        }
    }

    #[test]
    fn a_corrupted_prior_state_falls_back_to_something_safe() {
        let _guard = MEMORY_PRIORITY_LOCK.lock().unwrap();
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

/// Страховка через точку восстановления Windows.
///
/// Отличие от наивной реализации в одном, но решающем: успехом считается
/// не удавшийся вызов, а появившаяся точка. Windows придерживает создание —
/// если свежая точка уже есть, она молча пропускает новую. Программа,
/// считающая свой вызов успехом, отчитывается о защите, которой нет,
/// и это ровно плацебо из раздела 11.5 ТЗ.
pub struct RestorePointNet;

impl SafetyNet for RestorePointNet {
    fn prepare(&self, description: &str) -> (String, bool) {
        match bamboo_sys::create_restore_point(description) {
            Ok(outcome) => (outcome.explain().to_string(), outcome.is_protected()),
            Err(error) => (
                format!(
                    "Точку восстановления создать не удалось: {error}. Действие                      отменено: рискованные изменения без пути назад Bamboo не делает."
                ),
                false,
            ),
        }
    }
}

#[cfg(test)]
mod safety_net_tests {
    use super::*;

    #[test]
    fn a_refusal_explains_itself_and_cancels_the_action() {
        // Проверяем форму отказа, а не саму Windows: создание точки требует
        // прав администратора, и в тестах их может не быть. Важно, что при
        // любом исходе человек получает объяснение, а не пустую строку.
        let (note, protected) = RestorePointNet.prepare("Bamboo: проверка");
        assert!(!note.is_empty(), "молчаливый исход недопустим");
        if !protected {
            assert!(
                note.contains("отменено") || note.contains("выключена"),
                "отказ обязан объяснить причину: {note}"
            );
        }
    }
}

#[cfg(test)]
mod service_action_tests {
    use super::*;

    fn target(name: &str) -> Target {
        Target {
            app_key: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn a_trigger_service_is_never_disabled() {
        // Главная защита этих двух действий. Триггерная служба внешне
        // такая же «ручная», как обычная, но просыпается сама — по
        // появлению устройства, открытию порта, событию. Отключив её,
        // человек получит отказ чего-то постороннего и связь с причиной
        // не найдёт никогда.
        //
        // Планировщик заданий здесь не случайно: я считал его обычной
        // службой и ошибся — у него есть триггер по событию RPC. Значит
        // угадывать триггерность по имени нельзя, и проверка обязана
        // быть в коде, а не в голове.
        if bamboo_sys::has_start_trigger("Schedule").unwrap_or(false) {
            let error = SystemBackend
                .apply(Action::DisableService, &target("Schedule"))
                .expect_err("триггерную службу отключать нельзя");
            assert!(error.contains("триггеру"), "{error}");
        }
    }

    #[test]
    fn the_prior_state_of_a_service_records_its_trigger() {
        // Без записи о триггере откат вернул бы тип запуска и выглядел
        // успешным, хотя служба, просыпавшаяся сама, просыпаться
        // перестала бы.
        let Ok(prior) = SystemBackend.capture(Action::DisableService, &target("Dhcp")) else {
            return; // Без прав состояние не снять — это законный отказ.
        };
        let text = prior.to_string();
        assert!(text.contains("service_start_type"), "{text}");
        assert!(text.contains("service_trigger"), "{text}");
    }

    #[test]
    fn a_revert_without_a_recorded_start_type_is_refused() {
        // Угадывать прежний тип запуска нельзя: выставить «ручной» вместо
        // «автоматического» значит тихо оставить службу невключённой,
        // объявив откат сделанным.
        let error = SystemBackend
            .revert(Action::DisableService, &target("Dhcp"), &PriorState::new())
            .expect_err("без записи откат невозможен");
        assert!(error.contains("откат невозможен"), "{error}");
    }
}
