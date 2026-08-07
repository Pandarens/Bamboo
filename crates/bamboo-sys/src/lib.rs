//! Обёртки над системными API Windows.
//!
//! Единственный крейт Bamboo, где разрешён `unsafe`. Во всех остальных стоит
//! `#![forbid(unsafe_code)]` — это делает аудит выполнимым: чтобы проверить
//! проект на корректность работы с памятью, достаточно прочитать один крейт.
//!
//! Наружу отдаются только безопасные типы из `bamboo-core`.

#![cfg(windows)]

pub mod nt;
pub mod process;

pub use process::{ProcessBuffer, ProcessIter, RawProcess};
