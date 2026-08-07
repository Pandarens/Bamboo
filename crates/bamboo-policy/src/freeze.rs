//! Проверки перед заморозкой (ТЗ, раздел 11.2).
//!
//! Заморозка — единственное действие Bamboo, способное вызвать каскадный
//! отказ. Замороженный процесс продолжает удерживать мьютексы и критические
//! секции: если он держал разделяемый ресурс, зависнут все его потребители.
//!
//! Поэтому здесь всё устроено наоборот по сравнению с остальным кодом:
//! отказ — состояние по умолчанию, а разрешение выдаётся, только если
//! ни одна проверка не сработала.

use crate::whitelist::{immutable_reason, ProcessFacts};

/// Что известно о процессе перед заморозкой.
#[derive(Clone, Copy, Debug, Default)]
pub struct FreezeFacts<'a> {
    pub process: ProcessFacts<'a>,
    /// Соединения в состоянии ESTABLISHED.
    pub established_connections: u32,
    /// Открытые дескрипторы на пользовательские файлы вне временных папок.
    pub open_user_files: u32,
    /// Именованные объекты синхронизации, разделяемые с другими процессами.
    pub shared_sync_objects: bool,
    /// Зарегистрированные COM-серверы с активными клиентами.
    pub com_clients: u32,
    /// Игра или процесс с античит-модулем.
    pub game_or_anticheat: bool,
    /// VPN-клиент, средство шифрования, EDR.
    pub security_related: bool,
    /// Живые дочерние процессы.
    pub child_processes: u32,
    /// Процесс удерживает систему от сна.
    pub holds_power_request: bool,
}

/// Итог проверки.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FreezeVerdict {
    Allowed,
    Refused(&'static str),
}

impl FreezeVerdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, FreezeVerdict::Allowed)
    }

    pub fn reason(&self) -> Option<&'static str> {
        match self {
            FreezeVerdict::Allowed => None,
            FreezeVerdict::Refused(reason) => Some(reason),
        }
    }
}

/// Решает, можно ли замораживать процесс.
///
/// Проверки идут от самых серьёзных последствий к менее серьёзным,
/// чтобы в отказе называлась главная причина.
pub fn may_freeze(facts: &FreezeFacts<'_>) -> FreezeVerdict {
    if let Some(reason) = immutable_reason(&facts.process) {
        return FreezeVerdict::Refused(reason);
    }

    // Античиты интерпретируют внешнее вмешательство в процесс как читерство.
    // Цена ошибки — блокировка аккаунта пользователя, а не подтормаживание.
    if facts.game_or_anticheat {
        return FreezeVerdict::Refused(
            "игры и античит-модули: вмешательство может привести к блокировке аккаунта",
        );
    }

    if facts.security_related {
        return FreezeVerdict::Refused(
            "антивирус, VPN или средство шифрования: заморозка запрещена абсолютно",
        );
    }

    // Замороженный процесс продолжает держать мьютексы — зависнут все,
    // кто их ждёт.
    if facts.shared_sync_objects {
        return FreezeVerdict::Refused(
            "процесс разделяет объекты синхронизации с другими: заморозка приведёт \
             к взаимоблокировке",
        );
    }
    if facts.com_clients > 0 {
        return FreezeVerdict::Refused(
            "процесс обслуживает активных COM-клиентов: они зависнут вместе с ним",
        );
    }

    // Соединение оборвётся по таймауту, а приложение проснётся
    // в неконсистентном состоянии.
    if facts.established_connections > 0 {
        return FreezeVerdict::Refused("у процесса есть активные сетевые соединения");
    }

    if facts.open_user_files > 0 {
        return FreezeVerdict::Refused(
            "процесс держит открытыми пользовательские файлы: есть риск потери данных",
        );
    }

    if facts.child_processes > 0 {
        return FreezeVerdict::Refused("у процесса есть живые дочерние процессы");
    }

    if facts.holds_power_request {
        return FreezeVerdict::Refused(
            "процесс удерживает систему от сна: скорее всего, он занят работой",
        );
    }

    FreezeVerdict::Allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle_app() -> FreezeFacts<'static> {
        FreezeFacts {
            process: ProcessFacts {
                image_name: "заброшенное-приложение.exe",
                session_id: 1,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_truly_idle_application_may_be_frozen() {
        assert!(may_freeze(&idle_app()).is_allowed());
    }

    #[test]
    fn the_system_core_is_never_frozen() {
        let mut facts = idle_app();
        facts.process.image_name = "lsass.exe";
        assert!(!may_freeze(&facts).is_allowed());
    }

    #[test]
    fn games_are_refused_because_of_anticheat() {
        let mut facts = idle_app();
        facts.game_or_anticheat = true;

        let verdict = may_freeze(&facts);
        assert!(!verdict.is_allowed());
        assert!(verdict.reason().unwrap().contains("блокировке аккаунта"));
    }

    #[test]
    fn security_software_is_refused_absolutely() {
        let mut facts = idle_app();
        facts.security_related = true;
        assert!(!may_freeze(&facts).is_allowed());
    }

    #[test]
    fn shared_locks_mean_a_deadlock_risk() {
        let mut facts = idle_app();
        facts.shared_sync_objects = true;

        let verdict = may_freeze(&facts);
        assert!(verdict.reason().unwrap().contains("взаимоблокировке"));
    }

    #[test]
    fn an_open_connection_stops_the_freeze() {
        let mut facts = idle_app();
        facts.established_connections = 1;
        assert!(!may_freeze(&facts).is_allowed());
    }

    #[test]
    fn open_user_files_mean_possible_data_loss() {
        let mut facts = idle_app();
        facts.open_user_files = 3;
        assert!(may_freeze(&facts)
            .reason()
            .unwrap()
            .contains("потери данных"));
    }

    #[test]
    fn com_clients_would_hang_too() {
        let mut facts = idle_app();
        facts.com_clients = 2;
        assert!(!may_freeze(&facts).is_allowed());
    }

    #[test]
    fn live_children_stop_the_freeze() {
        let mut facts = idle_app();
        facts.child_processes = 1;
        assert!(!may_freeze(&facts).is_allowed());
    }

    #[test]
    fn a_process_holding_the_system_awake_is_busy() {
        let mut facts = idle_app();
        facts.holds_power_request = true;
        assert!(!may_freeze(&facts).is_allowed());
    }

    #[test]
    fn the_most_serious_reason_is_named_first() {
        // Сошлось всё сразу: назвать надо самое серьёзное последствие.
        let mut facts = idle_app();
        facts.game_or_anticheat = true;
        facts.established_connections = 5;
        facts.open_user_files = 2;

        assert!(may_freeze(&facts).reason().unwrap().contains("аккаунта"));
    }

    #[test]
    fn refusal_is_the_default_when_anything_is_uncertain() {
        // Каждый отдельный признак сам по себе достаточен для отказа.
        let checks: [fn(&mut FreezeFacts<'static>); 7] = [
            |f| f.game_or_anticheat = true,
            |f| f.security_related = true,
            |f| f.shared_sync_objects = true,
            |f| f.com_clients = 1,
            |f| f.established_connections = 1,
            |f| f.open_user_files = 1,
            |f| f.child_processes = 1,
        ];

        for apply in checks {
            let mut facts = idle_app();
            apply(&mut facts);
            assert!(!may_freeze(&facts).is_allowed());
        }
    }
}
