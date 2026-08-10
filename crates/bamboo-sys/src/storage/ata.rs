//! SMART у SATA через ATA pass-through.
//!
//! В отличие от NVMe здесь нет единой структуры: контроллер отдаёт таблицу
//! из тридцати атрибутов, смысл которых стандартизован лишь частично.
//! Показатель остаточного ресурса вендор-специфичен, и таблицу соответствий
//! приходится вести вручную (ТЗ, раздел 19 — открытый вопрос).

use core::mem::size_of;

use bamboo_core::storage::{SmartHealth, SmartSource};
use bamboo_core::{Bytes, Error, Result};

use super::device::Drive;
use super::ioctl::*;

/// Размер ответа SMART READ DATA.
const SMART_DATA_BYTES: usize = 512;
/// Атрибуты идут по 12 байт начиная с третьего байта ответа.
const ATTRIBUTE_SIZE: usize = 12;
const ATTRIBUTE_COUNT: usize = 30;
const ATTRIBUTE_TABLE_OFFSET: usize = 2;

/// Размер логического блока для пересчёта атрибута 241 в байты.
///
/// Оговорка: часть производителей считает этот атрибут не в блоках,
/// а в гигабайтах или в блоках по 32 МБ. Универсального способа отличить
/// одно от другого нет, поэтому берём стандартные 512 байт и помечаем
/// значение как оценку.
const LBA_BYTES: u64 = 512;

/// Один атрибут SMART.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attribute {
    pub id: u8,
    /// Нормализованное значение: обычно 100 при новом накопителе
    /// и убывает к порогу отказа.
    pub value: u8,
    pub worst: u8,
    /// Сырое 48-битное значение, смысл зависит от атрибута.
    pub raw: u64,
}

pub fn read_smart_data(drive: &Drive, vendor: &str, model: &str) -> Result<SmartHealth> {
    if !drive.is_writable() {
        return Err(Error::Unsupported(
            "для чтения SMART устройство должно быть открыто на запись",
        ));
    }

    // Сначала пробуем прямой ATA pass-through. Часть контроллеров, включая
    // бюджетные Apacer, отвергают его с ошибкой 1306 — тогда переходим
    // на старый SMART_RCV_DRIVE_DATA, который драйвер транслирует сам
    // и который принимает почти любой SATA-контроллер.
    match read_via_pass_through(drive) {
        Ok(data) => Ok(build_health(&parse_attributes(&data), vendor, model)),
        Err(_) => {
            let data = read_via_smart_ioctl(drive)?;
            Ok(build_health(&parse_attributes(&data), vendor, model))
        }
    }
}

/// Читает 512 байт SMART через `IOCTL_ATA_PASS_THROUGH`.
fn read_via_pass_through(drive: &Drive) -> Result<Vec<u8>> {
    let header_size = size_of::<ATA_PASS_THROUGH_EX>();
    let total = header_size + SMART_DATA_BYTES;

    let mut request = ATA_PASS_THROUGH_EX {
        Length: header_size as u16,
        AtaFlags: ATA_FLAGS_DRDY_REQUIRED | ATA_FLAGS_DATA_IN,
        DataTransferLength: SMART_DATA_BYTES as u32,
        TimeOutValue: 10,
        DataBufferOffset: header_size,
        ..Default::default()
    };

    // Регистры ATA: Features, SectorCount, LBALow, LBAMid, LBAHigh,
    // Device/Head, Command. Без магических 0x4F и 0xC2 в регистрах цилиндра
    // команда SMART не распознаётся контроллером.
    request.CurrentTaskFile[0] = ATA_SMART_READ_DATA;
    request.CurrentTaskFile[1] = 1;
    request.CurrentTaskFile[2] = 0;
    request.CurrentTaskFile[3] = ATA_SMART_LBA_MID;
    request.CurrentTaskFile[4] = ATA_SMART_LBA_HIGH;
    request.CurrentTaskFile[5] = 0;
    request.CurrentTaskFile[6] = ATA_COMMAND_SMART;

    let mut buffer = vec![0u8; total];
    // SAFETY: заголовок укладывается в начало буфера, размер проверен.
    unsafe {
        core::ptr::write_unaligned(buffer.as_mut_ptr().cast::<ATA_PASS_THROUGH_EX>(), request);
    }

    let input = buffer.clone();
    let returned = drive.control(IOCTL_ATA_PASS_THROUGH, &input, &mut buffer)?;

    if (returned as usize) < total {
        return Err(Error::Malformed("ответ SMART READ DATA короче ожидаемого"));
    }
    Ok(buffer[header_size..header_size + SMART_DATA_BYTES].to_vec())
}

