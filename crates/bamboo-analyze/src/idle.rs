//! Простаивающие приложения (ТЗ, раздел 9.5).
//!
//! Признак композитный, и это принципиально. Время работы само по себе
//! ничего не значит: Postgres может висеть месяцами, и это нормально.
//! Должны сойтись четыре измерения, а любой стоп-сигнал отменяет всё.

use bamboo_core::Bytes;

use crate::observation::{Observation, ObservationKind, Severity};

/// Минимальное время без взаимодействия.
const MIN_SINCE_INTERACTION_MS: u64 = 24 * 60 * 60 * 1000;

/// Что известно о приложении.
pub struct IdleInput<'a> {
    pub process_name: &'a str,
    /// Когда окно процесса последний раз было активным.
    ///
    /// Именно последнее взаимодействие, а не время запуска: это разные
    /// вещи, и путать их — главная ошибка всех «оптимизаторов».
    pub since_interaction_ms: u64,
    /// Есть ли у процесса окна вообще. Для служб критерий неприменим.
    pub has_windows: bool,

    /// Стоимость простоя за период бездействия.
    pub cpu_ms_while_idle: u64,
    pub written_while_idle: Bytes,
    pub wakeups_while_idle: u64,

    pub memory_now: Bytes,
    /// Росла ли память без взаимодействия.
    pub memory_grew: bool,

    /// Стоп-сигналы: незавершённая работа.
    pub established_connections: u32,
    pub open_user_files: u32,
    pub child_processes: u32,
    pub holds_power_request: bool,
}

/// Пороги, выше которых простой считается дорогим.
const EXPENSIVE_CPU_MS: u64 = 60_000;
const EXPENSIVE_WRITE: Bytes = Bytes(512 * 1024 * 1024);
const EXPENSIVE_WAKEUPS: u64 = 10_000;

pub fn analyze(input: &IdleInput<'_>) -> Option<Observation> {
    // Для процессов без окон понятия «последнее взаимодействие»
    // не существует. Они анализируются отдельно, здесь молчим.
    if !input.has_windows {
        return None;
    }

    if input.since_interaction_ms < MIN_SINCE_INTERACTION_MS {
        return None;
    }

    // Незавершённая работа. Любого признака достаточно, чтобы не трогать.
    if input.established_connections > 0
        || input.open_user_files > 0
        || input.child_processes > 0
        || input.holds_power_request
    {
        return None;
    }

    // Стоимость простоя. Приложение, честно спящее на нуле, не трогаем
    // независимо от того, сколько оно висит. Это условие отсекает
    // большинство ложных срабатываний.
    let costs = costs(input);
    if costs.is_empty() && !input.memory_grew {
        return None;
    }

    let days = input.since_interaction_ms / (24 * 60 * 60 * 1000);
    let summary = format!(
        "{} не использовался {days} {}, {}",
        input.process_name,
        plural_days(days),
        costs_text(&costs, input)
    );

    Some(
        Observation::new(
            ObservationKind::IdleApp,
            Severity::Notice,
            confidence(&costs, input),
            summary,
        )
        .with_detail(
            "Приложение ничем не занято, но продолжает тратить ресурсы. \
             Его можно закрыть или перевести в экономичный режим — \
             и то и другое обратимо.",
        ),
    )
}

/// Собирает конкретные, измеримые издержки простоя.
fn costs(input: &IdleInput<'_>) -> Vec<String> {
    let mut costs = Vec::new();

    if input.cpu_ms_while_idle >= EXPENSIVE_CPU_MS {
        costs.push(format!(
            "занял процессор на {:.0} минут",
            input.cpu_ms_while_idle as f64 / 60_000.0
        ));
    }
    if input.written_while_idle >= EXPENSIVE_WRITE {
        costs.push(format!("записал на диск {}", input.written_while_idle));
    }
    if input.wakeups_while_idle >= EXPENSIVE_WAKEUPS {
        costs.push(format!(
            "разбудил процессор {} раз",
            input.wakeups_while_idle
        ));
    }
    costs
}

fn costs_text(costs: &[String], input: &IdleInput<'_>) -> String {
    let mut parts = costs.to_vec();
    if input.memory_grew {
        parts.push(format!("вырос до {}", input.memory_now));
    }
    if parts.is_empty() {
        return format!("занимает {}", input.memory_now);
    }
    parts.join(", ")
}

