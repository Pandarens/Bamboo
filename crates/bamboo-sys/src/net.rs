//! Кто какой порт слушает.
//!
//! Нужно, чтобы отличить сервер разработки от прочего node.exe. Забытый
//! vite на живой машине держал 813 МБ вторые сутки, и рядом с ним жили
//! десять серверов инструментов Claude — те общаются через стандартный
//! ввод и порт не слушают. Слушающий порт отличает одно от другого точнее
//! любого разбора имён.

use std::collections::HashMap;

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

/// Порты, которые слушает каждый процесс, по возрастанию.
///
/// Пустой ответ — не ошибка: таблицу может не отдать брандмауэр
/// стороннего антивируса, и тогда о серверах просто молчим.
pub fn listening_ports() -> HashMap<u32, Vec<u16>> {
    let mut ports: HashMap<u32, Vec<u16>> = HashMap::new();
    for (family, row_size) in [
        (AF_INET, core::mem::size_of::<MIB_TCPROW_OWNER_PID>()),
        (AF_INET6, core::mem::size_of::<MIB_TCP6ROW_OWNER_PID>()),
    ] {
        let Some(table) = read_table(family as u32) else {
            continue;
        };
        // Таблица: число строк (u32), выравнивание, затем строки подряд.
        let count = u32::from_ne_bytes(table[..4].try_into().unwrap_or_default()) as usize;
        let offset = core::mem::align_of::<MIB_TCPROW_OWNER_PID>().max(4);
        for index in 0..count {
            let start = offset + index * row_size;
            let Some(row) = table.get(start..start + row_size) else {
                break;
            };
            let (port, pid) = if family == AF_INET {
                let row: MIB_TCPROW_OWNER_PID =
                    unsafe { core::ptr::read_unaligned(row.as_ptr().cast()) };
                (row.dwLocalPort, row.dwOwningPid)
            } else {
                let row: MIB_TCP6ROW_OWNER_PID =
                    unsafe { core::ptr::read_unaligned(row.as_ptr().cast()) };
                (row.dwLocalPort, row.dwOwningPid)
            };
            // Порт лежит в младших двух байтах в сетевом порядке.
            let port = u16::from_be((port & 0xFFFF) as u16);
            let list = ports.entry(pid).or_default();
            if !list.contains(&port) {
                list.push(port);
            }
        }
    }
    for list in ports.values_mut() {
        list.sort_unstable();
    }
    ports
}

/// Читает таблицу слушающих сокетов одного семейства адресов.
fn read_table(family: u32) -> Option<Vec<u8>> {
    let mut size: u32 = 0;
    // Между запросом размера и чтением могут открыться новые сокеты —
    // отсюда несколько попыток.
    for _ in 0..4 {
        let mut buffer = vec![0u8; size as usize];
        let status = unsafe {
            GetExtendedTcpTable(
                if buffer.is_empty() {
                    core::ptr::null_mut()
                } else {
                    buffer.as_mut_ptr().cast()
                },
                &mut size,
                0,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        match status {
            NO_ERROR if buffer.len() >= 4 => return Some(buffer),
            NO_ERROR | ERROR_INSUFFICIENT_BUFFER => continue,
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_listening_port_is_seen() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("порт для проверки");
        let port = listener.local_addr().unwrap().port();
        let ports = listening_ports();
        let ours = ports.get(&std::process::id()).cloned().unwrap_or_default();
        assert!(ours.contains(&port), "порт {port} не найден: {ours:?}");
    }

    #[test]
    fn ipv6_listeners_are_seen_too() {
        // vite по умолчанию слушает «localhost», а это часто ::1.
        let Ok(listener) = std::net::TcpListener::bind("[::1]:0") else {
            return; // IPv6 выключен — проверять нечего.
        };
        let port = listener.local_addr().unwrap().port();
        let ours = listening_ports()
            .get(&std::process::id())
            .cloned()
            .unwrap_or_default();
        assert!(ours.contains(&port), "порт {port} не найден: {ours:?}");
    }
}
