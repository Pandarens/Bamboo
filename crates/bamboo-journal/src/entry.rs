//! Запись журнала (ТЗ, раздел 12.1).

use bamboo_policy::Action;

/// Кто инициировал действие.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Actor {
    Manual,
    Auto,
}

impl Actor {
    /// Как записывается в базу. Не переводится никогда — см. довод
    /// у `Action::storage_key`.
    pub fn storage_key(self) -> &'static str {
        match self {
            Actor::Manual => "manual",
            Actor::Auto => "auto",
        }
    }

    /// Восстанавливает по ключу. Принимает и старые русские значения.
    pub fn from_storage_key(key: &str) -> Option<Actor> {
        [Actor::Manual, Actor::Auto]
            .into_iter()
            .find(|actor| actor.storage_key() == key || actor.as_str() == key)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Actor::Manual => "вручную",
            Actor::Auto => "автоматически",
        }
    }
}

/// Состояние записи.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Записана, но применение ещё не подтверждено.
    ///
    /// Ключевое состояние: запись создаётся **до** действия. Если процесс
    /// упадёт между записью и подтверждением, следующий запуск найдёт
    /// незакрытую запись и проверит фактическое состояние системы.
    Pending,
    Applied,
    Reverted,
    /// Окно наблюдения закрылось, действие осталось в силе.
    Expired,
    Failed,
}

impl Status {
    /// Как состояние записывается в базу.
    ///
    /// Ключ, а не название. Пока в колонку уходило русское «применено»,
    /// перевод интерфейса на другой язык молча сломал бы чтение всех
    /// существующих журналов: запись осталась бы, а разобрать её было
    /// бы нечем.
    pub fn storage_key(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Applied => "applied",
            Status::Reverted => "reverted",
            Status::Expired => "expired",
            Status::Failed => "failed",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "не подтверждено",
            Status::Applied => "применено",
            Status::Reverted => "откачено",
            Status::Expired => "прижилось",
            Status::Failed => "не удалось",
        }
    }

    /// Восстанавливает состояние из базы.
    ///
    /// Принимает и ключи, и старые русские названия: журналы, записанные
    /// до разделения, обязаны читаться дальше. Выбрасывать чужую историю
    /// ради своего удобства нельзя.
    pub fn parse(text: &str) -> Option<Status> {
        Some(match text {
            "pending" | "не подтверждено" => Status::Pending,
            "applied" | "применено" => Status::Applied,
            "reverted" | "откачено" => Status::Reverted,
            "expired" | "прижилось" => Status::Expired,
            "failed" | "не удалось" => Status::Failed,
            _ => return None,
        })
    }

    /// Действует ли изменение прямо сейчас.
    pub fn is_active(self) -> bool {
        matches!(self, Status::Applied | Status::Expired)
    }
}

/// К чему применено действие.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    pub app_key: String,
    pub pid: Option<u32>,
    pub service_name: Option<String>,
    pub task_path: Option<String>,
}

impl Target {
    pub fn app(app_key: &str) -> Self {
        Target {
            app_key: app_key.to_string(),
            ..Default::default()
        }
    }

    pub fn describe(&self) -> String {
        if let Some(service) = &self.service_name {
            return format!("служба {service}");
        }
        if let Some(task) = &self.task_path {
            return format!("задача {task}");
        }
        match self.pid {
            Some(pid) => format!("{} (PID {pid})", self.app_key),
            None => self.app_key.clone(),
        }
    }
}

/// Запись журнала.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: i64,
    pub at_unix_ms: i64,
    pub actor: Actor,
    pub profile: String,
    pub target: Target,
    pub action: Action,
    /// Полное состояние до изменения — достаточное для восстановления,
    /// даже если штатный откат не сработает.
    pub prior_state: String,
    pub observation: Option<String>,
    /// Докуда открыто окно наблюдения сторожевого таймера.
    pub watchdog_deadline_ms: i64,
    pub status: Status,
    /// Причина отката, если он был.
    pub revert_reason: Option<String>,
}

impl Entry {
    /// Открыто ли ещё окно наблюдения.
    pub fn watchdog_active(&self, now_ms: i64) -> bool {
        self.status == Status::Applied && now_ms < self.watchdog_deadline_ms
    }

