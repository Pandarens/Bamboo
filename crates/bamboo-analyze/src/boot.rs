//! Регрессии времени загрузки (ТЗ, раздел 9.8).
//!
//! Сравниваем не с абсолютным «хорошим» временем загрузки — его не существует,
//! у каждой машины своё, — а с собственной историей этой машины.

use crate::observation::{Observation, ObservationKind, Severity};

/// Одна загрузка.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootPoint {
    pub at_unix_ms: i64,
    pub total_ms: u64,
}

/// Сколько последних загрузок считаем «недавними».
const RECENT_COUNT: usize = 5;
/// Минимум загрузок в истории, иначе сравнивать не с чем.
const MIN_HISTORY: usize = 10;
/// Насколько должно вырасти, чтобы это было регрессией.
const REGRESSION_RATIO: f64 = 1.20;
/// И насколько в абсолютных секундах — чтобы не придираться к мелочи.
const REGRESSION_MIN_MS: u64 = 5_000;

/// Заключение о времени загрузки.
#[derive(Clone, Debug)]
pub struct BootVerdict {
    /// Типичное время загрузки за всю историю.
    pub baseline_ms: Option<u64>,
    /// Типичное время последних загрузок.
    pub recent_ms: Option<u64>,
    pub observation: Option<Observation>,
}

/// Анализирует историю загрузок.
///
/// `history` — от свежих к старым, как отдаёт журнал.
/// `culprits` — пары «компонент, сколько он добавил», для атрибуции.
pub fn analyze(history: &[BootPoint], culprits: &[(String, u64)]) -> BootVerdict {
    if history.len() < MIN_HISTORY {
        return BootVerdict {
            baseline_ms: median(&history.iter().map(|b| b.total_ms).collect::<Vec<_>>()),
            recent_ms: None,
            // История короткая — молчим. Сказать «стало хуже» по трём
            // загрузкам нельзя, а пугать без оснований запрещено.
            observation: None,
        };
    }

    let recent: Vec<u64> = history[..RECENT_COUNT].iter().map(|b| b.total_ms).collect();
    let older: Vec<u64> = history[RECENT_COUNT..].iter().map(|b| b.total_ms).collect();

    // Медиана, а не среднее: одна загрузка после большого обновления Windows
    // занимает втрое дольше обычной и утаскивает среднее за собой.
    let recent_ms = median(&recent);
    let baseline_ms = median(&older);

    let observation = match (recent_ms, baseline_ms) {
        (Some(recent), Some(baseline)) if is_regression(recent, baseline) => {
            Some(regression(recent, baseline, culprits))
        }
        (Some(recent), Some(_)) => Some(Observation::calm(
            ObservationKind::BootRegression,
            format!(
                "Система загружается за {:.0} с — как обычно",
                recent as f64 / 1000.0
            ),
        )),
        _ => None,
    };

    BootVerdict {
        baseline_ms,
        recent_ms,
        observation,
    }
}

fn is_regression(recent: u64, baseline: u64) -> bool {
    recent as f64 > baseline as f64 * REGRESSION_RATIO
        && recent.saturating_sub(baseline) >= REGRESSION_MIN_MS
}

fn regression(recent: u64, baseline: u64, culprits: &[(String, u64)]) -> Observation {
    let grew = recent.saturating_sub(baseline);

    let observation = Observation::new(
        ObservationKind::BootRegression,
        Severity::Notice,
        0.85,
        format!(
            "Загрузка стала дольше на {:.0} с: было около {:.0} с, стало {:.0} с",
            grew as f64 / 1000.0,
            baseline as f64 / 1000.0,
            recent as f64 / 1000.0
        ),
    );

    if culprits.is_empty() {
        return observation.with_detail(
            "Windows не назвала конкретный компонент. Стоит посмотреть, \
             что появилось в автозагрузке за последнее время.",
        );
    }

    let mut top: Vec<&(String, u64)> = culprits.iter().collect();
    top.sort_by_key(|(_, ms)| core::cmp::Reverse(*ms));
    let listed: Vec<String> = top
        .iter()
        .take(3)
        .map(|(name, ms)| format!("{name} — {:.1} с", *ms as f64 / 1000.0))
        .collect();

    observation.with_detail(format!(
        "Больше всего времени добавили: {}",
        listed.join(", ")
    ))
}

