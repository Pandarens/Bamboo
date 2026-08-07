//! Ошибки.

use core::fmt;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Вызов NT API вернул неуспешный `NTSTATUS`.
    Nt { call: &'static str, status: i32 },
    /// Вызов Win32 API не удался, код из `GetLastError`.
    Win32 { call: &'static str, code: u32 },
    /// Возможность недоступна на этой версии Windows или в этой конфигурации.
    ///
    /// Отдельный вариант, а не общая ошибка: по ТЗ при невозможности прочитать
    /// данные (например, SMART через RAID-контроллер) полагается честно сказать
    /// «не могу», а не показывать оценку.
    Unsupported(&'static str),
    /// Данные от системы не соответствуют ожидаемой структуре.
    Malformed(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Nt { call, status } => {
                write!(f, "{call} вернул NTSTATUS 0x{:08X}", *status as u32)
            }
            Error::Win32 { call, code } => write!(f, "{call} завершился с ошибкой {code}"),
            Error::Unsupported(what) => write!(f, "недоступно в этой конфигурации: {what}"),
            Error::Malformed(what) => write!(f, "неожиданный формат данных: {what}"),
        }
    }
}

impl std::error::Error for Error {}
