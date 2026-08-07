//! Часы для отметок сэмплов.

use bamboo_core::SampleTime;
use windows_sys::Win32::System::SystemInformation::GetTickCount64;

/// Монотонное время в миллисекундах от загрузки системы.
///
/// Настенные часы для интервалов не годятся: они прыгают при синхронизации
/// времени и при выходе из сна, а на прыжке назад загрузка процессора
/// вычисляется как деление на отрицательный интервал.
pub fn monotonic_ms() -> u64 {
    unsafe { GetTickCount64() }
}

/// Отметка «сейчас» для сэмпла: монотонная и настенная шкалы вместе.
pub fn now() -> SampleTime {
    SampleTime::new(monotonic_ms(), SampleTime::wall_clock_now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_clock_moves_forward() {
        let first = monotonic_ms();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let second = monotonic_ms();
        assert!(second >= first);
        assert!(second - first >= 30, "прошло всего {} мс", second - first);
    }

    #[test]
    fn wall_clock_is_in_a_plausible_range() {
        // Больше 2020 года и меньше 2100-го: ловим перепутанные единицы.
        let unix_ms = now().unix_ms;
        assert!(unix_ms > 1_577_836_800_000);
        assert!(unix_ms < 4_102_444_800_000);
    }
}
