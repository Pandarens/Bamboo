//! Исполнитель действий.
//!
//! Единственный путь, которым Bamboo меняет систему. Порядок жёсткий
//! и обойти его нельзя:
//!
//! 1. Проверка политикой — можно ли вообще
//! 2. Снятие состояния «до»
//! 3. Запись в журнал
//! 4. Само действие
//! 5. Подтверждение или отметка о неудаче
//!
//! Если процесс упадёт между третьим и пятым шагом, в журнале останется
//! неподтверждённая запись, и следующий запуск разберётся с ней.

use bamboo_journal::{Actor, Journal, Target};
use bamboo_policy::{decide, Action, Context, Decision};

use crate::state::PriorState;

/// Чем закончилась попытка.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Действие применено, в журнале запись с таким номером.
    Applied { journal_id: i64 },
    /// Режим симуляции: показали, что было бы сделано, и ничего не тронули.
    Simulated { would_do: String },
    /// Политика не пропустила.
    Refused { reason: String },
    /// Действие не удалось. Запись в журнале помечена как неудачная.
    Failed { journal_id: i64, error: String },
}

impl Outcome {
    pub fn is_applied(&self) -> bool {
        matches!(self, Outcome::Applied { .. })
    }

    pub fn describe(&self) -> String {
        match self {
            Outcome::Applied { journal_id } => format!("применено, запись №{journal_id}"),
            Outcome::Simulated { would_do } => format!("было бы сделано: {would_do}"),
            Outcome::Refused { reason } => format!("отказ: {reason}"),
            Outcome::Failed { error, .. } => format!("не удалось: {error}"),
        }
    }
}

/// То, что умеет применять и откатывать действия на конкретной системе.
///
/// Вынесено в трейт по двум причинам: чтобы исполнителя можно было
/// проверить тестами без изменения живой системы, и чтобы `bamboo-actuate`
/// не зависел напрямую от Windows.
/// Есть ли у системы путь назад, помимо нашего собственного отката.
///
/// Свой откат чинит то, что сделали мы. Точка восстановления страхует
/// случай, когда от нашего действия сломалось что-то ещё, — и без неё
/// действия уровня 5–6 выполнять нельзя.
pub trait SafetyNet {
    /// Готовит страховку перед действием. Возвращает объяснение для
    /// человека и признак того, что путь назад есть.
    fn prepare(&self, description: &str) -> (String, bool);
}

/// Страховка, которой нет.
///
/// Для действий уровня 1–4 она и не нужна: они обратимы своим же откатом.
pub struct NoSafetyNet;

impl SafetyNet for NoSafetyNet {
    fn prepare(&self, _description: &str) -> (String, bool) {
        (String::new(), true)
    }
}

/// Начиная с этого уровня риска действие требует точки восстановления.
///
/// Пятый уровень — остановка службы, шестой — её отключение. От них ломается
/// то, что от службы зависит, и наш откат такого не чинит.
pub const NEEDS_SAFETY_NET: u8 = 5;

pub trait Backend {
    /// Снимает состояние цели до изменения.
    fn capture(&self, action: Action, target: &Target) -> Result<PriorState, String>;

    /// Выполняет действие.
    fn apply(&self, action: Action, target: &Target) -> Result<(), String>;

    /// Возвращает состояние по записанному «до».
    fn revert(&self, action: Action, target: &Target, prior: &PriorState) -> Result<(), String>;
}

pub struct Executor<'a, B: Backend, S: SafetyNet = NoSafetyNet> {
    journal: &'a Journal,
    backend: B,
    safety_net: S,
}

impl<'a, B: Backend> Executor<'a, B, NoSafetyNet> {
    pub fn new(journal: &'a Journal, backend: B) -> Self {
        Executor {
            journal,
            backend,
            safety_net: NoSafetyNet,
        }
    }
}

impl<'a, B: Backend, S: SafetyNet> Executor<'a, B, S> {
    /// Исполнитель со страховкой: перед действиями уровня 5–6 он оставляет
    /// путь назад и отказывается действовать, если оставить его не вышло.
    pub fn with_safety_net(journal: &'a Journal, backend: B, safety_net: S) -> Self {
        Executor {
            journal,
            backend,
            safety_net,
        }
    }

