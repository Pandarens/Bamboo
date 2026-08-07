//! Расход ресурса SSD (ТЗ, раздел 9.4).
//!
//! Главное здесь — поведение по умолчанию. У типичного пользователя выходит
//! 10–20 ГБ записи в сутки при ресурсе в сотни терабайт, то есть накопителя
//! хватит на десятилетия. Честный вывод в подавляющем большинстве случаев:
//! «всё в порядке, израсходовано 2% ресурса». Утилиты, которые на этих же
//! данных рисуют тревогу, делают это ради продажи, а не ради пользы.

use bamboo_core::storage::SmartHealth;
use bamboo_core::Bytes;

use crate::observation::{Observation, ObservationKind, Severity};
use crate::tbw::{rating_for, TbwRating};

/// Порог суточной записи, выше которого это почти всегда аномалия.
const DAILY_WRITE_ALARM: Bytes = Bytes(200 * 1_000_000_000);
/// Во сколько раз запись должна превысить базовую линию машины.
const BASELINE_MULTIPLIER: f64 = 3.0;
/// Проекция короче этого срока — повод предупредить.
const YEARS_ALARM: f64 = 3.0;

/// Что известно о накопителе на момент анализа.
pub struct WearInput<'a> {
    pub drive_name: &'a str,
    pub capacity: Bytes,
    pub health: &'a SmartHealth,
    /// Среднесуточная запись за последнюю неделю.
    pub daily_write: Option<Bytes>,
    /// Собственная базовая линия машины: типичная суточная запись.
    pub baseline_daily_write: Option<Bytes>,
    /// Вырос ли счётчик ошибок носителя с прошлого чтения.
    pub media_errors_grew: bool,
    /// Кто больше всех писал за период — для атрибуции в предупреждении.
    pub top_writers: &'a [(String, Bytes)],
}

/// Заключение о состоянии накопителя.
#[derive(Clone, Debug)]
pub struct WearVerdict {
    pub rating: TbwRating,
    /// Доля выработанного ресурса в процентах.
    pub used_percent: Option<f64>,
    /// Сколько лет осталось при текущем темпе записи.
    pub years_left: Option<f64>,
    pub observation: Observation,
}

pub fn analyze(input: &WearInput<'_>) -> WearVerdict {
    let rating = rating_for(input.drive_name, input.capacity);
    let used_percent = used_percent(input, &rating);
    let years_left = years_left(input, &rating, used_percent);

    let mut alarms: Vec<String> = Vec::new();

    // Прямое предупреждение контроллера идёт первым и без смягчений:
    // это не наша интерпретация, а сообщение самого устройства.
    if let Some(warning) = input.health.critical_warning {
        for reason in warning.reasons() {
            alarms.push(reason.to_string());
        }
    }

    if input.media_errors_grew {
        alarms.push("выросло число ошибок целостности носителя".to_string());
    }

    if input.health.spare_below_threshold() {
        let spare = input.health.available_spare.unwrap_or(0);
        let threshold = input.health.available_spare_threshold.unwrap_or(0);
        alarms.push(format!(
            "резервных блоков осталось {spare}% при пороге производителя {threshold}%"
        ));
    }

    if let Some(daily) = input.daily_write {
        if daily > DAILY_WRITE_ALARM {
            alarms.push(format!("на диск пишется {daily} в сутки"));
        } else if let Some(baseline) = input.baseline_daily_write {
            let grew = baseline.as_u64() > 0
                && daily.as_u64() as f64 > baseline.as_u64() as f64 * BASELINE_MULTIPLIER;
            if grew {
                alarms.push(format!(
                    "запись выросла до {daily} в сутки против обычных {baseline}"
                ));
            }
        }
    }

    if years_left.is_some_and(|years| years < YEARS_ALARM) {
        alarms.push(format!(
            "при таком темпе ресурса хватит примерно на {:.1} года",
            years_left.unwrap()
        ));
    }

    let observation = if alarms.is_empty() {
        calm(input, used_percent, years_left, &rating)
    } else {
        warn(input, &alarms, &rating)
    };

    WearVerdict {
        rating,
        used_percent,
        years_left,
        observation,
    }
}

/// Оценка контроллера точнее нашей арифметики: он знает и о служебных
/// записях, и о реальном износе ячеек. Считаем сами только если её нет.
fn used_percent(input: &WearInput<'_>, rating: &TbwRating) -> Option<f64> {
    if let Some(percent) = input.health.wear_percent() {
        return Some(percent as f64);
    }
    let written = input.health.data_written?;
    if rating.total.as_u64() == 0 {
        return None;
    }
    Some(written.as_u64() as f64 / rating.total.as_u64() as f64 * 100.0)
}

