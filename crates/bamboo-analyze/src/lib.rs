//! Анализаторы Bamboo.
//!
//! Крейт не знает о Windows и не имеет права о ней узнать. На вход —
//! временные ряды и метаданные, на выход — наблюдения. Это позволяет
//! прогонять всю логику детекта на записанных трассах без живой системы:
//! проверить «правильно ли детектируется утечка», дёргая настоящий Windows,
//! невозможно.

#![forbid(unsafe_code)]

pub mod boot;
pub mod growth;
pub mod observation;
pub mod regression;
pub mod tbw;
pub mod wear;

pub use boot::{BootPoint, BootVerdict};
pub use growth::{GrowthInput, Point};
pub use observation::{Observation, ObservationKind, Severity};
pub use regression::{fit, Trend};
pub use tbw::{rating_for, TbwRating};
pub use wear::{WearInput, WearVerdict};
