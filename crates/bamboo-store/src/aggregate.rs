//! Агрегация временных рядов между уровнями хранения (ТЗ, раздел 8.2).
//!
//! Чистая арифметика без SQLite — её можно и нужно проверять отдельно
//! от базы.
//!
//! Правило агрегации из ТЗ: накопительные метрики складываются, мгновенные
//! сворачиваются в тройку «среднее, минимум, максимум». Три значения,
//! а не одно среднее, — иначе усреднение съест всплески, ради которых
//! всё и затевалось: приложение, разово занявшее 3 ГБ, в среднем за
//! пятнадцать минут выглядит скромно.

/// Точка ряда для уровней L1 и выше.
///
/// Шире, чем компактная точка L0 в `bamboo-collect`: на диске экономить
/// байты смысла нет, а мгновенные счётчики дескрипторов и потоков нужны
/// анализатору утечек.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SamplePoint {
    pub at_unix_ms: i64,
    /// Процессорное время за интервал.
    pub cpu_ms: u32,
    /// Прочитано за интервал.
    pub read_kib: u32,
    /// Записано за интервал.
    pub write_kib: u32,
    pub private_kib: u32,
    pub working_set_kib: u32,
    pub handles: u32,
    pub threads: u32,
}

/// Свёртка мгновенной метрики.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    pub avg: u32,
    pub min: u32,
    pub max: u32,
}

impl Stat {
    fn from_values(values: &[u32]) -> Stat {
        if values.is_empty() {
            return Stat::default();
        }
        let sum: u64 = values.iter().map(|v| *v as u64).sum();
        Stat {
            avg: (sum / values.len() as u64) as u32,
            min: *values.iter().min().unwrap(),
            max: *values.iter().max().unwrap(),
        }
    }

    /// Слияние двух свёрток при переходе на более грубый уровень.
    ///
    /// Среднее считается взвешенно по числу сэмплов: иначе редкий интервал
    /// с одним замером получит тот же вес, что полный.
    fn merge(self, self_samples: u32, other: Stat, other_samples: u32) -> Stat {
        let total = self_samples as u64 + other_samples as u64;
        if total == 0 {
            return Stat::default();
        }
        Stat {
            avg: ((self.avg as u64 * self_samples as u64 + other.avg as u64 * other_samples as u64)
                / total) as u32,
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

/// Агрегат за один интервал.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bucket {
    /// Начало интервала, миллисекунды эпохи Unix.
    pub start_ms: i64,
    /// Сколько сэмплов попало в интервал.
    pub samples: u32,

    pub cpu_ms: u64,
    pub read_kib: u64,
    pub write_kib: u64,

    pub private_kib: Stat,
    pub working_set_kib: Stat,
    pub handles: Stat,
    pub threads: Stat,
}

impl Bucket {
    /// Сливает два соседних агрегата. Используется при подъёме L2 → L3.
    pub fn merge(self, other: Bucket) -> Bucket {
        if self.samples == 0 {
            return other;
        }
        if other.samples == 0 {
            return self;
        }

        Bucket {
            start_ms: self.start_ms.min(other.start_ms),
            samples: self.samples + other.samples,
            cpu_ms: self.cpu_ms + other.cpu_ms,
            read_kib: self.read_kib + other.read_kib,
            write_kib: self.write_kib + other.write_kib,
            private_kib: self
                .private_kib
                .merge(self.samples, other.private_kib, other.samples),
            working_set_kib: self.working_set_kib.merge(
                self.samples,
                other.working_set_kib,
                other.samples,
            ),
            handles: self
                .handles
                .merge(self.samples, other.handles, other.samples),
            threads: self
                .threads
                .merge(self.samples, other.threads, other.samples),
        }
    }
}

/// Начало интервала, в который попадает отметка времени.
pub fn bucket_start(at_unix_ms: i64, bucket_ms: i64) -> i64 {
    debug_assert!(bucket_ms > 0);
    // Округление вниз, а не усечение делением: до 1970 года отметка
    // отрицательная, и обычное деление уехало бы на интервал вперёд.
    at_unix_ms.div_euclid(bucket_ms) * bucket_ms
}

/// Раскладывает точки по интервалам заданной длительности.
///
/// Порядок точек значения не имеет. Результат отсортирован по времени.
pub fn into_buckets(points: &[SamplePoint], bucket_ms: i64) -> Vec<Bucket> {
    if points.is_empty() || bucket_ms <= 0 {
        return Vec::new();
    }

    let mut starts: Vec<i64> = points
        .iter()
        .map(|p| bucket_start(p.at_unix_ms, bucket_ms))
        .collect();
    starts.sort_unstable();
    starts.dedup();

    starts
        .into_iter()
        .map(|start| {
            let inside: Vec<&SamplePoint> = points
                .iter()
                .filter(|p| bucket_start(p.at_unix_ms, bucket_ms) == start)
                .collect();

            let pick = |get: fn(&SamplePoint) -> u32| -> Vec<u32> {
                inside.iter().map(|p| get(p)).collect()
            };

            Bucket {
                start_ms: start,
                samples: inside.len() as u32,
                cpu_ms: inside.iter().map(|p| p.cpu_ms as u64).sum(),
                read_kib: inside.iter().map(|p| p.read_kib as u64).sum(),
                write_kib: inside.iter().map(|p| p.write_kib as u64).sum(),
                private_kib: Stat::from_values(&pick(|p| p.private_kib)),
                working_set_kib: Stat::from_values(&pick(|p| p.working_set_kib)),
                handles: Stat::from_values(&pick(|p| p.handles)),
                threads: Stat::from_values(&pick(|p| p.threads)),
            }
        })
        .collect()
}

/// Поднимает готовые агрегаты на более грубый уровень.
pub fn roll_up(buckets: &[Bucket], bucket_ms: i64) -> Vec<Bucket> {
    if buckets.is_empty() || bucket_ms <= 0 {
        return Vec::new();
    }

    let mut result: Vec<Bucket> = Vec::new();
    let mut sorted = buckets.to_vec();
    sorted.sort_by_key(|b| b.start_ms);

    for bucket in sorted {
        let start = bucket_start(bucket.start_ms, bucket_ms);
        match result.last_mut() {
            Some(last) if last.start_ms == start => {
                let merged = last.merge(bucket);
                *last = Bucket {
                    start_ms: start,
                    ..merged
                };
            }
            _ => result.push(Bucket {
                start_ms: start,
                ..bucket
            }),
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60_000;
    const QUARTER: i64 = 15 * 60_000;
    const HOUR: i64 = 3_600_000;

    fn point(at_ms: i64, cpu_ms: u32, private_kib: u32) -> SamplePoint {
        SamplePoint {
            at_unix_ms: at_ms,
            cpu_ms,
            read_kib: 10,
            write_kib: 20,
            private_kib,
            working_set_kib: private_kib / 2,
            handles: 300,
            threads: 12,
        }
    }

    #[test]
    fn points_fall_into_the_right_buckets() {
        let points = vec![
            point(0, 100, 1000),
            point(MINUTE - 1, 100, 2000),
            point(MINUTE, 100, 3000),
        ];
        let buckets = into_buckets(&points, MINUTE);

        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].samples, 2);
        assert_eq!(buckets[1].samples, 1);
    }

    #[test]
    fn cumulative_metrics_are_summed() {
        let points: Vec<SamplePoint> = (0..10).map(|i| point(i * 1000, 250, 1000)).collect();
        let buckets = into_buckets(&points, MINUTE);

        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].cpu_ms, 2_500);
        assert_eq!(buckets[0].write_kib, 200);
    }

