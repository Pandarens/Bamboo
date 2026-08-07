//! Базовая линия и период обучения (ТЗ, раздел 10).
//!
//! Аномалия определяется как отклонение от собственной базовой линии машины,
//! а не от абсолютных порогов. Абсолютные пороги оставлены только там,
//! где физика однозначна: рост ошибок носителя, резерв ниже порога
//! производителя.
//!
//! Статистики устойчивые — медиана и межквартильный размах, а не среднее
//! и стандартное отклонение. Распределения здесь сильно скошенные: одна
//! ночь с обновлением Windows уводит среднее так, что порог перестаёт
//! что-либо значить.

/// Период обучения: первые 14 суток Bamboo только наблюдает.
pub const LEARNING_PERIOD_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// Скользящее окно, по которому пересчитывается базовая линия.
pub const WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Минимум точек, ниже которого статистика не считается.
const MIN_SAMPLES: usize = 8;

/// Во сколько межквартильных размахов от медианы начинается аномалия.
///
/// Полтора размаха — общепринятая граница выброса. Ниже неё Bamboo молчит.
const OUTLIER_FACTOR: f64 = 1.5;

/// Устойчивое описание распределения.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Baseline {
    pub median: f64,
    /// Первый квартиль.
    pub q1: f64,
    /// Третий квартиль.
    pub q3: f64,
    pub samples: usize,
}

impl Baseline {
    /// Межквартильный размах.
    pub fn iqr(&self) -> f64 {
        (self.q3 - self.q1).max(0.0)
    }

    /// Граница, выше которой значение считается аномально большим.
    pub fn upper_fence(&self) -> f64 {
        self.q3 + OUTLIER_FACTOR * self.iqr()
    }

    /// Является ли значение аномально большим.
    ///
    /// На вырожденном распределении, где все значения одинаковы, размах
    /// равен нулю и границей становится сама медиана. Тогда аномалией
    /// считается только заметное превышение, а не любое отличие: иначе
    /// стабильное приложение начнёт «отклоняться» от 100 к 101.
    pub fn is_high(&self, value: f64) -> bool {
        if self.iqr() == 0.0 {
            return value > self.median * 1.5 && value > self.median;
        }
        value > self.upper_fence()
    }

    /// Во сколько раз значение отличается от типичного.
    pub fn ratio(&self, value: f64) -> Option<f64> {
        (self.median > 0.0).then(|| value / self.median)
    }
}

/// Считает базовую линию по ряду значений.
///
/// Возвращает `None`, если точек слишком мало: по трём замерам «типичное»
/// не определяется, а притворяться, что определяется, нельзя.
pub fn compute(values: &[f64]) -> Option<Baseline> {
    if values.len() < MIN_SAMPLES {
        return None;
    }

    let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.len() < MIN_SAMPLES {
        return None;
    }
    sorted.sort_by(f64::total_cmp);

    Some(Baseline {
        median: quantile(&sorted, 0.50),
        q1: quantile(&sorted, 0.25),
        q3: quantile(&sorted, 0.75),
        samples: sorted.len(),
    })
}

/// Квантиль с линейной интерполяцией по отсортированному ряду.
fn quantile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }

    let position = fraction * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;

    if lower == upper {
        return sorted[lower];
    }
    let weight = position - lower as f64;
    sorted[lower] * (1.0 - weight) + sorted[upper] * weight
}

/// Состояние обучения.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Learning {
    pub installed_at_unix_ms: i64,
}

impl Learning {
    /// Идёт ли ещё период обучения.
    pub fn in_progress(&self, now_ms: i64) -> bool {
        now_ms - self.installed_at_unix_ms < LEARNING_PERIOD_MS
    }

    /// Сколько суток осталось. Для строки «изучаю систему, осталось 9 дней».
    pub fn days_left(&self, now_ms: i64) -> i64 {
        let left = LEARNING_PERIOD_MS - (now_ms - self.installed_at_unix_ms);
        (left.max(0) + 86_399_999) / 86_400_000
    }

