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

/// Разбирает отметку времени в формате журналов Windows.
///
/// Вид `2026-08-07T09:12:33.1234567Z`: журнал событий отдаёт время в UTC
/// с точностью до 100 нс. Разбираем руками, чтобы не тащить в проект
/// библиотеку работы с датами ради одного формата.
///
/// Возвращает миллисекунды от начала эпохи Unix.
pub fn parse_iso8601_utc_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }

    let number = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();

    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    // Дробная часть может быть любой длины или отсутствовать вовсе.
    let fraction_ms = match text.get(19..20) {
        Some(".") => {
            let digits: String = text[20..]
                .chars()
                .take_while(char::is_ascii_digit)
                .take(3)
                .collect();
            let padded = format!("{digits:0<3}");
            padded.parse::<i64>().unwrap_or(0)
        }
        _ => 0,
    };

    let days = days_from_civil(year, month as u32, day as u32);
    Some(((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1000) + fraction_ms)
}

/// Число дней от 1970-01-01. Алгоритм Говарда Хиннанта: работает
/// для любых дат григорианского календаря без таблиц и ветвлений по годам.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year =
        (153 * (month as i64 + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_zero() {
        assert_eq!(
            parse_iso8601_utc_ms("1970-01-01T00:00:00.0000000Z"),
            Some(0)
        );
    }

    #[test]
    fn a_real_event_timestamp_is_parsed() {
        let ms = parse_iso8601_utc_ms("2026-08-07T09:12:33.1234567Z").unwrap();
        assert_eq!(ms, 1_786_093_953_123);

        // Перепроверка независимым способом: складываем начало года,
        // прошедшие сутки и время внутри суток.
        let year_start = parse_iso8601_utc_ms("2026-01-01T00:00:00Z").unwrap();
        let day_of_year = 31 + 28 + 31 + 30 + 31 + 30 + 31 + 7; // 7 августа
        let expected =
            year_start + (day_of_year - 1) * 86_400_000 + (9 * 3600 + 12 * 60 + 33) * 1000 + 123;
        assert_eq!(ms, expected);
    }

    #[test]
    fn fractional_part_is_optional() {
        let with = parse_iso8601_utc_ms("2024-02-29T12:00:00.5000000Z").unwrap();
        let without = parse_iso8601_utc_ms("2024-02-29T12:00:00Z").unwrap();
        assert_eq!(with - without, 500);
    }

    #[test]
    fn leap_years_are_handled() {
        let feb29 = parse_iso8601_utc_ms("2024-02-29T00:00:00Z").unwrap();
        let mar01 = parse_iso8601_utc_ms("2024-03-01T00:00:00Z").unwrap();
        assert_eq!(mar01 - feb29, 86_400_000);
    }

    #[test]
    fn century_rule_is_handled() {
        // 1900 не високосный, 2000 — високосный.
        let a = parse_iso8601_utc_ms("1900-02-28T00:00:00Z").unwrap();
        let b = parse_iso8601_utc_ms("1900-03-01T00:00:00Z").unwrap();
        assert_eq!(b - a, 86_400_000);

        let c = parse_iso8601_utc_ms("2000-02-28T00:00:00Z").unwrap();
        let d = parse_iso8601_utc_ms("2000-03-01T00:00:00Z").unwrap();
        assert_eq!(d - c, 2 * 86_400_000);
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(parse_iso8601_utc_ms(""), None);
        assert_eq!(parse_iso8601_utc_ms("не дата"), None);
        assert_eq!(parse_iso8601_utc_ms("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_iso8601_utc_ms("2026-08-07 09:12:33"), None);
    }

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
