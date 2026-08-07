//! Живая сессия трассировки: превращает события ETW в события процессов.

use bamboo_core::Result;
use bamboo_sys::etw::{self, EventFields, EventHeader, Flow, Session};

use crate::event::ProcessEvent;

/// Имя сессии. Постоянное: по нему же подчищаем сессию, оставшуюся
/// от прошлого запуска.
pub const SESSION_NAME: &str = "Bamboo-KernelProcess";

/// Постоянная сессия на провайдере `Kernel-Process`.
///
/// Объём — десятки событий в минуту на типичной системе, стоимость
/// пренебрежима (ТЗ, раздел 7.1).
pub struct ProcessTrace {
    session: Session,
}

impl ProcessTrace {
    /// Запускает сессию и подключает провайдера.
    pub fn start() -> Result<ProcessTrace> {
        let session = Session::start(SESSION_NAME)?;
        session.enable_provider(&etw::KERNEL_PROCESS_GUID, etw::KEYWORD_PROCESS)?;
        Ok(ProcessTrace { session })
    }

    /// Останавливает сессию.
    ///
    /// Можно вызывать из другого потока — это и есть способ прервать
    /// `consume`, который блокируется до конца сессии.
    pub fn stop(&mut self) -> Result<()> {
        self.session.stop()
    }

    /// Читает события до остановки сессии.
    ///
    /// Блокирует поток. Обработчик получает разобранное событие;
    /// вернув `false`, он просит прекратить чтение.
    pub fn consume(on_event: &mut dyn FnMut(ProcessEvent) -> bool) -> Result<()> {
        let mut handler = |header: EventHeader, fields: &EventFields| {
            match parse(&header, fields) {
                Some(event) => {
                    if on_event(event) {
                        Flow::Continue
                    } else {
                        Flow::Stop
                    }
                }
                // Событие не из тех, что нас интересуют, либо в нём нет
                // обязательных полей. Пропускаем молча: провайдер шлёт
                // и служебные записи.
                None => Flow::Continue,
            }
        };

        etw::consume(SESSION_NAME, &mut handler)
    }
}

impl Drop for ProcessTrace {
    fn drop(&mut self) {
        // Сессия ETW переживает создавший её процесс. Не остановить —
        // значит оставить её висеть в системе до перезагрузки.
        let _ = self.session.stop();
    }
}

/// Разбирает событие провайдера в событие процесса.
fn parse(header: &EventHeader, fields: &EventFields) -> Option<ProcessEvent> {
    match header.event_id {
        etw::EVENT_PROCESS_START => Some(ProcessEvent::Started {
            at_unix_ms: header.at_unix_ms,
            pid: fields.number("ProcessID")? as u32,
            parent_pid: fields.number("ParentProcessID").unwrap_or(0) as u32,
            // Имя приходит однобайтовой строкой; если его нет,
            // событие бесполезно.
            image_name: fields.text("ImageName")?,
            session_id: fields.number("SessionID").unwrap_or(0) as u32,
        }),

        etw::EVENT_PROCESS_STOP => Some(ProcessEvent::Stopped {
            at_unix_ms: header.at_unix_ms,
            pid: fields.number("ProcessID")? as u32,
            exit_code: fields.number("ExitCode").unwrap_or(0) as u32,
        }),

        _ => None,
    }
}

/// Останавливает сессию, оставшуюся от прошлого запуска.
pub fn stop_stale() -> Result<()> {
    etw::stop_stale(SESSION_NAME)
}