/// Медиана. Устойчива к выбросам, в отличие от среднего.
pub fn median(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    Some(if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) / 2
    } else {
        sorted[middle]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// История от свежих к старым, время в секундах.
    fn history(seconds: &[u64]) -> Vec<BootPoint> {
        seconds
            .iter()
            .enumerate()
            .map(|(index, secs)| BootPoint {
                at_unix_ms: 1_780_000_000_000 - index as i64 * 86_400_000,
                total_ms: secs * 1000,
            })
            .collect()
    }

    #[test]
    fn a_stable_machine_is_told_everything_is_fine() {
        let history = history(&[31, 30, 32, 29, 31, 30, 31, 33, 30, 31, 32, 30]);
        let verdict = analyze(&history, &[]);

        let observation = verdict.observation.unwrap();
        assert_eq!(observation.severity, Severity::Calm);
        assert!(observation.summary.contains("как обычно"));
    }

    #[test]
    fn a_regression_is_reported_with_numbers() {
        // Было около 30 с, стало около 45.
        let history = history(&[45, 46, 44, 45, 47, 30, 31, 29, 30, 31, 30, 32]);
        let verdict = analyze(&history, &[]);

        let observation = verdict.observation.unwrap();
        assert_eq!(observation.severity, Severity::Notice);
        assert!(observation.summary.contains("дольше на 15 с"));
        assert_eq!(verdict.baseline_ms, Some(30_000));
        assert_eq!(verdict.recent_ms, Some(45_000));
    }

    #[test]
    fn culprits_are_named_when_windows_knows_them() {
        let history = history(&[45, 46, 44, 45, 47, 30, 31, 29, 30, 31, 30, 32]);
        let culprits = vec![
            ("Steam Client Bootstrapper".to_string(), 9_400),
            ("OneDrive".to_string(), 2_100),
        ];
        let verdict = analyze(&history, &culprits);

        let detail = verdict.observation.unwrap().detail.unwrap();
        assert!(detail.starts_with("Больше всего времени добавили: Steam"));
    }

    #[test]
    fn a_small_slowdown_is_not_worth_mentioning() {
        // Плюс полторы секунды — в пределах разброса.
        let history = history(&[32, 31, 32, 31, 32, 30, 31, 30, 30, 31, 30, 30]);
        assert_eq!(
            analyze(&history, &[]).observation.unwrap().severity,
            Severity::Calm
        );
    }

    #[test]
    fn a_relative_jump_on_a_fast_machine_needs_absolute_seconds_too() {
        // Загрузка выросла с 8 до 11 секунд: относительно много,
        // по ощущениям — незаметно. Молчим.
        let history = history(&[11, 11, 11, 11, 11, 8, 8, 8, 8, 8, 8, 8]);
        assert_eq!(
            analyze(&history, &[]).observation.unwrap().severity,
            Severity::Calm
        );
    }

    #[test]
    fn one_freak_boot_does_not_create_a_regression() {
        // Одна загрузка после крупного обновления заняла три минуты.
        let history = history(&[180, 30, 31, 29, 30, 31, 30, 32, 30, 31, 30, 29]);
        assert_eq!(
            analyze(&history, &[]).observation.unwrap().severity,
            Severity::Calm,
            "медиана обязана пережить один выброс"
        );
    }

    #[test]
    fn a_short_history_produces_no_verdict() {
        let verdict = analyze(&history(&[30, 31, 32]), &[]);
        assert!(verdict.observation.is_none());
        assert_eq!(verdict.baseline_ms, Some(31_000));
    }

    #[test]
    fn empty_history_does_not_panic() {
        let verdict = analyze(&[], &[]);
        assert!(verdict.observation.is_none());
        assert_eq!(verdict.baseline_ms, None);
    }

    #[test]
    fn median_handles_both_parities() {
        assert_eq!(median(&[3, 1, 2]), Some(2));
        assert_eq!(median(&[4, 1, 2, 3]), Some(2));
        assert_eq!(median(&[]), None);
    }
}
