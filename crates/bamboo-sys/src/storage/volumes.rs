//! Разделы дисков: сколько места и сколько осталось (ТЗ, раздел 9.4).
//!
//! Накопитель и раздел — разные вещи, и человеку нужны обе. Накопитель
//! отвечает на вопрос «сколько ему осталось жить», раздел — на вопрос
//! «куда ещё влезет». Второй как раз и волнует чаще.
//!
//! Заодно свободное место — это причина тормозов, о которой мало кто
//! помнит: SSD, забитый под завязку, теряет скорость записи, потому что
//! контроллеру негде разворачиваться.

use bamboo_core::{Bytes, Error, Result};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW,
};
use windows_sys::Win32::System::WindowsProgramming::{
    DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
};

/// Что за раздел.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeKind {
    /// Обычный диск внутри машины.
    Fixed,
    /// Флешка или внешний диск.
    Removable,
    /// Сетевой диск.
    Network,
    /// Диск в оперативной памяти.
    RamDisk,
    Other,
}

impl VolumeKind {
    pub fn name(self) -> &'static str {
        match self {
            VolumeKind::Fixed => "внутренний",
            VolumeKind::Removable => "съёмный",
            VolumeKind::Network => "сетевой",
            VolumeKind::RamDisk => "в памяти",
            VolumeKind::Other => "прочий",
        }
    }
}

/// Раздел с буквой диска.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// Буква диска: `C`, `D` и так далее.
    pub letter: char,
    /// Метка тома, если задана.
    pub label: String,
    /// Файловая система: NTFS, exFAT и подобное.
    pub file_system: String,
    pub kind: VolumeKind,
    pub total: Bytes,
    pub free: Bytes,
}

impl Volume {
    pub fn used(&self) -> Bytes {
        Bytes(self.total.as_u64().saturating_sub(self.free.as_u64()))
    }

    /// Какая доля раздела занята, 0..1.
    pub fn usage(&self) -> f64 {
        if self.total.as_u64() == 0 {
            return 0.0;
        }
        self.used().as_u64() as f64 / self.total.as_u64() as f64
    }

    /// Мало ли места настолько, что это уже вредит скорости.
    ///
    /// Порог не в «остался гигабайт»: SSD начинает терять скорость записи
    /// заметно раньше, чем кончается место, — контроллеру нужен запас
    /// свободных ячеек, чтобы раскладывать данные. Десять процентов —
    /// общепринятая граница, после которой это становится заметно.
    pub fn is_cramped(&self) -> bool {
        self.total.as_u64() > 0 && self.usage() >= 0.90
    }
}

/// Перечисляет разделы с буквами.
///
/// Сетевые диски пропускаем: их занятость к здоровью этой машины
/// отношения не имеет, а запрос к недоступной сети подвешивает вызов
/// на секунды.
pub fn volumes() -> Result<Vec<Volume>> {
    let mut buffer = [0u16; 512];
    let length = unsafe { GetLogicalDriveStringsW(buffer.len() as u32, buffer.as_mut_ptr()) };
    if length == 0 {
        return Err(Error::Win32 {
            call: "GetLogicalDriveStringsW",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }

    let mut volumes = Vec::new();
    // Строки идут подряд, каждая с нулём, и заканчиваются двойным нулём.
    for chunk in buffer[..length as usize].split(|c| *c == 0) {
        if chunk.is_empty() {
            continue;
        }
        let root: Vec<u16> = chunk.iter().copied().chain(core::iter::once(0)).collect();
        let Some(letter) = char::from_u32(chunk[0] as u32) else {
            continue;
        };

        let kind = match unsafe { GetDriveTypeW(root.as_ptr()) } {
            DRIVE_FIXED => VolumeKind::Fixed,
            DRIVE_REMOVABLE => VolumeKind::Removable,
            DRIVE_REMOTE => VolumeKind::Network,
            DRIVE_RAMDISK => VolumeKind::RamDisk,
            _ => VolumeKind::Other,
        };
        if kind == VolumeKind::Network {
            continue;
        }

        let mut free: u64 = 0;
        let mut total: u64 = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(root.as_ptr(), core::ptr::null_mut(), &mut total, &mut free)
        };
        if ok == 0 {
            // Пустой привод или карт-ридер без карты: места нет, потому
            // что нет носителя. Это не ошибка, просто пропускаем.
            continue;
        }

        let (label, file_system) = volume_names(&root);
        volumes.push(Volume {
            letter: letter.to_ascii_uppercase(),
            label,
            file_system,
            kind,
            total: Bytes(total),
            free: Bytes(free),
        });
    }

    Ok(volumes)
}

/// Метка тома и файловая система.
fn volume_names(root: &[u16]) -> (String, String) {
    let mut label = [0u16; 256];
    let mut file_system = [0u16; 64];

    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            file_system.as_mut_ptr(),
            file_system.len() as u32,
        )
    };
    if ok == 0 {
        return (String::new(), String::new());
    }

    (utf16_to_string(&label), utf16_to_string(&file_system))
}

fn utf16_to_string(text: &[u16]) -> String {
    let end = text.iter().position(|c| *c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(total_gib: u64, free_gib: u64) -> Volume {
        Volume {
            letter: 'C',
            label: "Система".to_string(),
            file_system: "NTFS".to_string(),
            kind: VolumeKind::Fixed,
            total: Bytes::from_mib(total_gib * 1024),
            free: Bytes::from_mib(free_gib * 1024),
        }
    }

    #[test]
    fn usage_is_the_occupied_share() {
        let disk = volume(100, 25);
        assert_eq!(disk.used().as_u64(), Bytes::from_mib(75 * 1024).as_u64());
        assert!((disk.usage() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn a_nearly_full_disk_is_flagged() {
        // Девяносто процентов — граница, после которой SSD теряет скорость
        // записи: контроллеру негде раскладывать данные.
        assert!(volume(100, 5).is_cramped());
        assert!(!volume(100, 30).is_cramped());
    }

    #[test]
    fn an_empty_volume_does_not_divide_by_zero() {
        let disk = Volume {
            total: Bytes::ZERO,
            free: Bytes::ZERO,
            ..volume(0, 0)
        };
        assert_eq!(disk.usage(), 0.0);
        assert!(!disk.is_cramped());
    }

    #[test]
    fn every_kind_has_a_name() {
        for kind in [
            VolumeKind::Fixed,
            VolumeKind::Removable,
            VolumeKind::Network,
            VolumeKind::RamDisk,
            VolumeKind::Other,
        ] {
            assert!(!kind.name().is_empty());
        }
    }

    #[test]
    fn volumes_are_listed_on_a_live_system() {
        // На любой Windows есть системный раздел, и он читается без прав.
        let list = volumes().expect("разделы не перечислились");
        assert!(!list.is_empty(), "не найдено ни одного раздела");

        let system = list.iter().find(|v| v.letter == 'C');
        assert!(system.is_some(), "системный раздел не найден");

        for volume in &list {
            assert!(volume.free.as_u64() <= volume.total.as_u64());
            assert!(volume.letter.is_ascii_alphabetic());
        }
    }
}
