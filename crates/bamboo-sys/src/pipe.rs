//! Именованный канал между агентом и брокером (ТЗ, разделы 3.2 и 3.3).
//!
//! Самое опасное место проекта. Брокер работает под SYSTEM и выполняет
//! по этому каналу привилегированные операции. Ошибка в дескрипторе
//! безопасности превращает Bamboo в готовый способ повысить привилегии
//! до SYSTEM: любой процесс сможет попросить службу что-нибудь сделать.
//!
//! Отсюда три независимых уровня защиты:
//!
//! 1. Дескриптор безопасности — кто вообще может открыть канал
//! 2. `PIPE_REJECT_REMOTE_CLIENTS` — никаких клиентов по сети
//! 3. Проверка образа подключившегося процесса — кто именно пришёл

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
};

/// Дескриптор безопасности канала в языке SDDL.
///
/// Разбор по частям:
/// - `D:P` — свой список доступа, наследование от родителя отключено
/// - `(D;;GA;;;AN)` — запретить анонимному входу
/// - `(D;;GA;;;NU)` — запретить пришедшим по сети
/// - `(A;;GA;;;SY)` — разрешить SYSTEM: под ним работает сам брокер
/// - `(A;;GA;;;BA)` — разрешить `BUILTIN\Administrators`
/// - `(A;;GA;;;IU)` — разрешить интерактивным пользователям
///
/// Про `Everyone` отдельно. Соблазн написать `(D;;GA;;;WD)` и явно
/// запретить всем очень велик, но так делать нельзя: запрещающие записи
/// проверяются раньше разрешающих, а в `Everyone` входит **каждый**
/// пользователь, включая интерактивного. Такой запрет закрывает канал
/// вообще для всех, и агент перестаёт подключаться.
///
/// Правильный способ — не упоминать `Everyone` вовсе. Список доступа
/// работает по принципу «что не разрешено, то запрещено», поэтому
/// отсутствие в списке и есть отказ.
const PIPE_SDDL: &str = "D:P(D;;GA;;;AN)(D;;GA;;;NU)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)";

/// Размер буферов канала.
const BUFFER_BYTES: u32 = 64 * 1024;

/// Дескриптор безопасности, собранный из SDDL.
struct SecurityDescriptor(*mut core::ffi::c_void);

impl SecurityDescriptor {
    fn from_sddl(sddl: &str) -> Result<SecurityDescriptor> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(core::iter::once(0)).collect();
        let mut descriptor: *mut core::ffi::c_void = core::ptr::null_mut();

        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                core::ptr::null_mut(),
            )
        };

        if ok == 0 || descriptor.is_null() {
            return Err(Error::Win32 {
                call: "ConvertStringSecurityDescriptorToSecurityDescriptorW",
                code: unsafe { GetLastError() },
            });
        }
        Ok(SecurityDescriptor(descriptor))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

/// Серверный конец канала.
pub struct PipeServer {
    handle: HANDLE,
    /// Дескриптор безопасности должен пережить создание канала.
    _descriptor: SecurityDescriptor,
}

