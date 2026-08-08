//! Валидация запросов клиента (ТЗ, раздел 3.2).
//!
//! Брокер не доверяет агенту. Каждая команда проходит четыре проверки,
//! и отсутствие любой из них превращает Bamboo в локальную уязвимость
//! повышения привилегий.
//!
//! Логика здесь чистая: на вход факты о клиенте и запрос, на выход решение.
//! Проверять предохранители такого рода надо тестами, а не наблюдением
//! за живой SYSTEM-службой.

use bamboo_ipc::{ErrorCode, Request};

/// Что брокер знает о подключившемся клиенте.
#[derive(Clone, Copy, Debug)]
pub struct ClientFacts {
    /// Образ клиента совпал с нашим (по подписи или по хешу и пути).
    pub image_matches: bool,
    /// Клиент из той же пользовательской сессии.
    pub same_session: bool,
    /// Клиент пришёл по сети, а не локально.
    pub remote: bool,
}

/// Текущие настройки автономности брокера.
#[derive(Clone, Copy, Debug)]
pub struct BrokerPolicy {
    /// Максимальный уровень риска, разрешённый без подтверждения
    /// пользователя. По ТЗ автономный режим — только уровни 1–2.
    pub max_autonomous_risk: u8,
    /// Идёт ли период обучения: в нём действий нет вовсе.
    pub learning: bool,
}

impl Default for BrokerPolicy {
    fn default() -> Self {
        BrokerPolicy {
            max_autonomous_risk: 2,
            learning: false,
        }
    }
}

/// Итог валидации.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Запрос можно исполнять.
    Allow,
    /// Отказ с кодом и объяснением. Брокер не отказывает молча.
    Deny(ErrorCode, String),
}

impl Verdict {
    // Используется тестами и вызывающим кодом; в самом брокере ветвление
    // идёт через match, поэтому под бинарник помечаем допустимым.
    #[allow(dead_code)]
    pub fn is_allowed(&self) -> bool {
        matches!(self, Verdict::Allow)
    }
}

/// Пропускает запрос через все проверки раздела 3.2.
///
/// Порядок продуман: сначала отсекаем чужака, потом уже разбираемся
/// с содержанием запроса. Нет смысла обсуждать уровень риска команды
/// от процесса, который вообще не должен был подключиться.
pub fn validate(client: &ClientFacts, request: &Request, policy: &BrokerPolicy) -> Verdict {
    // 1. Никаких удалённых клиентов. Дублирует PIPE_REJECT_REMOTE_CLIENTS
    // на уровне логики: защита в глубину.
    if client.remote {
        return deny(
            ErrorCode::NotAuthorised,
            "удалённые клиенты не обслуживаются",
        );
    }

    // 2. Образ клиента должен совпадать с нашим.
    if !client.image_matches {
        return deny(
            ErrorCode::NotAuthorised,
            "образ подключившегося процесса не совпал с образом агента",
        );
    }

    // Читающие запросы дальше не проверяем: они дёшевы, безопасны
    // и ничего в системе не меняют.
    if !request.mutates() {
        return Verdict::Allow;
    }

    // 3. Изменяющие команды применимы только к своей сессии.
    if !client.same_session {
        return deny(
            ErrorCode::NotAuthorised,
            "изменять состояние можно только в своей сессии",
        );
    }

    // 4. Уровень риска против настроек автономности.
    if policy.learning {
        return deny(
            ErrorCode::NeedsConfirmation,
            "идёт период обучения, автономные действия недоступны",
        );
    }
    if request.risk() > policy.max_autonomous_risk {
        return deny(
            ErrorCode::NeedsConfirmation,
            format!(
                "действие уровня риска {} требует подтверждения пользователя",
                request.risk()
            ),
        );
    }

    Verdict::Allow
}

