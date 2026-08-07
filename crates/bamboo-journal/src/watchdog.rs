//! Сторожевой таймер (ТЗ, раздел 12.4).
//!
//! Ключевой механизм, отсутствующий у аналогов. Он превращает набор твиков
//! в систему, обучающуюся на конкретной машине: после каждого действия
//! открывается окно наблюдения, и если система начала вести себя хуже,
//! действие откатывается само.
//!
//! Логика здесь чистая — на вход снимки метрик до и после, на выход решение.

/// Окно наблюдения после применения действия.
pub const WINDOW_MS: i64 = 48 * 60 * 60 * 1000;

/// Во сколько раз должно вырасти число ошибок, чтобы это считалось
/// деградацией.
const ERROR_GROWTH_FACTOR: f64 = 2.0;
/// Минимальное число ошибок в базовой линии, ниже которого рост
/// не показателен: с одной ошибки до двух — это не деградация, а шум.
const MIN_BASELINE_ERRORS: u32 = 5;
/// Насколько может вырасти время загрузки.
const BOOT_GROWTH_RATIO: f64 = 1.20;

/// Метрики, зафиксированные на момент применения действия.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Baseline {
    /// Число записей уровня Error в журналах System и Application за сутки.
    pub daily_errors: u32,
    /// Типичное время загрузки.
    pub boot_ms: u64,
}

/// Что наблюдается сейчас.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    pub daily_errors: u32,
    pub boot_ms: u64,
    /// Появились события Application Error или Windows Error Reporting
    /// для затронутого приложения.
    pub app_crashes: u32,
    /// Пользователь вручную запустил то, что Bamboo усыпило или остановило.
    pub user_restarted_target: bool,
    /// Пользователь вручную разморозил процесс.
    pub user_unfroze_target: bool,
}

/// Почему сработал автооткат.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevertReason {
    ErrorsGrew { before: u32, after: u32 },
    ApplicationCrashed { count: u32 },
    BootSlowed { before_ms: u64, after_ms: u64 },
    UserRestartedIt,
    UserUnfrozeIt,
}

impl RevertReason {
    pub fn describe(&self) -> String {
        match self {
            RevertReason::ErrorsGrew { before, after } => {
                format!("число ошибок в журналах системы выросло с {before} до {after} за сутки")
            }
            RevertReason::ApplicationCrashed { count } => {
                format!("приложение аварийно завершилось {count} раз после изменения")
            }
            RevertReason::BootSlowed {
                before_ms,
                after_ms,
            } => format!(
                "загрузка замедлилась с {:.0} до {:.0} секунд",
                *before_ms as f64 / 1000.0,
                *after_ms as f64 / 1000.0
            ),
            RevertReason::UserRestartedIt => {
                "вы вручную запустили то, что Bamboo усыпил".to_string()
            }
            RevertReason::UserUnfrozeIt => "вы вручную разморозили процесс".to_string(),
        }
    }
}

/// Проверяет, не пора ли откатывать.
///
/// Действия пользователя проверяются первыми: если человек вручную вернул
/// то, что Bamboo тронул, обсуждать больше нечего — он высказался яснее
/// любых метрик.
pub fn should_revert(baseline: &Baseline, observed: &Observed) -> Option<RevertReason> {
    if observed.user_restarted_target {
        return Some(RevertReason::UserRestartedIt);
    }
    if observed.user_unfroze_target {
        return Some(RevertReason::UserUnfrozeIt);
    }

    if observed.app_crashes > 0 {
        return Some(RevertReason::ApplicationCrashed {
            count: observed.app_crashes,
        });
    }

    if baseline.daily_errors >= MIN_BASELINE_ERRORS
        && observed.daily_errors as f64 > baseline.daily_errors as f64 * ERROR_GROWTH_FACTOR
    {
        return Some(RevertReason::ErrorsGrew {
            before: baseline.daily_errors,
            after: observed.daily_errors,
        });
    }

    if baseline.boot_ms > 0 && observed.boot_ms as f64 > baseline.boot_ms as f64 * BOOT_GROWTH_RATIO
    {
        return Some(RevertReason::BootSlowed {
            before_ms: baseline.boot_ms,
            after_ms: observed.boot_ms,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> Baseline {
        Baseline {
            daily_errors: 10,
            boot_ms: 30_000,
        }
    }

    #[test]
    fn a_healthy_system_keeps_the_change() {
        let observed = Observed {
            daily_errors: 11,
            boot_ms: 31_000,
            ..Default::default()
        };
        assert_eq!(should_revert(&baseline(), &observed), None);
    }

    #[test]
    fn doubling_the_error_count_triggers_a_revert() {
        let observed = Observed {
            daily_errors: 25,
            boot_ms: 30_000,
            ..Default::default()
        };
        assert!(matches!(
            should_revert(&baseline(), &observed),
            Some(RevertReason::ErrorsGrew { .. })
        ));
    }

    #[test]
    fn noise_on_a_quiet_system_is_not_a_regression() {
        // С одной ошибки до трёх — формально втрое, по сути ничего.
        let quiet = Baseline {
            daily_errors: 1,
            boot_ms: 30_000,
        };
        let observed = Observed {
            daily_errors: 3,
            boot_ms: 30_000,
            ..Default::default()
        };
        assert_eq!(should_revert(&quiet, &observed), None);
    }

    #[test]
    fn an_application_crash_is_enough_on_its_own() {
        let observed = Observed {
            daily_errors: 10,
            boot_ms: 30_000,
            app_crashes: 1,
            ..Default::default()
        };
        assert!(matches!(
            should_revert(&baseline(), &observed),
            Some(RevertReason::ApplicationCrashed { count: 1 })
        ));
    }

    #[test]
    fn a_slower_boot_triggers_a_revert() {
        let observed = Observed {
            daily_errors: 10,
            boot_ms: 40_000,
            ..Default::default()
        };
        assert!(matches!(
            should_revert(&baseline(), &observed),
            Some(RevertReason::BootSlowed { .. })
        ));
    }

    #[test]
    fn the_user_undoing_it_by_hand_outranks_every_metric() {
        // Человек высказался яснее любых метрик.
        let observed = Observed {
            daily_errors: 1,
            boot_ms: 10_000,
            user_restarted_target: true,
            ..Default::default()
        };
        assert_eq!(
            should_revert(&baseline(), &observed),
            Some(RevertReason::UserRestartedIt)
        );
    }

    #[test]
    fn unfreezing_by_hand_is_also_a_verdict() {
        let observed = Observed {
            user_unfroze_target: true,
            ..Default::default()
        };
        assert_eq!(
            should_revert(&baseline(), &observed),
            Some(RevertReason::UserUnfrozeIt)
        );
    }

    #[test]
    fn without_a_baseline_nothing_is_judged() {
        // Базовой линии нет — сравнивать не с чем, откатывать не за что.
        let empty = Baseline::default();
        let observed = Observed {
            daily_errors: 100,
            boot_ms: 120_000,
            ..Default::default()
        };
        assert_eq!(should_revert(&empty, &observed), None);
    }

    #[test]
    fn every_reason_explains_itself() {
        let reasons = [
            RevertReason::ErrorsGrew {
                before: 10,
                after: 30,
            },
            RevertReason::ApplicationCrashed { count: 2 },
            RevertReason::BootSlowed {
                before_ms: 30_000,
                after_ms: 45_000,
            },
            RevertReason::UserRestartedIt,
            RevertReason::UserUnfrozeIt,
        ];
        for reason in reasons {
            assert!(!reason.describe().is_empty());
        }
    }
}
