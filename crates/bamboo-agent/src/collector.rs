//! Фоновый сбор метрик для виджета.
//!
//! Отдельный поток, без async-рантайма (ТЗ, раздел 4.1). Коллектор ничего
//! не знает про интерфейс и отдаёт готовые снимки в канал.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use bamboo_collect::Collector;
use bamboo_core::Bytes;

/// Сколько точек держим для спарклайна.
pub const SPARK_POINTS: usize = 48;

/// Одна строка в топе потребителей.
///
/// Числа держим сырыми, а не отформатированными: по ним сортирует главное
/// окно. Строку «12.3%» пришлось бы разбирать обратно, и сортировка по
/// памяти сломалась бы на первом же «1.2 ГБ» против «900 МБ».
#[derive(Clone, Debug)]
pub struct ProcessLine {
    pub name: String,
    pub pid: u32,
    pub threads: u32,
    pub cpu_percent: f32,
    pub memory: Bytes,
    /// Пояснение под именем: чем этот процесс примечателен.
    pub badge: String,
    /// Растёт ли память процесса и как быстро. `None` — не растёт или
    /// наблюдений пока мало; это нормальный и самый частый случай.
    pub memory_growth: Option<bamboo_analyze::MemoryTrend>,
    /// Есть ли у процесса окно, которое перестало разбирать сообщения.
    pub hung: bool,
    /// Сколько процесс читает и пишет на диск, байт в секунду.
    /// Именно скорость, а не сумма за интервал: интервал опроса плавает
    /// от секунды до минуты, и сырые дельты нельзя было бы сравнивать
    /// между собой.
    pub read_per_second: u64,
    pub write_per_second: u64,
}

/// Снимок для интерфейса.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub cpu_busy: f64,
    pub driver_ratio: f64,
    pub memory_used: Bytes,
    pub memory_total: Bytes,
    pub process_count: usize,
    pub top: Vec<ProcessLine>,
    /// История использования памяти, доли от максимума за окно, 0..1.
    /// Память меняется в узком диапазоне, поэтому её растягиваем по окну —
    /// иначе график был бы прямой на восьмидесяти процентах.
    pub spark: Vec<f32>,
    /// История загрузки процессора, доли от целой машины, 0..1.
    /// Здесь шкала абсолютная: сто процентов должны выглядеть как сто.
    pub spark_cpu: Vec<f32>,
    /// Собственное потребление Bamboo.
    pub own_memory: Bytes,
    pub cadence: String,
    /// Чем заняты накопители: имя, занятость и скорости.
    pub disks: Vec<DiskLine>,
    /// Файлы подкачки и их занятость.
    pub pagefiles: Vec<PagefileLine>,
    /// Объяснение, почему диск загружен, если он загружен. `None` — диск
    /// свободен либо виновник не назван честно.
    pub disk_pressure: Option<String>,
    /// Разделы: сколько места и сколько осталось.
    pub volumes: Vec<VolumeLine>,
    /// Сколько человек не трогал мышь и клавиатуру. От этого зависит,
    /// можно ли называть чужую работу фоновой.
    pub user_idle_ms: u64,
    /// Объяснение дисковой активности ядра, если оно грузит накопитель.
    pub system_io: Option<String>,
}

/// Раздел в снимке.
#[derive(Clone, Debug)]
pub struct VolumeLine {
    /// «C: Система (NTFS)».
    pub title: String,
    pub free: Bytes,
    pub total: Bytes,
    pub usage: f64,
    /// Места осталось так мало, что страдает скорость записи.
    pub cramped: bool,
}

/// Накопитель в снимке.
#[derive(Clone, Debug)]
pub struct DiskLine {
    pub name: String,
    /// Доля времени, когда накопитель был занят, 0..1.
    pub busy: f64,
    pub read_per_second: Bytes,
    pub write_per_second: Bytes,
    pub queue_depth: u32,
    /// Занят настолько, что это уже мешает.
    pub saturated: bool,
}

/// Файл подкачки в снимке.
#[derive(Clone, Debug)]
pub struct PagefileLine {
    /// «диск C» либо полный путь, если буква не разобралась.
    pub where_: String,
    pub in_use: Bytes,
    pub total: Bytes,
    pub peak: Bytes,
    pub usage: f64,
}

/// Флаг, которым интерфейс сообщает коллектору, открыт ли виджет.
/// От этого зависит частота опроса (ТЗ, раздел 6.2).
pub type WidgetVisible = Arc<AtomicBool>;

