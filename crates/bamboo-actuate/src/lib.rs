//! Исполнение действий и обратных рецептов.
//!
//! Единственный путь, которым Bamboo меняет систему. Всё, что меняет
//! состояние, проходит здесь и, значит, проходит через политику и журнал.
//! Обойти этот путь нельзя — в этом и смысл.

#![forbid(unsafe_code)]

pub mod executor;
pub mod state;

#[cfg(windows)]
mod windows;

pub use executor::{Backend, Executor, Outcome};
pub use state::{yes_no, PriorState};

#[cfg(windows)]
pub use windows::SystemBackend;