    /// Применяет действие, проведя его через все проверки.
    ///
    /// `dry_run` — режим симуляции из раздела 14.6 ТЗ: система не трогается,
    /// в журнал ничего не пишется, возвращается описание того, что было бы
    /// сделано.
    pub fn apply(
        &self,
        at_unix_ms: i64,
        context: &Context<'_>,
        target: &Target,
        actor: Actor,
        dry_run: bool,
    ) -> Outcome {
        match decide(context) {
            Decision::Refuse(reason) => return Outcome::Refused { reason },
            Decision::Apply | Decision::Offer => {}
        }

        // Страховка перед рискованным действием. Проверяем до симуляции:
        // в симуляции человек и должен узнать, что действие сорвётся.
        if context.action.risk() >= NEEDS_SAFETY_NET {
            let (note, protected) = self.safety_net.prepare(&format!(
                "Bamboo: {} для {}",
                context.action.name(),
                target.describe()
            ));
            if !protected {
                return Outcome::Refused { reason: note };
            }
        }

        let prior = match self.backend.capture(context.action, target) {
            Ok(prior) => prior,
            Err(error) => {
                // Не сняли состояние «до» — значит не сможем откатить.
                // Такое действие не выполняется.
                return Outcome::Refused {
                    reason: format!("не удалось снять состояние до изменения: {error}"),
                };
            }
        };

        if dry_run {
            return Outcome::Simulated {
                would_do: format!(
                    "{} для {} (сейчас: {prior})",
                    context.action.name(),
                    target.describe()
                ),
            };
        }

        // Запись до действия. Обратный порядок означал бы, что падение
        // между действием и записью оставит изменённую систему без следа.
        let journal_id = match self.journal.begin(&bamboo_journal::NewEntry {
            at_unix_ms,
            actor,
            profile: context.profile.name(),
            target,
            action: context.action,
            prior_state: &prior.to_string(),
            observation: None,
        }) {
            Ok(id) => id,
            Err(error) => {
                return Outcome::Refused {
                    reason: format!("журнал недоступен, действие отменено: {error}"),
                }
            }
        };

        match self.backend.apply(context.action, target) {
            Ok(()) => {
                let _ = self.journal.confirm(journal_id);
                Outcome::Applied { journal_id }
            }
            Err(error) => {
                let _ = self.journal.fail(journal_id, &error);
                Outcome::Failed { journal_id, error }
            }
        }
    }

    /// Откатывает одно действие по номеру записи.
    pub fn revert(&self, journal_id: i64, reason: &str) -> Result<(), String> {
        let entry = self
            .journal
            .get(journal_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("записи №{journal_id} в журнале нет"))?;

        if !entry.status.is_active() {
            // Откатывать нечего: действие уже откачено или не применялось.
            return Ok(());
        }

        let prior = PriorState::parse(&entry.prior_state);
        self.backend
            .revert(entry.action, &entry.target, &prior)
            .map_err(|error| error.to_string())?;

        self.journal
            .mark_reverted(journal_id, reason)
            .map_err(|error| error.to_string())
    }

    /// Откатывает всё, что применено начиная с указанного момента.
    ///
    /// Возвращает число откаченных записей и список тех, что не поддались.
    pub fn revert_all(&self, since_ms: i64, reason: &str) -> (usize, Vec<(i64, String)>) {
        let entries = match self.journal.to_revert(since_ms) {
            Ok(entries) => entries,
            Err(error) => return (0, vec![(0, error.to_string())]),
        };

        let mut reverted = 0;
        let mut failed = Vec::new();

        // to_revert отдаёт записи от свежих к старым — именно в таком
        // порядке и надо откатывать.
        for entry in entries {
            match self.revert(entry.id, reason) {
                Ok(()) => reverted += 1,
                Err(error) => failed.push((entry.id, error)),
            }
        }

        (reverted, failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bamboo_policy::{AutonomyMode, Profile, UserWhitelist};
    use std::cell::RefCell;

    /// Бэкенд-заглушка: помнит, что с ним делали.
    #[derive(Default)]
    struct FakeBackend {
        applied: RefCell<Vec<Action>>,
        reverted: RefCell<Vec<Action>>,
        fail_apply: bool,
        fail_capture: bool,
    }

    impl Backend for &FakeBackend {
        fn capture(&self, _action: Action, _target: &Target) -> Result<PriorState, String> {
            if self.fail_capture {
                return Err("процесс исчез".into());
            }
            Ok(PriorState::new().with("eco_qos", "нет"))
        }

        fn apply(&self, action: Action, _target: &Target) -> Result<(), String> {
            if self.fail_apply {
                return Err("доступ запрещён".into());
            }
            self.applied.borrow_mut().push(action);
            Ok(())
        }

        fn revert(
            &self,
            action: Action,
            _target: &Target,
            _prior: &PriorState,
        ) -> Result<(), String> {
            self.reverted.borrow_mut().push(action);
            Ok(())
        }
    }

    fn context<'a>(whitelist: &'a UserWhitelist, name: &'a str, action: Action) -> Context<'a> {
        Context {
            action,
            process: bamboo_policy::ProcessFacts {
                image_name: name,
                session_id: 1,
                ..Default::default()
            },
            app_key: name,
            app_class: None,
            profile: Profile::Normal,
            mode: AutonomyMode::Assist,
            learning: false,
            whitelist,
        }
    }

    #[test]
    fn a_permitted_action_is_applied_and_journalled() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        let outcome = executor.apply(
            1000,
            &context(&whitelist, "slack.exe", Action::EnableEcoQos),
            &Target::app("slack.exe"),
            Actor::Auto,
            false,
        );

        assert!(outcome.is_applied());
        assert_eq!(backend.applied.borrow().len(), 1);
        assert_eq!(journal.active().unwrap().len(), 1);
    }

    #[test]
    fn a_refused_action_never_reaches_the_system() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        let outcome = executor.apply(
            1000,
            &context(&whitelist, "lsass.exe", Action::EnableEcoQos),
            &Target::app("lsass.exe"),
            Actor::Auto,
            false,
        );

        assert!(matches!(outcome, Outcome::Refused { .. }));
        assert!(backend.applied.borrow().is_empty(), "система была тронута");
        assert_eq!(journal.count().unwrap(), 0, "в журнал попала лишняя запись");
    }

