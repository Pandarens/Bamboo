//! Базовые типы Bamboo.
//!
//! Крейт намеренно не зависит ни от Windows, ни от чего-либо ещё: это позволяет
//! собирать и тестировать его на любой платформе и не тащить системные типы
//! в аналитику.

#![forbid(unsafe_code)]

pub mod app;
pub mod error;
pub mod process;
pub mod storage;
pub mod system;
pub mod time;
pub mod units;

pub use app::AppKey;
pub use error::{Error, Result};
pub use process::{Pid, ProcessId, ProcessSample};
pub use storage::{BusType, CriticalWarning, DriveInfo, SmartHealth, SmartSource};
pub use system::{CoreTimes, MemoryStat, SystemSample};
pub use time::SampleTime;
pub use units::{Bytes, Nanos};
