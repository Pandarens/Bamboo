//! Загрузка видеокарты по процессам.
//!
//! Windows считает её сама и раздаёт через счётчики производительности,
//! разбитые по процессам: имя счётчика выглядит как
//! `pid_14980_luid_0x0_0x7ccc_phys_0_eng_0_engtype_3d`. Оттуда и берём —
//! это те же числа, что показывает диспетчер задач на вкладке
//! «Производительность».
//!
//! Про температуру видеокарты сразу и прямо: её здесь нет и не будет.
//! Windows температуру не публикует, а достать её можно только через
//! библиотеку производителя — NVML у NVIDIA, ADL у AMD. Городить это ради
//! числа, которого у половины машин всё равно не окажется, значит обещать
//! то, чего не сможем выполнить. Лучше честно не показывать поле, чем
//! показывать пустое.

use bamboo_core::{Error, Result};
use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

/// Не срезать значение на ста процентах.
///
/// У процесса несколько движков видеокарты, и сумма по ним законно бывает
/// больше сотни. Без этого флага PDH обрезал бы каждое слагаемое, и сумма
/// вышла бы неправдой. Константы в windows-sys нет, поэтому берём значение
/// из заголовка Pdh.h.
const PDH_FMT_NOCAP100: u32 = 0x8000;

/// Счётчик загрузки видеокарты по всем процессам сразу.
///
/// Имя английское намеренно: на локализованной Windows счётчики
/// переименованы, и `PdhAddEnglishCounterW` — единственный способ обратиться
/// к ним одинаково везде.
const GPU_COUNTER: &str = r"\GPU Engine(*)\Utilization Percentage";

/// Загрузка видеокарты одним процессом.
#[derive(Clone, Copy, Debug)]
pub struct GpuLoad {
    pub pid: u32,
    /// Проценты. Складываются по всем движкам процесса: отрисовка, кодек,
    /// копирование — человеку интересна сумма, а не разбивка по движкам.
    pub percent: f64,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Открытый счётчик загрузки видеокарты.
///
/// Держим открытым, а не открываем на каждый замер. Счётчик считает долю
/// времени **между** двумя опросами, поэтому разовый замер обязан был бы
/// подождать между ними секунду — секунда блокировки на каждый тик. Открытый
/// счётчик такого не требует: промежутком служит время от прошлого замера.
pub struct GpuCounter {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    /// Был ли уже первый опрос. До него значений нет вовсе: промежутка,
    /// за который считать долю, ещё не существует.
    primed: bool,
}

impl Drop for GpuCounter {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

impl GpuCounter {
    /// Открывает счётчик.
    ///
    /// Ошибка — не поломка: набор «GPU Engine» появился в Windows 10
    /// и требует драйвера WDDM 2.0, а на машине без него счётчиков просто
    /// нет. Тогда Bamboo так и скажет, вместо того чтобы рисовать пустой
    /// график и выдавать его за нулевую нагрузку.
    pub fn open() -> Result<GpuCounter> {
        let mut query: PDH_HQUERY = core::ptr::null_mut();
        let status = unsafe { PdhOpenQueryW(core::ptr::null(), 0, &mut query) };
        if status != 0 {
            return Err(Error::Win32 {
                call: "PdhOpenQuery",
                code: status as u32,
            });
        }
        let mut open = GpuCounter {
            query,
            counter: core::ptr::null_mut(),
            primed: false,
        };

        let status = unsafe {
            PdhAddEnglishCounterW(open.query, wide(GPU_COUNTER).as_ptr(), 0, &mut open.counter)
        };
        if status != 0 {
            return Err(Error::Unsupported(
                "счётчики загрузки видеокарты в этой системе недоступны",
            ));
        }
        Ok(open)
    }

    /// Снимает загрузку по процессам.
    ///
    /// Первый вызов возвращает пустой список: промежутка, за который
    /// считать долю, ещё нет. Это не ошибка, а устройство счётчиков.
    pub fn read(&mut self) -> Result<Vec<GpuLoad>> {
        let status = unsafe { PdhCollectQueryData(self.query) };
        if status != 0 {
            return Err(Error::Win32 {
                call: "PdhCollectQueryData",
                code: status as u32,
            });
        }
        if !self.primed {
            self.primed = true;
            return Ok(Vec::new());
        }
        Ok(fold_by_process(read_items(self.counter)?))
    }
}

/// Разовый замер: открывает счётчик, ждёт промежуток и читает.
///
/// Годится для проверки и для одиночного вопроса. Для записи наблюдения
/// нужен `GpuCounter`: секунда блокировки на каждый тик недопустима.
pub fn load_by_process() -> Result<Vec<GpuLoad>> {
    let mut counter = GpuCounter::open()?;
    counter.read()?;
    std::thread::sleep(std::time::Duration::from_millis(1000));
    counter.read()
}

/// Забирает значения счётчика по всем его экземплярам.
fn read_items(counter: PDH_HCOUNTER) -> Result<Vec<(String, f64)>> {
    let mut size: u32 = 0;
    let mut count: u32 = 0;

    // Первый вызов только сообщает нужный размер буфера.
    unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE | PDH_FMT_NOCAP100,
            &mut size,
            &mut count,
            core::ptr::null_mut(),
        )
    };
    if size == 0 {
        return Ok(Vec::new());
    }

