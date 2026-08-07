//! Фоновые всплески процессора (ТЗ, раздел 9.2).
//!
//! Смысл анализатора не в том, чтобы найти процесс, который грузит ядро —
//! это умеет диспетчер задач. Смысл в отделении работы от фоновой возни:
//! сборка проекта и рендер видео тоже грузят процессор, но это ровно то,
//! чего человек хочет. Поэтому условий пять, и все должны сойтись.

use crate::observation::{Observation, ObservationKind, Severity};
use crate::origin::Origin;

/// Точка ряда: время в миллисекундах и доля одного ядра.
pub type Load = (u64, f32);

/// Всплеском считается больше 20% одного ядра.
const SPIKE_THRESHOLD: f32 = 0.20;
/// И держаться это должно дольше 20 секунд.
const MIN_DURATION_MS: u64 = 20_000;
/// Пользователь считается ушедшим после трёх минут без ввода.
const MIN_IDLE_MS: u64 = 3 * 60 * 1000;

/// Что известно на момент анализа.
pub struct SpikeInput<'a> {
    pub process_name: &'a str,
    pub origin: Origin,
    /// Загрузка по времени, доля одного ядра.
    pub load: &'a [Load],
    /// Сколько времени человек не трогал компьютер.
    pub user_idle_ms: u64,
    /// Менялось ли активное окно за время всплеска.
    pub foreground_changed: bool,
    /// Держит ли кто-нибудь экран включённым: признак воспроизведения
    /// или длительной операции с прогрессом.
    pub display_required: bool,
    /// Полноэкранное приложение или режим «не беспокоить».
    pub fullscreen_or_busy: bool,
    /// Сколько этот процесс суммарно потратил за неделю, миллисекунды.
    pub weekly_cpu_ms: Option<u64>,
}

/// Ищет фоновый всплеск.
///
/// Пустой результат — нормальный и самый частый исход.
pub fn analyze(input: &SpikeInput<'_>) -> Option<Observation> {
    // Человек за компьютером — значит, это его работа, а не фон.
    if input.user_idle_ms < MIN_IDLE_MS {
        return None;
    }
    // Активное окно менялось: кто-то всё-таки был рядом.
    if input.foreground_changed {
        return None;
    }
    // Экран удерживают включённым — идёт воспроизведение или длительная
    // операция, которую человек запустил осознанно.
    if input.display_required {
        return None;
    }
    if input.fullscreen_or_busy {
        return None;
    }

    let (duration_ms, peak, started_at) = longest_spike(input.load)?;
    if duration_ms < MIN_DURATION_MS {
        return None;
    }

    let minutes = duration_ms as f64 / 60_000.0;
    let duration_text = if minutes >= 1.0 {
        format!("{minutes:.0} мин")
    } else {
        format!("{} с", duration_ms / 1000)
    };

    let summary = format!(
        "{}: {duration_text} нагрузки на {:.0}% ядра, пока вас не было. Источник — {}",
        input.process_name,
        peak * 100.0,
        input.origin.describe()
    );

    let mut detail = format!(
        "Всплеск начался через {} после начала наблюдения.",
        moment(started_at)
    );
    if let Some(weekly) = input.weekly_cpu_ms {
        detail.push_str(&format!(
            " За неделю этот процесс занял процессор суммарно на {:.0} мин.",
            weekly as f64 / 60_000.0
        ));
    }
    if !input.origin.is_actionable() {
        detail.push_str(" Вмешиваться не стоит: это штатная работа системы, она закончится сама.");
    }

    Some(
        Observation::new(
            ObservationKind::BackgroundCpu,
            // Не тревога: система имеет право работать в простое.
            // Это сообщение «вот куда уходило время», а не «у вас проблема».
            Severity::Notice,
            confidence(duration_ms, peak),
            summary,
        )
        .with_detail(detail),
    )
}

/// Самый длинный непрерывный участок выше порога.
///
/// Возвращает длительность, максимум на нём и время начала.
fn longest_spike(load: &[Load]) -> Option<(u64, f32, u64)> {
    if load.len() < 2 {
        return None;
    }

    let mut best: Option<(u64, f32, u64)> = None;
    let mut run_start: Option<usize> = None;

    for index in 0..load.len() {
        let above = load[index].1 >= SPIKE_THRESHOLD;

        match (above, run_start) {
            (true, None) => run_start = Some(index),
            (false, Some(start)) => {
                consider(&mut best, load, start, index - 1);
                run_start = None;
            }
            _ => {}
        }
    }

    if let Some(start) = run_start {
        consider(&mut best, load, start, load.len() - 1);
    }

    best
}

fn consider(best: &mut Option<(u64, f32, u64)>, load: &[Load], start: usize, end: usize) {
    if end <= start {
        return;
    }
    let duration = load[end].0.saturating_sub(load[start].0);
    let peak = load[start..=end]
        .iter()
        .map(|(_, value)| *value)
        .fold(0.0f32, f32::max);

    if best
        .map(|(best_duration, _, _)| duration > best_duration)
        .unwrap_or(true)
    {
        *best = Some((duration, peak, load[start].0));
    }
}

