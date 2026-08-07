//! Снимок всех процессов системы.
//!
//! Единственный источник для цикла опроса — `NtQuerySystemInformation`
//! с классом `SystemProcessInformation`. Один системный вызов отдаёт всё:
//! 1–3 мс на 300 процессов при буфере 300–500 КБ.
//!
//! Чего здесь сознательно нет: цикла `OpenProcess` + `GetProcessMemoryInfo`
//! по каждому PID. Это сотни системных вызовов за тик и открытие дескрипторов
//! на чужие процессы — ровно то поведение, на которое реагируют эвристики EDR.

use core::mem::size_of;

use bamboo_core::{Bytes, Error, Nanos, ProcessId, ProcessSample, Result};

use crate::nt::{
    nt_success, NtQuerySystemInformation, CLASS_PROCESS_INFORMATION, STATUS_INFO_LENGTH_MISMATCH,
    SYSTEM_PROCESS_INFORMATION as SysProcessInfo,
};

/// Начальный размер буфера. Хватает примерно на 300 процессов, то есть
/// на типичную машину — реаллокаций в установившемся режиме не будет.
const INITIAL_CAPACITY_BYTES: usize = 512 * 1024;

/// Переиспользуемый буфер под снимок процессов.
///
/// Буфер живёт между тиками и растёт только по `STATUS_INFO_LENGTH_MISMATCH`.
/// Выделять по мегабайту каждые пять секунд резидентная утилита не имеет права.
pub struct ProcessBuffer {
    /// Хранилище из `u64`, а не из `u8`, ради гарантированного выравнивания
    /// по 8 байт: структуры ядра содержат указатели и `i64`.
    storage: Vec<u64>,
    /// Сколько байт заполнено последним успешным запросом.
    filled: usize,
}

impl Default for ProcessBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessBuffer {
    pub fn new() -> Self {
        ProcessBuffer {
            storage: vec![0u64; INITIAL_CAPACITY_BYTES / 8],
            filled: 0,
        }
    }

    /// Текущий размер буфера в байтах — для контроля собственного бюджета.
    pub fn capacity_bytes(&self) -> usize {
        self.storage.len() * 8
    }

    /// Снимает состояние всех процессов.
    pub fn refresh(&mut self) -> Result<()> {
        // Между запросом размера и запросом данных система успевает запустить
        // новые процессы, поэтому цикл, а не два вызова подряд.
        for _ in 0..8 {
            let capacity = self.capacity_bytes();
            let mut returned: u32 = 0;
            let status = unsafe {
                NtQuerySystemInformation(
                    CLASS_PROCESS_INFORMATION,
                    self.storage.as_mut_ptr().cast(),
                    capacity as u32,
                    &mut returned,
                )
            };

            if nt_success(status) {
                self.filled = (returned as usize).min(capacity);
                return Ok(());
            }

            if status != STATUS_INFO_LENGTH_MISMATCH {
                self.filled = 0;
                return Err(Error::Nt {
                    call: "NtQuerySystemInformation(SystemProcessInformation)",
                    status,
                });
            }

            // Просим с запасом: к следующей попытке процессов станет больше.
            let needed = (returned as usize).max(capacity) + 64 * 1024;
            self.storage.resize(needed.div_ceil(8), 0);
        }

        self.filled = 0;
        Err(Error::Nt {
            call: "NtQuerySystemInformation(SystemProcessInformation)",
            status: STATUS_INFO_LENGTH_MISMATCH,
        })
    }

    /// Перебор процессов из последнего успешного снимка.
    pub fn iter(&self) -> ProcessIter<'_> {
        let bytes = unsafe {
            core::slice::from_raw_parts(self.storage.as_ptr().cast::<u8>(), self.filled)
        };
        ProcessIter {
            bytes,
            offset: 0,
            finished: self.filled < size_of::<SysProcessInfo>(),
        }
    }
}

pub struct ProcessIter<'a> {
    bytes: &'a [u8],
    offset: usize,
    finished: bool,
}

impl<'a> Iterator for ProcessIter<'a> {
    type Item = RawProcess<'a>;

    fn next(&mut self) -> Option<RawProcess<'a>> {
        if self.finished {
            return None;
        }

        let start = self.offset;
        if start + size_of::<SysProcessInfo>() > self.bytes.len() {
            self.finished = true;
            return None;
        }

        // SAFETY: границы проверены выше, буфер выровнен по 8 байт,
        // раскладка структуры зафиксирована проверками в `nt.rs`.
        let info = unsafe {
            core::ptr::read_unaligned(self.bytes.as_ptr().add(start).cast::<SysProcessInfo>())
        };

        // Имя образа лежит вне записи, в хвосте того же буфера.
        // Указатель мог оказаться нулевым — так система отдаёт процесс Idle.
        let name = unsafe { image_name_slice(&info) };