/// Читает 512 байт SMART через старый `SMART_RCV_DRIVE_DATA`.
///
/// Формат древний, времён IDE, но именно поэтому его поддерживают почти
/// все контроллеры: драйвер сам превращает его в нужную ATA-команду.
fn read_via_smart_ioctl(drive: &Drive) -> Result<Vec<u8>> {
    let mut params = SENDCMDINPARAMS {
        buffer_size: SMART_DATA_BYTES as u32,
        drive_number: drive.index() as u8,
        ..Default::default()
    };
    params.registers.features = SMART_READ_DATA;
    params.registers.sector_count = 1;
    params.registers.sector_number = 1;
    params.registers.cyl_low = ATA_SMART_LBA_MID;
    params.registers.cyl_high = ATA_SMART_LBA_HIGH;
    params.registers.drive_head = SMART_DRIVE_HEAD;
    params.registers.command = ATA_COMMAND_SMART;

    let input_size = SENDCMD_IN_HEADER_BYTES + SMART_DATA_BYTES;
    let mut input = vec![0u8; input_size];
    // SAFETY: заголовок укладывается в начало буфера нужного размера.
    unsafe {
        core::ptr::write_unaligned(input.as_mut_ptr().cast::<SENDCMDINPARAMS>(), params);
    }

    let output_size = SENDCMD_OUT_HEADER_BYTES + SMART_DATA_BYTES;
    let mut output = vec![0u8; output_size];

    let returned = drive.control(SMART_RCV_DRIVE_DATA, &input, &mut output)?;
    if (returned as usize) < output_size {
        return Err(Error::Malformed(
            "ответ SMART_RCV_DRIVE_DATA короче ожидаемого",
        ));
    }

    Ok(output[SENDCMD_OUT_HEADER_BYTES..SENDCMD_OUT_HEADER_BYTES + SMART_DATA_BYTES].to_vec())
}

/// Разбирает таблицу атрибутов.
fn parse_attributes(data: &[u8]) -> Vec<Attribute> {
    let mut attributes = Vec::new();
    for index in 0..ATTRIBUTE_COUNT {
        let start = ATTRIBUTE_TABLE_OFFSET + index * ATTRIBUTE_SIZE;
        if start + ATTRIBUTE_SIZE > data.len() {
            break;
        }
        let entry = &data[start..start + ATTRIBUTE_SIZE];

        // Нулевой идентификатор — пустая строка таблицы, не атрибут.
        if entry[0] == 0 {
            continue;
        }

        let mut raw = [0u8; 8];
        raw[..6].copy_from_slice(&entry[5..11]);

        attributes.push(Attribute {
            id: entry[0],
            value: entry[3],
            worst: entry[4],
            raw: u64::from_le_bytes(raw),
        });
    }
    attributes
}

fn find(attributes: &[Attribute], id: u8) -> Option<&Attribute> {
    attributes.iter().find(|a| a.id == id)
}

/// Идентификаторы атрибутов, у которых стандартизован смысл.
mod ids {
    pub const REALLOCATED_SECTORS: u8 = 5;
    pub const POWER_ON_HOURS: u8 = 9;
    pub const POWER_CYCLES: u8 = 12;
    pub const TEMPERATURE: u8 = 194;
    pub const PENDING_SECTORS: u8 = 197;
    pub const UNCORRECTABLE: u8 = 198;
    pub const TOTAL_LBA_WRITTEN: u8 = 241;
    pub const TOTAL_LBA_READ: u8 = 242;
}

/// Атрибут остаточного ресурса по производителю.
///
/// Единого стандарта нет: Samsung пишет ресурс в 177, Intel в 233,
/// Micron и Crucial в 202, большинство контроллеров Phison и SMI — в 231.
/// Порядок в списке — порядок проверки.
fn life_left_attribute_order(vendor: &str, model: &str) -> &'static [u8] {
    let text = format!("{vendor} {model}").to_lowercase();

    if text.contains("samsung") {
        &[177, 231, 202, 233]
    } else if text.contains("intel") {
        &[233, 231, 202, 177]
    } else if text.contains("crucial") || text.contains("micron") {
        &[202, 231, 233, 177]
    } else {
        // Неизвестный производитель: пробуем самый распространённый
        // атрибут первым. Если ни один не нашёлся — остатка ресурса
        // просто не будет, и это лучше выдуманного числа.
        &[231, 202, 177, 233]
    }
}