    pub fn describe(&self, now_ms: i64) -> String {
        if !self.in_progress(now_ms) {
            return "система изучена, базовая линия сформирована".to_string();
        }
        let days = self.days_left(now_ms);
        format!("изучаю систему, осталось {days} {}", plural_days(days))
    }
}

fn plural_days(days: i64) -> &'static str {
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

    const DAY: i64 = 86_400_000;

    fn steady() -> Vec<f64> {
        vec![10.0, 12.0, 11.0, 13.0, 10.0, 12.0, 11.0, 14.0, 12.0, 11.0]
    }

    #[test]
    fn a_typical_value_is_the_median() {
        let baseline = compute(&steady()).unwrap();
        assert!((baseline.median - 11.5).abs() < 0.6, "{}", baseline.median);
        assert!(baseline.q1 < baseline.median);
        assert!(baseline.q3 > baseline.median);
    }

    #[test]
    fn one_freak_night_does_not_move_the_baseline() {
        // Обновление Windows: одна ночь с записью в двадцать раз выше.
        // Среднее уехало бы вдвое, медиана — нет.
        let mut values = steady();
        values.push(400.0);

        let baseline = compute(&values).unwrap();
        assert!(
            baseline.median < 15.0,
            "медиану утащило до {}",
            baseline.median
        );
    }

    #[test]
    fn a_normal_value_is_not_an_anomaly() {
        let baseline = compute(&steady()).unwrap();
        assert!(!baseline.is_high(13.0));
    }

    #[test]
    fn a_real_outlier_is_caught() {
        let baseline = compute(&steady()).unwrap();
        assert!(baseline.is_high(100.0));
        assert!(baseline.ratio(100.0).unwrap() > 8.0);
    }

    #[test]
    fn a_perfectly_stable_series_does_not_flag_every_wobble() {
        // Все значения одинаковы, размах нулевой. Отклонение на единицу
        // не должно превращаться в аномалию.
        let flat = vec![100.0; 20];
        let baseline = compute(&flat).unwrap();

        assert_eq!(baseline.iqr(), 0.0);
        assert!(!baseline.is_high(101.0));
        assert!(!baseline.is_high(140.0));
        assert!(baseline.is_high(200.0));
    }

    #[test]
    fn too_few_samples_give_no_baseline() {
        assert!(compute(&[1.0, 2.0, 3.0]).is_none());
        assert!(compute(&[]).is_none());
    }

    #[test]
    fn broken_numbers_do_not_poison_the_baseline() {
        let mut values = steady();
        values.push(f64::NAN);
        values.push(f64::INFINITY);

        let baseline = compute(&values).unwrap();
        assert!(baseline.median.is_finite());
        assert_eq!(baseline.samples, steady().len());
    }

    #[test]
    fn the_learning_period_lasts_two_weeks() {
        let learning = Learning {
            installed_at_unix_ms: 0,
        };
        assert!(learning.in_progress(5 * DAY));
        assert!(!learning.in_progress(15 * DAY));
    }

    #[test]
    fn the_countdown_reads_naturally_in_russian() {
        let learning = Learning {
            installed_at_unix_ms: 0,
        };
        assert!(learning.describe(5 * DAY).contains("осталось 9 дней"));
        assert!(learning.describe(12 * DAY).contains("осталось 2 дня"));
        assert!(learning.describe(13 * DAY).contains("осталось 1 день"));
        assert!(learning.describe(20 * DAY).contains("система изучена"));
    }

    #[test]
    fn plural_forms_cover_the_teens() {
        assert_eq!(plural_days(1), "день");
        assert_eq!(plural_days(3), "дня");
        assert_eq!(plural_days(5), "дней");
        assert_eq!(plural_days(11), "дней");
        assert_eq!(plural_days(14), "дней");
        assert_eq!(plural_days(21), "день");
    }
}
