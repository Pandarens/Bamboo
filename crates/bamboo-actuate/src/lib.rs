//! Исполнение действий и обратных рецептов.
//!
//! Единственный путь, которым Bamboo меняет систему. Всё, что меняет
//! состояние, проходит здесь и, значит, проходит через политику и журнал.
//! Обойти этот путь нельзя — в этом и смысл.

#![forbid(unsafe_code)]

pub mod executor;
pub mod state;
pub mod watchdog;

#[cfg(windows)]
mod health;
#[cfg(windows)]
mod windows;

pub use executor::{Backend, Executor, Outcome};
pub use state::{yes_no, PriorState};
pub use watchdog::{remember_reverted, sweep, HealthProbe, WatchdogSweep};

#[cfg(windows)]
pub use health::LiveHealthProbe;
#[cfg(windows)]
pub use windows::SystemBackend;
