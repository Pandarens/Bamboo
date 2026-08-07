//! Отметки времени сэмплов.

use std::time::{SystemTime, UNIX_EPOCH};

/// Момент снятия сэмпла.
///
/// Хранятся обе шкалы. Монотонная нужна для вычисления интервалов: настенные
/// часы прыгают при синхронизации времени и при выходе из сна, и на них нельзя
/// считать загрузку процессора. Настенная нужна, чтобы показать пользователю
/// «в 03:14» и связать сэмпл с событиями журналов Windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SampleTime {
    /// Миллисекунды от произвольной точки отсчёта, монотонно неубывающие.
    pub monotonic_ms: u64,
    /// Миллисекунды от начала эпохи Unix.
    pub unix_ms: i64,
}

impl SampleTime {
    pub fn new(monotonic_ms: u64, unix_ms: i64) -> Self {
        SampleTime {
            monotonic_ms,
            unix_ms,
        }
    }

    /// Настенное время «сейчас». Монотонную часть заполняет вызывающий:
    /// её источник зависит от платформы.
    pub fn wall_clock_now() -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_millis() as i64,
            Err(e) => -(e.duration().as_millis() as i64),
        }
    }

    /// Интервал до более позднего сэмпла, в миллисекундах.
    pub fn elapsed_ms_since(&self, earlier: SampleTime) -> u64 {
        self.monotonic_ms.saturating_sub(earlier.monotonic_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_uses_monotonic_clock() {
        // Настенные часы отпрыгнули назад, монотонные — нет.
        let earlier = SampleTime::new(1_000, 1_700_000_000_000);
        let later = SampleTime::new(6_000, 1_600_000_000_000);
        assert_eq!(later.elapsed_ms_since(earlier), 5_000);
    }

    #[test]
    fn interval_never_goes_negative() {
        let a = SampleTime::new(6_000, 0);
        let b = SampleTime::new(1_000, 0);
        assert_eq!(b.elapsed_ms_since(a), 0);
    }
}
