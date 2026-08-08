//! Ответы брокера на читающие запросы (ТЗ, разделы 13.2, 5.1).
//!
//! Пока это разовый снимок метрик: брокер поднимает коллектор, делает два
//! тика (первый задаёт базу для дельт, второй — настоящую загрузку) и
//! отдаёт агенту сводку. Подписки на поток и история по приложению
//! появятся вместе с резидентным коллектором и хранилищем — сейчас брокер
//! обслуживает клиента по одному, синхронно.
//!
//! Коллектор Windows-специфичен, поэтому модуль целиком под `cfg(windows)`
//! и в переносимую часть не попадает.

use std::time::Duration;

use bamboo_collect::Collector;
use bamboo_ipc::{ErrorCode, Response, Scope};

/// Пауза между двумя тиками. Меньше секунды брать нельзя: процессорное
/// время учитывается квантами ~15 мс, и на коротком интервале загрузка
/// получается ступенчатой.
const WARMUP: Duration = Duration::from_millis(1000);

/// Отвечает на разовый снимок. Все области сводятся к одной сводке метрик:
/// разбивку по процессам и накопителям брокер отдаёт отдельными запросами,
/// когда они появятся.
pub fn query_snapshot(_scope: Scope) -> Response {
    metrics()
}

/// Снимает метрики системы двумя тиками коллектора.
fn metrics() -> Response {
    let mut collector = Collector::new();

    if let Err(error) = collector.tick() {
        return internal(format!("первый тик не удался: {error}"));
    }
    std::thread::sleep(WARMUP);
    let tick = match collector.tick() {
        Ok(tick) => tick,
        Err(error) => return internal(format!("второй тик не удался: {error}")),
    };

    let memory = &tick.system.memory;
    Response::Metrics {
        at_unix_ms: tick.at.unix_ms,
        // Загрузка приходит долей 0..1; протокол хочет проценты.
        cpu_busy_percent: percent(tick.cpu_busy()),
        memory_used_mib: memory.physical_used().as_mib_f64() as u32,
        memory_total_mib: memory.physical_total.as_mib_f64() as u32,
        process_count: tick.process_count.min(u32::MAX as usize) as u32,
    }
}

/// Доля 0..1 в целые проценты 0..100 с отсечкой сверху.
fn percent(ratio: f64) -> u16 {
    (ratio * 100.0).round().clamp(0.0, 100.0) as u16
}

fn internal(detail: String) -> Response {
    Response::Error {
        code: ErrorCode::Internal,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ratio_becomes_whole_percents() {
        assert_eq!(percent(0.0), 0);
        assert_eq!(percent(0.5), 50);
        assert_eq!(percent(1.0), 100);
        // Загрузка выше единицы физически невозможна, но округление
        // и накопленная погрешность не должны выдать 101%.
        assert_eq!(percent(1.5), 100);
    }

    #[test]
    fn a_snapshot_returns_live_metrics() {
        // На живой машине коллектор обязан снять метрики без прав.
        match query_snapshot(Scope::System) {
            Response::Metrics {
                memory_total_mib,
                process_count,
                cpu_busy_percent,
                ..
            } => {
                assert!(memory_total_mib > 0, "объём памяти не прочитался");
                assert!(process_count > 10, "процессов подозрительно мало");
                assert!(cpu_busy_percent <= 100);
            }
            other => panic!("ожидались метрики, получили {other:?}"),
        }
    }
}