fn years_left(input: &WearInput<'_>, rating: &TbwRating, used_percent: Option<f64>) -> Option<f64> {
    let daily = input.daily_write?;
    if daily.as_u64() == 0 || rating.total.as_u64() == 0 {
        return None;
    }

    // Остаток считаем от доли износа, если она известна: она учитывает
    // и то, что накопитель писал до установки Bamboo.
    let remaining = match used_percent {
        Some(percent) => rating.total.as_u64() as f64 * (1.0 - percent / 100.0),
        None => (rating.total - input.health.data_written.unwrap_or(Bytes::ZERO)).as_u64() as f64,
    };

    if remaining <= 0.0 {
        return Some(0.0);
    }
    Some(remaining / (daily.as_u64() as f64 * 365.0))
}

fn calm(
    input: &WearInput<'_>,
    used_percent: Option<f64>,
    years_left: Option<f64>,
    rating: &TbwRating,
) -> Observation {
    let summary = match used_percent {
        Some(percent) => format!(
            "С накопителем {} всё в порядке, израсходовано {percent:.0}% ресурса",
            input.drive_name
        ),
        None => format!(
            "С накопителем {} всё в порядке. Долю выработанного ресурса \
             этот накопитель не сообщает",
            input.drive_name
        ),
    };

    let mut detail = String::new();
    if let (Some(years), Some(daily)) = (years_left, input.daily_write) {
        if years > 50.0 {
            detail.push_str(&format!(
                "Пишется {daily} в сутки. При таком темпе ресурс переживёт и сам накопитель, \
                 и, скорее всего, компьютер."
            ));
        } else {
            detail.push_str(&format!(
                "Пишется {daily} в сутки, ресурса хватит примерно на {years:.0} лет."
            ));
        }
    }
    if rating.is_estimate {
        if !detail.is_empty() {
            detail.push(' ');
        }
        detail.push_str(
            "Паспортный ресурс этой модели неизвестен, взята консервативная \
             оценка 300 ТБ на терабайт ёмкости.",
        );
    }

    let observation = Observation::calm(ObservationKind::SsdWear, summary);
    if detail.is_empty() {
        observation
    } else {
        observation.with_detail(detail)
    }
}