    /// Пора ли закрывать окно наблюдения.
    pub fn watchdog_expired(&self, now_ms: i64) -> bool {
        self.status == Status::Applied && now_ms >= self.watchdog_deadline_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(status: Status, deadline: i64) -> Entry {
        Entry {
            id: 1,
            at_unix_ms: 0,
            actor: Actor::Auto,
            profile: "Обычный".into(),
            target: Target::app("slack"),
            action: Action::EnableEcoQos,
            prior_state: "{}".into(),
            observation: None,
            watchdog_deadline_ms: deadline,
            status,
            revert_reason: None,
        }
    }

    #[test]
    fn status_survives_a_round_trip() {
        for status in [
            Status::Pending,
            Status::Applied,
            Status::Reverted,
            Status::Expired,
            Status::Failed,
        ] {
            assert_eq!(Status::parse(status.as_str()), Some(status));
        }
    }

    #[test]
    fn only_applied_and_expired_changes_are_in_force() {
        assert!(Status::Applied.is_active());
        assert!(Status::Expired.is_active());
        assert!(!Status::Reverted.is_active());
        assert!(!Status::Pending.is_active());
        assert!(!Status::Failed.is_active());
    }

    #[test]
    fn the_watchdog_window_opens_and_closes() {
        let applied = entry(Status::Applied, 48 * 3_600_000);
        assert!(applied.watchdog_active(1000));
        assert!(!applied.watchdog_expired(1000));

        assert!(!applied.watchdog_active(50 * 3_600_000));
        assert!(applied.watchdog_expired(50 * 3_600_000));
    }

    #[test]
    fn a_reverted_action_is_not_watched_any_more() {
        let reverted = entry(Status::Reverted, 48 * 3_600_000);
        assert!(!reverted.watchdog_active(1000));
        assert!(!reverted.watchdog_expired(50 * 3_600_000));
    }

    #[test]
    fn a_target_describes_itself_readably() {
        assert_eq!(Target::app("slack").describe(), "slack");

        let service = Target {
            service_name: Some("SysMain".into()),
            ..Target::app("sysmain")
        };
        assert_eq!(service.describe(), "служба SysMain");

        let process = Target {
            pid: Some(4242),
            ..Target::app("slack.exe")
        };
        assert_eq!(process.describe(), "slack.exe (PID 4242)");
    }
}

#[cfg(test)]
mod storage_key_tests {
    use super::*;

    #[test]
    fn old_journals_written_in_russian_still_read() {
        // Главное требование этой правки. Журналы, записанные до
        // разделения хранения и показа, обязаны читаться дальше:
        // выбрасывать чужую историю ради своего удобства нельзя.
        assert_eq!(Status::parse("применено"), Some(Status::Applied));
        assert_eq!(Status::parse("не подтверждено"), Some(Status::Pending));
        assert_eq!(Actor::from_storage_key("вручную"), Some(Actor::Manual));
    }

    #[test]
    fn new_journals_are_written_with_stable_keys() {
        // Ключ уходит в колонку базы и не меняется никогда — иначе
        // перевод интерфейса сломал бы чтение записанного вчера.
        assert_eq!(Status::Applied.storage_key(), "applied");
        assert_eq!(Actor::Auto.storage_key(), "auto");

        for status in [
            Status::Pending,
            Status::Applied,
            Status::Reverted,
            Status::Expired,
            Status::Failed,
        ] {
            assert!(status.storage_key().is_ascii(), "{status:?}");
            assert_eq!(Status::parse(status.storage_key()), Some(status));
        }
    }

    #[test]
    fn a_key_is_not_the_same_as_a_name() {
        // Если бы они совпадали, разделение было бы бессмысленным:
        // название должно быть свободно переводиться.
        for status in [Status::Applied, Status::Reverted, Status::Failed] {
            assert_ne!(status.storage_key(), status.as_str());
        }
    }

    #[test]
    fn nonsense_in_the_column_is_not_silently_accepted() {
        assert_eq!(Status::parse("что-то не то"), None);
        assert_eq!(Actor::from_storage_key(""), None);
    }
}
