//! Активность накопителей и файлы подкачки (ТЗ, раздел 9.4).
//!
//! Отвечает на вопрос, который чаще всего задают про «тормозит компьютер»:
//! чем занят диск. Диспетчер задач показывает «активность 100%» и на этом
//! останавливается — а из-за чего она, не говорит.
//!
//! Считаем то же, что и он: `IOCTL_DISK_PERFORMANCE` отдаёт накопительные
//! счётчики байт и времени, а активность выводится из отношения занятого
//! времени к прошедшему. Поэтому одного замера мало — нужны два подряд.

use core::mem::size_of;

use bamboo_core::{Bytes, Error, Result};

use super::device::Drive;
use super::ioctl::{DISK_PERFORMANCE, IOCTL_DISK_PERFORMANCE};

/// Сырые счётчики накопителя на момент замера.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskCounters {
    /// Номер накопителя в системе.
    pub index: u32,
    pub bytes_read: u64,
    pub bytes_written: u64,
    /// Время чтения, единицы по 100 нс.
    pub read_time: i64,
    pub write_time: i64,
    pub idle_time: i64,
    /// Момент замера по часам ядра, единицы по 100 нс.
    pub query_time: i64,
    /// Сколько запросов ждёт очереди прямо сейчас.
    pub queue_depth: u32,
    /// Сколько операций чтения и записи накопитель выполнил всего.
    /// Нужны, чтобы поделить на них время и получить задержку одной
    /// операции — то, что человек и чувствует.
    pub read_count: u32,
    pub write_count: u32,
}

/// Что происходило с накопителем между двумя замерами.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DiskActivity {
    pub index: u32,
    /// Доля времени, когда накопитель был занят, 0..1.
    pub busy: f64,
    /// Скорость чтения за интервал.
    pub read_per_second: Bytes,
    pub write_per_second: Bytes,
    pub queue_depth: u32,
    /// Сколько в среднем занимает одна операция, миллисекунды.
    ///
    /// Честный признак «диск не успевает», в отличие от длины очереди.
    /// Очередь сама по себе ничего не значит: у NVMe их десятки штук
    /// по тысяче команд, и глубокая очередь там — признак хорошей
    /// пропускной способности, а не беды. А вот когда одна операция
    /// занимает сто миллисекунд, ждёт всё, что к диску обратилось,
    /// и человек это видит.
    ///
    /// Ноль означает «за интервал не было ни одной операции», а не
    /// «мгновенно»: делить на ноль и выдавать результат за измерение
    /// нельзя.
    pub latency_ms: f64,
}

impl DiskActivity {
    /// Занят ли накопитель настолько, что это уже мешает.
    ///
    /// Порог не в ста процентах: очередь начинает расти задолго до полной
    /// занятости, и человек чувствует задержки раньше, чем счётчик упрётся
    /// в потолок.
    pub fn is_saturated(&self) -> bool {
        self.busy >= 0.85
    }
}

/// Читает счётчики накопителя.
///
/// Требует открытого устройства. Права администратора не нужны: счётчики
/// доступны на чтение любому.
pub fn read_counters(drive: &Drive) -> Result<DiskCounters> {
    let mut buffer = vec![0u8; size_of::<DISK_PERFORMANCE>()];
    let returned = drive.control(IOCTL_DISK_PERFORMANCE, &[], &mut buffer)?;

    if (returned as usize) < size_of::<DISK_PERFORMANCE>() {
        return Err(Error::Malformed("счётчики накопителя короче ожидаемых"));
    }

    // SAFETY: буфер заполнен драйвером как DISK_PERFORMANCE, длина проверена.
    let raw = unsafe { core::ptr::read_unaligned(buffer.as_ptr() as *const DISK_PERFORMANCE) };

    Ok(DiskCounters {
        index: raw.StorageDeviceNumber,
        bytes_read: raw.BytesRead.max(0) as u64,
        bytes_written: raw.BytesWritten.max(0) as u64,
        read_time: raw.ReadTime,
        write_time: raw.WriteTime,
        idle_time: raw.IdleTime,
        query_time: raw.QueryTime,
        queue_depth: raw.QueueDepth,
        read_count: raw.ReadCount,
        write_count: raw.WriteCount,
    })
}