/// Проверяет, правдоподобен ли объём записи для такой наработки.
///
/// Атрибут 241 по соглашению считает логические блоки по 512 байт, но
/// соглашение это не стандарт: часть дешёвых контроллеров пишет туда
/// гигабайты, часть — свои единицы, и узнать какие именно неоткуда.
/// Пересчёт тогда даёт бессмыслицу вроде «записано 3.87 МБ» у диска
/// с наработкой в три тысячи часов.
///
/// Отличить эти случаи можно по здравому смыслу: любой накопитель,
/// проживший сотню часов, записал больше гигабайта — одни только журналы
/// Windows дают больше. Если насчитали меньше, значит единицы атрибута
/// нам неизвестны, и мы не показываем ничего. Выдуманная цифра хуже
/// честного пробела (ТЗ, разделы 5.7 и 19).
fn plausible_written(written: Bytes, power_on_hours: Option<u64>) -> Option<Bytes> {
    const MIN_PLAUSIBLE: u64 = 1024 * 1024 * 1024; // 1 ГБ
    const HOURS_TO_JUDGE: u64 = 100;

    match power_on_hours {
        Some(hours) if hours >= HOURS_TO_JUDGE && written.as_u64() < MIN_PLAUSIBLE => None,
        _ => Some(written),
    }
}

