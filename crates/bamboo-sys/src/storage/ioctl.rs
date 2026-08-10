//! Коды IOCTL и структуры драйверов хранения.
//!
//! Объявляем сами по той же причине, что и NT API: в `windows-sys` часть
//! структур отсутствует, а раскладка здесь критична — мы разбираем сырой
//! ответ контроллера.

#![allow(non_snake_case, non_camel_case_types)]

/// Сборка кода IOCTL по правилам Windows: `CTL_CODE` из `winioctl.h`.
/// Приводим формулу целиком, чтобы константы ниже читались, а не сверялись
/// с документацией.
pub const fn ctl_code(device: u32, function: u32, method: u32, access: u32) -> u32 {
    (device << 16) | (access << 14) | (function << 2) | method
}

const FILE_DEVICE_MASS_STORAGE: u32 = 0x0000_002D;
const FILE_DEVICE_CONTROLLER: u32 = 0x0000_0004;
const FILE_DEVICE_DISK: u32 = 0x0000_0007;

const METHOD_BUFFERED: u32 = 0;
const FILE_ANY_ACCESS: u32 = 0;
const FILE_READ_ACCESS: u32 = 1;
const FILE_WRITE_ACCESS: u32 = 2;

/// Запрос свойств устройства. Работает без прав администратора.
pub const IOCTL_STORAGE_QUERY_PROPERTY: u32 = ctl_code(
    FILE_DEVICE_MASS_STORAGE,
    0x0500,
    METHOD_BUFFERED,
    FILE_ANY_ACCESS,
);

/// Геометрия диска, отсюда берём ёмкость. Тоже без прав администратора.
pub const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 =
    ctl_code(FILE_DEVICE_DISK, 0x0028, METHOD_BUFFERED, FILE_ANY_ACCESS);

/// Прямая передача ATA-команды. Требует открытия устройства на чтение
/// и запись, то есть прав администратора.
/// `IOCTL_DISK_PERFORMANCE`: счётчики активности накопителя.
///
/// То же самое, чем меряет диспетчер задач: сколько байт прочитано
/// и записано, сколько времени накопитель был занят и сколько простаивал.
/// Из отношения занятости к общему времени и получается «активность 100%».
pub const IOCTL_DISK_PERFORMANCE: u32 = ctl_code(0x0000_0007, 0x0008, 0, 0);

/// Счётчики производительности накопителя.
///
/// Времена в единицах по 100 наносекунд, как принято в ядре Windows.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DISK_PERFORMANCE {
    pub BytesRead: i64,
    pub BytesWritten: i64,
    pub ReadTime: i64,
    pub WriteTime: i64,
    pub IdleTime: i64,
    pub ReadCount: u32,
    pub WriteCount: u32,
    pub QueueDepth: u32,
    pub SplitCount: u32,
    pub QueryTime: i64,
    pub StorageDeviceNumber: u32,
    pub StorageManagerName: [u16; 8],
}

pub const IOCTL_ATA_PASS_THROUGH: u32 = ctl_code(
    FILE_DEVICE_CONTROLLER,
    0x040B,
    METHOD_BUFFERED,
    FILE_READ_ACCESS | FILE_WRITE_ACCESS,
);

pub const STORAGE_DEVICE_PROPERTY: u32 = 0;
pub const STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY: u32 = 50;
pub const PROPERTY_STANDARD_QUERY: u32 = 0;

pub const PROTOCOL_TYPE_NVME: u32 = 3;
pub const NVME_DATA_TYPE_LOG_PAGE: u32 = 2;
/// Номер страницы SMART / Health Information в журнале NVMe.
pub const NVME_LOG_PAGE_HEALTH_INFO: u32 = 0x02;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct STORAGE_PROTOCOL_SPECIFIC_DATA {
    pub ProtocolType: u32,
    pub DataType: u32,
    pub ProtocolDataRequestValue: u32,
    pub ProtocolDataRequestSubValue: u32,
    pub ProtocolDataOffset: u32,
    pub ProtocolDataLength: u32,
    pub FixedProtocolReturnData: u32,
    pub ProtocolDataRequestSubValue2: u32,
    pub ProtocolDataRequestSubValue3: u32,
    pub ProtocolDataRequestSubValue4: u32,
}

