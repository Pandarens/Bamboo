//! Подбор и применение действий.
//!
//! По умолчанию — симуляция (ТЗ, раздел 14.6): показываем, что было бы
//! сделано, и ничего не трогаем. Это режим по умолчанию не из осторожности
//! разработчика, а потому что он снимает недоверие: человек видит список
//! до того, как что-то произошло.

use std::path::PathBuf;
use std::time::Duration;

use bamboo_actuate::{Executor, SystemBackend};
use bamboo_collect::{Collector, TrackedProcess};
use bamboo_core::Result;
use bamboo_journal::{Actor, Journal, Target};
use bamboo_policy::{
    Action, AutonomyMode, Context, Decision, ProcessFacts, Profile, UserWhitelist,
};

/// Кандидатом считается фоновый процесс, который что-то делает.
///
/// Совсем спящий процесс трогать незачем: EcoQoS ему ничего не сэкономит,
/// а запись в журнале появится.
const MIN_CPU_SHARE: f32 = 0.005;

/// Где живёт журнал действий.
pub fn journal_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("Bamboo").join("journal.db")
}

fn open_journal() -> Result<Journal> {
    let path = journal_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    Journal::open(&path).map_err(|_| bamboo_core::Error::Unsupported("журнал не открылся"))
}

/// Предложение по конкретному процессу.
pub struct Proposal {
    pub image_name: String,
    pub pid: u32,
    pub action: Action,
    pub cpu_share: f32,
    pub decision: Decision,
}

/// Собирает кандидатов и прогоняет их через политику.
pub fn plan(apply: bool) -> Result<()> {
    let mut collector = Collector::new();
    collector.tick()?;
    std::thread::sleep(Duration::from_millis(1200));
    collector.tick()?;

    let own_pid = std::process::id();
    let whitelist = UserWhitelist::new();

    let mut proposals: Vec<Proposal> = Vec::new();
    for process in collector.table().top_by_cpu(40) {
        if process.pid() == own_pid || !is_candidate(process) {
            continue;
        }

        let context = context(process, &whitelist);
        proposals.push(Proposal {
            image_name: process.image_name.to_string(),
            pid: process.pid(),
            action: context.action,
            cpu_share: process.cpu_share,
            decision: bamboo_policy::decide(&context),
        });
    }

    crate::render::proposals(&proposals, apply);

    if !apply {
        return Ok(());
    }

    let journal = open_journal()?;
    let executor = Executor::new(&journal, SystemBackend);
    let now = bamboo_core::SampleTime::wall_clock_now();

    let mut applied = 0;
    for proposal in proposals
        .iter()
        .filter(|proposal| !matches!(proposal.decision, Decision::Refuse(_)))
    {
        let facts = ProcessFacts {
            image_name: &proposal.image_name,
            session_id: 1,
            ..Default::default()
        };
        let context = Context {
            action: proposal.action,
            process: facts,
            app_key: &proposal.image_name,
            app_class: None,
            profile: Profile::Normal,
            mode: AutonomyMode::Assist,
            learning: false,
            whitelist: &whitelist,
        };
        let target = Target {
            app_key: proposal.image_name.clone(),
            pid: Some(proposal.pid),
            ..Default::default()
        };

        let outcome = executor.apply(now, &context, &target, Actor::Manual, false);
        crate::render::outcome(&proposal.image_name, proposal.pid, &outcome);
        if outcome.is_applied() {
            applied += 1;
        }
    }

    println!("\nПрименено действий: {applied}. Откат: bamboo revert --all");
    Ok(())
}

fn is_candidate(process: &TrackedProcess) -> bool {
    // Только процессы пользовательской сессии: всё, что в сессии 0,
    // относится к системной инфраструктуре.
    process.session_id != 0 && process.cpu_share >= MIN_CPU_SHARE
}

fn context<'a>(process: &'a TrackedProcess, whitelist: &'a UserWhitelist) -> Context<'a> {
    Context {
        action: Action::EnableEcoQos,
        process: ProcessFacts {
            image_name: &process.image_name,
            session_id: process.session_id,
            ..Default::default()
        },
        app_key: &process.image_name,
        app_class: None,
        profile: Profile::Normal,
        mode: AutonomyMode::Assist,
        learning: false,
        whitelist,
    }
}

/// Печатает журнал действий.
pub fn show_journal() -> Result<()> {
    let journal = open_journal()?;
    let entries = journal
        .since(0)
        .map_err(|_| bamboo_core::Error::Unsupported("журнал не читается"))?;
    crate::render::journal(&entries, &journal_path().display().to_string());
    Ok(())
}

/// Откатывает действия.
pub fn revert(args: &[String]) -> Result<()> {
    let journal = open_journal()?;
    let executor = Executor::new(&journal, SystemBackend);

    if args.iter().any(|arg| arg == "--all") {
        let (reverted, failed) = executor.revert_all(0, "полный сброс по запросу пользователя");
        println!("Откачено записей: {reverted}");
        for (id, error) in &failed {
            println!("  запись №{id}: {error}");
        }
        return Ok(());
    }

    let id = args
        .iter()
        .position(|arg| arg == "--id")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<i64>().ok());

    match id {
        Some(id) => match executor.revert(id, "откат по запросу пользователя")
        {
            Ok(()) => println!("Запись №{id} откачена."),
            Err(error) => println!("Откатить не удалось: {error}"),
        },
        None => println!("Укажите, что откатывать: --id N или --all"),
    }
    Ok(())
}
