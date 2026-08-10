//! Сырые объявления NT API.
//!
//! `NtQuerySystemInformation` официально не документирован и в `windows-sys`
//! приезжает с неполными структурами, поэтому объявляем всё сами. Заодно это
//! даёт контроль над раскладкой полей и позволяет проверить её тестами.
//!
//! Единственное место в проекте, где описываются бинарные структуры ядра.

#![allow(non_snake_case, non_camel_case_types)]

use core::ffi::c_void;

pub type NTSTATUS = i32;

pub const STATUS_INFO_LENGTH_MISMATCH: NTSTATUS = 0xC000_0004_u32 as NTSTATUS;

/// Класс `SystemProcessInformation`: связный список процессов с потоками.
pub const CLASS_PROCESS_INFORMATION: u32 = 5;
/// Класс `SystemProcessorPerformanceInformation`: времена на каждое ядро.
pub const CLASS_PROCESSOR_PERFORMANCE: u32 = 8;
/// Класс `SystemInterruptInformation`: детализация прерываний.
pub const CLASS_INTERRUPT_INFORMATION: u32 = 23;
/// Класс `SystemPagefileInformation`: файлы подкачки и их занятость.
pub const CLASS_PAGEFILE_INFORMATION: u32 = 18;

pub const fn nt_success(status: NTSTATUS) -> bool {
    status >= 0
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UNICODE_STRING {
    pub Length: u16,
    pub MaximumLength: u16,
    pub Buffer: *mut u16,
}

/// Запись о файле подкачки. Записи идут цепочкой: `NextEntryOffset` — сдвиг
/// до следующей в байтах, ноль означает конец. Размеры даны в страницах.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SYSTEM_PAGEFILE_INFORMATION {
    pub NextEntryOffset: u32,
    pub TotalSize: u32,
    pub TotalInUse: u32,
    pub PeakUsage: u32,
    pub PageFileName: UNICODE_STRING,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CLIENT_ID {
    pub UniqueProcess: isize,
    pub UniqueThread: isize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SYSTEM_THREAD_INFORMATION {
    pub KernelTime: i64,
    pub UserTime: i64,
    pub CreateTime: i64,
    pub WaitTime: u32,
    pub StartAddress: *mut c_void,
    pub ClientId: CLIENT_ID,
    pub Priority: i32,
    pub BasePriority: i32,
    pub ContextSwitches: u32,
    pub ThreadState: u32,
    pub WaitReason: u32,
}

/// Запись о процессе. За ней в буфере сразу идёт массив
/// `SYSTEM_THREAD_INFORMATION` длиной `NumberOfThreads`, а следующая запись
/// находится по смещению `NextEntryOffset` от начала текущей.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SYSTEM_PROCESS_INFORMATION {
    pub NextEntryOffset: u32,
    pub NumberOfThreads: u32,
    pub WorkingSetPrivateSize: i64,
    pub HardFaultCount: u32,
    pub NumberOfThreadsHighWatermark: u32,
    pub CycleTime: u64,
    pub CreateTime: i64,
    pub UserTime: i64,
    pub KernelTime: i64,
    pub ImageName: UNICODE_STRING,
    pub BasePriority: i32,
    pub UniqueProcessId: isize,
    pub InheritedFromUniqueProcessId: isize,
    pub HandleCount: u32,
    pub SessionId: u32,
    pub UniqueProcessKey: usize,
    pub PeakVirtualSize: usize,
    pub VirtualSize: usize,
    pub PageFaultCount: u32,
    pub PeakWorkingSetSize: usize,
    pub WorkingSetSize: usize,
    pub QuotaPeakPagedPoolUsage: usize,
    pub QuotaPagedPoolUsage: usize,
    pub QuotaPeakNonPagedPoolUsage: usize,
    pub QuotaNonPagedPoolUsage: usize,
    pub PagefileUsage: usize,
    pub PeakPagefileUsage: usize,
    pub PrivatePageCount: usize,
    pub ReadOperationCount: i64,
    pub WriteOperationCount: i64,
    pub OtherOperationCount: i64,
    pub ReadTransferCount: i64,
    pub WriteTransferCount: i64,
    pub OtherTransferCount: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SYSTEM_PROCESSOR_PERFORMANCE_INFO {
    /// Внимание: `KernelTime` включает в себя `IdleTime`.
    pub IdleTime: i64,
    pub KernelTime: i64,
    pub UserTime: i64,
    pub DpcTime: i64,
    pub InterruptTime: i64,
    pub InterruptCount: u32,
}

// Раскладка структур ядра — не то место, где можно ошибиться молча.
// Смещения взяты из ntdll x64 и зафиксированы проверками на этапе компиляции.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(core::mem::size_of::<SYSTEM_PROCESS_INFORMATION>() == 256);
    assert!(core::mem::size_of::<SYSTEM_THREAD_INFORMATION>() == 80);
    assert!(core::mem::size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFO>() == 48);
    assert!(core::mem::offset_of!(SYSTEM_PROCESS_INFORMATION, ImageName) == 56);
    assert!(core::mem::offset_of!(SYSTEM_PROCESS_INFORMATION, UniqueProcessId) == 80);
    assert!(core::mem::offset_of!(SYSTEM_PROCESS_INFORMATION, PrivatePageCount) == 200);
    assert!(core::mem::offset_of!(SYSTEM_PROCESS_INFORMATION, ReadTransferCount) == 232);
};

#[link(name = "ntdll")]
extern "system" {
    /// Запрос системной информации. Буфер и его длина задаются вызывающим;
    /// при нехватке места возвращается `STATUS_INFO_LENGTH_MISMATCH`
    /// и требуемый размер в `ReturnLength`.
    pub fn NtQuerySystemInformation(
        SystemInformationClass: u32,
        SystemInformation: *mut c_void,
        SystemInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> NTSTATUS;
}
