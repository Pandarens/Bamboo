//! Температуры, которые Windows отдаёт без сторонних драйверов.
//!
//! Честно доступно немногое, и это стоит сказать прямо, а не рисовать
//! выдуманные градусы.
//!
//! Термозоны ACPI — счётчики Windows. Они есть в основном на ноутбуках;
//! на настольной машине с i5-12400, где это писалось, их нет вовсе,
//! а запрос через WMI отвечает «Не поддерживается».
//!
//! Датчики ядер процессора и видеокарты читаются только драйвером ядра
//! вроде WinRing0, на котором стоят популярные мониторы температуры. Это
//! известная уязвимость: драйвер даёт любой программе доступ к памяти ядра,
//! и Microsoft вносит его в список блокируемых. Bamboo его не ставит —
//! показать градусы ценой дыры в системе значило бы сделать хуже тому,
//! кого берёшься оберегать.
//!
//! Диски отдают температуру в SMART — её и берём.

use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

/// Один замер температуры.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// Что мерили: имя диска или термозоны.
    pub name: String,
    pub celsius: f64,
}

/// Температура в десятых долях кельвина — так её отдаёт счётчик.
const ZONES: &str = r"\Thermal Zone Information(*)\High Precision Temperature";

/// Правдоподобный диапазон. За его краями — сломанный датчик или
/// перепутанные единицы, и показывать такое число хуже, чем промолчать.
const PLAUSIBLE: core::ops::RangeInclusive<f64> = 1.0..=125.0;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Термозоны ACPI. Пусто — на этой машине их нет; это обычный исход,
/// а не ошибка.
pub fn thermal_zones() -> Vec<Reading> {
    let mut query: PDH_HQUERY = core::ptr::null_mut();
    if unsafe { PdhOpenQueryW(core::ptr::null(), 0, &mut query) } != 0 {
        return Vec::new();
    }
    let mut counter: PDH_HCOUNTER = core::ptr::null_mut();
    let added = unsafe { PdhAddEnglishCounterW(query, wide(ZONES).as_ptr(), 0, &mut counter) };
    let readings = if added == 0 && unsafe { PdhCollectQueryData(query) } == 0 {
        read_array(counter)
            .into_iter()
            .map(|(name, tenths_of_kelvin)| Reading {
                name,
                celsius: tenths_of_kelvin / 10.0 - 273.15,
            })
            .filter(|reading| PLAUSIBLE.contains(&reading.celsius))
            .collect()
    } else {
        Vec::new()
    };
    unsafe { PdhCloseQuery(query) };
    readings
}

/// Температуры дисков из SMART.
///
/// Чтение открывает устройство и обращается к драйверу — вызывать его
/// на каждом тике нельзя; вызывающий делает это раз в несколько минут.
pub fn disk_temperatures() -> Vec<Reading> {
    crate::enumerate_drives()
        .iter()
        .filter_map(|info| {
            let celsius = f64::from(crate::read_smart(info).ok()?.temperature_c?);
            PLAUSIBLE.contains(&celsius).then(|| Reading {
                name: info.display_name(),
                celsius,
            })
        })
        .collect()
}

/// Значения счётчика по всем экземплярам.
fn read_array(counter: PDH_HCOUNTER) -> Vec<(String, f64)> {
    let mut size: u32 = 0;
    let mut count: u32 = 0;
    // Первый вызов только сообщает нужный размер буфера.
    unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut size,
            &mut count,
            core::ptr::null_mut(),
        )
    };
    if size == 0 {
        return Vec::new();
    }
    // Буфер выравниваем по структуре: PDH кладёт в него структуры,
    // а следом за ними — строки имён.
    let slots = size as usize / core::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>() + 1;
    let mut buffer: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> = Vec::with_capacity(slots);
    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut size,
            &mut count,
            buffer.as_mut_ptr(),
        )
    };
    if status != 0 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        let item = unsafe { &*buffer.as_ptr().add(index) };
        if item.szName.is_null() {
            continue;
        }
        let mut length = 0usize;
        // Имена счётчиков коротки; предел защищает от чтения за краем.
        while length < 512 && unsafe { *item.szName.add(length) } != 0 {
            length += 1;
        }
        let name =
            String::from_utf16_lossy(unsafe { core::slice::from_raw_parts(item.szName, length) });
        out.push((name, unsafe { item.FmtValue.Anonymous.doubleValue }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thermal_zones_are_believable_or_absent() {
        // На машине разработки термозон нет — пустой ответ и есть правда.
        // Где они есть, числа обязаны быть правдоподобными.
        for zone in thermal_zones() {
            assert!(
                PLAUSIBLE.contains(&zone.celsius),
                "{}: {}",
                zone.name,
                zone.celsius
            );
        }
    }

    #[test]
    fn disk_temperatures_are_believable_or_absent() {
        // Без прав администратора SMART закрыт — тогда список пуст,
        // и это не поломка.
        for disk in disk_temperatures() {
            assert!(!disk.name.is_empty());
            assert!(
                PLAUSIBLE.contains(&disk.celsius),
                "{}: {}",
                disk.name,
                disk.celsius
            );
        }
    }
}