fn build_health(attributes: &[Attribute], vendor: &str, model: &str) -> SmartHealth {
    let life_left = life_left_attribute_order(vendor, model)
        .iter()
        .find_map(|id| find(attributes, *id))
        // Нормализованное значение ресурса не бывает больше 100:
        // если больше — атрибут значит что-то другое.
        .filter(|attribute| attribute.value <= 100)
        .map(|attribute| attribute.value);

    let temperature_c = find(attributes, ids::TEMPERATURE).and_then(|attribute| {
        // В сыром значении температура лежит в младшем байте, в старших
        // у многих моделей минимум и максимум за всё время.
        let celsius = (attribute.raw & 0xFF) as i16;
        (1..=120).contains(&celsius).then_some(celsius)
    });

    let media_errors = find(attributes, ids::UNCORRECTABLE)
        .map(|a| a.raw as u128)
        .or_else(|| find(attributes, ids::PENDING_SECTORS).map(|a| a.raw as u128));

    let hours = find(attributes, ids::POWER_ON_HOURS).map(|a| a.raw);

    SmartHealth {
        source: Some(SmartSource::AtaSmart),
        critical_warning: None,
        temperature_c,
        available_spare: None,
        available_spare_threshold: None,
        percentage_used: None,
        life_left_percent: life_left,
        data_written: find(attributes, ids::TOTAL_LBA_WRITTEN)
            .map(|a| Bytes(a.raw.saturating_mul(LBA_BYTES)))
            .and_then(|written| plausible_written(written, hours)),
        data_read: find(attributes, ids::TOTAL_LBA_READ)
            .map(|a| Bytes(a.raw.saturating_mul(LBA_BYTES)))
            .and_then(|read| plausible_written(read, hours)),
        power_on_hours: hours,
        power_cycles: find(attributes, ids::POWER_CYCLES).map(|a| a.raw),
        unsafe_shutdowns: None,
        media_errors,
        reallocated_sectors: find(attributes, ids::REALLOCATED_SECTORS).map(|a| a.raw),
        pending_sectors: find(attributes, ids::PENDING_SECTORS).map(|a| a.raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attribute(data: &mut [u8], slot: usize, id: u8, value: u8, raw: u64) {
        let start = ATTRIBUTE_TABLE_OFFSET + slot * ATTRIBUTE_SIZE;
        data[start] = id;
        data[start + 3] = value;
        data[start + 4] = value;
        data[start + 5..start + 11].copy_from_slice(&raw.to_le_bytes()[..6]);
    }

    fn sample_data() -> Vec<u8> {
        let mut data = vec![0u8; SMART_DATA_BYTES];
        data[0] = 0x10; // ревизия таблицы
        attribute(&mut data, 0, ids::REALLOCATED_SECTORS, 100, 0);
        attribute(&mut data, 1, ids::POWER_ON_HOURS, 99, 14_500);
        attribute(&mut data, 2, ids::POWER_CYCLES, 99, 1_200);
        attribute(&mut data, 3, 177, 94, 60); // Samsung, остаток ресурса
        attribute(&mut data, 4, ids::TEMPERATURE, 70, 38);
        attribute(&mut data, 5, ids::TOTAL_LBA_WRITTEN, 99, 40_000_000_000);
        data
    }

    #[test]
    fn attributes_are_parsed_and_empty_slots_skipped() {
        let attributes = parse_attributes(&sample_data());
        assert_eq!(attributes.len(), 6, "пустые строки таблицы попали в разбор");
        assert_eq!(find(&attributes, ids::POWER_ON_HOURS).unwrap().raw, 14_500);
    }

    #[test]
    fn samsung_life_left_comes_from_attribute_177() {
        let attributes = parse_attributes(&sample_data());
        let health = build_health(&attributes, "", "Samsung SSD 860 EVO");
        assert_eq!(health.life_left_percent, Some(94));
        assert_eq!(health.wear_percent(), Some(6));
    }

    #[test]
    fn total_lba_written_converts_to_bytes() {
        let attributes = parse_attributes(&sample_data());
        let health = build_health(&attributes, "", "Samsung SSD 860 EVO");
        // 40 млрд блоков по 512 байт — примерно 20 ТБ.
        assert_eq!(health.data_written, Some(Bytes(40_000_000_000 * 512)));
    }

    #[test]
    fn temperature_comes_from_the_low_byte() {
        let attributes = parse_attributes(&sample_data());
        let health = build_health(&attributes, "", "Samsung SSD 860 EVO");
        assert_eq!(health.temperature_c, Some(38));
    }

    #[test]
    fn nonsense_temperature_is_dropped() {
        let mut data = sample_data();
        attribute(&mut data, 4, ids::TEMPERATURE, 70, 0);
        let health = build_health(&parse_attributes(&data), "", "Samsung SSD 860 EVO");
        assert_eq!(health.temperature_c, None);
    }

    #[test]
    fn unknown_vendor_gets_no_invented_life_left() {
        // Атрибутов ресурса в таблице нет вовсе.
        let mut data = vec![0u8; SMART_DATA_BYTES];
        attribute(&mut data, 0, ids::POWER_ON_HOURS, 99, 100);
        let health = build_health(&parse_attributes(&data), "Apacer", "AS350 512GB");
        assert_eq!(health.life_left_percent, None);
        assert_eq!(health.wear_percent(), None);
    }

    #[test]
    fn a_counter_masquerading_as_life_left_is_rejected() {
        // Нормализованное значение больше 100 означает, что под этим
        // идентификатором у модели лежит что-то другое.
        let mut data = vec![0u8; SMART_DATA_BYTES];
        attribute(&mut data, 0, 231, 253, 0);
        let health = build_health(&parse_attributes(&data), "", "Kingston A400");
        assert_eq!(health.life_left_percent, None);
    }

    #[test]
    fn vendor_changes_which_attribute_wins() {
        let mut data = vec![0u8; SMART_DATA_BYTES];
        attribute(&mut data, 0, 177, 90, 0);
        attribute(&mut data, 1, 233, 70, 0);
        let attributes = parse_attributes(&data);

        assert_eq!(
            build_health(&attributes, "", "Samsung SSD 970").life_left_percent,
            Some(90)
        );
        assert_eq!(
            build_health(&attributes, "", "Intel SSDSC2BB").life_left_percent,
            Some(70)
        );
    }
}

#[cfg(test)]
mod plausibility_tests {
    use super::*;

    #[test]
    fn an_impossible_write_volume_is_dropped() {
        // Ровно случай Apacer AS350: 2571 час наработки и «3.87 МБ»
        // записи. Столько не бывает — единицы атрибута нам неизвестны,
        // и показывать это число нельзя.
        assert_eq!(plausible_written(Bytes(4_058_624), Some(2571)), None);
    }

    #[test]
    fn a_realistic_write_volume_is_kept() {
        let written = Bytes(20 * 1024 * 1024 * 1024 * 1024); // 20 ТБ
        assert_eq!(plausible_written(written, Some(2571)), Some(written));
    }

    #[test]
    fn a_fresh_drive_is_not_judged() {
        // У нового накопителя записи и правда может быть мало —
        // судить о единицах атрибута не по чему.
        let written = Bytes(500 * 1024 * 1024);
        assert_eq!(plausible_written(written, Some(3)), Some(written));
        assert_eq!(plausible_written(written, None), Some(written));
    }
}
