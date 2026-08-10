//! Проверки журнала на настоящей базе.

use bamboo_journal::{Actor, Journal, NewEntry, Status, Target};
use bamboo_policy::Action;

const HOUR: i64 = 3_600_000;

fn journal() -> Journal {
    Journal::in_memory().expect("журнал не открылся")
}

fn begin(journal: &Journal, at: i64, action: Action) -> i64 {
    journal
        .begin(&NewEntry {
            at_unix_ms: at,
            actor: Actor::Auto,
            profile: "Обычный",
            target: &Target::app("slack"),
            action,
            prior_state: r#"{"eco_qos":false}"#,
            observation: Some("наблюдение о фоновой нагрузке"),
        })
        .expect("запись не создалась")
}

#[test]
fn a_new_entry_starts_unconfirmed() {
    let journal = journal();
    let id = begin(&journal, 1000, Action::EnableEcoQos);

    let entry = journal.get(id).unwrap().unwrap();
    // Запись создаётся до действия — иначе падение между ними оставит
    // изменённую систему без следа в журнале.
    assert_eq!(entry.status, Status::Pending);
    assert_eq!(entry.action, Action::EnableEcoQos);
    assert_eq!(entry.prior_state, r#"{"eco_qos":false}"#);
}

#[test]
fn a_confirmed_entry_becomes_active() {
    let journal = journal();
    let id = begin(&journal, 1000, Action::EnableEcoQos);
    journal.confirm(id).unwrap();

    assert_eq!(journal.get(id).unwrap().unwrap().status, Status::Applied);
    assert_eq!(journal.active().unwrap().len(), 1);
}

#[test]
fn a_crash_leaves_the_entry_for_the_next_start_to_sort_out() {
    let journal = journal();
    begin(&journal, 1000, Action::EnableEcoQos);
    let id = begin(&journal, 2000, Action::LowerMemoryPriority);
    journal.confirm(id).unwrap();

    // Первая запись осталась без подтверждения: процесс упал между
    // записью и подтверждением.
    let unconfirmed = journal.unconfirmed().unwrap();
    assert_eq!(unconfirmed.len(), 1);
    assert_eq!(unconfirmed[0].action, Action::EnableEcoQos);
}

#[test]
fn a_failed_action_is_not_active() {
    let journal = journal();
    let id = begin(&journal, 1000, Action::EnableEcoQos);
    journal.fail(id, "проверка").unwrap();

    assert_eq!(journal.get(id).unwrap().unwrap().status, Status::Failed);
    assert!(journal.active().unwrap().is_empty());
    assert!(journal.unconfirmed().unwrap().is_empty());
}

#[test]
fn a_revert_records_its_reason() {
    let journal = journal();
    let id = begin(&journal, 1000, Action::EnableEcoQos);
    journal.confirm(id).unwrap();
    journal
        .mark_reverted(id, "вы вручную запустили то, что Bamboo усыпил")
        .unwrap();

    let entry = journal.get(id).unwrap().unwrap();
    assert_eq!(entry.status, Status::Reverted);
    assert!(entry.revert_reason.unwrap().contains("вручную"));
    assert!(journal.active().unwrap().is_empty());
}

#[test]
fn the_watchdog_window_lasts_forty_eight_hours() {
    let journal = journal();
    let id = begin(&journal, 0, Action::EnableEcoQos);
    journal.confirm(id).unwrap();

    assert_eq!(journal.under_watch(24 * HOUR).unwrap().len(), 1);
    assert!(journal.under_watch(49 * HOUR).unwrap().is_empty());
}

#[test]
fn an_expired_change_stays_in_force() {
    let journal = journal();
    let id = begin(&journal, 0, Action::EnableEcoQos);
    journal.confirm(id).unwrap();
    journal.mark_expired(id).unwrap();

    let entry = journal.get(id).unwrap().unwrap();
    assert_eq!(entry.status, Status::Expired);
    assert!(
        entry.status.is_active(),
        "прижившееся изменение всё ещё действует"
    );
    assert_eq!(journal.active().unwrap().len(), 1);
}

#[test]
fn a_full_reset_reverts_in_reverse_order() {
    let journal = journal();
    // Откатывать надо в обратном порядке применения: раннее действие
    // может помешать откату более позднего.
    for (index, action) in [
        Action::EnableEcoQos,
        Action::LowerMemoryPriority,
        Action::DelayServiceStart,
    ]
    .into_iter()
    .enumerate()
    {
        let id = begin(&journal, index as i64 * HOUR, action);
        journal.confirm(id).unwrap();
    }

    let to_revert = journal.to_revert(0).unwrap();
    assert_eq!(to_revert.len(), 3);
    assert_eq!(to_revert[0].action, Action::DelayServiceStart);
    assert_eq!(to_revert[2].action, Action::EnableEcoQos);
}

#[test]
fn reverting_a_period_leaves_older_entries_alone() {
    let journal = journal();
    let old = begin(&journal, 0, Action::EnableEcoQos);
    journal.confirm(old).unwrap();
    let recent = begin(&journal, 10 * HOUR, Action::LowerMemoryPriority);
    journal.confirm(recent).unwrap();

    let to_revert = journal.to_revert(5 * HOUR).unwrap();
    assert_eq!(to_revert.len(), 1);
    assert_eq!(to_revert[0].id, recent);
}

#[test]
fn already_reverted_entries_are_not_reverted_twice() {
    let journal = journal();
    let id = begin(&journal, 0, Action::EnableEcoQos);
    journal.confirm(id).unwrap();
    journal.mark_reverted(id, "вручную").unwrap();

    assert!(journal.to_revert(0).unwrap().is_empty());
}

#[test]
fn the_journal_survives_reopening() {
    let path = std::env::temp_dir().join("bamboo-test-journal.db");
    let _ = std::fs::remove_file(&path);

    let id = {
        let journal = Journal::open(&path).unwrap();
        let id = begin(&journal, 1000, Action::StopService);
        journal.confirm(id).unwrap();
        id
    };

    {
        let journal = Journal::open(&path).unwrap();
        let entry = journal.get(id).unwrap().unwrap();
        assert_eq!(entry.action, Action::StopService);
        assert_eq!(entry.status, Status::Applied);
        assert_eq!(journal.count().unwrap(), 1);
    }

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

#[test]
fn targets_of_every_kind_round_trip() {
    let journal = journal();

    let service = Target {
        app_key: "sysmain".into(),
        service_name: Some("SysMain".into()),
        ..Default::default()
    };
    let id = journal
        .begin(&NewEntry {
            at_unix_ms: 0,
            actor: Actor::Manual,
            profile: "Обычный",
            target: &service,
            action: Action::StopService,
            prior_state: "{}",
            observation: None,
        })
        .unwrap();

    let entry = journal.get(id).unwrap().unwrap();
    assert_eq!(entry.target.service_name.as_deref(), Some("SysMain"));
    assert_eq!(entry.actor, Actor::Manual);
    assert_eq!(entry.target.describe(), "служба SysMain");
}
