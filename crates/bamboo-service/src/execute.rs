//! Исполнение прошедших валидацию запросов (ТЗ, разделы 3.1, 13).
//!
//! Сюда запрос попадает, только когда `validate` вернул `Allow`. Дальше он
//! идёт через исполнителя `bamboo-actuate` — тот же путь, что и у CLI:
//! политика, снятие состояния «до», запись в журнал, действие,
//! подтверждение. Никакой отдельной привилегированной ветки у брокера нет,
//! и это сознательно: один путь изменения системы — один предмет аудита.
//!
//! Обобщён по `Backend`, чтобы проверяться тестами на поддельном бэкенде
//! и журнале в памяти, не трогая живую систему.

use bamboo_actuate::{Backend, Executor, Outcome};
use bamboo_ipc::{ErrorCode, Request, Response};
use bamboo_journal::{Actor, Target};
use bamboo_policy::{Action, AutonomyMode, Context, ProcessFacts, Profile, UserWhitelist};

/// Исполняет запрос и формирует ответ.
///
/// Читающие запросы (метрики, история, наблюдения) брокер пока не
/// обслуживает: у него нет своего коллектора и хранилища — они подключатся
/// отдельно. Честно отвечаем `NotImplemented`, а не выдумываем данные.
pub fn run<B: Backend>(
    request: &Request,
    executor: &Executor<'_, B>,
    whitelist: &UserWhitelist,
    now_unix_ms: i64,
) -> Response {
    match request {
        Request::Apply {
            action,
            app_key,
            pid,
            dry_run,
        } => apply(
            executor,
            whitelist,
            *action,
            app_key,
            *pid,
            *dry_run,
            now_unix_ms,
        ),

        Request::Revert { journal_id } => {
            match executor.revert(*journal_id, "откат по запросу агента") {
                Ok(()) => Response::ActionResult {
                    journal_id: *journal_id,
                    status: "откачено".into(),
                },
                Err(error) => internal(error),
            }
        }

        Request::RevertAll { since_unix_ms } => {
            let (reverted, failed) =
                executor.revert_all(*since_unix_ms, "полный откат по запросу агента");
            if failed.is_empty() {
                Response::ActionResult {
                    journal_id: 0,
                    status: format!("откачено записей: {reverted}"),
                }
            } else {
                Response::Error {
                    code: ErrorCode::Internal,
                    detail: format!(
                        "откачено {reverted}, не удалось {}: {}",
                        failed.len(),
                        failed
                            .iter()
                            .map(|(id, err)| format!("№{id} — {err}"))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                }
            }
        }

        // Изменяющие, но пока не проведённые до исполнения команды.
        Request::SetProfile { .. }
        | Request::Whitelist { .. }
        | Request::Investigate { .. }
        | Request::CaptureDump { .. } => not_implemented(request),

        // Читающие запросы: брокер их ещё не обслуживает.
        Request::Subscribe { .. }
        | Request::Unsubscribe { .. }
        | Request::QuerySnapshot { .. }
        | Request::QueryHistory { .. }
        | Request::QueryObservations { .. }
        | Request::ExportReport { .. } => not_implemented(request),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply<B: Backend>(
    executor: &Executor<'_, B>,
    whitelist: &UserWhitelist,
    action: Action,
    app_key: &str,
    pid: Option<u32>,
    dry_run: bool,
    now_unix_ms: i64,
) -> Response {
    // Действие к процессу без PID применить нельзя — исполнитель это тоже
    // отсечёт, но понятнее сказать сразу и правильным кодом.
    if action.targets_process() && pid.is_none() {
        return Response::Error {
            code: ErrorCode::UnknownTarget,
            detail: format!("{} требует PID процесса", action.name()),
        };
    }

    let context = Context {
        action,
        process: ProcessFacts {
            image_name: app_key,
            session_id: 1,
            ..Default::default()
        },
        app_key,
        app_class: None,
        // Брокер в автономном режиме работает по обычному профилю; смена
        // профиля идёт отдельной командой SetProfile.
        profile: Profile::Normal,
        mode: AutonomyMode::Assist,
        learning: false,
        whitelist,
    };
    let target = Target {
        app_key: app_key.to_string(),
        pid,
        ..Default::default()
    };

    match executor.apply(now_unix_ms, &context, &target, Actor::Auto, dry_run) {
        Outcome::Applied { journal_id } => Response::ActionResult {
            journal_id,
            status: "применено".into(),
        },
        Outcome::Simulated { would_do } => Response::ActionResult {
            journal_id: 0,
            status: format!("симуляция: {would_do}"),
        },
        Outcome::Refused { reason } => Response::Error {
            code: ErrorCode::RefusedByPolicy,
            detail: reason,
        },
        Outcome::Failed { journal_id, error } => Response::Error {
            code: ErrorCode::Internal,
            detail: format!("запись №{journal_id}: {error}"),
        },
    }
}

fn internal(detail: String) -> Response {
    Response::Error {
        code: ErrorCode::Internal,
        detail,
    }
}

fn not_implemented(request: &Request) -> Response {
    Response::Error {
        code: ErrorCode::NotImplemented,
        detail: format!("брокер пока не обслуживает запрос {request:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bamboo_journal::Journal;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeBackend {
        applied: RefCell<Vec<Action>>,
        reverted: RefCell<Vec<Action>>,
    }

    impl Backend for &FakeBackend {
        fn capture(&self, _a: Action, _t: &Target) -> Result<bamboo_actuate::PriorState, String> {
            Ok(bamboo_actuate::PriorState::new().with("eco_qos", "нет"))
        }
        fn apply(&self, a: Action, _t: &Target) -> Result<(), String> {
            self.applied.borrow_mut().push(a);
            Ok(())
        }
        fn revert(
            &self,
            a: Action,
            _t: &Target,
            _p: &bamboo_actuate::PriorState,
        ) -> Result<(), String> {
            self.reverted.borrow_mut().push(a);
            Ok(())
        }
    }

    fn eco_qos_apply() -> Request {
        Request::Apply {
            action: Action::EnableEcoQos,
            app_key: "slack".into(),
            pid: Some(100),
            dry_run: false,
        }
    }

    #[test]
    fn an_allowed_apply_reaches_the_backend_and_journal() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        let response = run(&eco_qos_apply(), &executor, &whitelist, 1000);

        match response {
            Response::ActionResult { journal_id, status } => {
                assert!(journal_id > 0, "действие должно попасть в журнал");
                assert_eq!(status, "применено");
            }
            other => panic!("ожидался ActionResult, получили {other:?}"),
        }
        assert_eq!(backend.applied.borrow().len(), 1);
        assert_eq!(journal.active().unwrap().len(), 1);
    }

    #[test]
    fn a_dry_run_changes_nothing_but_reports_what_it_would_do() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        let request = Request::Apply {
            action: Action::EnableEcoQos,
            app_key: "slack".into(),
            pid: Some(100),
            dry_run: true,
        };
        let response = run(&request, &executor, &whitelist, 1000);

        match response {
            Response::ActionResult { journal_id, status } => {
                assert_eq!(journal_id, 0);
                assert!(status.starts_with("симуляция"));
            }
            other => panic!("ожидался ActionResult симуляции, получили {other:?}"),
        }
        assert!(backend.applied.borrow().is_empty());
        assert_eq!(journal.count().unwrap(), 0);
    }

    #[test]
    fn applying_then_reverting_walks_the_full_path() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        let Response::ActionResult { journal_id, .. } =
            run(&eco_qos_apply(), &executor, &whitelist, 1000)
        else {
            panic!("действие не применилось");
        };

        let response = run(&Request::Revert { journal_id }, &executor, &whitelist, 2000);
        match response {
            Response::ActionResult { status, .. } => assert_eq!(status, "откачено"),
            other => panic!("ожидался ActionResult отката, получили {other:?}"),
        }
        assert_eq!(backend.reverted.borrow().len(), 1);
        assert!(journal.active().unwrap().is_empty());
    }

    #[test]
    fn a_process_action_without_a_pid_is_rejected_clearly() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        let request = Request::Apply {
            action: Action::EnableEcoQos,
            app_key: "slack".into(),
            pid: None,
            dry_run: false,
        };
        match run(&request, &executor, &whitelist, 1000) {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::UnknownTarget),
            other => panic!("ожидался отказ UnknownTarget, получили {other:?}"),
        }
        assert!(backend.applied.borrow().is_empty());
    }

    #[test]
    fn a_read_request_is_honestly_not_implemented() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        let request = Request::QueryObservations { since_unix_ms: 0 };
        match run(&request, &executor, &whitelist, 1000) {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::NotImplemented),
            other => panic!("ожидался NotImplemented, получили {other:?}"),
        }
    }

    #[test]
    fn reverting_all_reports_the_count() {
        let journal = Journal::in_memory().unwrap();
        let backend = FakeBackend::default();
        let executor = Executor::new(&journal, &backend);
        let whitelist = UserWhitelist::new();

        run(&eco_qos_apply(), &executor, &whitelist, 1000);
        let response = run(
            &Request::RevertAll { since_unix_ms: 0 },
            &executor,
            &whitelist,
            2000,
        );
        match response {
            Response::ActionResult { status, .. } => {
                assert!(status.contains("откачено записей: 1"))
            }
            other => panic!("ожидался ActionResult, получили {other:?}"),
        }
    }
}
