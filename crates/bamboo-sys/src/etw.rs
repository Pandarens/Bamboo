//! Сессия трассировки ETW (ТЗ, раздел 7.5).
//!
//! Зачем это нужно вообще: опрос раз в пять секунд не видит процессы,
//! живущие две секунды. А именно короткоживущие процессы — `CompatTelRunner`,
//! `MoUsoCoreWorker`, `TiWorker`, сканы Defender — и вызывают внезапные фризы.
//! Пользователь открывает диспетчер задач, а там уже пусто.
//!
//! Своя обёртка, а не готовый крейт: нужен контроль над размером буферов
//! и режимом реального времени.
//!
//! Значения свойств читаем через TDH по именам, а не по смещениям в сыром
//! буфере. Смещения зависят от версии события и молча разъезжаются между
//! сборками Windows; имена — часть манифеста провайдера и стабильны.

use core::ffi::c_void;

use bamboo_core::{Error, Result};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_SUCCESS};
use windows_sys::Win32::System::Diagnostics::Etw::*;

/// Провайдер `Microsoft-Windows-Kernel-Process`.
pub const KERNEL_PROCESS_GUID: GUID = GUID::from_u128(0x22FB2CD6_0E7B_422B_A0C7_2FAD1FD0E716);
/// Ключевое слово `WINEVENT_KEYWORD_PROCESS`: только запуск и завершение
/// процессов, без потоков и загрузки образов.
pub const KEYWORD_PROCESS: u64 = 0x10;

/// Идентификаторы событий провайдера Kernel-Process.
pub const EVENT_PROCESS_START: u16 = 1;
pub const EVENT_PROCESS_STOP: u16 = 2;

/// Размер буфера сессии. Больше не нужно: событий десятки в минуту.
const BUFFER_SIZE_KB: u32 = 64;
const MIN_BUFFERS: u32 = 4;
/// Сбрасывать буферы раз в секунду, иначе редкие события залипают в них
/// до заполнения.
const FLUSH_TIMER_SECONDS: u32 = 1;

/// Что делать после обработки события.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// Заголовок события.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventHeader {
    pub event_id: u16,
    pub process_id: u32,
    pub thread_id: u32,
    /// Время события, миллисекунды эпохи Unix.
    pub at_unix_ms: i64,
}

/// Доступ к полям события. Живёт только внутри обработчика.
pub struct EventFields {
    record: *const EVENT_RECORD,
}

impl EventFields {
    /// Числовое поле. Читается по имени из манифеста провайдера.
    pub fn number(&self, name: &str) -> Option<u64> {
        let bytes = self.raw(name)?;
        Some(match bytes.len() {
            1 => bytes[0] as u64,
            2 => u16::from_le_bytes([bytes[0], bytes[1]]) as u64,
            4 => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64,
            8 => u64::from_le_bytes(bytes[..8].try_into().ok()?),
            _ => return None,
        })
    }

    /// Строковое поле в UTF-16.
    pub fn text(&self, name: &str) -> Option<String> {
        let bytes = self.raw(name)?;
        if bytes.len() < 2 {
            return None;
        }
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        Some(String::from_utf16_lossy(&units))
    }

    fn raw(&self, name: &str) -> Option<Vec<u8>> {
        let wide: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();

        let descriptor = PROPERTY_DATA_DESCRIPTOR {
            PropertyName: wide.as_ptr() as u64,
            ArrayIndex: u32::MAX,
            Reserved: 0,
        };

        let mut size: u32 = 0;
        // SAFETY: record жив всё время работы обработчика, дескриптор
        // указывает на буфер имени, который живёт до конца функции.
        let status = unsafe {
            TdhGetPropertySize(self.record, 0, core::ptr::null(), 1, &descriptor, &mut size)
        };
        if status != ERROR_SUCCESS || size == 0 || size > 64 * 1024 {
            return None;
        }

        let mut buffer = vec![0u8; size as usize];
        let status = unsafe {
            TdhGetProperty(
                self.record,
                0,
                core::ptr::null(),
                1,
                &descriptor,
                size,
                buffer.as_mut_ptr(),
            )
        };
        (status == ERROR_SUCCESS).then_some(buffer)
    }
}

