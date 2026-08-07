//! Линейная регрессия методом наименьших квадратов.
//!
//! Используется для детекта монотонного роста памяти и дескрипторов.
//! Наклон сам по себе ничего не значит — важна пара «наклон плюс R²»:
//! пилообразный график с нормальным поведением приложения тоже даёт
//! положительный наклон, но плохо ложится на прямую.

/// Результат подгонки прямой.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trend {
    /// Наклон в единицах Y за единицу X.
    pub slope: f64,
    pub intercept: f64,
    /// Коэффициент детерминации, 0..1. Насколько точки ложатся на прямую.
    pub r_squared: f64,
    pub points: usize,
}

/// Подгоняет прямую к ряду точек.
///
/// Возвращает `None`, если точек меньше трёх или все X совпадают.
pub fn fit(points: &[(f64, f64)]) -> Option<Trend> {
    if points.len() < 3 {
        return None;
    }

    let n = points.len() as f64;
    let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / n;
    let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / n;

    // Центрируем X: время в миллисекундах — большие числа, и без
    // центрирования на разностях квадратов теряется точность.
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    let mut syy = 0.0;
    for (x, y) in points {
        let dx = x - mean_x;
        let dy = y - mean_y;
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }

    if sxx == 0.0 {
        return None;
    }

    let slope = sxy / sxx;
    let intercept = mean_y - slope * mean_x;

    // Ряд-константа: дисперсии нет, прямая описывает его идеально,
    // но роста в нём тоже нет. Возвращаем нулевую объяснённую долю,
    // чтобы такой ряд никогда не проходил порог детекта.
    let r_squared = if syy == 0.0 {
        0.0
    } else {
        (sxy * sxy / (sxx * syy)).clamp(0.0, 1.0)
    };

    Some(Trend {
        slope,
        intercept,
        r_squared,
        points: points.len(),
    })
}

/// Проверяет отсутствие сброса: минимум последней трети ряда выше
/// максимума первой трети.
///
/// Отсекает приложения, которые память честно отдают. У браузера с
/// закрытыми вкладками наклон за четыре часа может быть положительным,
/// но провалы в графике означают, что память возвращается.
pub fn never_released(values: &[f64]) -> bool {
    if values.len() < 6 {
        return false;
    }
    let third = values.len() / 3;
    let first_max = values[..third].iter().copied().fold(f64::MIN, f64::max);
    let last_min = values[values.len() - third..]
        .iter()
        .copied()
        .fold(f64::MAX, f64::min);
    last_min > first_max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: usize, slope: f64, noise: impl Fn(usize) -> f64) -> Vec<(f64, f64)> {
        (0..n)
            .map(|i| (i as f64, 100.0 + slope * i as f64 + noise(i)))
            .collect()
    }

    #[test]
    fn perfect_line_is_recovered() {
        let trend = fit(&line(20, 2.5, |_| 0.0)).unwrap();
        assert!((trend.slope - 2.5).abs() < 1e-9);
        assert!((trend.intercept - 100.0).abs() < 1e-9);
        assert!((trend.r_squared - 1.0).abs() < 1e-9);
    }

    #[test]
    fn noise_lowers_the_fit_but_keeps_the_slope() {
        let noisy = line(60, 1.0, |i| if i % 2 == 0 { 12.0 } else { -12.0 });
        let trend = fit(&noisy).unwrap();
        assert!((trend.slope - 1.0).abs() < 0.2);
        assert!(
            trend.r_squared < 0.9,
            "R² {} слишком высок",
            trend.r_squared
        );
    }

    #[test]
    fn sawtooth_does_not_look_like_a_line() {
        // Классическое поведение живого приложения: набрал, отдал, набрал.
        let sawtooth: Vec<(f64, f64)> = (0..60)
            .map(|i| (i as f64, 100.0 + (i % 10) as f64 * 20.0))
            .collect();
        let trend = fit(&sawtooth).unwrap();
        assert!(trend.r_squared < 0.5, "R² {}", trend.r_squared);
    }

    #[test]
    fn constant_series_is_not_growth() {
        let flat: Vec<(f64, f64)> = (0..30).map(|i| (i as f64, 512.0)).collect();
        let trend = fit(&flat).unwrap();
        assert_eq!(trend.slope, 0.0);
        assert_eq!(
            trend.r_squared, 0.0,
            "плоский ряд не должен считаться ростом"
        );
    }

    #[test]
    fn too_few_points_give_nothing() {
        assert!(fit(&[(0.0, 1.0), (1.0, 2.0)]).is_none());
        assert!(fit(&[]).is_none());
    }

    #[test]
    fn large_x_values_do_not_lose_precision() {
        // Время в миллисекундах от загрузки — числа порядка 10^10.
        let base = 40_000_000_000.0;
        let points: Vec<(f64, f64)> = (0..100)
            .map(|i| (base + i as f64 * 5000.0, 1000.0 + i as f64 * 3.0))
            .collect();
        let trend = fit(&points).unwrap();
        assert!((trend.slope - 3.0 / 5000.0).abs() < 1e-12);
        assert!(trend.r_squared > 0.999);
    }

    #[test]
    fn released_memory_is_detected() {
        let growing: Vec<f64> = (0..30).map(|i| 100.0 + i as f64).collect();
        assert!(never_released(&growing));

        // Вырос и вернулся к исходному.
        let mut released: Vec<f64> = (0..15).map(|i| 100.0 + i as f64 * 10.0).collect();
        released.extend((0..15).map(|i| 100.0 + i as f64));
        assert!(!never_released(&released));
    }

    #[test]
    fn short_series_is_not_judged() {
        assert!(!never_released(&[1.0, 2.0, 3.0]));
    }
}
