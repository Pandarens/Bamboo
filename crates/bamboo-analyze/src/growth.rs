//! Детект монотонного роста памяти, дескрипторов и объектов GDI (ТЗ, раздел 9.1).
//!
//! Важная оговорка по формулировкам. Снаружи, без инструментирования процесса,
//! утечка не детектируется — детектируется монотонный рост. Он может быть
//! и штатным поведением: кэш, который приложение честно освободит под
//! давлением памяти, выглядит точно так же. Поэтому выводы формулируются
//! как «похоже на утечку», а не «обнаружена утечка».

use bamboo_core::Bytes;

use crate::observation::{Observation, ObservationKind, Severity};
use crate::regression::{fit, never_released};

/// Точка ряда: время в миллисекундах и значение.
pub type Point = (u64, f64);

/// Минимальное время жизни процесса, начиная с которого имеет смысл
/// говорить о росте. У короткоживущего процесса растёт всё.
pub const MIN_LIFETIME_MS: u64 = 6 * 60 * 60 * 1000;

/// Минимальная длина окна наблюдения.
pub const MIN_WINDOW_MS: u64 = 4 * 60 * 60 * 1000;

/// Пороги детекта.
const MEMORY_SLOPE_MB_PER_HOUR: f64 = 20.0;
const MEMORY_MIN_R2: f64 = 0.90;
const HANDLES_PER_HOUR: f64 = 100.0;
const HANDLES_MIN_R2: f64 = 0.85;
const GDI_PER_HOUR: f64 = 50.0;

/// Что известно о процессе на момент анализа.
pub struct GrowthInput<'a> {
    pub process_name: &'a str,
    /// Сколько процесс прожил.
    pub lifetime_ms: u64,
    /// Приватная память по времени, байты.
    pub private_bytes: &'a [Point],
    /// Число дескрипторов по времени.
    pub handles: &'a [Point],
    /// Число объектов GDI по времени. Пусто для процессов без окон.
    pub gdi_objects: &'a [Point],
}

/// Анализирует рост. Возвращает наблюдения только там, где признак сошёлся.
///
/// Пустой результат — нормальный и самый частый исход.
pub fn analyze(input: &GrowthInput<'_>) -> Vec<Observation> {
    let mut observations = Vec::new();

    // Рост дескрипторов и объектов GDI идёт первым: это классические утечки,
    // хорошо видимые снаружи и почти всегда настоящие, в отличие от роста
    // памяти, который часто оказывается кэшем.
    if let Some(observation) = handle_growth(input) {
        observations.push(observation);
    }
    if let Some(observation) = gdi_growth(input) {
        observations.push(observation);
    }
    if let Some(observation) = memory_growth(input) {
        observations.push(observation);
    }

    observations
}

fn window_ms(series: &[Point]) -> u64 {
    match (series.first(), series.last()) {
        (Some((start, _)), Some((end, _))) => end.saturating_sub(*start),
        _ => 0,
    }
}

fn as_points(series: &[Point]) -> Vec<(f64, f64)> {
    series.iter().map(|(t, v)| (*t as f64, *v)).collect()
}

fn memory_growth(input: &GrowthInput<'_>) -> Option<Observation> {
    if input.lifetime_ms < MIN_LIFETIME_MS || window_ms(input.private_bytes) < MIN_WINDOW_MS {
        return None;
    }

    let trend = fit(&as_points(input.private_bytes))?;
    let mb_per_hour = trend.slope * 3_600_000.0 / (1024.0 * 1024.0);

    if mb_per_hour < MEMORY_SLOPE_MB_PER_HOUR || trend.r_squared < MEMORY_MIN_R2 {
        return None;
    }

    let values: Vec<f64> = input.private_bytes.iter().map(|(_, v)| *v).collect();
    if !never_released(&values) {
        return None;
    }

    let current = Bytes(values.last().copied().unwrap_or(0.0) as u64);
    let hours = window_ms(input.private_bytes) as f64 / 3_600_000.0;

    Some(
        Observation::new(
            ObservationKind::MemoryGrowth,
            Severity::Notice,
            confidence_from_fit(trend.r_squared, MEMORY_MIN_R2),
            format!(
                "{}: память растёт на {:.0} МБ в час уже {:.0} часов, сейчас {current}",
                input.process_name, mb_per_hour, hours
            ),
        )
        .with_detail(
            "Похоже на утечку памяти. Снаружи отличить утечку от растущего кэша \
             невозможно, поэтому наверняка сказать нельзя. Можно снять дамп \
             процесса — это готовый артефакт для разработчика приложения.",
        ),
    )
}