    #[test]
    fn a_dry_run_changes_nothing_and_writes_nothing() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        let outcome = executor.apply(
            1000,
            &context(&whitelist, "slack.exe", Action::EnableEcoQos),
            &Target::app("slack.exe"),
            Actor::Auto,
            true,
        );

        assert!(matches!(outcome, Outcome::Simulated { .. }));
        assert!(backend.applied.borrow().is_empty());
        assert_eq!(journal.count().unwrap(), 0);
        assert!(outcome.describe().contains("экономичный режим"));
    }

    #[test]
    fn without_a_prior_state_the_action_is_refused() {
        // Не сняли «до» — не сможем откатить. Такое не выполняется.
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend {
            fail_capture: true,
            ..Default::default()
        };
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        let outcome = executor.apply(
            1000,
            &context(&whitelist, "slack.exe", Action::EnableEcoQos),
            &Target::app("slack.exe"),
            Actor::Auto,
            false,
        );

        assert!(matches!(outcome, Outcome::Refused { .. }));
        assert_eq!(journal.count().unwrap(), 0);
    }

    #[test]
    fn a_failed_action_is_marked_in_the_journal() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend {
            fail_apply: true,
            ..Default::default()
        };
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        let outcome = executor.apply(
            1000,
            &context(&whitelist, "slack.exe", Action::EnableEcoQos),
            &Target::app("slack.exe"),
            Actor::Auto,
            false,
        );

        assert!(matches!(outcome, Outcome::Failed { .. }));
        // Запись есть, но действующей не считается.
        assert_eq!(journal.count().unwrap(), 1);
        assert!(journal.active().unwrap().is_empty());
    }

    #[test]
    fn reverting_restores_the_system_and_marks_the_entry() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        let Outcome::Applied { journal_id } = executor.apply(
            1000,
            &context(&whitelist, "slack.exe", Action::EnableEcoQos),
            &Target::app("slack.exe"),
            Actor::Auto,
            false,
        ) else {
            panic!("действие не применилось");
        };

        executor.revert(journal_id, "проверка").unwrap();

        assert_eq!(backend.reverted.borrow().len(), 1);
        assert!(journal.active().unwrap().is_empty());
    }

    #[test]
    fn reverting_twice_is_harmless() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        let Outcome::Applied { journal_id } = executor.apply(
            1000,
            &context(&whitelist, "slack.exe", Action::EnableEcoQos),
            &Target::app("slack.exe"),
            Actor::Auto,
            false,
        ) else {
            panic!("действие не применилось");
        };

        executor.revert(journal_id, "первый").unwrap();
        executor.revert(journal_id, "второй").unwrap();

        // Второй откат не должен снова трогать систему.
        assert_eq!(backend.reverted.borrow().len(), 1);
    }

    #[test]
    fn a_full_reset_reverts_everything_newest_first() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        for (index, action) in [Action::EnableEcoQos, Action::LowerMemoryPriority]
            .into_iter()
            .enumerate()
        {
            executor.apply(
                1000 + index as i64 * 1000,
                &context(&whitelist, "slack.exe", action),
                &Target::app("slack.exe"),
                Actor::Auto,
                false,
            );
        }

        let (reverted, failed) = executor.revert_all(0, "полный сброс");
        assert_eq!(reverted, 2);
        assert!(failed.is_empty());
        assert_eq!(
            backend.reverted.borrow()[0],
            Action::LowerMemoryPriority,
            "откат должен идти в обратном порядке применения"
        );
        assert!(journal.active().unwrap().is_empty());
    }

    #[test]
    fn reverting_an_unknown_entry_reports_an_error() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        assert!(executor.revert(999, "нет такой").is_err());
    }
}

