//! Открытие физических накопителей и чтение их паспортных данных.

use core::mem::size_of;

use bamboo_core::storage::{BusType, DriveInfo};
use bamboo_core::{Bytes, Error, Result};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use super::ioctl::*;

/// Сколько номеров `PhysicalDriveN` перебирать. Больше 32 накопителей
/// на потребительской машине не бывает, а на серверных Bamboo не рассчитан.
const MAX_DRIVES: u32 = 32;

/// Открытый дескриптор физического накопителя.
pub struct Drive {
    handle: HANDLE,
    index: u32,
    writable: bool,
}

impl Drive {
    /// Открывает накопитель только для запросов свойств.
    ///
    /// Нулевые права доступа — не оптимизация, а необходимость: с ними
    /// `IOCTL_STORAGE_QUERY_PROPERTY` работает от обычного пользователя,
    /// то есть SMART у NVMe читается без UAC.
    pub fn open(index: u32) -> Result<Drive> {
        Self::open_with(index, 0, false)
    }

    /// Открывает накопитель на чтение и запись — нужно для ATA pass-through.
    /// Требует прав администратора.
    pub fn open_for_ata(index: u32) -> Result<Drive> {
        const GENERIC_READ: u32 = 0x8000_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        Self::open_with(index, GENERIC_READ | GENERIC_WRITE, true)
    }

    fn open_with(index: u32, access: u32, writable: bool) -> Result<Drive> {
        let path: Vec<u16> = format!("\\\\.\\PhysicalDrive{index}\0")
            .encode_utf16()
            .collect();

        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                core::ptr::null(),
                OPEN_EXISTING,
                0,
                core::ptr::null_mut(),
            )
        };

        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            let code = unsafe { GetLastError() };
            if code == ERROR_ACCESS_DENIED && writable {
                return Err(Error::Unsupported(
                    "чтение SMART у SATA требует прав администратора",
                ));
            }
            return Err(Error::Win32 {
                call: "CreateFileW(PhysicalDrive)",
                code,
            });
        }

        Ok(Drive {
            handle,
            index,
            writable,
        })
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Отправляет IOCTL с общим входным и выходным буфером.
    pub(super) fn control(&self, code: u32, input: &[u8], output: &mut [u8]) -> Result<u32> {
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                code,
                input.as_ptr().cast(),
                input.len() as u32,
                output.as_mut_ptr().cast(),
                output.len() as u32,
                &mut returned,
                core::ptr::null_mut(),
            )
        };

        if ok == 0 {
            return Err(Error::Win32 {
                call: "DeviceIoControl",
                code: unsafe { GetLastError() },
            });
        }
        Ok(returned)
    }

    /// Паспортные данные накопителя: модель, прошивка, серийный номер, шина.
    pub fn info(&self) -> Result<DriveInfo> {
        let query = STORAGE_PROPERTY_QUERY_WITH_PROTOCOL {
            PropertyId: STORAGE_DEVICE_PROPERTY,
            QueryType: PROPERTY_STANDARD_QUERY,
            ProtocolSpecific: STORAGE_PROTOCOL_SPECIFIC_DATA::default(),
        };

        let mut output = [0u8; 2048];
        let returned = self.control(IOCTL_STORAGE_QUERY_PROPERTY, as_bytes(&query), &mut output)?;

        if (returned as usize) < size_of::<STORAGE_DEVICE_DESCRIPTOR_HEADER>() {
            return Err(Error::Malformed("дескриптор устройства короче заголовка"));
        }

        // SAFETY: длина проверена, буфер выровнен по границе массива u8,
        // читаем через read_unaligned.
        let header = unsafe {
            core::ptr::read_unaligned(output.as_ptr().cast::<STORAGE_DEVICE_DESCRIPTOR_HEADER>())
        };

        let text = |offset: u32| ansi_at(&output[..returned as usize], offset as usize);

        Ok(DriveInfo {
            index: self.index,
            vendor: text(header.VendorIdOffset).unwrap_or_default(),
            model: text(header.ProductIdOffset).unwrap_or_default(),
            firmware: text(header.ProductRevisionOffset).unwrap_or_default(),
            serial: text(header.SerialNumberOffset).filter(|s| !s.is_empty()),
            bus: bus_type(header.BusType),
            capacity: self.capacity().unwrap_or(Bytes::ZERO),
            removable: header.RemovableMedia != 0,
        })
    }

    fn capacity(&self) -> Result<Bytes> {
        let mut output = [0u8; size_of::<DISK_GEOMETRY_EX_HEADER>()];
        self.control(IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut output)?;
        let geometry =
            unsafe { core::ptr::read_unaligned(output.as_ptr().cast::<DISK_GEOMETRY_EX_HEADER>()) };
        Ok(Bytes(geometry.DiskSize.max(0) as u64))
    }
}

impl Drop for Drive {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// Перечисляет физические накопители.
///
/// Идём по номерам подряд, а не через SetupAPI: номера могут быть
/// с пропусками после отключения устройства, поэтому промахи пропускаем
/// и продолжаем.
pub fn enumerate() -> Vec<DriveInfo> {
    let mut drives = Vec::new();
    for index in 0..MAX_DRIVES {
        if let Ok(drive) = Drive::open(index) {
            if let Ok(info) = drive.info() {
                drives.push(info);
            }
        }
    }
    drives
}

fn bus_type(raw: u32) -> BusType {
    match raw {
        0x03 => BusType::Ata,
        0x07 => BusType::Usb,
        0x08 => BusType::Raid,
        0x0A => BusType::Sas,
        0x0B => BusType::Sata,
        0x0C => BusType::Sd,
        0x0E => BusType::Virtual,
        0x11 => BusType::Nvme,
        other => BusType::Other(other),
    }
}

/// Читает строку в кодировке ANSI по смещению внутри дескриптора.
/// Нулевое смещение означает, что поля нет.
fn ansi_at(buffer: &[u8], offset: usize) -> Option<String> {
    if offset == 0 || offset >= buffer.len() {
        return None;
    }
    let tail = &buffer[offset..];
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    Some(
        tail[..end]
            .iter()
            .map(|&b| b as char)
            .collect::<String>()
            .trim()
            .to_string(),
    )
}

pub(super) fn as_bytes<T>(value: &T) -> &[u8] {
    // SAFETY: читаем структуру как байты для передачи в драйвер,
    // время жизни среза привязано к ссылке.
    unsafe { core::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_least_one_drive_is_found() {
        let drives = enumerate();
        assert!(!drives.is_empty(), "система загрузилась хоть с чего-то");
    }

    #[test]
    fn system_drive_has_a_model_and_capacity() {
        let drives = enumerate();
        let first = &drives[0];

        assert!(!first.model.is_empty(), "модель пустая");
        assert!(
            first.capacity > Bytes::from_mib(1024),
            "ёмкость {} выглядит неправдоподобно",
            first.capacity
        );
        assert_ne!(first.bus, BusType::Other(0), "тип шины не распознан");
    }

    #[test]
    fn properties_are_readable_without_elevation() {
        // Ключевое свойство: запрос свойств работает от обычного
        // пользователя. Если это сломается, агент без прав администратора
        // перестанет видеть накопители.
        let drive = Drive::open(0).expect("PhysicalDrive0 не открылся");
        assert!(!drive.is_writable());
        assert!(drive.info().is_ok());
    }

    #[test]
    fn missing_drive_reports_an_error() {
        assert!(Drive::open(MAX_DRIVES + 100).is_err());
    }
}