impl PipeServer {
    /// Создаёт канал.
    ///
    /// `first_instance` обязателен для первого экземпляра: без флага
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` чужой процесс может успеть создать
    /// канал с таким же именем раньше нас и перехватывать запросы.
    pub fn create(name: &str) -> Result<PipeServer> {
        let descriptor = SecurityDescriptor::from_sddl(PIPE_SDDL)?;

        let attributes = SECURITY_ATTRIBUTES {
            nLength: core::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };

        let wide: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();

        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                // Режим сообщений, а не потока байт, и отказ удалённым
                // клиентам на уровне системы.
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                4,
                BUFFER_BYTES,
                BUFFER_BYTES,
                0,
                &attributes,
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            return Err(Error::Win32 {
                call: "CreateNamedPipeW",
                code: unsafe { GetLastError() },
            });
        }

        Ok(PipeServer {
            handle,
            _descriptor: descriptor,
        })
    }

    /// Ждёт подключения клиента.
    pub fn accept(&self) -> Result<u32> {
        let ok = unsafe { ConnectNamedPipe(self.handle, core::ptr::null_mut()) };
        let code = unsafe { GetLastError() };

        // Клиент мог подключиться между созданием канала и этим вызовом —
        // это не ошибка, а обычная гонка.
        if ok == 0 && code != ERROR_PIPE_CONNECTED {
            return Err(Error::Win32 {
                call: "ConnectNamedPipe",
                code,
            });
        }

        self.client_pid()
    }

    /// PID подключившегося клиента.
    ///
    /// Отсюда начинается проверка «кто именно пришёл»: по PID берётся путь
    /// образа и сравнивается с нашим собственным.
    pub fn client_pid(&self) -> Result<u32> {
        let mut pid: u32 = 0;
        let ok = unsafe { GetNamedPipeClientProcessId(self.handle, &mut pid) };
        if ok == 0 {
            return Err(Error::Win32 {
                call: "GetNamedPipeClientProcessId",
                code: unsafe { GetLastError() },
            });
        }
        Ok(pid)
    }

    pub fn read(&self, buffer: &mut [u8]) -> Result<usize> {
        read_handle(self.handle, buffer)
    }

    pub fn write(&self, data: &[u8]) -> Result<usize> {
        write_handle(self.handle, data)
    }

    /// Отключает текущего клиента, оставляя канал готовым к следующему.
    pub fn disconnect(&self) -> Result<()> {
        let ok = unsafe { DisconnectNamedPipe(self.handle) };
        if ok == 0 {
            return Err(Error::Win32 {
                call: "DisconnectNamedPipe",
                code: unsafe { GetLastError() },
            });
        }
        Ok(())
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// Клиентский конец канала.
pub struct PipeClient {
    handle: HANDLE,
}

impl PipeClient {
    pub fn connect(name: &str) -> Result<PipeClient> {
        const GENERIC_READ: u32 = 0x8000_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;

        let wide: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();

        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                core::ptr::null(),
                OPEN_EXISTING,
                0,
                core::ptr::null_mut(),
            )
        };

        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(Error::Win32 {
                call: "CreateFileW(pipe)",
                code: unsafe { GetLastError() },
            });
        }

        Ok(PipeClient { handle })
    }

    pub fn read(&self, buffer: &mut [u8]) -> Result<usize> {
        read_handle(self.handle, buffer)
    }

    pub fn write(&self, data: &[u8]) -> Result<usize> {
        write_handle(self.handle, data)
    }
}

impl Drop for PipeClient {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

fn read_handle(handle: HANDLE, buffer: &mut [u8]) -> Result<usize> {
    let mut read: u32 = 0;
    let ok = unsafe {
        ReadFile(
            handle,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut read,
            core::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "ReadFile(pipe)",
            code: unsafe { GetLastError() },
        });
    }
    Ok(read as usize)
}

fn write_handle(handle: HANDLE, data: &[u8]) -> Result<usize> {
    let mut written: u32 = 0;
    let ok = unsafe {
        WriteFile(
            handle,
            data.as_ptr(),
            data.len() as u32,
            &mut written,
            core::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "WriteFile(pipe)",
            code: unsafe { GetLastError() },
        });
    }
    Ok(written as usize)
}

/// Проверяет, что клиент — процесс с тем же образом, что и мы.
///
/// Для неподписанных сборок это единственная доступная проверка
/// подлинности: сравнение полного пути образа. Для подписанных к ней
/// добавится проверка подписи.
pub fn client_is_same_image(client_pid: u32) -> Result<bool> {
    let ours = crate::process::full_image_path(std::process::id())?;
    let theirs = crate::process::full_image_path(client_pid)?;
    Ok(ours.eq_ignore_ascii_case(&theirs))
}

/// Сессия, которой принадлежит процесс.
///
/// Отдельная проверка подлинности рядом с совпадением образа (ТЗ, раздел
/// 3.2). Брокер под SYSTEM обслуживает конкретную интерактивную сессию;
/// сравнивать SID клиента с SID брокера бессмысленно — брокер это SYSTEM,
/// а агент работает под пользователем, и они заведомо разные. Осмысленно
/// другое: из какой сессии пришёл клиент. Пайп именован по сессии, но
/// полагаться на одно имя — значит доверять; `ProcessIdToSessionId`
/// проверяет это независимо, и работает одинаково под SYSTEM и в консоли.
pub fn process_session_id(pid: u32) -> Result<u32> {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;

    let mut session: u32 = 0;
    if unsafe { ProcessIdToSessionId(pid, &mut session) } == 0 {
        return Err(Error::Win32 {
            call: "ProcessIdToSessionId",
            code: unsafe { GetLastError() },
        });
    }
    Ok(session)
}

/// Проверяет, что клиент пришёл из ожидаемой сессии.
///
/// Ошибку определения сессии считаем непрохождением проверки: не смогли
/// подтвердить — значит не подтвердили, отказ безопаснее допуска.
pub fn client_in_session(client_pid: u32, expected_session: u32) -> bool {
    process_session_id(client_pid).is_ok_and(|session| session == expected_session)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_name() -> String {
        // Имя должно быть уникальным: канал живёт в общем пространстве имён,
        // и параллельные тесты не должны мешать друг другу.
        format!(
            "\\\\.\\pipe\\bamboo-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )
    }

    #[test]
    fn the_security_descriptor_parses() {
        // Если SDDL сломается, канал создастся с унаследованными правами —
        // то есть с дырой. Проверяем разбор отдельно.
        assert!(SecurityDescriptor::from_sddl(PIPE_SDDL).is_ok());
    }

    #[test]
    fn a_broken_descriptor_is_reported_not_ignored() {
        assert!(SecurityDescriptor::from_sddl("это не sddl").is_err());
    }

    #[test]
    fn a_pipe_can_be_created_without_administrator_rights() {
        // Брокер под SYSTEM создаст канал тем более, но агент должен уметь
        // подключаться, а тесты — проходить у обычного пользователя.
        let name = unique_name();
        assert!(PipeServer::create(&name).is_ok());
    }

    #[test]
    fn the_same_name_cannot_be_taken_twice() {
        // FILE_FLAG_FIRST_PIPE_INSTANCE: чужой процесс не должен успеть
        // создать канал с нашим именем и перехватывать запросы.
        let name = unique_name();
        let _first = PipeServer::create(&name).unwrap();
        assert!(PipeServer::create(&name).is_err());
    }

    #[test]
    fn an_ordinary_user_can_open_the_pipe() {
        // Проверка на грабли из комментария к PIPE_SDDL: явный запрет
        // для Everyone закрыл бы канал вообще для всех, и агент перестал
        // бы подключаться к брокеру.
        let name = unique_name();
        let _server = PipeServer::create(&name).unwrap();
        assert!(
            PipeClient::connect(&name).is_ok(),
            "обычный пользователь не смог открыть канал"
        );
    }

    #[test]
    fn a_message_survives_a_round_trip() {
        use std::sync::mpsc;
        use std::time::Duration;

        let name = unique_name();
        let server = PipeServer::create(&name).unwrap();

        let (connected_tx, connected_rx) = mpsc::channel();
        let client_name = name.clone();
        let client = std::thread::spawn(move || {
            let client = match PipeClient::connect(&client_name) {
                Ok(client) => {
                    connected_tx.send(Ok(())).ok();
                    client
                }
                Err(error) => {
                    connected_tx.send(Err(error.to_string())).ok();
                    return String::new();
                }
            };
            client.write(b"ping").unwrap();

            let mut buffer = [0u8; 64];
            let read = client.read(&mut buffer).unwrap();
            String::from_utf8_lossy(&buffer[..read]).to_string()
        });

        // Ждём подключения до вызова accept: иначе неудача клиента
        // оставила бы сервер висеть в ConnectNamedPipe навсегда,
        // и тест не упал бы, а завис.
        match connected_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => panic!("клиент не подключился: {error}"),
            Err(_) => panic!("клиент не подключился за 10 секунд"),
        }

        let client_pid = server.accept().unwrap();
        assert_eq!(client_pid, std::process::id(), "клиент — наш же процесс");

        let mut buffer = [0u8; 64];
        let read = server.read(&mut buffer).unwrap();
        assert_eq!(&buffer[..read], b"ping");
        server.write(b"pong").unwrap();

        assert_eq!(client.join().unwrap(), "pong");
    }

    #[test]
    fn the_client_image_check_recognises_our_own_process() {
        assert!(client_is_same_image(std::process::id()).unwrap());
    }

    #[test]
    fn our_own_session_is_readable_and_matches() {
        // Собственная сессия обязана определяться и совпадать сама с собой.
        let ours = process_session_id(std::process::id()).unwrap();
        assert!(client_in_session(std::process::id(), ours));
    }

    #[test]
    fn a_wrong_expected_session_fails_the_check() {
        let ours = process_session_id(std::process::id()).unwrap();
        // Сессии с таким номером у нас заведомо нет.
        assert!(!client_in_session(
            std::process::id(),
            ours.wrapping_add(999)
        ));
    }

    #[test]
    fn a_dead_process_has_no_session() {
        // PID, которого нет: определить сессию нельзя, проверка не проходит.
        assert!(!client_in_session(0xFFFF_FFF0, 1));
    }

    #[test]
    fn connecting_to_a_pipe_that_does_not_exist_fails() {
        assert!(PipeClient::connect("\\\\.\\pipe\\bamboo-такого-нет").is_err());
    }
}