    #[test]
    fn spikes_survive_aggregation() {
        // Ровный ряд с одним всплеском: среднее его прячет, максимум — нет.
        // Ради этого в ТЗ и написано хранить три значения вместо одного.
        let mut points: Vec<SamplePoint> = (0..30).map(|i| point(i * 1000, 10, 500_000)).collect();
        points[15].private_kib = 3_000_000;

        let bucket = into_buckets(&points, MINUTE).remove(0);
        assert_eq!(bucket.private_kib.max, 3_000_000);
        assert_eq!(bucket.private_kib.min, 500_000);
        assert!(bucket.private_kib.avg < 700_000, "среднее спрятало всплеск");
    }

    #[test]
    fn rolling_up_preserves_totals_and_extremes() {
        let points: Vec<SamplePoint> = (0..240)
            .map(|i| point(i * 15_000, 100, 1000 + i as u32))
            .collect();

        let quarters = into_buckets(&points, QUARTER);
        let hours = roll_up(&quarters, HOUR);

        let cpu_before: u64 = quarters.iter().map(|b| b.cpu_ms).sum();
        let cpu_after: u64 = hours.iter().map(|b| b.cpu_ms).sum();
        assert_eq!(cpu_before, cpu_after, "процессорное время потерялось");

        let samples_before: u32 = quarters.iter().map(|b| b.samples).sum();
        let samples_after: u32 = hours.iter().map(|b| b.samples).sum();
        assert_eq!(samples_before, samples_after);

        assert_eq!(
            hours.iter().map(|b| b.private_kib.max).max(),
            quarters.iter().map(|b| b.private_kib.max).max()
        );
    }

    #[test]
    fn rolled_up_buckets_start_on_the_hour() {
        let points: Vec<SamplePoint> = (0..240).map(|i| point(i * 15_000, 100, 1000)).collect();
        let hours = roll_up(&into_buckets(&points, QUARTER), HOUR);
        assert!(hours.iter().all(|b| b.start_ms % HOUR == 0));
    }

    #[test]
    fn weighted_average_respects_sample_counts() {
        // Полный интервал со значением 100 и одиночный замер со значением 1000.
        let full = Bucket {
            start_ms: 0,
            samples: 99,
            private_kib: Stat {
                avg: 100,
                min: 100,
                max: 100,
            },
            ..Default::default()
        };
        let lone = Bucket {
            start_ms: 0,
            samples: 1,
            private_kib: Stat {
                avg: 1000,
                min: 1000,
                max: 1000,
            },
            ..Default::default()
        };

        let merged = full.merge(lone);
        assert_eq!(merged.samples, 100);
        // Невзвешенное среднее дало бы 550 — вчетверо мимо.
        assert_eq!(merged.private_kib.avg, 109);
        assert_eq!(merged.private_kib.max, 1000);
    }

    #[test]
    fn merging_with_an_empty_bucket_changes_nothing() {
        let bucket = Bucket {
            start_ms: 0,
            samples: 5,
            cpu_ms: 500,
            ..Default::default()
        };
        assert_eq!(bucket.merge(Bucket::default()), bucket);
        assert_eq!(Bucket::default().merge(bucket), bucket);
    }

    #[test]
    fn bucket_start_rounds_down_before_the_epoch() {
        assert_eq!(bucket_start(0, MINUTE), 0);
        assert_eq!(bucket_start(MINUTE + 1, MINUTE), MINUTE);
        // Обычное деление здесь уехало бы на интервал вперёд.
        assert_eq!(bucket_start(-1, MINUTE), -MINUTE);
    }

    #[test]
    fn empty_input_gives_empty_output() {
        assert!(into_buckets(&[], MINUTE).is_empty());
        assert!(roll_up(&[], HOUR).is_empty());
    }
}