/// Запускает поток сбора. Возвращает конец канала и флаг видимости.
pub fn spawn() -> (Receiver<Snapshot>, WidgetVisible) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let visible: WidgetVisible = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&visible);

    std::thread::Builder::new()
        .name("bamboo-collect".into())
        .spawn(move || run(sender, flag))
        .expect("не удалось запустить поток сбора");

    (receiver, visible)
}

/// Как часто пересчитывать тренды роста памяти.
///
/// Регрессия по суточному ряду каждого из трёхсот процессов — работа не на
/// каждый тик: на живой машине это съело почти целое ядро, при бюджете
/// в 1% из раздела 4 ТЗ. А смысла в такой частоте нет: ряд L1 пополняется
/// раз в минуту, чаще этого результат просто не меняется.
const GROWTH_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// Как часто перечитывать список файлов подкачки.
///
/// Их размер и состав меняются медленно, а запрос к ядру не бесплатный.
/// Занятость подкачки скачками не ходит: раз в полминуты более чем хватает.
const PAGEFILES_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

fn run(sender: Sender<Snapshot>, visible: WidgetVisible) {
    let mut collector = Collector::new();
    let mut memory_history: Vec<u64> = Vec::with_capacity(SPARK_POINTS);
    let mut cpu_history: Vec<f32> = Vec::with_capacity(SPARK_POINTS);
    // Кэш трендов по процессам между пересчётами.
    let mut growth: std::collections::HashMap<u32, bamboo_analyze::MemoryTrend> =
        std::collections::HashMap::new();
    let mut growth_at: Option<std::time::Instant> = None;
    // Счётчики накопителей с прошлого тика: активность считается по разнице.
    let mut disk_before: Vec<bamboo_sys::storage::DiskCounters> = Vec::new();
    let mut pagefiles: Vec<PagefileLine> = Vec::new();
    let mut pagefiles_at: Option<std::time::Instant> = None;
    let mut volumes: Vec<VolumeLine> = Vec::new();

    loop {
        collector.set_widget_open(visible.load(Ordering::Relaxed));

        let tick = match collector.tick() {
            Ok(tick) => tick,
            Err(_) => {
                // Разовый сбой опроса не повод убивать поток: следующий тик
                // почти наверняка пройдёт.
                std::thread::sleep(collector.next_interval());
                continue;
            }
        };

        let used = tick.system.memory.physical_used();
        memory_history.push(used.as_u64());
        if memory_history.len() > SPARK_POINTS {
            memory_history.remove(0);
        }

        cpu_history.push(tick.cpu_busy() as f32);
        if cpu_history.len() > SPARK_POINTS {
            cpu_history.remove(0);
        }

        // Тренды роста пересчитываем редко: это самая дорогая часть тика.
        let recompute = growth_at.is_none_or(|at| at.elapsed() >= GROWTH_EVERY);
        if recompute {
            growth.clear();
            for process in collector.table().iter() {
                if let Some(trend) = bamboo_analyze::memory_trend(
                    &process.level1.private_series(),
                    process.observed_ms(),
                ) {
                    growth.insert(process.pid(), trend);
                }
            }
            growth_at = Some(std::time::Instant::now());
        }

        // Зависшие окна — один обход на весь тик, а не запрос про каждый
        // процесс: EnumWindows всё равно обходит все окна системы.
        let hung = bamboo_sys::hung_process_ids();

        // Накопители: счётчики накопительные, поэтому активность считаем
        // по разнице с прошлым тиком, а не отдельной парой замеров — это
        // избавляет от лишней паузы внутри тика.
        let (disks, disk_now) = read_disks(&disk_before);
        disk_before = disk_now;

        // Разделы и подкачка меняются медленно — читаем их вместе и редко.
        if pagefiles_at.is_none_or(|at| at.elapsed() >= PAGEFILES_EVERY) {
            pagefiles = read_pagefiles();
            volumes = read_volumes();
            pagefiles_at = Some(std::time::Instant::now());
        }

        // Берём все процессы, а не только топ по процессору: главное окно
        // сортирует их само, и по памяти в том числе. Обрежь мы список по
        // CPU, самый прожорливый по памяти процесс мог бы в него не попасть
        // именно потому, что процессор он не грузит.
        let mut top: Vec<ProcessLine> = collector
            .table()
            .iter()
            .map(|process| ProcessLine {
                name: process.image_name.to_string(),
                pid: process.pid(),
                threads: process.threads,
                cpu_percent: process.cpu_share * 100.0,
                memory: Bytes::from_kib(process.last_point().private_kib as u64),
                badge: badge_for(process),
                memory_growth: growth.get(&process.pid()).copied(),
                hung: hung.contains(&process.pid()),
                read_per_second: per_second(process.last_point().read_kib, tick.interval_ms),
                write_per_second: per_second(process.last_point().write_kib, tick.interval_ms),
            })
            .collect();

        // Порядок по умолчанию — по процессору: виджет показывает первые
        // строки и ждёт именно топ потребителей.
        top.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));

        // Считаем до сборки снимка: дальше список процессов уедет в него.
        let disk_pressure = explain_disk_pressure(&disks, &top);
        let system_io = explain_system(&top, used, tick.system.memory.physical_total);

        let snapshot = Snapshot {
            cpu_busy: tick.cpu_busy(),
            driver_ratio: tick.driver_time(),
            memory_used: used,
            memory_total: tick.system.memory.physical_total,
            process_count: tick.process_count,
            top,
            spark: normalise(&memory_history),
            spark_cpu: cpu_history.iter().map(|v| v.clamp(0.0, 1.0)).collect(),
            own_memory: bamboo_sys::own_memory()
                .map(|m| m.working_set)
                .unwrap_or(Bytes::ZERO),
            cadence: cadence_name(tick.cadence),
            disk_pressure,
            disks,
            pagefiles: pagefiles.clone(),
            volumes: volumes.clone(),
            user_idle_ms: bamboo_sys::idle_ms().unwrap_or(0),
            system_io,
        };

        // Интерфейс закрылся — поток должен закончиться вместе с ним.
        if sender.send(snapshot).is_err() {
            return;
        }

        std::thread::sleep(collector.next_interval());
    }
}

