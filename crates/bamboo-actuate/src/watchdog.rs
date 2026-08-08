//! Сторожевой таймер в действии (ТЗ, раздел 12.4).
//!
//! Логику «пора ли откатывать» держит `bamboo-journal::watchdog` — она
//! чистая. Здесь оркестровка: пройти по действующим записям под наблюдением,
//! спросить у каждой метрики, не стало ли хуже, и при деградации откатить
//! через исполнителя. Плюс закрыть окно наблюдения у тех, кто его пережил.
//!
//! Это ключевой механизм, отсутствующий у аналогов: он превращает набор
//! твиков в систему, обучающуюся на конкретной машине.

use bamboo_journal::watchdog::{should_revert, Baseline, Observed};
use bamboo_journal::{Journal, Status};
use bamboo_policy::UserWhitelist;

use crate::executor::{Backend, Executor};

/// Как получить текущее состояние затронутой цели.
///
/// Реализация на живой системе читает журналы событий и время загрузки;
/// в тестах — возвращает заданные значения. Так автооткат проверяется
/// без ожидания реальной деградации.
pub trait HealthProbe {
    /// Метрики на момент применения действия — база сравнения.
    fn baseline(&self, journal_id: i64) -> Baseline;
    /// Что наблюдается сейчас.
    fn observed(&self, journal_id: i64) -> Observed;
}

/// Итог одного прохода сторожевого таймера.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchdogSweep {
    /// Записи, откаченные из-за деградации: номер и причина.
    pub reverted: Vec<(i64, String)>,
    /// Записи, чьё окно наблюдения закрылось без проблем.
    pub expired: Vec<i64>,
}

/// Прогоняет сторожевой таймер по всем действующим записям.
///
/// `now_ms` — текущее время. Записи, у которых окно наблюдения ещё открыто,
/// проверяются на деградацию; у кого закрылось — помечаются прижившимися.
pub fn sweep<B: Backend>(
    executor: &Executor<'_, B>,
    journal: &Journal,
    probe: &impl HealthProbe,
    now_ms: i64,
) -> WatchdogSweep {
    let mut sweep = WatchdogSweep::default();

    let active = match journal.active() {
        Ok(active) => active,
        Err(_) => return sweep,
    };

    for entry in active {
        // Прижившиеся изменения (Expired) больше не трогаем.
        if entry.status != Status::Applied {
            continue;
        }

        if entry.watchdog_expired(now_ms) {
            // Окно наблюдения закрылось без деградации — изменение осталось.
            if journal.mark_expired(entry.id).is_ok() {
                sweep.expired.push(entry.id);
            }
            continue;
        }

        if !entry.watchdog_active(now_ms) {
            continue;
        }

        let baseline = probe.baseline(entry.id);
        let observed = probe.observed(entry.id);

        if let Some(reason) = should_revert(&baseline, &observed) {
            let text = reason.describe();
            // Откат вернёт систему и пометит запись; если он не удался,
            // запись остаётся действующей и попадёт в следующий проход.
            if executor.revert(entry.id, &text).is_ok() {
                sweep.reverted.push((entry.id, text));
            }
        }
    }

    sweep
}

