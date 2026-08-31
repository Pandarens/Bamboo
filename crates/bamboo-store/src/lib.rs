//! Персистентность Bamboo.
//!
//! Задача, ради которой крейт существует (ТЗ, раздел 8.1): 300 процессов
//! на сэмпл раз в 5 секунд — это 5.2 миллиона записей в сутки. Хранить
//! строку на процесс на сэмпл нельзя, поэтому данные живут на четырёх
//! уровнях с падающим разрешением:
//!
//! | Уровень | Разрешение | Глубина | Где |
//! |---|---|---|---|
//! | L0 | 1 с | 10 мин | память, `bamboo-collect` |
//! | L1 | 1 мин | 24 ч | память, `bamboo-collect` |
//! | L2 | 15 мин | 30 сут | SQLite, здесь |
//! | L3 | 1 ч | 12 мес | SQLite, здесь |

#![forbid(unsafe_code)]

pub mod aggregate;
pub mod schema;
pub mod store;
pub mod trace;

pub use aggregate::{bucket_start, into_buckets, roll_up, Bucket, SamplePoint, Stat};
pub use schema::{L2_BUCKET_MS, L2_RETENTION_MS, L3_BUCKET_MS, L3_RETENTION_MS, SCHEMA_VERSION};
pub use store::{BootEntry, FreezeEntry, Level, SmartSnapshot, Store};
pub use trace::{Trace, TraceFrame, TraceProcess, TRACE_VERSION};
