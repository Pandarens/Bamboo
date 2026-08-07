//! Журнал здоровья NVMe (SMART / Health Information Log, страница 0x02).
//!
//! Читается штатным `IOCTL_STORAGE_QUERY_PROPERTY` и не требует прав
//! администратора — этим NVMe выгодно отличается от SATA.

use core::mem::size_of;

use bamboo_core::storage::{CriticalWarning, SmartHealth, SmartSource};
use bamboo_core::{Bytes, Error, Result};

use super::device::{as_bytes, Drive};
use super::ioctl::*;

/// Размер страницы журнала.
const LOG_PAGE_BYTES: u32 = 512;

/// Единица измерения объёма в журнале NVMe: 1000 блоков по 512 байт.
/// Не 512 КиБ и не 500 КБ — ровно 512 000 байт по спецификации.
const DATA_UNIT_BYTES: u128 = 512_000;

pub fn read_health_log(drive: &Drive) -> Result<SmartHealth> {
    let query = STORAGE_PROPERTY_QUERY_WITH_PROTOCOL {
        PropertyId: STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY,
        QueryType: PROPERTY_STANDARD_QUERY,
        ProtocolSpecific: STORAGE_PROTOCOL_SPECIFIC_DATA {
            ProtocolType: PROTOCOL_TYPE_NVME,
            DataType: NVME_DATA_TYPE_LOG_PAGE,
            ProtocolDataRequestValue: NVME_LOG_PAGE_HEALTH_INFO,
            ProtocolDataRequestSubValue: 0,
            // Смещение отсчитывается от начала STORAGE_PROTOCOL_SPECIFIC_DATA
            // и не может быть меньше её размера.
            ProtocolDataOffset: size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>() as u32,
            ProtocolDataLength: LOG_PAGE_BYTES,
            ..Default::default()
        },
    };

    let mut output = [0u8; 4096];
    let returned = drive.control(IOCTL_STORAGE_QUERY_PROPERTY, as_bytes(&query), &mut output)?;

    let descriptor_size = size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>();
    if (returned as usize) < descriptor_size {
        return Err(Error::Malformed("ответ короче дескриптора протокола"));
    }

    let descriptor = unsafe {
        core::ptr::read_unaligned(output.as_ptr().cast::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>())
    };

    // Данные лежат по смещению от начала ProtocolSpecificData, а она сама
    // начинается после Version и Size.
    let start = 2 * size_of::<u32>() + descriptor.ProtocolSpecificData.ProtocolDataOffset as usize;
    let length = descriptor.ProtocolSpecificData.ProtocolDataLength as usize;

    if length < 512 || start + length > output.len() || start + length > returned as usize {
        return Err(Error::Malformed(
            "контроллер вернул журнал здоровья неожиданного размера",
        ));
    }

    Ok(parse_health_log(&output[start..start + length]))
}

/// Разбирает 512 байт страницы 0x02 согласно NVM Express Base Specification.
fn parse_health_log(page: &[u8]) -> SmartHealth {
    let u16_at = |offset: usize| u16::from_le_bytes([page[offset], page[offset + 1]]);
    let u128_at = |offset: usize| {
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&page[offset..offset + 16]);
        u128::from_le_bytes(bytes)
    };
    let bytes_at = |offset: usize| {
        Bytes((u128_at(offset).saturating_mul(DATA_UNIT_BYTES)).min(u64::MAX as u128) as u64)
    };

    // Температура хранится в кельвинах. Ноль означает «датчик не отвечает»,
    // а не абсолютный ноль.
    let kelvin = u16_at(1);
    let temperature_c = (kelvin != 0).then(|| kelvin as i16 - 273);

    SmartHealth {
        source: Some(SmartSource::NvmeHealthLog),
        critical_warning: Some(CriticalWarning(page[0])),
        temperature_c,
        available_spare: Some(page[3]),
        available_spare_threshold: Some(page[4]),
        percentage_used: Some(page[5]),
        life_left_percent: None,
        data_read: Some(bytes_at(32)),
        data_written: Some(bytes_at(48)),
        power_cycles: Some(u128_at(112).min(u64::MAX as u128) as u64),
        power_on_hours: Some(u128_at(128).min(u64::MAX as u128) as u64),
        unsafe_shutdowns: Some(u128_at(144).min(u64::MAX as u128) as u64),
        media_errors: Some(u128_at(160)),
        reallocated_sectors: None,
        pending_sectors: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Собирает правдоподобную страницу журнала для проверки разбора.
    fn page() -> Vec<u8> {
        let mut page = vec![0u8; 512];
        page[0] = 0x00; // предупреждений нет
        page[1..3].copy_from_slice(&(273u16 + 41).to_le_bytes()); // 41 °C
        page[3] = 100; // резерв
        page[4] = 10; // порог
        page[5] = 2; // израсходовано 2%
        page[32..48].copy_from_slice(&40_000_000u128.to_le_bytes()); // чтение
        page[48..64].copy_from_slice(&20_000_000u128.to_le_bytes()); // запись
        page[112..128].copy_from_slice(&1_500u128.to_le_bytes());
        page[128..144].copy_from_slice(&9_000u128.to_le_bytes());
        page[144..160].copy_from_slice(&12u128.to_le_bytes());
        page[160..176].copy_from_slice(&0u128.to_le_bytes());
        page
    }

    #[test]
    fn health_log_is_parsed() {
        let health = parse_health_log(&page());

        assert_eq!(health.temperature_c, Some(41));
        assert_eq!(health.percentage_used, Some(2));
        assert_eq!(health.available_spare, Some(100));
        assert_eq!(health.power_on_hours, Some(9_000));
        assert_eq!(health.unsafe_shutdowns, Some(12));
        assert!(health.critical_warning.unwrap().is_clear());
        assert!(!health.spare_below_threshold());
    }

    #[test]
    fn data_units_convert_to_bytes() {
        let health = parse_health_log(&page());
        // 20 000 000 единиц по 512 000 байт — это 10.24 ТБ записей.
        assert_eq!(health.data_written, Some(Bytes(20_000_000 * 512_000)));
        assert!(health.data_written.unwrap().as_u64() > 10_000_000_000_000);
    }

    #[test]
    fn dead_temperature_sensor_is_not_reported_as_minus_273() {
        let mut page = page();
        page[1..3].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(parse_health_log(&page).temperature_c, None);
    }

    #[test]
    fn worn_out_drive_is_recognised() {
        let mut page = page();
        page[0] = 0x01; // резерв ниже порога
        page[3] = 4;
        page[4] = 10;
        page[5] = 118; // контроллер считает ресурс выработанным

        let health = parse_health_log(&page);
        assert!(health.spare_below_threshold());
        assert!(!health.critical_warning.unwrap().is_clear());
        // Значение выше 100 — не ошибка разбора, а сообщение контроллера.
        assert_eq!(health.wear_percent(), Some(118));
    }
}
