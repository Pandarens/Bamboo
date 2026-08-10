//! Хранилище журнала.
//!
//! Отдельный файл от основной базы — так требует ТЗ (раздел 12.2): журнал
//! должен пережить повреждение базы телеметрии. Потерять историю метрик
//! неприятно, потерять возможность откатить изменения — недопустимо.

use std::path::Path;

use bamboo_policy::Action;
use rusqlite::{params, Connection, OptionalExtension};

use crate::entry::{Actor, Entry, Status, Target};
use crate::watchdog::WINDOW_MS;

pub type Result<T> = rusqlite::Result<T>;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS journal (
    id            INTEGER PRIMARY KEY,
    at_ms         INTEGER NOT NULL,
    actor         TEXT NOT NULL,
    profile       TEXT NOT NULL,
    app_key       TEXT NOT NULL,
    pid           INTEGER,
    service_name  TEXT,
    task_path     TEXT,
    action        TEXT NOT NULL,
    prior_state   TEXT NOT NULL,
    observation   TEXT,
    watchdog_ms   INTEGER NOT NULL,
    status        TEXT NOT NULL,
    revert_reason TEXT
);
CREATE INDEX IF NOT EXISTS journal_status ON journal(status);
CREATE INDEX IF NOT EXISTS journal_at ON journal(at_ms);
"#;

/// Всё, что нужно знать о действии до его выполнения.
pub struct NewEntry<'a> {
    pub at_unix_ms: i64,
    pub actor: Actor,
    pub profile: &'a str,
    pub target: &'a Target,
    pub action: Action,
    /// Состояние цели до изменения. Без него действие не выполняется.
    pub prior_state: &'a str,
    /// Наблюдение, из-за которого действие предложено.
    pub observation: Option<&'a str>,
}

pub struct Journal {
    conn: Connection,
}

impl Journal {
    pub fn open(path: impl AsRef<Path>) -> Result<Journal> {
        Self::prepare(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Journal> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Journal> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // Здесь, в отличие от базы телеметрии, FULL: потеря последней
        // транзакции означает применённое действие без записи об откате.
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Journal { conn })
    }