#[cfg(test)]
mod safety_net_gate_tests {
    use super::*;
    use bamboo_journal::{Actor, Journal};
    use bamboo_policy::{AutonomyMode, Context, ProcessFacts, Profile, UserWhitelist};

    /// Страховка, которой не удалось.
    struct NoWayBack;

    impl SafetyNet for NoWayBack {
        fn prepare(&self, _description: &str) -> (String, bool) {
            (
                "Защита системы выключена, действие отменено.".to_string(),
                false,
            )
        }
    }

    /// Бэкенд, который считает, сколько раз его тронули.
    struct CountingBackend(std::cell::Cell<usize>);

    impl Backend for CountingBackend {
        fn capture(&self, _action: Action, _target: &Target) -> Result<PriorState, String> {
            Ok(PriorState::new().with("что-то", "было"))
        }
        fn apply(&self, _action: Action, _target: &Target) -> Result<(), String> {
            self.0.set(self.0.get() + 1);
            Ok(())
        }
        fn revert(
            &self,
            _action: Action,
            _target: &Target,
            _prior: &PriorState,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    fn context(action: Action, whitelist: &UserWhitelist) -> Context<'_> {
        Context {
            action,
            process: ProcessFacts {
                image_name: "чужая-служба",
                session_id: 1,
                ..Default::default()
            },
            app_key: "чужая-служба",
            app_class: None,
            profile: Profile::Normal,
            mode: AutonomyMode::Assist,
            learning: false,
            whitelist,
        }
    }

    #[test]
    fn a_risky_action_without_a_way_back_is_refused_and_not_performed() {
        // Смысл всей страховки: если пути назад нет, действие не делается.
        // Отказаться после — поздно, служба уже остановлена.
        let journal = Journal::in_memory().unwrap();
        let backend = CountingBackend(std::cell::Cell::new(0));
        let whitelist = UserWhitelist::new();

        let executor = Executor::with_safety_net(&journal, backend, NoWayBack);
        let outcome = executor.apply(
            0,
            &context(Action::StopService, &whitelist),
            &Target {
                app_key: "чужая-служба".to_string(),
                ..Default::default()
            },
            Actor::Manual,
            false,
        );

        match outcome {
            Outcome::Refused { reason } => {
                assert!(reason.contains("отменено"), "{reason}");
            }
            other => panic!("рискованное действие прошло без страховки: {other:?}"),
        }
        // И в журнале следа быть не должно: действия не было.
        assert_eq!(journal.count().unwrap(), 0);
    }

    #[test]
    fn a_harmless_action_needs_no_safety_net() {
        // Экономичный режим обратим своим же откатом. Требовать под него
        // точку восстановления значило бы мешать без причины.
        let journal = Journal::in_memory().unwrap();
        let whitelist = UserWhitelist::new();

        let executor = Executor::with_safety_net(
            &journal,
            CountingBackend(std::cell::Cell::new(0)),
            NoWayBack,
        );
        let outcome = executor.apply(
            0,
            &context(Action::EnableEcoQos, &whitelist),
            &Target {
                app_key: "чужая-служба".to_string(),
                pid: Some(1234),
                ..Default::default()
            },
            Actor::Manual,
            false,
        );

        assert!(
            matches!(outcome, Outcome::Applied { .. }),
            "безобидное действие не должно упираться в страховку: {outcome:?}"
        );
    }

    #[test]
    fn the_threshold_matches_the_risk_levels_from_the_spec() {
        // Пятый уровень — остановка службы, шестой — отключение. Ниже
        // страховка не нужна: там всё чинится своим же откатом.
        assert!(Action::StopService.risk() >= NEEDS_SAFETY_NET);
        assert!(Action::DisableService.risk() >= NEEDS_SAFETY_NET);
        assert!(Action::FreezeProcess.risk() < NEEDS_SAFETY_NET);
        assert!(Action::EnableEcoQos.risk() < NEEDS_SAFETY_NET);
    }
}