        if info.NextEntryOffset == 0 {
            self.finished = true;
        } else {
            self.offset = start + info.NextEntryOffset as usize;
            if self.offset <= start || self.offset >= self.bytes.len() {
                self.finished = true;
            }
        }

        Some(RawProcess { info, name })
    }
}

/// Читает имя образа как срез UTF-16 без копирования.
///
/// # Safety
/// `info` должен быть получен из живого буфера снимка: `Buffer` указывает
/// внутрь него, и срез не должен пережить буфер.
unsafe fn image_name_slice<'a>(info: &SysProcessInfo) -> &'a [u16] {
    if info.ImageName.Buffer.is_null() || info.ImageName.Length == 0 {
        return &[];
    }
    core::slice::from_raw_parts(info.ImageName.Buffer, info.ImageName.Length as usize / 2)
}

/// Одна запись снимка. Имя образа заимствовано из буфера — преобразование
/// в `String` делается только для впервые увиденных процессов, а не каждый тик.
pub struct RawProcess<'a> {
    info: SysProcessInfo,
    name: &'a [u16],
}

impl<'a> RawProcess<'a> {
    pub fn pid(&self) -> u32 {
        self.info.UniqueProcessId as u32
    }

    pub fn parent_pid(&self) -> u32 {
        self.info.InheritedFromUniqueProcessId as u32
    }

    pub fn create_time(&self) -> i64 {
        self.info.CreateTime
    }

    pub fn id(&self) -> ProcessId {
        ProcessId::new(self.pid(), self.info.CreateTime)
    }

    pub fn thread_count(&self) -> u32 {
        self.info.NumberOfThreads
    }

    /// Имя образа как есть, в UTF-16.
    pub fn image_name_utf16(&self) -> &'a [u16] {
        self.name
    }

    /// Имя образа строкой. Выделяет память, поэтому в цикле опроса не вызывается.
    pub fn image_name(&self) -> String {
        if self.name.is_empty() {
            // Единственный процесс без имени — Idle с нулевым PID.
            return if self.pid() == 0 {
                "Idle".to_string()
            } else {
                String::new()
            };
        }
        String::from_utf16_lossy(self.name)
    }

    pub fn to_sample(&self) -> ProcessSample {
        let i = &self.info;
        ProcessSample {
            id: self.id(),
            parent_pid: self.parent_pid(),
            session_id: i.SessionId,
            base_priority: i.BasePriority,
            threads: i.NumberOfThreads,
            handles: i.HandleCount,
            cpu_user: Nanos::from_100ns(i.UserTime),
            cpu_kernel: Nanos::from_100ns(i.KernelTime),
            working_set_private: Bytes(i.WorkingSetPrivateSize.max(0) as u64),
            private_pages: Bytes(i.PrivatePageCount as u64),
            virtual_size: Bytes(i.VirtualSize as u64),
            read_bytes: i.ReadTransferCount.max(0) as u64,
            write_bytes: i.WriteTransferCount.max(0) as u64,
            other_bytes: i.OtherTransferCount.max(0) as u64,
            read_ops: i.ReadOperationCount.max(0) as u64,
            write_ops: i.WriteOperationCount.max(0) as u64,
            other_ops: i.OtherOperationCount.max(0) as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_sees_the_current_process() {
        let mut buffer = ProcessBuffer::new();
        buffer.refresh().expect("снимок процессов не удался");

        let me = std::process::id();
        let found = buffer.iter().find(|p| p.pid() == me);
        let found = found.expect("собственный процесс отсутствует в снимке");

        assert!(found.thread_count() > 0);
        assert!(found.image_name().to_lowercase().contains("bamboo"));
        assert!(found.to_sample().private_pages > bamboo_core::Bytes::ZERO);
    }

    #[test]
    fn snapshot_returns_a_plausible_process_list() {
        let mut buffer = ProcessBuffer::new();
        buffer.refresh().unwrap();

        let count = buffer.iter().count();
        assert!(count > 10, "процессов подозрительно мало: {count}");

        // Idle всегда идёт первым и всегда имеет нулевой PID.
        let first = buffer.iter().next().unwrap();
        assert_eq!(first.pid(), 0);

        // System — PID 4 на всех поддерживаемых версиях Windows.
        assert!(buffer.iter().any(|p| p.pid() == 4));
    }

    #[test]
    fn buffer_is_reused_between_snapshots() {
        let mut buffer = ProcessBuffer::new();
        buffer.refresh().unwrap();
        let capacity = buffer.capacity_bytes();
        for _ in 0..5 {
            buffer.refresh().unwrap();
        }
        assert_eq!(
            buffer.capacity_bytes(),
            capacity,
            "буфер не должен расти на установившемся числе процессов"
        );
    }
}