fn confidence(costs: &[String], input: &IdleInput<'_>) -> f32 {
    let signals = costs.len() + usize::from(input.memory_grew);
    match signals {
        0 => 0.3,
        1 => 0.6,
        2 => 0.8,
        _ => 0.95,
    }
}

fn plural_days(days: u64) -> &'static str {
    let last_two = days % 100;
    let last = days % 10;
    if (11..=14).contains(&last_two) {
        return "дней";
    }
    match last {
        1 => "день",
        2..=4 => "дня",
        _ => "дней",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: u64 = 24 * 60 * 60 * 1000;

    fn slack() -> IdleInput<'static> {
        IdleInput {
            process_name: "Slack",
            since_interaction_ms: 3 * DAY_MS,
            has_windows: true,
            cpu_ms_while_idle: 300_000,
            written_while_idle: Bytes(2 * 1024 * 1024 * 1024),
            wakeups_while_idle: 40_000,
            memory_now: Bytes(2_100 * 1024 * 1024),
            memory_grew: true,
            established_connections: 0,
            open_user_files: 0,
            child_processes: 0,
            holds_power_request: false,
        }
    }

    fn honest_sleeper() -> IdleInput<'static> {
        IdleInput {
            process_name: "Спокойное приложение",
            since_interaction_ms: 30 * DAY_MS,
            has_windows: true,
            cpu_ms_while_idle: 0,
            written_while_idle: Bytes::ZERO,
            wakeups_while_idle: 0,
            memory_now: Bytes(50 * 1024 * 1024),
            memory_grew: false,
            established_connections: 0,
            open_user_files: 0,
            child_processes: 0,
            holds_power_request: false,
        }
    }

    #[test]
    fn the_verdict_is_concrete_and_measurable() {
        // Не «программа давно запущена», а конкретные цифры.
        let observation = analyze(&slack()).unwrap();
        assert!(observation.summary.contains("Slack"));
        assert!(observation.summary.contains("3 дня"));
        assert!(observation.summary.contains("разбудил процессор 40000 раз"));
    }

    #[test]
    fn an_honestly_sleeping_app_is_left_alone() {
        // Висит месяц, но не тратит ничего. Трогать не за что.
        assert!(analyze(&honest_sleeper()).is_none());
    }

    #[test]
    fn a_server_without_windows_is_not_judged_here() {
        // Postgres может висеть месяцами, и это нормально.
        let mut postgres = slack();
        postgres.process_name = "postgres.exe";
        postgres.has_windows = false;
        assert!(analyze(&postgres).is_none());
    }

    #[test]
    fn recent_use_cancels_everything() {
        let mut recent = slack();
        recent.since_interaction_ms = 2 * 60 * 60 * 1000;
        assert!(analyze(&recent).is_none());
    }

    #[test]
    fn unfinished_work_is_a_stop_signal() {
        let stoppers: [fn(&mut IdleInput<'static>); 4] = [
            |input| input.established_connections = 1,
            |input| input.open_user_files = 1,
            |input| input.child_processes = 1,
            |input| input.holds_power_request = true,
        ];

        for apply in stoppers {
            let mut input = slack();
            apply(&mut input);
            assert!(analyze(&input).is_none(), "стоп-сигнал не остановил вывод");
        }
    }

    #[test]
    fn time_alone_is_never_a_reason() {
        // Ключевое требование ТЗ: время работы само по себе не сигнал.
        let mut ancient = honest_sleeper();
        ancient.since_interaction_ms = 365 * DAY_MS;
        assert!(analyze(&ancient).is_none());
    }

    #[test]
    fn a_single_expensive_signal_is_enough() {
        let mut input = honest_sleeper();
        input.wakeups_while_idle = 50_000;
        let observation = analyze(&input).unwrap();
        assert!(observation.confidence < 0.7, "уверенность завышена");
    }

    #[test]
    fn more_signals_mean_more_confidence() {
        let weak = {
            let mut input = honest_sleeper();
            input.wakeups_while_idle = 50_000;
            analyze(&input).unwrap().confidence
        };
        assert!(analyze(&slack()).unwrap().confidence > weak);
    }

    #[test]
    fn memory_growth_alone_counts_as_a_signal() {
        let mut input = honest_sleeper();
        input.memory_grew = true;
        assert!(analyze(&input).is_some());
    }
}
