//! Трассировка процессов через ETW.
//!
//! Решает проблему, которую опросом не решить в принципе: снимок раз
//! в пять секунд не видит процесс, живущий две. А именно короткоживущие
//! процессы — `CompatTelRunner`, `MoUsoCoreWorker`, `TiWorker`, сканы
//! Defender, задачи планировщика — и вызывают внезапные фризы. Пользователь
//! открывает диспетчер задач, а там уже пусто.
//!
//! Учащать опрос бессмысленно: чтобы поймать двухсекундный процесс, пришлось
//! бы опрашивать систему раз в секунду круглосуточно, и Bamboo сам стал бы
//! той нагрузкой, с которой борется.
//!
//! Разбор событий и видеорегистратор — чистая логика без `unsafe`.
//! Работа с самим ETW живёт в `bamboo-sys`, как и весь остальной `unsafe`.

#![forbid(unsafe_code)]

pub mod event;
pub mod recorder;
pub mod tracker;

#[cfg(windows)]
pub mod session;

pub use event::{CompletedProcess, ProcessEvent};
pub use recorder::{Conditions, Dump, Recorder, TriggerReason};
pub use tracker::{ImageSummary, ProcessTracker};

#[cfg(windows)]
pub use session::{stop_stale, ProcessTrace, SESSION_NAME};
