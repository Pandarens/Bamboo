//! Обёртки над системными API Windows.
//!
//! Единственный крейт Bamboo, где разрешён `unsafe`. Во всех остальных стоит
//! `#![forbid(unsafe_code)]` — это делает аудит выполнимым: чтобы проверить
//! проект на корректность работы с памятью, достаточно прочитать один крейт.
//!
//! Наружу отдаются только безопасные типы из `bamboo-core`.

#![cfg(windows)]

pub mod budget;
pub mod clock;
pub mod cpu;
pub mod memory;
pub mod nt;
pub mod power;
pub mod process;
pub mod storage;
pub mod user;

pub use budget::{apply_self_limits, own_memory, OwnMemory};
pub use clock::{monotonic_ms, now};
pub use cpu::CpuTimesBuffer;
pub use memory::{memory_stat, system_counts, SystemCounts};
pub use power::{power_status, PowerSource, PowerStatus};
pub use process::{ProcessBuffer, ProcessIter, RawProcess};
pub use storage::{enumerate as enumerate_drives, read_smart, Drive};
pub use user::{idle_ms, notification_state, NotificationState};