/// Уверенность растёт с длительностью и высотой всплеска: минутная
/// загрузка целого ядра убедительнее двадцати секунд на четверти.
fn confidence(duration_ms: u64, peak: f32) -> f32 {
    let by_duration = (duration_ms as f32 / 300_000.0).min(1.0);
    let by_peak = (peak / 1.0).min(1.0);
    (0.5 + 0.25 * by_duration + 0.25 * by_peak).clamp(0.0, 1.0)
}

fn moment(at_ms: u64) -> String {
    let minutes = at_ms / 60_000;
    if minutes == 0 {
        format!("{} с", at_ms / 1000)
    } else {
        format!("{minutes} мин")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ряд с шагом в секунду: `seconds` точек с заданной загрузкой.
    fn steady(seconds: u64, value: f32) -> Vec<Load> {
        (0..=seconds).map(|s| (s * 1000, value)).collect()
    }

    fn input<'a>(load: &'a [Load]) -> SpikeInput<'a> {
        SpikeInput {
            process_name: "CompatTelRunner.exe",
            origin: Origin::ScheduledTask(Some(
                "\\Microsoft\\Windows\\Application Experience\\Microsoft Compatibility Appraiser"
                    .to_string(),
            )),
            load,
            user_idle_ms: 30 * 60 * 1000,
            foreground_changed: false,
            display_required: false,
            fullscreen_or_busy: false,
            weekly_cpu_ms: None,
        }
    }

    #[test]
    fn a_night_time_spike_is_reported_with_its_source() {
        let load = steady(240, 0.9);
        let observation = analyze(&input(&load)).unwrap();

        assert_eq!(observation.kind, ObservationKind::BackgroundCpu);
        assert!(observation.summary.contains("CompatTelRunner.exe"));
        assert!(observation.summary.contains("Compatibility Appraiser"));
        assert!(observation.summary.contains("4 мин"));
    }

    #[test]
    fn a_short_burst_is_ignored() {
        // Десять секунд — это не всплеск, а обычная жизнь процесса.
        let load = steady(10, 0.9);
        assert!(analyze(&input(&load)).is_none());
    }

    #[test]
    fn a_quiet_process_is_ignored() {
        let load = steady(300, 0.05);
        assert!(analyze(&input(&load)).is_none());
    }

    #[test]
    fn work_done_while_the_user_is_present_is_not_a_spike() {
        // Сборка проекта: человек за компьютером, это его работа.
        let load = steady(600, 1.0);
        let mut input = input(&load);
        input.user_idle_ms = 5_000;
        assert!(analyze(&input).is_none());
    }

    #[test]
    fn video_playback_is_not_a_spike() {
        // Экран удерживают включённым — идёт воспроизведение.
        let load = steady(600, 0.5);
        let mut input = input(&load);
        input.display_required = true;
        assert!(analyze(&input).is_none());
    }

    #[test]
    fn a_game_in_the_foreground_is_not_a_spike() {
        let load = steady(600, 1.0);
        let mut input = input(&load);
        input.fullscreen_or_busy = true;
        assert!(analyze(&input).is_none());
    }

    #[test]
    fn someone_switching_windows_cancels_the_verdict() {
        let load = steady(600, 1.0);
        let mut input = input(&load);
        input.foreground_changed = true;
        assert!(analyze(&input).is_none());
    }

    #[test]
    fn the_longest_run_wins_over_earlier_short_ones() {
        let mut load = steady(5, 0.9); // короткий всплеск
        load.extend((6..=20).map(|s| (s * 1000, 0.01))); // затишье
        load.extend((21..=140).map(|s| (s * 1000, 0.6))); // длинный всплеск

        let observation = analyze(&input(&load)).unwrap();
        assert!(
            observation.summary.contains("2 мин"),
            "{}",
            observation.summary
        );
        assert!(observation.summary.contains("60% ядра"));
    }

    #[test]
    fn system_work_is_reported_but_not_offered_for_action() {
        let load = steady(240, 0.9);
        let mut input = input(&load);
        input.origin = Origin::WindowsUpdate;

        let detail = analyze(&input).unwrap().detail.unwrap();
        assert!(detail.contains("закончится сама"));
    }

    #[test]
    fn weekly_total_enriches_the_report() {
        let load = steady(240, 0.9);
        let mut input = input(&load);
        input.weekly_cpu_ms = Some(3_600_000);

        let detail = analyze(&input).unwrap().detail.unwrap();
        assert!(detail.contains("60 мин"));
    }

    #[test]
    fn confidence_grows_with_size_and_length() {
        let short = analyze(&input(&steady(25, 0.25))).unwrap().confidence;
        let long = analyze(&input(&steady(600, 1.0))).unwrap().confidence;
        assert!(long > short);
        assert!((0.0..=1.0).contains(&short));
    }

    #[test]
    fn an_empty_series_produces_nothing() {
        assert!(analyze(&input(&[])).is_none());
        assert!(analyze(&input(&[(0, 0.9)])).is_none());
    }
}
