//! Уровень L1 из схемы даунсемплинга (ТЗ, раздел 8.2).
//!
//! L0 — секундное разрешение на 10 минут, всё в памяти (`table.rs`).
//! L1 — минутное разрешение на 24 часа, тоже в памяти. При переходе с L0
//! на L1 накопительные метрики суммируются, мгновенные сворачиваются
//! в среднее, минимум и максимум — три значения, чтобы не потерять всплеск
//! при усреднении.
//!
//! Держим L1 в памяти, а не в SQLite: сутки минутных точек на приложение —
//! 1440 записей, но приложений, живущих сутки, единицы, и на диск это писать
//! незачем. На диск уходят уже уровни L2 и L3.

use crate::ring::RingBuffer;
use crate::table::MetricPoint;

/// Глубина L1 в точках: 1440 минут — ровно сутки.
pub const L1_CAPACITY: usize = 1440;

/// Длительность интервала L1.
pub const L1_BUCKET_MS: u64 = 60_000;

/// Свёртка мгновенной метрики: среднее, минимум, максимум.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    pub avg: u32,
    pub min: u32,
    pub max: u32,
}

/// Минутный агрегат по одному приложению.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MinutePoint {
    /// Начало минуты, миллисекунды монотонных часов.
    pub minute_ms: u64,
    /// Сколько секундных точек вошло в минуту.
    pub samples: u16,
    /// Процессорное время за минуту, накопительно.
    pub cpu_ms: u32,
    pub read_kib: u32,
    pub write_kib: u32,
    pub private_kib: Stat,
    pub working_set_kib: Stat,
}

/// Накопитель минутного агрегата. Копит секундные точки, пока идёт минута,
/// и отдаёт готовый `MinutePoint` при её завершении.
#[derive(Clone, Debug)]
struct MinuteAccumulator {
    minute_ms: u64,
    samples: u16,
    cpu_ms: u64,
    read_kib: u64,
    write_kib: u64,
    private_sum: u64,
    private_min: u32,
    private_max: u32,
    ws_sum: u64,
    ws_min: u32,
    ws_max: u32,
}

impl MinuteAccumulator {
    fn new(minute_ms: u64) -> Self {
        MinuteAccumulator {
            minute_ms,
            samples: 0,
            cpu_ms: 0,
            read_kib: 0,
            write_kib: 0,
            private_sum: 0,
            private_min: u32::MAX,
            private_max: 0,
            ws_sum: 0,
            ws_min: u32::MAX,
            ws_max: 0,
        }
    }

    fn add(&mut self, point: &MetricPoint) {
        self.samples = self.samples.saturating_add(1);
        self.cpu_ms += point.cpu_ms as u64;
        self.read_kib += point.read_kib as u64;
        self.write_kib += point.write_kib as u64;

        self.private_sum += point.private_kib as u64;
        self.private_min = self.private_min.min(point.private_kib);
        self.private_max = self.private_max.max(point.private_kib);

        self.ws_sum += point.working_set_private_kib as u64;
        self.ws_min = self.ws_min.min(point.working_set_private_kib);
        self.ws_max = self.ws_max.max(point.working_set_private_kib);
    }

    fn finish(&self) -> MinutePoint {
        let samples = self.samples.max(1) as u64;
        MinutePoint {
            minute_ms: self.minute_ms,
            samples: self.samples,
            cpu_ms: self.cpu_ms.min(u32::MAX as u64) as u32,
            read_kib: self.read_kib.min(u32::MAX as u64) as u32,
            write_kib: self.write_kib.min(u32::MAX as u64) as u32,
            private_kib: Stat {
                avg: (self.private_sum / samples) as u32,
                min: if self.private_min == u32::MAX {
                    0
                } else {
                    self.private_min
                },
                max: self.private_max,
            },
            working_set_kib: Stat {
                avg: (self.ws_sum / samples) as u32,
                min: if self.ws_min == u32::MAX {
                    0
                } else {
                    self.ws_min
                },
                max: self.ws_max,
            },
        }
    }
}

/// Ряд L1 для одного приложения: сутки минутных агрегатов.
#[derive(Clone, Debug)]
pub struct Level1Series {
    history: RingBuffer<MinutePoint>,
    current: Option<MinuteAccumulator>,
}

impl Default for Level1Series {
    fn default() -> Self {
        Self::new()
    }
}

impl Level1Series {
    pub fn new() -> Self {
        Level1Series {
            history: RingBuffer::with_capacity(L1_CAPACITY),
            current: None,
        }
    }

    fn minute_of(at_ms: u64) -> u64 {
        (at_ms / L1_BUCKET_MS) * L1_BUCKET_MS
    }

