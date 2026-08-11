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

/// Что машина умеет по части сна и питания.
///
/// Нужно ради одного вопроса из ТЗ 9.7: «сколько батареи стоил процесс
/// в современном ждущем режиме». Вопрос осмыслен не на всякой машине,
/// и выяснить это надо **до** того, как рисовать пустые графики.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerCapabilities {
    /// Поддерживается ли современный ждущий режим (S0 Low Power Idle).
    /// На стационарных машинах его обычно нет.
    pub modern_standby: bool,
    /// Есть ли батарея вообще.
    pub has_battery: bool,
    /// Поддерживается ли гибернация.
    pub hibernate: bool,
}

impl PowerCapabilities {
    /// Можно ли вообще говорить о расходе батареи во сне.
    ///
    /// Без современного ждущего режима процессы во сне не работают —
    /// машина уходит в S3, и расходовать батарею там нечему. Без батареи
    /// расход не с чем соотносить. В обоих случаях честный ответ —
    /// «неизмеримо», а не число.
    pub fn standby_drain_measurable(self) -> bool {
        self.modern_standby && self.has_battery
    }

    /// Почему разряд во сне измерить нельзя. `None` — можно.
    ///
    /// Отдельным объяснением, а не пустотой: человек, открывший раздел
    /// питания, должен узнать причину, а не решить, что Bamboo сломался.
    pub fn why_not_measurable(self) -> Option<&'static str> {
        match (self.modern_standby, self.has_battery) {
            (true, true) => None,
            (false, true) => Some(
                "Эта машина не поддерживает современный ждущий режим: она уходит                  в обычный сон, где программы не работают вовсе. Расходовать                  батарею во сне здесь нечему, поэтому и мерить нечего.",
            ),
            (true, false) => Some(
                "У машины нет батареи, поэтому расход во сне не с чем соотносить.                  Сам ждущий режим при этом работает.",
            ),
            (false, false) => Some(
                "Стационарная машина: батареи нет, современный ждущий режим                  не поддерживается. Раздел про разряд во сне здесь не про что —                  и выдумывать числа Bamboo не станет.",
            ),
        }
    }
}

/// Спрашивает у Windows, что машина умеет.
pub fn power_capabilities() -> Result<PowerCapabilities> {
    use windows_sys::Win32::System::Power::{GetPwrCapabilities, SYSTEM_POWER_CAPABILITIES};

    let mut caps: SYSTEM_POWER_CAPABILITIES = unsafe { core::mem::zeroed() };
    let ok = unsafe { GetPwrCapabilities(&mut caps) };
    if !ok {
        return Err(Error::Win32 {
            call: "GetPwrCapabilities",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }

    Ok(PowerCapabilities {
        modern_standby: caps.AoAc,
        has_battery: caps.SystemBatteriesPresent,
        hibernate: caps.SystemS4 && caps.HiberFilePresent,
    })
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    #[test]
    fn a_desktop_without_a_battery_is_refused_with_a_reason() {
        // Главное правило раздела 9.7: отказ вместо оценки. Пустой график
        // с подписью «разряд во сне» на стационарной машине — обман.
        let desktop = PowerCapabilities {
            modern_standby: false,
            has_battery: false,
            hibernate: true,
        };
        assert!(!desktop.standby_drain_measurable());

        let why = desktop.why_not_measurable().expect("причина обязательна");
        assert!(why.contains("Стационарная"), "{why}");
        assert!(why.contains("выдумывать"), "{why}");
    }

    #[test]
    fn each_missing_piece_gets_its_own_explanation() {
        // Причины разные, и человеку они говорят разное: «машина не умеет»
        // и «батареи нет» — не одно и то же.
        let no_standby = PowerCapabilities {
            modern_standby: false,
            has_battery: true,
            hibernate: true,
        };
        let no_battery = PowerCapabilities {
            modern_standby: true,
            has_battery: false,
            hibernate: true,
        };

        let one = no_standby.why_not_measurable().unwrap();
        let other = no_battery.why_not_measurable().unwrap();
        assert_ne!(one, other);
        assert!(one.contains("обычный сон"), "{one}");
        assert!(other.contains("нет батареи"), "{other}");
    }

    #[test]
    fn a_laptop_with_modern_standby_is_measurable() {
        let laptop = PowerCapabilities {
            modern_standby: true,
            has_battery: true,
            hibernate: false,
        };
        assert!(laptop.standby_drain_measurable());
        assert_eq!(laptop.why_not_measurable(), None);
    }

    #[test]
    fn the_real_machine_answers_without_error() {
        // Живая проверка: сам вызов обязан удаваться на любой машине.
        // Что именно он вернёт — зависит от железа, и это нормально.
        let caps = power_capabilities().expect("возможности питания");
        // Взаимная согласованность: измеримо ровно тогда, когда причины нет.
        assert_eq!(
            caps.standby_drain_measurable(),
            caps.why_not_measurable().is_none()
        );
    }
}
