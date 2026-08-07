//! Сбор метрик: опрос системы, дельты, кольцевые буферы.
//!
//! Здесь живёт уровень L0 из раздела 8.2 ТЗ — история в памяти, без диска.
//! Уровни L1–L3 и персистентность появятся в `bamboo-store`.

#![forbid(unsafe_code)]

pub mod cadence;
pub mod ring;
pub mod table;

#[cfg(windows)]
pub mod collector;

pub use cadence::{Cadence, CadenceController, Conditions};
pub use ring::RingBuffer;
pub use table::{MetricPoint, ProcessIdentity, ProcessTable, TickChanges, TrackedProcess, L0_CAPACITY};

#[cfg(windows)]
pub use collector::{Collector, Tick};