/// Считает активность между двумя замерами.
///
/// Возвращает `None`, если замеры не идут подряд или время не сдвинулось:
/// делить на ноль и выдавать это за измерение нельзя.
pub fn activity_between(before: DiskCounters, after: DiskCounters) -> Option<DiskActivity> {
    if before.index != after.index {
        return None;
    }

    let elapsed = after.query_time.saturating_sub(before.query_time);
    if elapsed <= 0 {
        return None;
    }

    let busy_time = (after.read_time.saturating_sub(before.read_time))
        .saturating_add(after.write_time.saturating_sub(before.write_time));

    // Занятость может слегка превысить сто процентов: чтение и запись идут
    // параллельно, и времена складываются. Для показа это бессмысленно,
    // поэтому подрезаем.
    let busy = (busy_time as f64 / elapsed as f64).clamp(0.0, 1.0);

    let seconds = elapsed as f64 / 10_000_000.0;
    let read = after.bytes_read.saturating_sub(before.bytes_read);
    let written = after.bytes_written.saturating_sub(before.bytes_written);

    // Задержка одной операции: всё время обслуживания, поделённое на число
    // обслуженных запросов. Счётчики 32-разрядные и переполняются, поэтому
    // разность берём с переносом — иначе на переполнении вышло бы огромное
    // отрицательное число операций и бессмысленная задержка.
    let operations = after
        .read_count
        .wrapping_sub(before.read_count)
        .saturating_add(after.write_count.wrapping_sub(before.write_count));
    let latency_ms = if operations == 0 {
        0.0
    } else {
        // Времена в единицах по 100 нс: делим на 10 000, чтобы получить
        // миллисекунды.
        busy_time as f64 / operations as f64 / 10_000.0
    };

    Some(DiskActivity {
        index: after.index,
        busy,
        read_per_second: Bytes((read as f64 / seconds) as u64),
        write_per_second: Bytes((written as f64 / seconds) as u64),
        queue_depth: after.queue_depth,
        latency_ms,
    })
}

/// Файл подкачки и его занятость.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pagefile {
    /// Путь вида `\??\C:\pagefile.sys`.
    pub name: String,
    pub total: Bytes,
    pub in_use: Bytes,
    /// Наибольшая занятость с момента загрузки.
    pub peak: Bytes,
}

impl Pagefile {
    /// Буква диска, на котором лежит файл. `None`, если путь непривычного
    /// вида — выдумывать букву не станем.
    pub fn drive_letter(&self) -> Option<char> {
        let cleaned = self.name.trim_start_matches("\\??\\");
        let mut chars = cleaned.chars();
        let letter = chars.next()?;
        (letter.is_ascii_alphabetic() && chars.next() == Some(':'))
            .then(|| letter.to_ascii_uppercase())
    }

    /// Какая доля файла занята, 0..1.
    pub fn usage(&self) -> f64 {
        if self.total.as_u64() == 0 {
            return 0.0;
        }
        self.in_use.as_u64() as f64 / self.total.as_u64() as f64
    }
}

/// Перечисляет файлы подкачки.
///
/// Пустой список — не ошибка: подкачку можно отключить, и на машинах
/// с большой памятью это встречается.
pub fn pagefiles() -> Result<Vec<Pagefile>> {
    use crate::nt::{
        nt_success, NtQuerySystemInformation, CLASS_PAGEFILE_INFORMATION,
        STATUS_INFO_LENGTH_MISMATCH, SYSTEM_PAGEFILE_INFORMATION,
    };

    let page_bytes = crate::memory::page_size() as u64;
    let mut buffer = vec![0u8; 4096];

    loop {
        let mut needed: u32 = 0;
        // SAFETY: буфер живёт дольше вызова, размер передаём его собственный.
        let status = unsafe {
            NtQuerySystemInformation(
                CLASS_PAGEFILE_INFORMATION,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut needed,
            )
        };

        if status == STATUS_INFO_LENGTH_MISMATCH {
            // Растём с запасом: между вызовами список мог пополниться.
            buffer.resize((needed as usize).max(buffer.len() * 2) + 1024, 0);
            continue;
        }
        if !nt_success(status) {
            return Err(Error::Unsupported(
                "список файлов подкачки прочитать не удалось",
            ));
        }

        let mut files = Vec::new();
        let mut offset = 0usize;
        loop {
            if offset + size_of::<SYSTEM_PAGEFILE_INFORMATION>() > buffer.len() {
                break;
            }
            // SAFETY: запись целиком лежит в буфере, что проверено выше.
            let entry = unsafe {
                core::ptr::read_unaligned(
                    buffer.as_ptr().add(offset) as *const SYSTEM_PAGEFILE_INFORMATION
                )
            };

            let name = read_unicode_string(&entry.PageFileName);
            files.push(Pagefile {
                name,
                total: Bytes(entry.TotalSize as u64 * page_bytes),
                in_use: Bytes(entry.TotalInUse as u64 * page_bytes),
                peak: Bytes(entry.PeakUsage as u64 * page_bytes),
            });

            if entry.NextEntryOffset == 0 {
                break;
            }
            offset += entry.NextEntryOffset as usize;
        }
        return Ok(files);
    }
}

