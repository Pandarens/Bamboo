//! Единицы измерения.
//!
//! Смысл обёрток — не дать перепутать байты с килобайтами, а наносекунды
//! с интервалами по 100 нс, в которых Windows отдаёт процессорное время.

use core::fmt;
use core::ops::{Add, AddAssign, Sub};

/// Объём в байтах.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bytes(pub u64);

impl Bytes {
    pub const ZERO: Bytes = Bytes(0);

    pub const fn from_kib(kib: u64) -> Self {
        Bytes(kib * 1024)
    }

    pub const fn from_mib(mib: u64) -> Self {
        Bytes(mib * 1024 * 1024)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn as_kib(self) -> u64 {
        self.0 / 1024
    }

    pub fn as_mib_f64(self) -> f64 {
        self.0 as f64 / (1024.0 * 1024.0)
    }

    /// Разница без ухода в отрицательные значения: счётчики Windows иногда
    /// сбрасываются (перезапуск процесса, переполнение), и паниковать тут нельзя.
    pub const fn saturating_sub(self, other: Bytes) -> Bytes {
        Bytes(self.0.saturating_sub(other.0))
    }
}

impl Add for Bytes {
    type Output = Bytes;
    fn add(self, rhs: Bytes) -> Bytes {
        Bytes(self.0.saturating_add(rhs.0))
    }
}

impl AddAssign for Bytes {
    fn add_assign(&mut self, rhs: Bytes) {
        self.0 = self.0.saturating_add(rhs.0);
    }
}

impl Sub for Bytes {
    type Output = Bytes;
    fn sub(self, rhs: Bytes) -> Bytes {
        self.saturating_sub(rhs)
    }
}

/// Человекочитаемый вид. Основание 1024, подписи привычные — Б, КБ, МБ, ГБ, ТБ.
impl fmt::Display for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
        let mut value = self.0 as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit + 1 < UNITS.len() {
            value /= 1024.0;
            unit += 1;
        }
        if unit == 0 {
            write!(f, "{} {}", self.0, UNITS[0])
        } else if value < 10.0 {
            write!(f, "{value:.2} {}", UNITS[unit])
        } else {
            write!(f, "{value:.1} {}", UNITS[unit])
        }
    }
}

/// Интервал времени в наносекундах.
///
/// Ядро отдаёт процессорное время в интервалах по 100 нс (`LARGE_INTEGER`),
/// поэтому конструктор `from_100ns` — основной способ создания.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nanos(pub u64);

impl Nanos {
    pub const ZERO: Nanos = Nanos(0);

    /// Из интервалов по 100 нс, в которых Windows хранит `UserTime`/`KernelTime`.
    pub const fn from_100ns(ticks: i64) -> Self {
        Nanos((ticks as u64).wrapping_mul(100))
    }

    pub const fn from_millis(ms: u64) -> Self {
        Nanos(ms * 1_000_000)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / 1_000_000_000.0
    }

    pub const fn as_millis(self) -> u64 {
        self.0 / 1_000_000
    }

    pub const fn saturating_sub(self, other: Nanos) -> Nanos {
        Nanos(self.0.saturating_sub(other.0))
    }

    /// Доля одного ядра за интервал: 1.0 означает полностью занятое ядро.
    ///
    /// Возвращает `0.0` при нулевом интервале, чтобы не ловить деление на ноль
    /// на первом тике, когда предыдущего замера ещё нет.
    pub fn core_load(self, over: Nanos) -> f64 {
        if over.0 == 0 {
            0.0
        } else {
            self.0 as f64 / over.0 as f64
        }
    }
}

impl Add for Nanos {
    type Output = Nanos;
    fn add(self, rhs: Nanos) -> Nanos {
        Nanos(self.0.saturating_add(rhs.0))
    }
}

impl AddAssign for Nanos {
    fn add_assign(&mut self, rhs: Nanos) {
        self.0 = self.0.saturating_add(rhs.0);
    }
}

impl Sub for Nanos {
    type Output = Nanos;
    fn sub(self, rhs: Nanos) -> Nanos {
        self.saturating_sub(rhs)
    }
}

impl fmt::Display for Nanos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.as_secs_f64();
        if secs >= 60.0 {
            write!(f, "{} мин {:.0} с", (secs / 60.0) as u64, secs % 60.0)
        } else if secs >= 1.0 {
            write!(f, "{secs:.1} с")
        } else {
            write!(f, "{:.0} мс", secs * 1000.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_display_scales() {
        assert_eq!(Bytes(512).to_string(), "512 Б");
        assert_eq!(Bytes::from_kib(1536).to_string(), "1.50 МБ");
        assert_eq!(Bytes::from_mib(2048).to_string(), "2.00 ГБ");
    }

    #[test]
    fn bytes_never_go_negative() {
        assert_eq!(Bytes(10) - Bytes(100), Bytes::ZERO);
    }

    #[test]
    fn cpu_time_converts_from_kernel_ticks() {
        // 10 000 интервалов по 100 нс = 1 мс.
        assert_eq!(Nanos::from_100ns(10_000), Nanos::from_millis(1));
    }

    #[test]
    fn core_load_is_a_share_of_one_core() {
        let spent = Nanos::from_millis(500);
        let window = Nanos::from_millis(1000);
        assert!((spent.core_load(window) - 0.5).abs() < f64::EPSILON);
        assert_eq!(spent.core_load(Nanos::ZERO), 0.0);
    }
}