/// Запрос свойства. В настоящем `STORAGE_PROPERTY_QUERY` за двумя полями
/// идёт массив переменной длины; здесь он раскрыт в конкретные параметры
/// протокола, которые мы и передаём.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct STORAGE_PROPERTY_QUERY_WITH_PROTOCOL {
    pub PropertyId: u32,
    pub QueryType: u32,
    pub ProtocolSpecific: STORAGE_PROTOCOL_SPECIFIC_DATA,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct STORAGE_PROTOCOL_DATA_DESCRIPTOR {
    pub Version: u32,
    pub Size: u32,
    pub ProtocolSpecificData: STORAGE_PROTOCOL_SPECIFIC_DATA,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct STORAGE_DEVICE_DESCRIPTOR_HEADER {
    pub Version: u32,
    pub Size: u32,
    pub DeviceType: u8,
    pub DeviceTypeModifier: u8,
    pub RemovableMedia: u8,
    pub CommandQueueing: u8,
    /// Смещения строк от начала дескриптора. Ноль означает «поля нет».
    pub VendorIdOffset: u32,
    pub ProductIdOffset: u32,
    pub ProductRevisionOffset: u32,
    pub SerialNumberOffset: u32,
    pub BusType: u32,
    pub RawPropertiesLength: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DISK_GEOMETRY_EX_HEADER {
    pub Cylinders: i64,
    pub MediaType: u32,
    pub TracksPerCylinder: u32,
    pub SectorsPerTrack: u32,
    pub BytesPerSector: u32,
    pub DiskSize: i64,
}

/// Запрос ATA-команды. За структурой в том же буфере лежит область данных,
/// на которую указывает `DataBufferOffset`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ATA_PASS_THROUGH_EX {
    pub Length: u16,
    pub AtaFlags: u16,
    pub PathId: u8,
    pub TargetId: u8,
    pub Lun: u8,
    pub ReservedAsUchar: u8,
    pub DataTransferLength: u32,
    pub TimeOutValue: usize,
    pub ReservedAsUlong: usize,
    pub DataBufferOffset: usize,
    pub PreviousTaskFile: [u8; 8],
    pub CurrentTaskFile: [u8; 8],
}

pub const ATA_FLAGS_DRDY_REQUIRED: u16 = 0x01;
pub const ATA_FLAGS_DATA_IN: u16 = 0x02;

/// Старый путь чтения SMART: `SMART_RCV_DRIVE_DATA`.
///
/// Драйвер хранилища транслирует его в ATA-команду сам, поэтому его
/// принимают контроллеры, отвергающие прямой `IOCTL_ATA_PASS_THROUGH`
/// с ошибкой 1306. Обнаружено на живом Apacer AS350.
pub const SMART_RCV_DRIVE_DATA: u32 = 0x0007_C088;

/// Регистры устройства IDE. Ровно 8 байт.
#[allow(clippy::upper_case_acronyms)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IDEREGS {
    pub features: u8,
    pub sector_count: u8,
    pub sector_number: u8,
    pub cyl_low: u8,
    pub cyl_high: u8,
    pub drive_head: u8,
    pub command: u8,
    pub reserved: u8,
}

/// Вход `SMART_RCV_DRIVE_DATA`. За заголовком идёт область данных.
#[allow(clippy::upper_case_acronyms)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SENDCMDINPARAMS {
    pub buffer_size: u32,
    pub registers: IDEREGS,
    pub drive_number: u8,
    pub reserved: [u8; 3],
    pub reserved_dwords: [u32; 4],
    // bBuffer[1] — гибкий хвост, кладём отдельно в общий буфер.
}

/// Заголовок выхода `SMART_RCV_DRIVE_DATA`. За ним — 512 байт SMART-данных.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SENDCMDOUTPARAMS_HEADER {
    pub buffer_size: u32,
    pub driver_error: u8,
    pub ide_error: u8,
    pub reserved: [u8; 2],
    pub reserved_dwords: [u32; 2],
    // bBuffer[1] — данные начинаются здесь.
}

/// Подкоманда SMART READ DATA для регистра Features.
pub const SMART_READ_DATA: u8 = 0xD0;
/// Значение регистра Device/Head для доступа к SMART.
pub const SMART_DRIVE_HEAD: u8 = 0xA0;

/// Начало данных в выходном буфере: после заголовка SENDCMDOUTPARAMS.
pub const SENDCMD_OUT_HEADER_BYTES: usize = 16;
/// Начало данных во входном буфере: после заголовка SENDCMDINPARAMS.
pub const SENDCMD_IN_HEADER_BYTES: usize = 32;

/// Команда SMART и её подкоманда чтения данных.
pub const ATA_COMMAND_SMART: u8 = 0xB0;
pub const ATA_SMART_READ_DATA: u8 = 0xD0;
/// Магические значения в регистрах цилиндра, без них команда SMART
/// не распознаётся.
pub const ATA_SMART_LBA_MID: u8 = 0x4F;
pub const ATA_SMART_LBA_HIGH: u8 = 0xC2;

// Раскладка структур — часть двоичного контракта с драйверами.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(core::mem::size_of::<ATA_PASS_THROUGH_EX>() == 56);
    assert!(core::mem::size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>() == 40);
    assert!(core::mem::size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>() == 48);
    assert!(core::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR_HEADER>() == 36);
    assert!(core::mem::size_of::<IDEREGS>() == 8);
    // Заголовки должны совпадать со смещениями данных, иначе разбор
    // ответа уедет.
    assert!(core::mem::size_of::<SENDCMDINPARAMS>() == SENDCMD_IN_HEADER_BYTES);
    assert!(core::mem::size_of::<SENDCMDOUTPARAMS_HEADER>() == SENDCMD_OUT_HEADER_BYTES);
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Значения из `winioctl.h` и `ntddscsi.h`. Если формула `ctl_code`
    /// разъедется, промахнёмся мимо драйвера и получим невнятную ошибку.
    #[test]
    fn ioctl_codes_match_the_headers() {
        assert_eq!(IOCTL_STORAGE_QUERY_PROPERTY, 0x002D_1400);
        assert_eq!(IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, 0x0007_00A0);
        assert_eq!(IOCTL_ATA_PASS_THROUGH, 0x0004_D02C);
    }
}