/// Читает строку ядра. Буфер указывает внутрь того же ответа.
fn read_unicode_string(text: &crate::nt::UNICODE_STRING) -> String {
    if text.Buffer.is_null() || text.Length == 0 {
        return String::new();
    }
    // SAFETY: ядро вернуло указатель на строку длиной Length байт внутри
    // нашего же буфера; читаем ровно столько символов.
    let slice = unsafe {
        core::slice::from_raw_parts(text.Buffer, text.Length as usize / size_of::<u16>())
    };
    String::from_utf16_lossy(slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counters(index: u32, at: i64, read: u64, written: u64, busy: i64) -> DiskCounters {
        DiskCounters {
            index,
            bytes_read: read,
            bytes_written: written,
            read_time: busy,
            write_time: 0,
            idle_time: 0,
            query_time: at,
            queue_depth: 0,
            read_count: 0,
            write_count: 0,
        }
    }

    const SECOND: i64 = 10_000_000;

    #[test]
    fn a_busy_second_reads_as_full_activity() {
        // Секунда прошла, и всю её накопитель был занят чтением.
        let before = counters(0, 0, 0, 0, 0);
        let after = counters(0, SECOND, 0, 0, SECOND);

        let activity = activity_between(before, after).unwrap();
        assert!((activity.busy - 1.0).abs() < 1e-9);
        assert!(activity.is_saturated());
    }

    #[test]
    fn an_idle_disk_is_not_saturated() {
        let before = counters(0, 0, 0, 0, 0);
        let after = counters(0, SECOND, 0, 0, 0);

        let activity = activity_between(before, after).unwrap();
        assert_eq!(activity.busy, 0.0);
        assert!(!activity.is_saturated());
    }

    #[test]
    fn speed_is_bytes_over_elapsed_time() {
        // 100 МБ прочитано за две секунды — значит 50 МБ/с.
        let before = counters(0, 0, 0, 0, 0);
        let after = counters(0, 2 * SECOND, 100 * 1024 * 1024, 0, 0);

        let activity = activity_between(before, after).unwrap();
        assert_eq!(activity.read_per_second.as_u64(), 50 * 1024 * 1024);
    }

    #[test]
    fn parallel_read_and_write_do_not_exceed_full_activity() {
        // Чтение и запись идут одновременно, и времена складываются.
        // Показывать «180% занятости» нельзя.
        let before = counters(0, 0, 0, 0, 0);
        let mut after = counters(0, SECOND, 0, 0, SECOND);
        after.write_time = SECOND * 4 / 5;

        assert_eq!(activity_between(before, after).unwrap().busy, 1.0);
    }

    #[test]
    fn counters_of_different_disks_are_not_compared() {
        let before = counters(0, 0, 0, 0, 0);
        let after = counters(1, SECOND, 0, 0, 0);
        assert_eq!(activity_between(before, after), None);
    }

    #[test]
    fn a_frozen_clock_yields_no_measurement() {
        // Два замера в один момент: делить не на что.
        let before = counters(0, SECOND, 0, 0, 0);
        let after = counters(0, SECOND, 0, 0, 0);
        assert_eq!(activity_between(before, after), None);
    }

    #[test]
    fn a_pagefile_path_yields_its_drive_letter() {
        let file = Pagefile {
            name: "\\??\\C:\\pagefile.sys".to_string(),
            total: Bytes::from_mib(4096),
            in_use: Bytes::from_mib(1024),
            peak: Bytes::from_mib(2048),
        };
        assert_eq!(file.drive_letter(), Some('C'));
        assert!((file.usage() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn an_unusual_path_gets_no_invented_letter() {
        let file = Pagefile {
            name: "\\Device\\Harddisk0\\swap".to_string(),
            total: Bytes::from_mib(1024),
            in_use: Bytes::ZERO,
            peak: Bytes::ZERO,
        };
        assert_eq!(file.drive_letter(), None);
        assert_eq!(file.usage(), 0.0);
    }

    #[test]
    fn pagefiles_are_listed_on_a_live_system() {
        // Список может быть пуст (подкачку отключают), но запрос обязан
        // отработать без ошибки и без прав администратора.
        let files = pagefiles().expect("список файлов подкачки не прочитался");
        for file in &files {
            assert!(file.total.as_u64() > 0, "файл подкачки нулевого размера");
            assert!(file.in_use.as_u64() <= file.total.as_u64());
        }
    }
}
