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
#[derive(Clone, Debug)]
pub struct ProcessLine {
    pub name: String,
    pub cpu_percent: f32,
    pub memory: Bytes,
    /// Пояснение под именем: чем этот процесс примечателен.
    pub badge: String,
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
    /// История использования памяти, доли от максимума, 0..1.
    pub spark: Vec<f32>,
    /// Собственное потребление Bamboo.
    pub own_memory: Bytes,
    pub cadence: String,
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

fn run(sender: Sender<Snapshot>, visible: WidgetVisible) {
    let mut collector = Collector::new();
    let mut memory_history: Vec<u64> = Vec::with_capacity(SPARK_POINTS);

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

        let top = collector
            .table()
            .top_by_cpu(4)
            .into_iter()
            .map(|process| ProcessLine {
                name: process.image_name.to_string(),
                cpu_percent: process.cpu_share * 100.0,
                memory: Bytes::from_kib(process.last_point().private_kib as u64),
                badge: badge_for(process),
            })
            .collect();

        let snapshot = Snapshot {
            cpu_busy: tick.cpu_busy(),
            driver_ratio: tick.driver_time(),
            memory_used: used,
            memory_total: tick.system.memory.physical_total,
            process_count: tick.process_count,
            top,
            spark: normalise(&memory_history),
            own_memory: bamboo_sys::own_memory()
                .map(|m| m.working_set)
                .unwrap_or(Bytes::ZERO),
            cadence: cadence_name(tick.cadence),
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