fn handle_growth(input: &GrowthInput<'_>) -> Option<Observation> {
    if input.lifetime_ms < MIN_LIFETIME_MS || window_ms(input.handles) < MIN_WINDOW_MS {
        return None;
    }

    let trend = fit(&as_points(input.handles))?;
    let per_hour = trend.slope * 3_600_000.0;

    if per_hour < HANDLES_PER_HOUR || trend.r_squared < HANDLES_MIN_R2 {
        return None;
    }

    let current = input.handles.last().map(|(_, v)| *v).unwrap_or(0.0);

    Some(
        Observation::new(
            ObservationKind::HandleGrowth,
            Severity::Warning,
            confidence_from_fit(trend.r_squared, HANDLES_MIN_R2),
            format!(
                "{}: число дескрипторов растёт на {:.0} в час, сейчас {:.0}",
                input.process_name, per_hour, current
            ),
        )
        .with_detail(
            "Дескрипторы, в отличие от памяти, приложение не кэширует. \
             Ровный рост почти всегда означает настоящую утечку.",
        ),
    )
}

fn gdi_growth(input: &GrowthInput<'_>) -> Option<Observation> {
    if input.lifetime_ms < MIN_LIFETIME_MS || window_ms(input.gdi_objects) < MIN_WINDOW_MS {
        return None;
    }

    let trend = fit(&as_points(input.gdi_objects))?;
    let per_hour = trend.slope * 3_600_000.0;

    if per_hour < GDI_PER_HOUR || trend.r_squared < HANDLES_MIN_R2 {
        return None;
    }

    let current = input.gdi_objects.last().map(|(_, v)| *v).unwrap_or(0.0);

    Some(
        Observation::new(
            ObservationKind::GdiGrowth,
            Severity::Warning,
            confidence_from_fit(trend.r_squared, HANDLES_MIN_R2),
            format!(
                "{}: объектов GDI становится больше на {:.0} в час, сейчас {:.0}",
                input.process_name, per_hour, current
            ),
        )
        .with_detail(
            "У процесса есть предел в 10 000 объектов GDI. При его достижении \
             интерфейс приложения перестанет отрисовываться.",
        ),
    )
}