fn cadence_name(cadence: bamboo_collect::Cadence) -> String {
    use bamboo_collect::Cadence;
    match cadence {
        Cadence::WidgetOpen => "виджет открыт, опрос раз в секунду",
        Cadence::Active => "опрос раз в 5 секунд",
        Cadence::UserIdle => "вас нет за компьютером, опрос раз в 15 секунд",
        Cadence::Battery => "питание от батареи, опрос раз в 15 секунд",
        Cadence::FullScreen => "полноэкранный режим, опрос раз в 30 секунд",
        Cadence::BatteryLow => "батарея на исходе, опрос раз в минуту",
    }
    .to_string()
}

/// Короткое пояснение, чем процесс примечателен.
///
/// Пока наблюдений из `bamboo-analyze` в агенте нет, поэтому пишем только
/// то, что видно прямо сейчас, без домыслов.
fn badge_for(process: &bamboo_collect::TrackedProcess) -> String {
    let hours = process.observed_ms() / 3_600_000;
    let mut parts: Vec<String> = Vec::new();

    if hours >= 1 {
        parts.push(format!("под наблюдением {hours} ч"));
    }
    if process.last_point().write_kib > 1024 {
        parts.push("активно пишет на диск".to_string());
    }
    parts.join(", ")
}

/// Приводит ряд к долям от максимума. Пустой или плоский ряд даёт нули —
/// рисовать всплеск там, где его нет, нельзя.
fn normalise(values: &[u64]) -> Vec<f32> {
    let max = values.iter().copied().max().unwrap_or(0);
    let min = values.iter().copied().min().unwrap_or(0);
    if max == 0 || max == min {
        return vec![0.5; values.len()];
    }
    values
        .iter()
        .map(|value| (value - min) as f32 / (max - min) as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spark_scales_to_the_window() {
        let spark = normalise(&[100, 200, 300]);
        assert_eq!(spark, vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn a_flat_series_is_drawn_flat() {
        // Ряд без изменений не должен превращаться в пилу из-за
        // растягивания на весь диапазон.
        assert_eq!(normalise(&[500, 500, 500]), vec![0.5, 0.5, 0.5]);
        assert!(normalise(&[]).is_empty());
    }
}

/// Переводит дельту за интервал в скорость, байт в секунду.
///
/// Интервал опроса плавает от секунды до минуты (ТЗ, раздел 6.2), поэтому
/// сырые дельты между собой несравнимы: одна и та же цифра за секунду
/// и за минуту означает разную нагрузку.
fn per_second(kib: u32, interval_ms: u64) -> u64 {
    if interval_ms == 0 {
        return 0;
    }
    (kib as u64 * 1024).saturating_mul(1000) / interval_ms
}

#[cfg(test)]
mod speed_tests {
    use super::*;

    #[test]
    fn a_delta_becomes_bytes_per_second() {
        // 1024 КиБ за две секунды — это половина мегабайта в секунду.
        assert_eq!(per_second(1024, 2000), 512 * 1024);
    }

    #[test]
    fn the_first_tick_has_no_interval_and_no_speed() {
        // На первом тике интервала нет: показать скорость неоткуда,
        // и выдумывать её нельзя.
        assert_eq!(per_second(5000, 0), 0);
    }
}

/// Снимает счётчики накопителей и считает активность относительно прошлого
/// тика. Возвращает готовые строки и свежие счётчики для следующего раза.
fn read_disks(
    before: &[bamboo_sys::storage::DiskCounters],
) -> (Vec<DiskLine>, Vec<bamboo_sys::storage::DiskCounters>) {
    use bamboo_sys::storage::{activity_between, read_counters, Drive};

    let mut lines = Vec::new();
    let mut counters = Vec::new();

    for info in bamboo_sys::enumerate_drives() {
        let Ok(device) = Drive::open(info.index) else {
            continue;
        };
        let Ok(now) = read_counters(&device) else {
            continue;
        };

        if let Some(previous) = before.iter().find(|c| c.index == now.index) {
            if let Some(activity) = activity_between(*previous, now) {
                lines.push(DiskLine {
                    name: info.display_name(),
                    busy: activity.busy,
                    read_per_second: activity.read_per_second,
                    write_per_second: activity.write_per_second,
                    queue_depth: activity.queue_depth,
                    saturated: activity.is_saturated(),
                });
            }
        }
        counters.push(now);
    }

    (lines, counters)
}

/// Читает файлы подкачки и готовит их к показу.
fn read_pagefiles() -> Vec<PagefileLine> {
    bamboo_sys::storage::pagefiles()
        .unwrap_or_default()
        .into_iter()
        .map(|file| PagefileLine {
            where_: file
                .drive_letter()
                .map(|letter| format!("диск {letter}"))
                .unwrap_or_else(|| file.name.clone()),
            in_use: file.in_use,
            total: file.total,
            peak: file.peak,
            usage: file.usage(),
        })
        .collect()
}

/// Насколько активно процесс должен работать с диском, чтобы называть его
/// виновником. Ниже мегабайта в секунду это фон, а не нагрузка.
const CULPRIT_FLOOR: u64 = 1024 * 1024;

/// Номер процесса ядра. Своих дел у него нет: он работает с диском
/// по поручению других, и в списке виновных выглядит бессмысленно.
const SYSTEM_PID: u32 = 4;

/// Объясняет работу ядра с диском, если именно оно грузит накопитель.
///
/// Тот самый случай «диск на сто процентов, а виноват System»: диспетчер
/// задач показывает это и молчит. Мы ищем рядом с ядром спутников —
/// антивирус, индексатор, обновление, сжатие памяти — и по ним называем
/// вероятную причину. Не нашли спутников — так и говорим.
fn explain_system(processes: &[ProcessLine], used: Bytes, total: Bytes) -> Option<String> {
    use bamboo_analyze::systemio::{classify, Bystander, Bystanders};

    let system = processes.iter().find(|line| line.pid == SYSTEM_PID)?;
    let system_io = system
        .read_per_second
        .saturating_add(system.write_per_second);
    if system_io < CULPRIT_FLOOR {
        return None;
    }

    let mut bystanders = Bystanders {
        memory_used_share: if total.as_u64() == 0 {
            0.0
        } else {
            used.as_u64() as f64 / total.as_u64() as f64
        },
        ..Default::default()
    };

    for line in processes {
        // Спутником считаем только того, кто сам чем-то занят: сама
        // по себе запущенная служба ничего не объясняет.
        let busy = line.cpu_percent >= 1.0
            || line.read_per_second.saturating_add(line.write_per_second) >= 512 * 1024;

        match classify(&line.name) {
            Some(Bystander::Antivirus) if busy => bystanders.antivirus_busy = true,
            Some(Bystander::Indexer) if busy => bystanders.indexer_busy = true,
            Some(Bystander::Update) if busy => bystanders.update_busy = true,
            Some(Bystander::Defrag) if busy => bystanders.defrag_busy = true,
            // Сжатие памяти живёт всегда, поэтому важно не «работает ли»,
            // а сколько памяти оно набрало.
            Some(Bystander::MemoryCompression) => {
                bystanders.memory_compression_busy = line.memory.as_u64() > 128 * 1024 * 1024
            }
            _ => {}
        }

        if line.pid != SYSTEM_PID
            && line.read_per_second.saturating_add(line.write_per_second) >= 20 * 1024 * 1024
        {
            bystanders.user_bulk_io = true;
        }
    }

    let verdict = bamboo_analyze::explain_system_io(bystanders);
    Some(format!(
        "Ядро Windows работает с диском на {}/с. Похоже на «{}»: {}. {}",
        Bytes(system_io),
        verdict.cause.name(),
        verdict.because,
        verdict.cause.advice()
    ))
}

/// Объясняет, почему диск занят, и называет виновника, если он очевиден.
///
/// Это ответ на вопрос, который диспетчер задач оставляет без ответа:
/// он показывает «активность 100%» и молчит о том, из-за чего она.
///
/// Виновника называем, только когда он один и заметно выделяется. Если
/// диск занят, а никто из процессов на него всерьёз не пишет, честно
/// говорим и это: так бывает при работе антивируса, индексатора или
/// самого накопителя, и врать про «виноват chrome.exe» нельзя.
fn explain_disk_pressure(disks: &[DiskLine], processes: &[ProcessLine]) -> Option<String> {
    let busiest = disks
        .iter()
        .filter(|disk| disk.saturated)
        .max_by(|a, b| a.busy.total_cmp(&b.busy))?;

    let culprit = processes
        .iter()
        .max_by_key(|line| line.read_per_second.saturating_add(line.write_per_second))
        .filter(|line| line.read_per_second.saturating_add(line.write_per_second) >= CULPRIT_FLOOR);

    Some(match culprit {
        Some(line) => format!(
            "{} загружен на {:.0}%: больше всех работает {} — чтение {}/с, запись {}/с",
            busiest.name,
            busiest.busy * 100.0,
            line.name,
            Bytes(line.read_per_second),
            Bytes(line.write_per_second),
        ),
        None => format!(
            "{} загружен на {:.0}%, но заметного виновника среди процессов нет. \
             Так выглядит работа антивируса, индексатора поиска или самого \
             накопителя — например, сборка мусора у SSD",
            busiest.name,
            busiest.busy * 100.0,
        ),
    })
}

#[cfg(test)]
mod pressure_tests {
    use super::*;

    fn disk(busy: f64) -> DiskLine {
        DiskLine {
            name: "Диск 0".to_string(),
            busy,
            read_per_second: Bytes::ZERO,
            write_per_second: Bytes::ZERO,
            queue_depth: 0,
            saturated: busy >= 0.85,
        }
    }

    fn process(name: &str, write: u64) -> ProcessLine {
        ProcessLine {
            name: name.to_string(),
            pid: 1,
            threads: 1,
            cpu_percent: 0.0,
            memory: Bytes::ZERO,
            badge: String::new(),
            memory_growth: None,
            hung: false,
            read_per_second: 0,
            write_per_second: write,
        }
    }

    #[test]
    fn a_free_disk_needs_no_explanation() {
        let explanation = explain_disk_pressure(&[disk(0.2)], &[process("chrome.exe", 50 << 20)]);
        assert_eq!(explanation, None);
    }

    #[test]
    fn a_busy_disk_names_the_process_behind_it() {
        let explanation =
            explain_disk_pressure(&[disk(1.0)], &[process("chrome.exe", 45 << 20)]).unwrap();
        assert!(explanation.contains("загружен на 100%"), "{explanation}");
        assert!(explanation.contains("chrome.exe"), "{explanation}");
    }

    #[test]
    fn a_busy_disk_without_a_culprit_says_so_honestly() {
        // Диск занят, но процессы почти ничего не пишут: виноват кто-то
        // невидимый снаружи. Назначать виновным первого попавшегося нельзя.
        let explanation =
            explain_disk_pressure(&[disk(0.95)], &[process("idle.exe", 1024)]).unwrap();
        assert!(
            explanation.contains("виновника среди процессов нет"),
            "{explanation}"
        );
        assert!(!explanation.contains("idle.exe"), "{explanation}");
    }

    #[test]
    fn the_busiest_disk_is_the_one_reported() {
        let mut quiet = disk(0.9);
        quiet.name = "Медленный".to_string();
        let mut loud = disk(1.0);
        loud.name = "Загруженный".to_string();

        let explanation =
            explain_disk_pressure(&[quiet, loud], &[process("app.exe", 10 << 20)]).unwrap();
        assert!(explanation.starts_with("Загруженный"), "{explanation}");
    }
}

/// Читает разделы и готовит их к показу.
fn read_volumes() -> Vec<VolumeLine> {
    bamboo_sys::storage::volumes()
        .unwrap_or_default()
        .into_iter()
        .map(|volume| {
            let label = if volume.label.is_empty() {
                String::new()
            } else {
                format!(" {}", volume.label)
            };
            let fs = if volume.file_system.is_empty() {
                String::new()
            } else {
                format!(" ({})", volume.file_system)
            };
            VolumeLine {
                title: format!("{}:{label}{fs}", volume.letter),
                free: volume.free,
                total: volume.total,
                usage: volume.usage(),
                cramped: volume.is_cramped(),
            }
        })
        .collect()
}
