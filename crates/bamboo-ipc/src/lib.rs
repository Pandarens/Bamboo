//! Протокол связи агента и брокера (ТЗ, раздел 13).
//!
//! Агент работает без прав администратора, брокер — под SYSTEM. Всё, что
//! проходит по каналу, приходит от менее доверенной стороны к более
//! доверенной, поэтому брокер не верит агенту ни в чём: ни в размере
//! кадра, ни в допустимости запроса.
//!
//! Отсутствие этих проверок превращает Bamboo в локальную уязвимость
//! повышения привилегий — любой процесс сможет попросить SYSTEM-службу
//! выполнить действие.

#![forbid(unsafe_code)]

pub mod frame;
pub mod message;
pub mod pipe;

pub use frame::{decode, decode_message, encode, encode_message, MAX_FRAME_BYTES};
pub use message::{ErrorCode, Request, Response, Scope, Stream};
pub use pipe::{pipe_name, PipeGuard};