/// Переводит качество подгонки в уверенность: у порога — низкая,
/// у идеальной прямой — высокая.
fn confidence_from_fit(r_squared: f64, threshold: f64) -> f32 {
    let span = 1.0 - threshold;
    if span <= 0.0 {
        return 1.0;
    }
    (0.5 + 0.5 * ((r_squared - threshold) / span)).clamp(0.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3_600_000;
    const MB: f64 = 1024.0 * 1024.0;

    /// Ряд за `hours` часов с шагом в минуту.
    fn series(hours: u64, start: f64, per_hour: f64, wobble: impl Fn(u64) -> f64) -> Vec<Point> {
        (0..hours * 60)
            .map(|minute| {
                let t = minute * 60_000;
                let value = start + per_hour * (minute as f64 / 60.0) + wobble(minute);
                (t, value)
            })
            .collect()
    }

    fn input<'a>(name: &'a str, private: &'a [Point]) -> GrowthInput<'a> {
        GrowthInput {
            process_name: name,
            lifetime_ms: 12 * HOUR,
            private_bytes: private,
            handles: &[],
            gdi_objects: &[],
        }
    }

    #[test]
    fn steady_memory_growth_is_reported_as_suspicion() {
        let private = series(5, 300.0 * MB, 40.0 * MB, |_| 0.0);
        let observations = analyze(&input("teams.exe", &private));

        assert_eq!(observations.len(), 1);
        let observation = &observations[0];
        assert_eq!(observation.kind, ObservationKind::MemoryGrowth);
        // Формулировка обязана быть предположением, а не диагнозом.
        assert!(observation
            .detail
            .as_ref()
            .unwrap()
            .contains("Похоже на утечку"));
        assert!(observation.summary.contains("teams.exe"));
    }

    #[test]
    fn slow_growth_stays_below_the_threshold() {
        // 5 МБ в час — это шум, а не утечка.
        let private = series(5, 300.0 * MB, 5.0 * MB, |_| 0.0);
        assert!(analyze(&input("app.exe", &private)).is_empty());
    }

    #[test]
    fn a_process_that_releases_memory_is_left_alone() {
        // Растёт, потом отдаёт — обычный кэш браузера.
        let mut private = series(3, 300.0 * MB, 100.0 * MB, |_| 0.0);
        let tail_start = private.last().unwrap().0;
        private.extend((1..=120).map(|minute| {
            (
                tail_start + minute * 60_000,
                300.0 * MB + minute as f64 * MB,
            )
        }));

        assert!(
            analyze(&input("chrome.exe", &private)).is_empty(),
            "приложение отдало память, тревожить не за что"
        );
    }

    #[test]
    fn young_process_is_not_judged() {
        let private = series(5, 300.0 * MB, 40.0 * MB, |_| 0.0);
        let mut input = input("app.exe", &private);
        input.lifetime_ms = 2 * HOUR;
        assert!(analyze(&input).is_empty());
    }

    #[test]
    fn short_window_is_not_enough() {
        // Ряд всего за час — окна в четыре часа нет.
        let private = series(1, 300.0 * MB, 100.0 * MB, |_| 0.0);
        assert!(analyze(&input("app.exe", &private)).is_empty());
    }

    #[test]
    fn active_work_does_not_trigger_a_false_positive() {
        // Сборка проекта: память скачет вверх-вниз в большом диапазоне.
        let private = series(5, 500.0 * MB, 30.0 * MB, |minute| {
            if minute % 7 < 3 {
                400.0 * MB
            } else {
                -300.0 * MB
            }
        });
        assert!(analyze(&input("cargo.exe", &private)).is_empty());
    }

    #[test]
    fn handle_leak_outranks_memory_growth() {
        let private = series(5, 300.0 * MB, 40.0 * MB, |_| 0.0);
        let handles = series(5, 2_000.0, 400.0, |_| 0.0);
        let observations = analyze(&GrowthInput {
            process_name: "leaky.exe",
            lifetime_ms: 12 * HOUR,
            private_bytes: &private,
            handles: &handles,
            gdi_objects: &[],
        });

        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].kind, ObservationKind::HandleGrowth);
        assert_eq!(observations[0].severity, Severity::Warning);
    }

    #[test]
    fn gdi_growth_is_reported() {
        let gdi = series(5, 300.0, 120.0, |_| 0.0);
        let observations = analyze(&GrowthInput {
            process_name: "painty.exe",
            lifetime_ms: 12 * HOUR,
            private_bytes: &[],
            handles: &[],
            gdi_objects: &gdi,
        });

        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].kind, ObservationKind::GdiGrowth);
    }

    #[test]
    fn confidence_grows_with_the_fit() {
        let near_threshold = confidence_from_fit(0.90, 0.90);
        let perfect = confidence_from_fit(1.0, 0.90);
        assert!(near_threshold < perfect);
        assert!((0.0..=1.0).contains(&near_threshold));
        assert_eq!(perfect, 1.0);
    }

    #[test]
    fn a_flat_process_produces_nothing() {
        let private = series(6, 300.0 * MB, 0.0, |_| 0.0);
        assert!(analyze(&input("postgres.exe", &private)).is_empty());
    }
}
