//! Накопители: перечисление и чтение SMART.
//!
//! Два пути. У NVMe журнал здоровья читается штатным запросом свойств
//! и не требует прав администратора. У SATA приходится посылать ATA-команду
//! напрямую, а для этого устройство нужно открыть на запись — то есть
//! от администратора. В продукте это делает брокер под SYSTEM.

pub mod activity;
mod ata;
mod device;
mod ioctl;
mod nvme;
pub mod volumes;

pub use activity::{
    activity_between, pagefiles, read_counters, DiskActivity, DiskCounters, Pagefile,
};
pub use device::{enumerate, Drive};
pub use volumes::{volumes, Volume, VolumeKind};

use bamboo_core::storage::{BusType, DriveInfo, SmartHealth};
use bamboo_core::{Error, Result};

/// Читает здоровье накопителя, выбирая способ по типу шины.
pub fn read_smart(info: &DriveInfo) -> Result<SmartHealth> {
    match info.bus {
        BusType::Nvme => {
            let drive = Drive::open(info.index)?;
            nvme::read_health_log(&drive)
        }
        BusType::Sata | BusType::Ata => {
            let drive = Drive::open_for_ata(info.index)?;
            ata::read_smart_data(&drive, &info.vendor, &info.model)
        }
        // Не «оценка по косвенным признакам», а честный отказ: за
        // RAID-контроллером или USB-мостом настоящих данных накопителя нет.
        BusType::Raid => Err(Error::Unsupported(
            "накопитель за RAID-контроллером: SMART отдельных дисков недоступен",
        )),
        BusType::Usb => Err(Error::Unsupported(
            "USB-мост не транслирует SMART-команды накопителя",
        )),
        other => {
            let _ = other;
            Err(Error::Unsupported("шина не поддерживает чтение SMART"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_either_works_or_says_why_not() {
        for info in enumerate() {
            match read_smart(&info) {
                Ok(health) => {
                    assert!(health.source.is_some());
                }
                Err(error) => {
                    // Отказ обязан быть объяснимым. Молчаливого «нет данных»
                    // в продукте быть не должно.
                    let text = error.to_string();
                    assert!(!text.is_empty());
                    println!("{}: {}", info.display_name(), text);
                }
            }
        }
    }
}
