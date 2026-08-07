//! Питание и батарея.
//!
//! На батарее Bamboo обязан вести себя тише: реже опрашивать систему
//! и не запускать тяжёлые анализаторы (ТЗ, разделы 6.2 и 11.4).

use bamboo_core::{Error, Result};
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

/// Источник питания.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerSource {
    /// Питание от сети.
    Ac,
    /// Питание от батареи.
    Battery,
    /// Определить не удалось — так бывает на стационарных машинах
    /// и в виртуальных.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerStatus {
    pub source: PowerSource,
    /// Заряд в процентах, если батарея есть.
    pub battery_percent: Option<u8>,
    /// Включён ли режим экономии заряда.
    pub saver_on: bool,
}

impl PowerStatus {
    pub fn on_battery(&self) -> bool {
        self.source == PowerSource::Battery
    }

    /// Заряд ниже 20% — по ТЗ основание перейти на минимальную частоту опроса.
    pub fn battery_low(&self) -> bool {
        self.on_battery() && self.battery_percent.is_some_and(|p| p < 20)
    }
}

pub fn power_status() -> Result<PowerStatus> {
    let mut raw: SYSTEM_POWER_STATUS = unsafe { core::mem::zeroed() };
    let ok = unsafe { GetSystemPowerStatus(&mut raw) };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "GetSystemPowerStatus",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }

    let source = match raw.ACLineStatus {
        0 => PowerSource::Battery,
        1 => PowerSource::Ac,
        _ => PowerSource::Unknown,
    };

    // 255 в этом поле означает «неизвестно», а не полный заряд.
    let battery_percent = (raw.BatteryLifePercent <= 100).then_some(raw.BatteryLifePercent);

    Ok(PowerStatus {
        source,
        battery_percent,
        saver_on: raw.SystemStatusFlag == 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_status_is_readable() {
        let status = power_status().unwrap();
        if let Some(percent) = status.battery_percent {
            assert!(percent <= 100);
        }
    }

    #[test]
    fn unknown_charge_is_not_treated_as_low() {
        let status = PowerStatus {
            source: PowerSource::Battery,
            battery_percent: None,
            saver_on: false,
        };
        assert!(!status.battery_low());
    }

    #[test]
    fn low_battery_only_counts_on_battery() {
        let plugged = PowerStatus {
            source: PowerSource::Ac,
            battery_percent: Some(5),
            saver_on: false,
        };
        assert!(!plugged.battery_low());

        let unplugged = PowerStatus {
            source: PowerSource::Battery,
            battery_percent: Some(5),
            saver_on: false,
        };
        assert!(unplugged.battery_low());
    }
}