/// Управляющий дескриптор сессии.
///
/// Сессия ETW переживает создавший её процесс. Если не остановить — она
/// останется висеть в системе и будет копить буферы до перезагрузки,
/// а следующий запуск Bamboo получит `ERROR_ALREADY_EXISTS`. Поэтому
/// остановка и в `Drop`, и отдельной функцией на старте.
pub struct Session {
    name: Vec<u16>,
    handle: CONTROLTRACE_HANDLE,
}

impl Session {
    /// Запускает сессию реального времени.
    pub fn start(name: &str) -> Result<Session> {
        // Подчищаем за прошлым запуском, если он не успел остановиться.
        let _ = stop_stale(name);

        let wide: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();
        let mut buffer = properties_buffer(&wide);
        let mut handle: CONTROLTRACE_HANDLE = Default::default();

        let status = unsafe {
            StartTraceW(
                &mut handle,
                wide.as_ptr(),
                buffer.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>(),
            )
        };

        match status {
            ERROR_SUCCESS => Ok(Session { name: wide, handle }),
            ERROR_ACCESS_DENIED => Err(Error::Unsupported(
                "запуск сессии ETW требует прав администратора \
                 или членства в группе «Пользователи журналов производительности»",
            )),
            ERROR_ALREADY_EXISTS => {
                Err(Error::Unsupported("сессия ETW с таким именем уже запущена"))
            }
            code => Err(Error::Win32 {
                call: "StartTraceW",
                code,
            }),
        }
    }