    /// Записывает намерение выполнить действие.
    ///
    /// Вызывается **до** самого действия. Если процесс упадёт между этим
    /// вызовом и подтверждением, запись останется в состоянии «не
    /// подтверждено», и следующий запуск проверит фактическое состояние.
    pub fn begin(&self, entry: &NewEntry<'_>) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO journal
                (at_ms, actor, profile, app_key, pid, service_name, task_path,
                 action, prior_state, observation, watchdog_ms, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                entry.at_unix_ms,
                entry.actor.as_str(),
                entry.profile,
                entry.target.app_key,
                entry.target.pid,
                entry.target.service_name,
                entry.target.task_path,
                entry.action.name(),
                entry.prior_state,
                entry.observation,
                entry.at_unix_ms + WINDOW_MS,
                Status::Pending.as_str(),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Подтверждает, что действие выполнено.
    pub fn confirm(&self, id: i64) -> Result<()> {
        self.set_status(id, Status::Applied, None)
    }

    /// Отмечает, что действие выполнить не удалось.
    /// Отмечает неудачу вместе с причиной.
    ///
    /// Причину храним в том же поле, что и причину отката: для человека это
    /// одно и то же — объяснение, почему запись в таком состоянии. Запись
    /// «не удалось» без причины бесполезна: пользователь видит отказ и не
    /// понимает, прав ему не хватило или процесс успел закрыться.
    pub fn fail(&self, id: i64, reason: &str) -> Result<()> {
        self.set_status(id, Status::Failed, Some(reason))
    }

    /// Отмечает откат.
    pub fn mark_reverted(&self, id: i64, reason: &str) -> Result<()> {
        self.set_status(id, Status::Reverted, Some(reason))
    }

    /// Закрывает окно наблюдения: изменение прижилось.
    pub fn mark_expired(&self, id: i64) -> Result<()> {
        self.set_status(id, Status::Expired, None)
    }

    fn set_status(&self, id: i64, status: Status, reason: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE journal SET status = ?2, revert_reason = COALESCE(?3, revert_reason)
             WHERE id = ?1",
            params![id, status.as_str(), reason],
        )?;
        Ok(())
    }

    pub fn get(&self, id: i64) -> Result<Option<Entry>> {
        self.conn
            .query_row(
                &format!("{SELECT} WHERE id = ?1"),
                params![id],
                Self::read_row,
            )
            .optional()
    }

    /// Записи, оставшиеся без подтверждения после падения.
    ///
    /// При следующем запуске их надо разобрать: проверить фактическое
    /// состояние системы и либо подтвердить, либо откатить.
    pub fn unconfirmed(&self) -> Result<Vec<Entry>> {
        let mut statement = self
            .conn
            .prepare(&format!("{SELECT} WHERE status = ?1 ORDER BY at_ms"))?;
        let rows = statement.query_map(params![Status::Pending.as_str()], Self::read_row)?;
        rows.collect()
    }

    /// Действия, которые сейчас в силе.
    pub fn active(&self) -> Result<Vec<Entry>> {
        let mut statement = self.conn.prepare(&format!(
            "{SELECT} WHERE status IN (?1, ?2) ORDER BY at_ms DESC"
        ))?;
        let rows = statement.query_map(
            params![Status::Applied.as_str(), Status::Expired.as_str()],
            Self::read_row,
        )?;
        rows.collect()
    }

    /// Действия под наблюдением сторожевого таймера.
    pub fn under_watch(&self, now_ms: i64) -> Result<Vec<Entry>> {
        let mut statement = self.conn.prepare(&format!(
            "{SELECT} WHERE status = ?1 AND watchdog_ms > ?2 ORDER BY at_ms"
        ))?;
        let rows =
            statement.query_map(params![Status::Applied.as_str(), now_ms], Self::read_row)?;
        rows.collect()
    }

    /// Записи за период, от свежих к старым.
    pub fn since(&self, from_ms: i64) -> Result<Vec<Entry>> {
        let mut statement = self
            .conn
            .prepare(&format!("{SELECT} WHERE at_ms >= ?1 ORDER BY at_ms DESC"))?;
        let rows = statement.query_map(params![from_ms], Self::read_row)?;
        rows.collect()
    }

    /// Всё, что нужно откатить для полного сброса, от свежих к старым.
    ///
    /// Порядок важен: откатывать надо в обратном порядке применения,
    /// иначе более раннее действие может помешать откату более позднего.
    pub fn to_revert(&self, since_ms: i64) -> Result<Vec<Entry>> {
        let mut statement = self.conn.prepare(&format!(
            "{SELECT} WHERE status IN (?1, ?2) AND at_ms >= ?3 ORDER BY at_ms DESC"
        ))?;
        let rows = statement.query_map(
            params![Status::Applied.as_str(), Status::Expired.as_str(), since_ms],
            Self::read_row,
        )?;
        rows.collect()
    }

    pub fn count(&self) -> Result<u32> {
        self.conn
            .query_row("SELECT COUNT(*) FROM journal", [], |row| row.get(0))
    }

    fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
        let action_name: String = row.get(8)?;
        Ok(Entry {
            id: row.get(0)?,
            at_unix_ms: row.get(1)?,
            actor: if row.get::<_, String>(2)? == Actor::Auto.as_str() {
                Actor::Auto
            } else {
                Actor::Manual
            },
            profile: row.get(3)?,
            target: Target {
                app_key: row.get(4)?,
                pid: row.get(5)?,
                service_name: row.get(6)?,
                task_path: row.get(7)?,
            },
            action: action_from_name(&action_name).unwrap_or(Action::EnableEcoQos),
            prior_state: row.get(9)?,
            observation: row.get(10)?,
            watchdog_deadline_ms: row.get(11)?,
            status: Status::parse(&row.get::<_, String>(12)?).unwrap_or(Status::Failed),
            revert_reason: row.get(13)?,
        })
    }
}

const SELECT: &str = "SELECT id, at_ms, actor, profile, app_key, pid, service_name, task_path, \
     action, prior_state, observation, watchdog_ms, status, revert_reason FROM journal";

/// Восстанавливает действие по его названию.
fn action_from_name(name: &str) -> Option<Action> {
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
    ALL.into_iter().find(|action| action.name() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_survives_a_round_trip_through_its_name() {
        // Названия действий — часть формата журнала. Разъедутся —
        // и откатить старые записи станет невозможно.
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
        for action in ALL {
            assert_eq!(action_from_name(action.name()), Some(action));
        }
    }
}
