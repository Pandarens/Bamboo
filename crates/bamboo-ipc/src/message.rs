//! Сообщения протокола (ТЗ, раздел 13.2).

use bamboo_policy::Action;

/// Что агент просит у брокера.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Подписка на поток метрик.
    Subscribe {
        stream: Stream,
        interval_ms: u32,
    },
    Unsubscribe {
        stream: Stream,
    },
    /// Разовый снимок.
    QuerySnapshot {
        scope: Scope,
    },
    /// Временной ряд по приложению.
    QueryHistory {
        app_key: String,
        from_unix_ms: i64,
        to_unix_ms: i64,
    },
    /// Наблюдения анализаторов.
    QueryObservations {
        since_unix_ms: i64,
    },
    /// Применение действия.
    ///
    /// Флаг симуляции обязателен и не имеет значения по умолчанию:
    /// вызывающий обязан сказать, чего он хочет.
    Apply {
        action: Action,
        app_key: String,
        pid: Option<u32>,
        dry_run: bool,
    },
    Revert {
        journal_id: i64,
    },
    RevertAll {
        since_unix_ms: i64,
    },
    SetProfile {
        profile: String,
    },
    Whitelist {
        app_key: String,
    },
    /// Сессия расследования ETW.
    Investigate {
        duration_s: u32,
    },
    CaptureDump {
        pid: u32,
    },
    ExportReport {
        from_unix_ms: i64,
        to_unix_ms: i64,
    },
}

/// Поток данных для подписки.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Metrics,
    Observations,
}

/// Область снимка.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Processes,
    System,
    Storage,
}

/// Что брокер отвечает агенту.
#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    Metrics {
        at_unix_ms: i64,
        cpu_busy_percent: u16,
        memory_used_mib: u32,
        memory_total_mib: u32,
        process_count: u32,
    },
    Observation {
        kind: String,
        severity: String,
        summary: String,
    },
    ActionResult {
        journal_id: i64,
        status: String,
    },
    /// Сработал автооткат.
    WatchdogFired {
        journal_id: i64,
        reason: String,
    },
    ProfileChanged {
        profile: String,
        reason: String,
    },
    /// Отказ. Всегда с кодом и объяснением — молча брокер не отказывает.
    Error {
        code: ErrorCode,
        detail: String,
    },
}

/// Код отказа.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    /// Клиент не прошёл проверку подлинности.
    NotAuthorised,
    /// Действие запрещено политикой.
    RefusedByPolicy,
    /// Действие требует подтверждения пользователя.
    NeedsConfirmation,
    /// Такой цели не существует.
    UnknownTarget,
    /// Возможность ещё не реализована.
    NotImplemented,
    /// Внутренняя ошибка.
    Internal,
    /// Сообщение не разобралось.
    Malformed,
}

impl ErrorCode {
    pub fn describe(self) -> &'static str {
        match self {
            ErrorCode::NotAuthorised => "клиент не прошёл проверку",
            ErrorCode::RefusedByPolicy => "запрещено политикой",
            ErrorCode::NeedsConfirmation => "нужно подтверждение пользователя",
            ErrorCode::UnknownTarget => "цель не найдена",
            ErrorCode::NotImplemented => "возможность ещё не реализована",
            ErrorCode::Internal => "внутренняя ошибка брокера",
            ErrorCode::Malformed => "сообщение не разобралось",
        }
    }
}

impl Request {
    /// Меняет ли запрос состояние системы.
    ///
    /// Нужно брокеру для решения, применять ли полную валидацию:
    /// читающие запросы дёшевы и безопасны, изменяющие — нет.
    pub fn mutates(&self) -> bool {
        match self {
            Request::Apply { dry_run, .. } => !dry_run,
            Request::Revert { .. }
            | Request::RevertAll { .. }
            | Request::SetProfile { .. }
            | Request::Whitelist { .. }
            | Request::Investigate { .. }
            | Request::CaptureDump { .. } => true,
            _ => false,
        }
    }

    /// Уровень риска запроса, если он что-то меняет.
    pub fn risk(&self) -> u8 {
        match self {
            Request::Apply {
                action,
                dry_run: false,
                ..
            } => action.risk(),
            _ if self.mutates() => 1,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_requests_do_not_mutate() {
        let reads = [
            Request::QuerySnapshot {
                scope: Scope::Processes,
            },
            Request::QueryObservations { since_unix_ms: 0 },
            Request::Subscribe {
                stream: Stream::Metrics,
                interval_ms: 1000,
            },
            Request::ExportReport {
                from_unix_ms: 0,
                to_unix_ms: 1,
            },
        ];
        for request in reads {
            assert!(!request.mutates(), "{request:?} не должен менять систему");
            assert_eq!(request.risk(), 0);
        }
    }

    #[test]
    fn a_dry_run_does_not_mutate() {
        // На этом флаге держится весь режим симуляции.
        let simulated = Request::Apply {
            action: Action::DisableService,
            app_key: "x".into(),
            pid: None,
            dry_run: true,
        };
        assert!(!simulated.mutates());
        assert_eq!(simulated.risk(), 0);
    }

    #[test]
    fn a_real_apply_carries_the_actions_risk() {
        let dangerous = Request::Apply {
            action: Action::DisableService,
            app_key: "x".into(),
            pid: None,
            dry_run: false,
        };
        assert!(dangerous.mutates());
        assert_eq!(dangerous.risk(), 6);

        let harmless = Request::Apply {
            action: Action::EnableEcoQos,
            app_key: "x".into(),
            pid: Some(1),
            dry_run: false,
        };
        assert_eq!(harmless.risk(), 1);
    }

    #[test]
    fn reverting_is_a_mutating_request() {
        assert!(Request::Revert { journal_id: 1 }.mutates());
        assert!(Request::RevertAll { since_unix_ms: 0 }.mutates());
    }

    #[test]
    fn every_error_code_explains_itself() {
        for code in [
            ErrorCode::NotAuthorised,
            ErrorCode::RefusedByPolicy,
            ErrorCode::NeedsConfirmation,
            ErrorCode::UnknownTarget,
            ErrorCode::NotImplemented,
            ErrorCode::Internal,
            ErrorCode::Malformed,
        ] {
            assert!(!code.describe().is_empty());
        }
    }
}