/// После автоотката цель добавляется в пользовательский белый список,
/// чтобы Bamboo больше не предлагал с ней то же самое (ТЗ, раздел 12.4).
///
/// Вынесено отдельно: белый список живёт у агента, а не у исполнителя,
/// и вызывающий сам решает, когда его обновить.
pub fn remember_reverted(whitelist: &mut UserWhitelist, app_key: &str) {
    whitelist.add(app_key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PriorState;
    use bamboo_journal::{Actor, NewEntry, Target};
    use bamboo_policy::{Action, UserWhitelist};
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeBackend {
        reverted: RefCell<Vec<i64>>,
    }

    impl Backend for &FakeBackend {
        fn capture(&self, _a: Action, _t: &Target) -> Result<PriorState, String> {
            Ok(PriorState::new().with("eco_qos", "нет"))
        }
        fn apply(&self, _a: Action, _t: &Target) -> Result<(), String> {
            Ok(())
        }
        fn revert(&self, _a: Action, _t: &Target, _p: &PriorState) -> Result<(), String> {
            Ok(())
        }
    }

    /// Проба, отвечающая одинаково для всех записей.
    struct FixedProbe {
        baseline: Baseline,
        observed: Observed,
    }

    impl HealthProbe for FixedProbe {
        fn baseline(&self, _id: i64) -> Baseline {
            self.baseline
        }
        fn observed(&self, _id: i64) -> Observed {
            self.observed
        }
    }

    const HOUR: i64 = 3_600_000;

    fn apply_one(journal: &Journal, at_ms: i64) -> i64 {
        let id = journal
            .begin(&NewEntry {
                at_unix_ms: at_ms,
                actor: Actor::Auto,
                profile: "Обычный",
                target: &Target::app("slack"),
                action: Action::EnableEcoQos,
                prior_state: "eco_qos=нет",
                observation: None,
            })
            .unwrap();
        journal.confirm(id).unwrap();
        id
    }

    fn healthy_probe() -> FixedProbe {
        FixedProbe {
            baseline: Baseline {
                daily_errors: 10,
                boot_ms: 30_000,
            },
            observed: Observed {
                daily_errors: 11,
                boot_ms: 31_000,
                ..Default::default()
            },
        }
    }

    #[test]
    fn a_healthy_change_is_not_reverted_while_watched() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        apply_one(&journal, 0);

        let sweep = sweep(&executor, &journal, &healthy_probe(), 24 * HOUR);
        assert!(sweep.reverted.is_empty());
        assert!(sweep.expired.is_empty(), "окно ещё открыто");
        assert!(backend.reverted.borrow().is_empty());
    }

    #[test]
    fn a_degradation_triggers_an_automatic_revert() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let id = apply_one(&journal, 0);

        // Ошибки в журналах системы удвоились после изменения.
        let probe = FixedProbe {
            baseline: Baseline {
                daily_errors: 10,
                boot_ms: 30_000,
            },
            observed: Observed {
                daily_errors: 30,
                boot_ms: 30_000,
                ..Default::default()
            },
        };

        let sweep = sweep(&executor, &journal, &probe, 24 * HOUR);
        assert_eq!(sweep.reverted.len(), 1);
        assert_eq!(sweep.reverted[0].0, id);
        assert!(sweep.reverted[0].1.contains("ошибок"));
        assert!(
            journal.active().unwrap().is_empty(),
            "запись должна быть откачена"
        );
    }

    #[test]
    fn a_change_that_survives_the_window_is_marked_expired() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let id = apply_one(&journal, 0);

        // Прошло больше 48 часов — окно закрылось.
        let sweep = sweep(&executor, &journal, &healthy_probe(), 50 * HOUR);
        assert_eq!(sweep.expired, vec![id]);
        assert!(sweep.reverted.is_empty());

        // Прижившееся изменение всё ещё в силе.
        let entry = journal.get(id).unwrap().unwrap();
        assert_eq!(entry.status, Status::Expired);
        assert!(entry.status.is_active());
    }

    #[test]
    fn the_user_undoing_it_by_hand_reverts_immediately() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        apply_one(&journal, 0);

        let probe = FixedProbe {
            baseline: Baseline {
                daily_errors: 10,
                boot_ms: 30_000,
            },
            observed: Observed {
                user_restarted_target: true,
                ..Default::default()
            },
        };

        let sweep = sweep(&executor, &journal, &probe, HOUR);
        assert_eq!(sweep.reverted.len(), 1);
        assert!(sweep.reverted[0].1.contains("вручную"));
    }

    #[test]
    fn an_already_expired_entry_is_left_alone() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let id = apply_one(&journal, 0);
        journal.mark_expired(id).unwrap();

        // Второй проход не должен ни откатывать, ни трогать прижившееся.
        let sweep = sweep(&executor, &journal, &healthy_probe(), 60 * HOUR);
        assert!(sweep.reverted.is_empty());
        assert!(sweep.expired.is_empty());
    }

    #[test]
    fn a_reverted_target_lands_in_the_whitelist() {
        // После автоотката Bamboo больше не предлагает то же самое.
        let mut whitelist = UserWhitelist::new();
        remember_reverted(&mut whitelist, "slack");
        assert!(whitelist.contains("slack", None));
    }
}