    /// Подключает провайдера к сессии.
    pub fn enable_provider(&self, provider: &GUID, keywords: u64) -> Result<()> {
        const LEVEL_INFORMATION: u8 = 4;

        let status = unsafe {
            EnableTraceEx2(
                self.handle,
                provider,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER,
                LEVEL_INFORMATION,
                keywords,
                0,
                0,
                core::ptr::null(),
            )
        };

        if status != ERROR_SUCCESS {
            return Err(Error::Win32 {
                call: "EnableTraceEx2",
                code: status,
            });
        }
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.handle.Value == 0 {
            return Ok(());
        }
        let mut buffer = properties_buffer(&self.name);
        let status = unsafe {
            ControlTraceW(
                self.handle,
                core::ptr::null(),
                buffer.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>(),
                EVENT_TRACE_CONTROL_STOP,
            )
        };
        self.handle = Default::default();

        if status != ERROR_SUCCESS {
            return Err(Error::Win32 {
                call: "ControlTraceW(stop)",
                code: status,
            });
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Останавливает сессию, оставшуюся от прошлого запуска.
pub fn stop_stale(name: &str) -> Result<()> {
    let wide: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();
    let mut buffer = properties_buffer(&wide);

    let status = unsafe {
        ControlTraceW(
            Default::default(),
            wide.as_ptr(),
            buffer.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>(),
            EVENT_TRACE_CONTROL_STOP,
        )
    };

    if status != ERROR_SUCCESS {
        return Err(Error::Win32 {
            call: "ControlTraceW(stop stale)",
            code: status,
        });
    }
    Ok(())
}

/// Готовит буфер `EVENT_TRACE_PROPERTIES` вместе с местом под имя сессии.
///
/// Имя должно лежать в том же выделении сразу за структурой, а его смещение —
/// в `LoggerNameOffset`. Отдельным указателем здесь передать нельзя.
fn properties_buffer(name: &[u16]) -> Vec<u8> {
    let header = core::mem::size_of::<EVENT_TRACE_PROPERTIES>();
    let name_bytes = name.len() * 2;
    let mut buffer = vec![0u8; header + name_bytes];

    // SAFETY: буфер только что выделен нужного размера и выровнен,
    // Vec<u8> даёт выравнивание не хуже, чем нужно структуре с u64.
    let properties = unsafe { &mut *buffer.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>() };
    properties.Wnode.BufferSize = (header + name_bytes) as u32;
    properties.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
    // ClientContext = 2 означает системное время: отметки событий приезжают
    // в FILETIME и их можно сопоставить с журналами Windows. По умолчанию
    // здесь счётчик производительности, привязать который не к чему.
    properties.Wnode.ClientContext = 2;
    properties.BufferSize = BUFFER_SIZE_KB;
    properties.MinimumBuffers = MIN_BUFFERS;
    properties.MaximumBuffers = MIN_BUFFERS * 4;
    properties.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
    properties.FlushTimer = FLUSH_TIMER_SECONDS;
    properties.LoggerNameOffset = header as u32;

    buffer
}

/// Контекст, который прокидывается в обработчик событий ETW.
struct CallbackContext<'a> {
    on_event: &'a mut dyn FnMut(EventHeader, &EventFields) -> Flow,
    stop_requested: bool,
}

/// Читает события сессии. Блокирует поток до остановки сессии.
///
/// Обработчик получает заголовок и доступ к полям. Вернув `Flow::Stop`,
/// он просит завершить чтение — фактическая остановка произойдёт,
/// когда сессия будет закрыта.
pub fn consume(
    session_name: &str,
    on_event: &mut dyn FnMut(EventHeader, &EventFields) -> Flow,
) -> Result<()> {
    let mut wide: Vec<u16> = session_name
        .encode_utf16()
        .chain(core::iter::once(0))
        .collect();

    let mut context = CallbackContext {
        on_event,
        stop_requested: false,
    };

    let mut logfile: EVENT_TRACE_LOGFILEW = unsafe { core::mem::zeroed() };
    logfile.LoggerName = wide.as_mut_ptr();
    logfile.Anonymous1.ProcessTraceMode =
        PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    logfile.Anonymous2.EventRecordCallback = Some(event_callback);
    logfile.Context = (&mut context as *mut CallbackContext).cast::<c_void>();

    let handle = unsafe { OpenTraceW(&mut logfile) };
    // Признак неудачи у OpenTraceW — не ноль, а все единицы.
    if handle.Value == u64::MAX {
        return Err(Error::Win32 {
            call: "OpenTraceW",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }

    let status = unsafe { ProcessTrace(&handle, 1, core::ptr::null(), core::ptr::null()) };
    unsafe { CloseTrace(handle) };

    if status != ERROR_SUCCESS {
        return Err(Error::Win32 {
            call: "ProcessTrace",
            code: status,
        });
    }
    Ok(())
}

/// Обработчик, который вызывает ETW на каждое событие.
unsafe extern "system" fn event_callback(record: *mut EVENT_RECORD) {
    if record.is_null() {
        return;
    }

    let context = (*record).UserContext.cast::<CallbackContext>();
    if context.is_null() {
        return;
    }
    let context = &mut *context;
    if context.stop_requested {
        return;
    }

    let descriptor = (*record).EventHeader.EventDescriptor;
    let header = EventHeader {
        event_id: descriptor.Id,
        process_id: (*record).EventHeader.ProcessId,
        thread_id: (*record).EventHeader.ThreadId,
        at_unix_ms: bamboo_core::time::filetime_to_unix_ms((*record).EventHeader.TimeStamp),
    };

    let fields = EventFields { record };
    if (context.on_event)(header, &fields) == Flow::Stop {
        context.stop_requested = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_buffer_has_the_name_right_after_the_struct() {
        let name: Vec<u16> = "проверка"
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        let buffer = properties_buffer(&name);

        let header = core::mem::size_of::<EVENT_TRACE_PROPERTIES>();
        assert_eq!(buffer.len(), header + name.len() * 2);

        let properties = unsafe { &*buffer.as_ptr().cast::<EVENT_TRACE_PROPERTIES>() };
        assert_eq!(properties.LoggerNameOffset as usize, header);
        assert_eq!(properties.Wnode.BufferSize as usize, buffer.len());
        assert_eq!(properties.BufferSize, BUFFER_SIZE_KB);
        assert_eq!(properties.LogFileMode, EVENT_TRACE_REAL_TIME_MODE);
        assert_eq!(properties.FlushTimer, FLUSH_TIMER_SECONDS);
        // Системное время вместо счётчика производительности.
        assert_eq!(properties.Wnode.ClientContext, 2);
    }

    #[test]
    fn starting_a_session_either_works_or_says_what_is_missing() {
        // Без прав администратора сессия не запустится, и это штатно.
        // Важно, что причина названа понятно, а не кодом ошибки.
        match Session::start("bamboo-test-session") {
            Ok(mut session) => {
                session
                    .enable_provider(&KERNEL_PROCESS_GUID, KEYWORD_PROCESS)
                    .expect("провайдер не подключился к своей же сессии");
                session.stop().expect("сессия не остановилась");
            }
            Err(error) => {
                let text = error.to_string();
                assert!(
                    text.contains("администратора") || text.contains("StartTraceW"),
                    "непонятная причина отказа: {text}"
                );
            }
        }
    }

    #[test]
    fn stopping_a_session_that_does_not_exist_is_an_error_not_a_crash() {
        assert!(stop_stale("bamboo-такой-сессии-нет").is_err());
    }
}
