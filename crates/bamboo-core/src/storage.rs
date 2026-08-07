//! Накопители и их здоровье.
//!
//! Типы общие для NVMe и SATA. Часть полей у SATA недоступна или доступна
//! только косвенно, поэтому почти всё — `Option`. Это осознанно: по ТЗ
//! при невозможности прочитать данные полагается сказать «не могу»,
//! а не показать правдоподобную оценку.

use core::fmt;

use crate::units::Bytes;

/// Интерфейс подключения накопителя.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BusType {
    Nvme,
    Sata,
    Ata,
    Usb,
    Sas,
    /// Накопитель за RAID-контроллером. SMART отдельных дисков обычно
    /// недоступен — контроллер показывает виртуальный том.
    Raid,
    Sd,
    Virtual,
    Other(u32),
}

impl BusType {
    /// Есть ли принципиальная возможность прочитать SMART.
    pub fn smart_reachable(self) -> bool {
        matches!(self, BusType::Nvme | BusType::Sata | BusType::Ata)
    }

    pub fn name(self) -> &'static str {
        match self {
            BusType::Nvme => "NVMe",
            BusType::Sata => "SATA",
            BusType::Ata => "ATA",
            BusType::Usb => "USB",
            BusType::Sas => "SAS",
            BusType::Raid => "RAID",
            BusType::Sd => "SD",
            BusType::Virtual => "виртуальный",
            BusType::Other(_) => "неизвестный",
        }
    }
}

impl fmt::Display for BusType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Физический накопитель.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveInfo {
    /// Номер в `\\.\PhysicalDriveN`.
    pub index: u32,
    pub vendor: String,
    pub model: String,
    pub firmware: String,
    pub serial: Option<String>,
    pub bus: BusType,
    pub capacity: Bytes,
    pub removable: bool,
}

impl DriveInfo {
    /// Имя для показа пользователю: производитель обычно уже входит
    /// в модель, дублировать его незачем.
    pub fn display_name(&self) -> String {
        let model = self.model.trim();
        let vendor = self.vendor.trim();
        if vendor.is_empty() || model.to_lowercase().contains(&vendor.to_lowercase()) {
            model.to_string()
        } else {
            format!("{vendor} {model}")
        }
    }
}

/// Откуда получены показатели здоровья.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SmartSource {
    /// Health Information Log контроллера NVMe.
    NvmeHealthLog,
    /// SMART READ DATA через ATA pass-through.
    AtaSmart,
}

/// Предупреждение контроллера NVMe (регистр Critical Warning).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CriticalWarning(pub u8);

impl CriticalWarning {
    pub const NONE: CriticalWarning = CriticalWarning(0);

    pub fn is_clear(self) -> bool {
        self.0 == 0
    }

    /// Расшифровка битов. Это прямое предупреждение самого контроллера,
    /// а не наша интерпретация, поэтому показывается всегда и без смягчений.
    pub fn reasons(self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if self.0 & 0x01 != 0 {
            reasons.push("резервных блоков осталось меньше порога производителя");
        }
        if self.0 & 0x02 != 0 {
            reasons.push("температура вышла за допустимый диапазон");
        }
        if self.0 & 0x04 != 0 {
            reasons.push("надёжность носителя снижена");
        }
        if self.0 & 0x08 != 0 {
            reasons.push("накопитель переведён в режим только для чтения");
        }
        if self.0 & 0x10 != 0 {
            reasons.push("отказала система резервного питания энергозависимой памяти");
        }
        reasons
    }
}

/// Показатели здоровья накопителя.
///
/// Все поля, кроме источника, необязательны: набор доступных показателей
/// зависит и от интерфейса, и от конкретной модели.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SmartHealth {
    pub source: Option<SmartSource>,
    pub critical_warning: Option<CriticalWarning>,
    /// Температура в градусах Цельсия.
    pub temperature_c: Option<i16>,
    /// Остаток резервных блоков в процентах.
    pub available_spare: Option<u8>,
    /// Порог, ниже которого производитель считает резерв исчерпанным.
    pub available_spare_threshold: Option<u8>,
    /// Оценка износа контроллером. Может превышать 100 — это не ошибка,
    /// а сообщение «расчётный ресурс выработан».
    pub percentage_used: Option<u8>,
    /// Остаток ресурса в процентах по вендор-специфичному атрибуту SATA.
    pub life_left_percent: Option<u8>,
    /// Накопленный объём хост-записей.
    pub data_written: Option<Bytes>,
    /// Накопленный объём хост-чтений.
    pub data_read: Option<Bytes>,
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    pub unsafe_shutdowns: Option<u64>,
    /// Ошибки целостности носителя. Важен не сам счётчик, а его рост.
    pub media_errors: Option<u128>,
    /// Переназначенные секторы (SATA).
    pub reallocated_sectors: Option<u64>,
    /// Секторы, ожидающие переназначения (SATA).
    pub pending_sectors: Option<u64>,
}

impl SmartHealth {
    /// Насколько выработан ресурс, в процентах, из любого доступного источника.
    pub fn wear_percent(&self) -> Option<u8> {
        if let Some(used) = self.percentage_used {
            return Some(used);
        }
        self.life_left_percent
            .map(|left| 100u8.saturating_sub(left))
    }

    /// Опустился ли резерв ниже порога производителя.
    pub fn spare_below_threshold(&self) -> bool {
        match (self.available_spare, self.available_spare_threshold) {
            (Some(spare), Some(threshold)) => spare < threshold,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_controller_reports_nothing() {
        assert!(CriticalWarning::NONE.is_clear());
        assert!(CriticalWarning::NONE.reasons().is_empty());
    }

    #[test]
    fn warning_bits_are_decoded() {
        let warning = CriticalWarning(0b0000_0101);
        let reasons = warning.reasons();
        assert_eq!(reasons.len(), 2);
        assert!(!warning.is_clear());
    }

    #[test]
    fn wear_falls_back_to_the_sata_attribute() {
        let nvme = SmartHealth {
            percentage_used: Some(7),
            life_left_percent: Some(99),
            ..Default::default()
        };
        assert_eq!(nvme.wear_percent(), Some(7));

        let sata = SmartHealth {
            life_left_percent: Some(94),
            ..Default::default()
        };
        assert_eq!(sata.wear_percent(), Some(6));

        assert_eq!(SmartHealth::default().wear_percent(), None);
    }

    #[test]
    fn spare_needs_both_numbers_to_be_judged() {
        let known = SmartHealth {
            available_spare: Some(3),
            available_spare_threshold: Some(10),
            ..Default::default()
        };
        assert!(known.spare_below_threshold());

        // Порога нет — сравнивать не с чем, тревожить нельзя.
        let unknown = SmartHealth {
            available_spare: Some(3),
            ..Default::default()
        };
        assert!(!unknown.spare_below_threshold());
    }

    #[test]
    fn vendor_is_not_duplicated_in_the_name() {
        let drive = DriveInfo {
            index: 0,
            vendor: "Samsung".into(),
            model: "Samsung SSD 990 PRO".into(),
            firmware: "1B2QJXD7".into(),
            serial: None,
            bus: BusType::Nvme,
            capacity: Bytes::from_mib(1024 * 1024),
            removable: false,
        };
        assert_eq!(drive.display_name(), "Samsung SSD 990 PRO");
    }

    #[test]
    fn raid_is_marked_as_unreachable_for_smart() {
        assert!(!BusType::Raid.smart_reachable());
        assert!(!BusType::Usb.smart_reachable());
        assert!(BusType::Nvme.smart_reachable());
    }
}
