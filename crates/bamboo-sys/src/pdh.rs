//! Счётчик производительности с одним значением.
//!
//! Общая обвязка для счётчиков вида «одно число за промежуток»: чтение
//! из подкачки, доля номинальной частоты процессора. Появилась, когда
//! таких счётчиков стало два и обвязка начала копироваться дословно.
//!
//! Счётчик по нескольким экземплярам сразу — другое дело, и живёт
//! отдельно в `gpu`: там нужен разбор массива по процессам.
//!
//! Имена счётчиков берутся английские. На локализованной Windows счётчики
//! переименованы, и `PdhAddEnglishCounterW` — единственный способ
//! обратиться к ним одинаково везде.

use bamboo_core::{Error, Result};
use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Открытый счётчик.
///
/// Держим открытым, а не открываем на каждый замер. Значение считается
/// **между** двумя опросами, поэтому разовый замер обязан был бы подождать
/// между ними секунду — секунда блокировки на каждый тик недопустима.
/// Открытый счётчик такого не требует: промежутком служит время
/// от прошлого замера.
pub struct Counter {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    /// Был ли первый опрос. До него значения нет вовсе: промежутка,
    /// за который считать, ещё не существует.
    primed: bool,
}

impl Drop for Counter {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

impl Counter {
    /// Открывает счётчик по английскому имени.
    ///
    /// Ошибка — не всегда поломка: часть наборов счётчиков есть не везде.
    /// Поэтому `missing` говорит, какого именно счётчика не нашлось,
    /// и вызывающий решает, обойтись без него или нет.
    pub fn open(path: &str, missing: &'static str) -> Result<Counter> {
        let mut query: PDH_HQUERY = core::ptr::null_mut();
        let status = unsafe { PdhOpenQueryW(core::ptr::null(), 0, &mut query) };
        if status != 0 {
            return Err(Error::Win32 {
                call: "PdhOpenQuery",
                code: status as u32,
            });
        }
        let mut open = Counter {
            query,
            counter: core::ptr::null_mut(),
            primed: false,
        };

        let status =
            unsafe { PdhAddEnglishCounterW(open.query, wide(path).as_ptr(), 0, &mut open.counter) };
        if status != 0 {
            return Err(Error::Unsupported(missing));
        }
        Ok(open)
    }

    /// Снимает значение.
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

/// Несколько счётчиков в одном запросе.
///
/// Для мгновенных величин вроде размеров пулов памяти: их значение готово
/// после первого же опроса, и открывать на каждую свой запрос незачем —
/// один сбор данных на все.
pub struct CounterSet {
    query: PDH_HQUERY,
    /// По счётчику на путь. `None` — такого счётчика в системе нет.
    counters: Vec<Option<PDH_HCOUNTER>>,
}

impl Drop for CounterSet {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

impl CounterSet {
    /// Открывает набор. Отсутствующий счётчик — не ошибка: его место
    /// в ответе будет пустым, а остальные прочитаются.
    pub fn open(paths: &[&str]) -> Result<CounterSet> {
        let mut query: PDH_HQUERY = core::ptr::null_mut();
        let status = unsafe { PdhOpenQueryW(core::ptr::null(), 0, &mut query) };
        if status != 0 {
            return Err(Error::Win32 {
                call: "PdhOpenQuery",
                code: status as u32,
            });
        }
        let mut set = CounterSet {
            query,
            counters: Vec::with_capacity(paths.len()),
        };
        for path in paths {
            let mut counter: PDH_HCOUNTER = core::ptr::null_mut();
            let status =
                unsafe { PdhAddEnglishCounterW(set.query, wide(path).as_ptr(), 0, &mut counter) };
            set.counters.push((status == 0).then_some(counter));
        }
        Ok(set)
    }

    /// Значения по порядку путей. `None` — счётчика нет или он не ответил.
    pub fn read(&mut self) -> Vec<Option<f64>> {
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return vec![None; self.counters.len()];
        }
        self.counters
            .iter()
            .map(|counter| {
                let counter = (*counter)?;
                let mut value: PDH_FMT_COUNTERVALUE = unsafe { core::mem::zeroed() };
                let status = unsafe {
                    PdhGetFormattedCounterValue(
                        counter,
                        PDH_FMT_DOUBLE,
                        core::ptr::null_mut(),
                        &mut value,
                    )
                };
                (status == 0).then_some(unsafe { value.Anonymous.doubleValue })
            })
            .collect()
    }
}