fn deny(code: ErrorCode, detail: impl Into<String>) -> Verdict {
    Verdict::Deny(code, detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bamboo_policy::Action;

    fn trusted() -> ClientFacts {
        ClientFacts {
            image_matches: true,
            same_session: true,
            remote: false,
        }
    }

    fn read() -> Request {
        Request::QueryObservations { since_unix_ms: 0 }
    }

    fn eco_qos() -> Request {
        Request::Apply {
            action: Action::EnableEcoQos,
            app_key: "slack".into(),
            pid: Some(100),
            dry_run: false,
        }
    }

    fn stop_service() -> Request {
        Request::Apply {
            action: Action::StopService,
            app_key: "sysmain".into(),
            pid: None,
            dry_run: false,
        }
    }

    #[test]
    fn a_trusted_client_may_read() {
        assert!(validate(&trusted(), &read(), &BrokerPolicy::default()).is_allowed());
    }

    #[test]
    fn a_remote_client_is_refused_outright() {
        let mut client = trusted();
        client.remote = true;
        // Даже чтение: удалённому клиенту тут делать нечего вовсе.
        assert!(!validate(&client, &read(), &BrokerPolicy::default()).is_allowed());
    }

    #[test]
    fn a_foreign_image_cannot_do_anything() {
        let mut client = trusted();
        client.image_matches = false;
        assert!(!validate(&client, &read(), &BrokerPolicy::default()).is_allowed());
        assert!(!validate(&client, &eco_qos(), &BrokerPolicy::default()).is_allowed());
    }

    #[test]
    fn a_low_risk_action_is_allowed_autonomously() {
        assert!(validate(&trusted(), &eco_qos(), &BrokerPolicy::default()).is_allowed());
    }

    #[test]
    fn a_high_risk_action_needs_confirmation() {
        let verdict = validate(&trusted(), &stop_service(), &BrokerPolicy::default());
        match verdict {
            Verdict::Deny(ErrorCode::NeedsConfirmation, _) => {}
            other => panic!("ожидалось требование подтверждения, получили {other:?}"),
        }
    }

    #[test]
    fn changing_another_session_is_refused() {
        let mut client = trusted();
        client.same_session = false;
        // Читать из чужой сессии можно, менять — нет.
        assert!(validate(&client, &read(), &BrokerPolicy::default()).is_allowed());
        assert!(!validate(&client, &eco_qos(), &BrokerPolicy::default()).is_allowed());
    }

    #[test]
    fn during_learning_nothing_is_applied_autonomously() {
        let policy = BrokerPolicy {
            learning: true,
            ..Default::default()
        };
        assert!(!validate(&trusted(), &eco_qos(), &policy).is_allowed());
        // Но читать по-прежнему можно.
        assert!(validate(&trusted(), &read(), &policy).is_allowed());
    }

    #[test]
    fn a_dry_run_of_a_dangerous_action_is_allowed() {
        // Симуляция ничего не меняет, поэтому проходит даже для уровня 6.
        let simulated = Request::Apply {
            action: Action::DisableService,
            app_key: "x".into(),
            pid: None,
            dry_run: true,
        };
        assert!(validate(&trusted(), &simulated, &BrokerPolicy::default()).is_allowed());
    }

    #[test]
    fn every_denial_carries_an_explanation() {
        let denials = [
            validate(
                &ClientFacts {
                    remote: true,
                    ..trusted()
                },
                &read(),
                &BrokerPolicy::default(),
            ),
            validate(&trusted(), &stop_service(), &BrokerPolicy::default()),
        ];
        for verdict in denials {
            if let Verdict::Deny(_, detail) = verdict {
                assert!(!detail.is_empty(), "отказ без объяснения");
            } else {
                panic!("ожидался отказ");
            }
        }
    }

    #[test]
    fn image_check_comes_before_risk_check() {
        // Чужой образ должен отсекаться раньше, чем брокер начнёт
        // рассуждать об уровне риска команды.
        let mut client = trusted();
        client.image_matches = false;
        match validate(&client, &stop_service(), &BrokerPolicy::default()) {
            Verdict::Deny(ErrorCode::NotAuthorised, _) => {}
            other => panic!("образ должен проверяться первым, получили {other:?}"),
        }
    }
}