    /// Добавляет секундную точку L0. При переходе на новую минуту закрывает
    /// предыдущую и кладёт её в историю.
    pub fn push(&mut self, at_ms: u64, point: &MetricPoint) {
        let minute = Self::minute_of(at_ms);

        match &mut self.current {
            Some(acc) if acc.minute_ms == minute => acc.add(point),
            Some(acc) => {
                // Минута сменилась: закрываем прежнюю, начинаем новую.
                let finished = acc.finish();
                self.history.push(finished);
                let mut fresh = MinuteAccumulator::new(minute);
                fresh.add(point);
                self.current = Some(fresh);
            }
            None => {
                let mut fresh = MinuteAccumulator::new(minute);
                fresh.add(point);
                self.current = Some(fresh);
            }
        }
    }

    /// Завершённые минуты, от старых к свежим. Текущая незакрытая минута
    /// сюда не входит.
    pub fn finished(&self) -> impl Iterator<Item = &MinutePoint> + '_ {
        self.history.iter()
    }

    /// Сколько завершённых минут в истории.
    pub fn len(&self) -> usize {
        self.history.len()
    }

    pub fn is_empty(&self) -> bool {
        self.history.is_empty() && self.current.is_none()
    }

    /// Сколько байт занято под минутную историю. Нужно для контроля
    /// бюджета памяти наравне с L0.
    pub fn allocated_bytes(&self) -> usize {
        self.history.allocated_bytes()
            + self
                .current
                .as_ref()
                .map_or(0, |_| core::mem::size_of::<MinuteAccumulator>())
    }

    /// Временной ряд приватной памяти для анализатора роста: пары
    /// «время, байты». Берётся среднее за минуту.
    pub fn private_series(&self) -> Vec<(u64, f64)> {
        self.history
            .iter()
            .map(|point| (point.minute_ms, point.private_kib.avg as f64 * 1024.0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(cpu_ms: u32, private_kib: u32) -> MetricPoint {
        MetricPoint {
            cpu_ms,
            private_kib,
            working_set_private_kib: private_kib / 2,
            read_kib: 10,
            write_kib: 20,
        }
    }

    #[test]
    fn seconds_within_a_minute_fold_into_one_point() {
        let mut series = Level1Series::new();
        // 60 секундных точек одной минуты, затем одна точка следующей.
        for second in 0..60 {
            series.push(second * 1000, &point(100, 1000));
        }
        // Пока минута не закрыта, история пуста.
        assert_eq!(series.len(), 0);

        series.push(60_000, &point(100, 2000));
        assert_eq!(series.len(), 1, "первая минута должна закрыться");
    }

    #[test]
    fn cumulative_metrics_are_summed_over_the_minute() {
        let mut series = Level1Series::new();
        for second in 0..60 {
            series.push(second * 1000, &point(50, 1000));
        }
        series.push(60_000, &point(0, 0)); // закрываем минуту

        let minute = series.finished().next().unwrap();
        assert_eq!(minute.samples, 60);
        assert_eq!(
            minute.cpu_ms,
            60 * 50,
            "процессорное время не просуммировано"
        );
    }

    #[test]
    fn spikes_survive_the_minute_average() {
        // Ровная минута с одним всплеском памяти: среднее его прячет,
        // максимум — нет. Ради этого и хранится тройка значений.
        let mut series = Level1Series::new();
        for second in 0..59 {
            series.push(second * 1000, &point(10, 500_000));
        }
        series.push(59_000, &point(10, 3_000_000)); // всплеск
        series.push(60_000, &point(0, 0)); // закрываем минуту

        let minute = series.finished().next().unwrap();
        assert_eq!(minute.private_kib.max, 3_000_000);
        assert_eq!(minute.private_kib.min, 500_000);
        assert!(minute.private_kib.avg < 600_000, "среднее спрятало всплеск");
    }

    #[test]
    fn a_day_of_minutes_stays_bounded() {
        let mut series = Level1Series::new();
        // Больше суток минут: старые должны вытесняться.
        for minute in 0..(L1_CAPACITY as u64 + 100) {
            // Две точки на минуту, чтобы она закрылась при переходе.
            series.push(minute * 60_000, &point(100, 1000));
            series.push(minute * 60_000 + 30_000, &point(100, 1000));
        }
        assert!(
            series.len() <= L1_CAPACITY,
            "история L1 вышла за сутки: {}",
            series.len()
        );
    }

    #[test]
    fn a_private_series_is_extracted_for_the_growth_analyzer() {
        let mut series = Level1Series::new();
        for minute in 0..5u64 {
            let value = 100_000 + minute as u32 * 10_000;
            series.push(minute * 60_000, &point(100, value));
            series.push(minute * 60_000 + 30_000, &point(100, value));
        }
        series.push(5 * 60_000, &point(0, 0)); // закрываем пятую минуту

        let out = series.private_series();
        assert_eq!(out.len(), 5);
        // Ряд растёт — ровно то, что нужно анализатору утечек.
        assert!(out[4].1 > out[0].1);
    }

    #[test]
    fn an_empty_series_is_recognisable() {
        let series = Level1Series::new();
        assert!(series.is_empty());
        assert_eq!(series.len(), 0);
        assert!(series.private_series().is_empty());
    }
}
