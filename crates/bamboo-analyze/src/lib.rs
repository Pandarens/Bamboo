//! Анализаторы Bamboo.
//!
//! Крейт не знает о Windows и не имеет права о ней узнать. На вход —
//! временные ряды и метаданные, на выход — наблюдения. Это позволяет
//! прогонять всю логику детекта на записанных трассах без живой системы:
//! проверить «правильно ли детектируется утечка», дёргая настоящий Windows,
//! невозможно.

#![forbid(unsafe_code)]

pub mod baseline;
pub mod boot;
pub mod driver;
pub mod growth;
pub mod idle;
pub mod observation;
pub mod origin;
pub mod regression;
pub mod report;
pub mod spike;
pub mod tbw;
pub mod wear;

pub use baseline::{Baseline, Learning};
pub use boot::{BootPoint, BootVerdict};
pub use driver::DriverInput;
pub use growth::{GrowthInput, Point};
pub use idle::IdleInput;
pub use observation::{Observation, ObservationKind, Severity};
pub use origin::{attribute, Origin, ProcessDescriptor};
pub use regression::{fit, Trend};
pub use report::{weekly_html, weekly_json, weekly_markdown, ActionEffect, WeeklyData};
pub use spike::SpikeInput;
pub use tbw::{rating_for, TbwRating};
pub use wear::{WearInput, WearVerdict};