    // Буфер выравниваем по структуре, а не по байту: PDH кладёт в него
    // структуры, а следом за ними — строки имён.
    let slots = size as usize / core::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>() + 1;
    let mut buffer: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> = Vec::with_capacity(slots);

    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE | PDH_FMT_NOCAP100,
            &mut size,
            &mut count,
            buffer.as_mut_ptr(),
        )
    };
    if status != 0 {
        return Err(Error::Win32 {
            call: "PdhGetFormattedCounterArray",
            code: status as u32,
        });
    }

    let mut out = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        let item = unsafe { &*buffer.as_ptr().add(index) };
        if item.szName.is_null() {
            continue;
        }
        let name = unsafe { wide_to_string(item.szName) };
        let value = unsafe { item.FmtValue.Anonymous.doubleValue };
        out.push((name, value));
    }
    Ok(out)
}

unsafe fn wide_to_string(pointer: *const u16) -> String {
    let mut length = 0usize;
    while unsafe { *pointer.add(length) } != 0 {
        length += 1;
        // Имена счётчиков коротки; предел защищает от чтения за краем,
        // если строка вдруг окажется незавершённой.
        if length > 512 {
            break;
        }
    }
    String::from_utf16_lossy(unsafe { core::slice::from_raw_parts(pointer, length) })
}

/// Складывает загрузку по процессам.
///
/// У процесса несколько движков — отрисовка, кодек, копирование, — и каждый
/// приходит своим счётчиком. Человеку нужна сумма: он спрашивает «сколько
/// игра занимает видеокарту», а не «сколько занимает движок 3D».
fn fold_by_process(items: Vec<(String, f64)>) -> Vec<GpuLoad> {
    use std::collections::HashMap;

    let mut totals: HashMap<u32, f64> = HashMap::new();
    for (name, value) in items {
        let Some(pid) = pid_from_instance(&name) else {
            continue;
        };
        *totals.entry(pid).or_default() += value;
    }

    let mut out: Vec<GpuLoad> = totals
        .into_iter()
        .filter(|(_, percent)| *percent > 0.0)
        .map(|(pid, percent)| GpuLoad { pid, percent })
        .collect();
    out.sort_by(|a, b| b.percent.total_cmp(&a.percent));
    out
}

/// Достаёт номер процесса из имени экземпляра счётчика.
///
/// Имя выглядит как `pid_14980_luid_0x00000000_0x00007ccc_phys_0_eng_0_engtype_3d`.
fn pid_from_instance(name: &str) -> Option<u32> {
    name.strip_prefix("pid_")?
        .split('_')
        .next()?
        .parse::<u32>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pid_is_read_out_of_the_instance_name() {
        assert_eq!(
            pid_from_instance("pid_14980_luid_0x00000000_0x00007ccc_phys_0_eng_0_engtype_3d"),
            Some(14980)
        );
    }

    #[test]
    fn a_strange_instance_name_is_skipped() {
        assert_eq!(pid_from_instance("engtype_3d"), None);
        assert_eq!(pid_from_instance("pid_"), None);
        assert_eq!(pid_from_instance("pid_нечисло_luid_0"), None);
        assert_eq!(pid_from_instance(""), None);
    }

    #[test]
    fn engines_of_one_process_are_added_up() {
        // Игра занимает и отрисовку, и кодек. Человек спрашивает «сколько
        // она занимает видеокарту», а не «сколько занимает движок 3D».
        let items = vec![
            (
                "pid_100_luid_0x0_0x1_phys_0_eng_0_engtype_3d".to_string(),
                40.0,
            ),
            (
                "pid_100_luid_0x0_0x1_phys_0_eng_1_engtype_videoencode".to_string(),
                15.0,
            ),
            (
                "pid_200_luid_0x0_0x1_phys_0_eng_0_engtype_3d".to_string(),
                5.0,
            ),
        ];

        let folded = fold_by_process(items);
        assert_eq!(folded.len(), 2);
        // Порядок — от самого нагруженного: виновник должен быть первым.
        assert_eq!(folded[0].pid, 100);
        assert!((folded[0].percent - 55.0).abs() < 0.01, "{folded:?}");
        assert_eq!(folded[1].pid, 200);
    }

    #[test]
    fn idle_processes_are_not_listed() {
        // Счётчики приходят на все процессы, у которых вообще есть
        // видеоконтекст, — а это половина системы с нулём.
        let items = vec![(
            "pid_100_luid_0x0_0x1_phys_0_eng_0_engtype_3d".to_string(),
            0.0,
        )];
        assert!(fold_by_process(items).is_empty());
    }

    #[test]
    fn the_first_read_of_an_open_counter_is_empty_not_wrong() {
        // До первого опроса промежутка нет, и значений тоже. Выдать за них
        // нули значило бы соврать про простаивающую видеокарту.
        let Ok(mut counter) = GpuCounter::open() else {
            return; // Счётчиков нет — проверять нечего.
        };
        assert!(counter.read().unwrap().is_empty());
    }

    #[test]
    fn an_open_counter_gives_values_on_the_second_read() {
        let Ok(mut counter) = GpuCounter::open() else {
            return;
        };
        counter.read().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(600));

        for load in counter.read().unwrap() {
            assert!(load.pid > 0);
            assert!(load.percent > 0.0);
        }
    }

    #[test]
    fn the_real_counters_are_readable() {
        // Живая проверка: набор «GPU Engine» есть не везде, и узнать это
        // надо здесь, а не у пользователя.
        match load_by_process() {
            Ok(loads) => {
                for load in &loads {
                    assert!(load.pid > 0);
                    assert!(load.percent > 0.0);
                }
            }
            Err(error) => {
                // Отсутствие счётчиков — не провал теста: на машине без
                // WDDM 2.0 их правда нет.
                let text = error.to_string();
                assert!(text.contains("недоступны"), "неожиданная ошибка: {text}");
            }
        }
    }
}
