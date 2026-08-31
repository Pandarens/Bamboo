//! Насколько сильно система читает из подкачки прямо сейчас.
//!
//! Это тот самый счётчик, без которого Bamboo не замечал подвисаний
//! на машине, где память кончается. Прежняя проверка смотрела на долю
//! занятой памяти и требовала 92%. Замер на живой машине показал, почему
//! этого мало: в момент, когда из подкачки читалось 6519 страниц
//! за секунду — 25 МБ случайного чтения, — занято было 87%. Порог
//! не срабатывал, а человек в это время смотрел, как набранный текст
//! появляется с задержкой.
//!
//! Доля занятой памяти вообще плохой признак: 95% занятой памяти без
//! чтения из подкачки — это здоровая система, которая просто использует
//! то, что есть. А 85% с постоянным чтением — это уже толкотня. Признак
//! подвисания — само чтение, и мерить надо его.
//!
//! Считаются жёсткие промахи: страницы, которых не оказалось в памяти
//! и которые пришлось поднимать с диска. Мягкие промахи, где страница
//! нашлась в списке ожидания, сюда не входят — они почти бесплатны
//! и к подвисаниям отношения не имеют.

use bamboo_core::{Error, Result};
use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

/// Имя английское намеренно — тот же довод, что у счётчиков видеокарты:
/// на локализованной Windows счётчики переименованы, и обратиться к ним
/// одинаково везде даёт только `PdhAddEnglishCounterW`.
const PAGING_COUNTER: &str = r"\Memory\Pages Input/sec";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Открытый счётчик чтения из подкачки.
///
/// Держим открытым по той же причине, что и счётчик видеокарты: значение
/// считается **между** двумя опросами, и разовый замер обязан был бы ждать
/// секунду между ними. Секунда блокировки на каждый тик недопустима.
pub struct PagingCounter {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    /// Был ли первый опрос. До него значения нет: промежутка, за который
    /// считать скорость, ещё не существует.
    primed: bool,
}

impl Drop for PagingCounter {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

impl PagingCounter {
    pub fn open() -> Result<PagingCounter> {
        let mut query: PDH_HQUERY = core::ptr::null_mut();
        let status = unsafe { PdhOpenQueryW(core::ptr::null(), 0, &mut query) };
        if status != 0 {
            return Err(Error::Win32 {
                call: "PdhOpenQuery",
                code: status as u32,
            });
        }
        let mut open = PagingCounter {
            query,
            counter: core::ptr::null_mut(),
            primed: false,
        };

        let status = unsafe {
            PdhAddEnglishCounterW(
                open.query,
                wide(PAGING_COUNTER).as_ptr(),
                0,
                &mut open.counter,
            )
        };
        if status != 0 {
            return Err(Error::Unsupported(
                "счётчик чтения из подкачки в этой системе недоступен",
            ));
        }
        Ok(open)
    }

    /// Страниц в секунду, поднятых с диска.
    ///
    /// Первый вызов возвращает ноль: промежутка ещё нет. Это не ошибка,
    /// а устройство счётчиков, и ноль здесь честнее выдуманного числа.
    pub fn read(&mut self) -> Result<f64> {
        let status = unsafe { PdhCollectQueryData(self.query) };
        if status != 0 {
            return Err(Error::Win32 {
                call: "PdhCollectQueryData",
                code: status as u32,
            });
        }
        if !self.primed {
            self.primed = true;
            return Ok(0.0);
        }

        let mut value: PDH_FMT_COUNTERVALUE = unsafe { core::mem::zeroed() };
        let status = unsafe {
            PdhGetFormattedCounterValue(
                self.counter,
                PDH_FMT_DOUBLE,
                core::ptr::null_mut(),
                &mut value,
            )
        };
        if status != 0 {
            return Err(Error::Win32 {
                call: "PdhGetFormattedCounterValue",
                code: status as u32,
            });
        }
        // Отрицательного здесь быть не может, но счётчики иногда отдают
        // мусор на первом промежутке после пробуждения из сна.
        Ok(unsafe { value.Anonymous.doubleValue }.max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counter_opens_and_reads_a_sane_number() {
        // Счётчик «Memory» есть в любой Windows — в отличие от «GPU Engine»,
        // которого может не быть. Поэтому здесь не пропускаем тест
        // при ошибке открытия, а честно падаем.
        let mut counter = PagingCounter::open().expect("счётчик памяти обязан открываться");

        // Первый замер — ноль по устройству счётчиков.
        assert_eq!(counter.read().unwrap(), 0.0);

        std::thread::sleep(std::time::Duration::from_millis(300));
        let rate = counter.read().unwrap();
        assert!(
            rate >= 0.0,
            "скорость чтения из подкачки не бывает меньше нуля"
        );
        // Верхняя граница нарочно нелепая: смысл не в том, чтобы угадать
        // нагрузку, а в том, чтобы поймать чтение мусора из объединения.
        assert!(rate < 10_000_000.0, "счётчик отдал бессмыслицу: {rate}");
    }
}