fn warn(input: &WearInput<'_>, alarms: &[String], rating: &TbwRating) -> Observation {
    let summary = format!("Накопитель {}: {}", input.drive_name, alarms.join("; "));

    let mut detail = String::new();
    if !input.top_writers.is_empty() {
        detail.push_str("Больше всего писали: ");
        let writers: Vec<String> = input
            .top_writers
            .iter()
            .map(|(name, bytes)| format!("{name} — {bytes}"))
            .collect();
        detail.push_str(&writers.join(", "));
        detail.push('.');
    }
    if rating.is_estimate {
        if !detail.is_empty() {
            detail.push(' ');
        }
        detail.push_str("Паспортный ресурс модели неизвестен, проекция приблизительная.");
    }

    let observation = Observation::new(
        ObservationKind::SsdWear,
        Severity::Warning,
        // Прямое предупреждение контроллера — факт, а не догадка.
        if input.health.critical_warning.is_some_and(|w| !w.is_clear()) {
            1.0
        } else {
            0.8
        },
        summary,
    );

    if detail.is_empty() {
        observation
    } else {
        observation.with_detail(detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bamboo_core::storage::{CriticalWarning, SmartSource};

    fn gigabytes(gb: u64) -> Bytes {
        Bytes(gb * 1_000_000_000)
    }

    fn healthy() -> SmartHealth {
        SmartHealth {
            source: Some(SmartSource::NvmeHealthLog),
            critical_warning: Some(CriticalWarning::NONE),
            temperature_c: Some(38),
            available_spare: Some(100),
            available_spare_threshold: Some(10),
            percentage_used: Some(2),
            data_written: Some(gigabytes(12_000)),
            ..Default::default()
        }
    }

    fn input<'a>(health: &'a SmartHealth, daily: Option<Bytes>) -> WearInput<'a> {
        WearInput {
            drive_name: "Samsung SSD 980 PRO",
            capacity: gigabytes(1000),
            health,
            daily_write: daily,
            baseline_daily_write: Some(gigabytes(15)),
            media_errors_grew: false,
            top_writers: &[],
        }
    }

    #[test]
    fn a_normal_drive_gets_a_calming_verdict() {
        let health = healthy();
        let verdict = analyze(&input(&health, Some(gigabytes(15))));

        assert_eq!(verdict.observation.severity, Severity::Calm);
        assert!(verdict.observation.summary.contains("всё в порядке"));
        assert!(verdict.observation.summary.contains("2%"));
        // 588 ТБ остатка при 15 ГБ в сутки — больше века.
        assert!(verdict.years_left.unwrap() > 50.0);
    }

    #[test]
    fn a_century_of_life_is_not_stated_as_a_number_of_years() {
        let health = healthy();
        let verdict = analyze(&input(&health, Some(gigabytes(15))));
        let detail = verdict.observation.detail.unwrap();
        assert!(detail.contains("переживёт"), "получили: {detail}");
    }

    #[test]
    fn huge_daily_writes_raise_a_warning_with_attribution() {
        let health = healthy();
        let writers = [
            ("Docker Desktop".to_string(), gigabytes(180)),
            ("chrome.exe".to_string(), gigabytes(40)),
        ];
        let mut input = input(&health, Some(gigabytes(240)));
        input.top_writers = &writers;

        let verdict = analyze(&input);
        assert_eq!(verdict.observation.severity, Severity::Warning);
        assert!(verdict
            .observation
            .detail
            .unwrap()
            .contains("Docker Desktop"));
    }

    #[test]
    fn controller_warning_is_reported_verbatim() {
        let mut health = healthy();
        health.critical_warning = Some(CriticalWarning(0x04));

        let verdict = analyze(&input(&health, Some(gigabytes(10))));
        assert_eq!(verdict.observation.severity, Severity::Warning);
        assert_eq!(verdict.observation.confidence, 1.0);
        assert!(verdict
            .observation
            .summary
            .contains("надёжность носителя снижена"));
    }

    #[test]
    fn growing_media_errors_are_a_warning_even_on_a_new_drive() {
        let health = healthy();
        let mut input = input(&health, Some(gigabytes(10)));
        input.media_errors_grew = true;

        let verdict = analyze(&input);
        assert_eq!(verdict.observation.severity, Severity::Warning);
        assert!(verdict.observation.summary.contains("ошибок целостности"));
    }

    #[test]
    fn spare_below_threshold_is_a_warning() {
        let mut health = healthy();
        health.available_spare = Some(5);
        health.available_spare_threshold = Some(10);

        let verdict = analyze(&input(&health, Some(gigabytes(10))));
        assert_eq!(verdict.observation.severity, Severity::Warning);
    }

    #[test]
    fn a_threefold_jump_over_the_baseline_is_noticed() {
        let health = healthy();
        // 60 ГБ против обычных 15 — вчетверо, но абсолютный порог не пройден.
        let verdict = analyze(&input(&health, Some(gigabytes(60))));
        assert_eq!(verdict.observation.severity, Severity::Warning);
        assert!(verdict.observation.summary.contains("обычных"));
    }

    #[test]
    fn a_moderate_rise_over_the_baseline_is_left_alone() {
        let health = healthy();
        let verdict = analyze(&input(&health, Some(gigabytes(30))));
        assert_eq!(verdict.observation.severity, Severity::Calm);
    }

    #[test]
    fn a_nearly_worn_drive_warns_about_the_projection() {
        let mut health = healthy();
        health.percentage_used = Some(96);

        // 4% от 600 ТБ — это 24 ТБ, при 50 ГБ в сутки чуть больше года.
        let verdict = analyze(&input(&health, Some(gigabytes(50))));
        assert_eq!(verdict.observation.severity, Severity::Warning);
        assert!(verdict.years_left.unwrap() < YEARS_ALARM);
    }

    #[test]
    fn unknown_model_is_marked_as_an_estimate() {
        let health = healthy();
        let mut input = input(&health, Some(gigabytes(15)));
        input.drive_name = "Apacer AS350 512GB";
        input.capacity = gigabytes(512);

        let verdict = analyze(&input);
        assert!(verdict.rating.is_estimate);
        assert!(verdict.observation.detail.unwrap().contains("оценка"));
    }

    #[test]
    fn a_drive_that_hides_its_wear_says_so_instead_of_guessing() {
        let health = SmartHealth {
            source: Some(SmartSource::AtaSmart),
            ..Default::default()
        };
        let verdict = analyze(&input(&health, None));

        assert_eq!(verdict.observation.severity, Severity::Calm);
        assert_eq!(verdict.used_percent, None);
        assert_eq!(verdict.years_left, None);
        assert!(verdict.observation.summary.contains("не сообщает"));
    }
}
