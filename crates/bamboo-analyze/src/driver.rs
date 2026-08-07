//! Нагрузка уровня драйверов (ТЗ, раздел 9.3).
//!
//! Ценность этого анализатора высокая и специфическая: пользователи месяцами
//! ищут «вирус», когда причина — драйвер сетевой карты или звука. Ни один
//! процессный монитор этого не покажет, потому что показывать нечего:
//! время уходит в отложенные вызовы процедур и обработчики прерываний,
//! а они не принадлежат ни одному процессу.

use crate::observation::{Observation, ObservationKind, Severity};

/// Ниже этой загрузки говорить не о чем.
const MIN_TOTAL_BUSY: f64 = 0.15;
/// Процессы должны объяснять меньше этой доли нагрузки.
const MAX_EXPLAINED_BY_PROCESSES: f64 = 0.60;
/// И в DPC с прерываниями должно уходить больше этой доли.
const MIN_DRIVER_RATIO: f64 = 0.10;

/// Что известно на момент анализа. Все доли — 0..1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DriverInput {
    /// Суммарная занятость процессора.
    pub total_busy: f64,
    /// Сколько из неё объясняется суммой по процессам.
    pub process_sum: f64,
    /// Доля времени в DPC и прерываниях.
    pub driver_ratio: f64,
}

impl DriverInput {
    /// Какую часть нагрузки объясняют процессы.
    fn explained(&self) -> f64 {
        if self.total_busy <= 0.0 {
            return 1.0;
        }
        (self.process_sum / self.total_busy).clamp(0.0, 1.0)
    }
}

pub fn analyze(input: &DriverInput) -> Option<Observation> {
    if input.total_busy < MIN_TOTAL_BUSY {
        return None;
    }
    if input.explained() >= MAX_EXPLAINED_BY_PROCESSES {
        return None;
    }
    if input.driver_ratio < MIN_DRIVER_RATIO {
        return None;
    }

    let unexplained = (1.0 - input.explained()) * 100.0;

    Some(
        Observation::new(
            ObservationKind::DriverLoad,
            Severity::Notice,
            confidence(input),
            format!(
                "Процессор занят на {:.0}%, но процессы объясняют только {:.0}% этой нагрузки. \
                 {:.0}% времени уходит в драйверы",
                input.total_busy * 100.0,
                input.explained() * 100.0,
                input.driver_ratio * 100.0
            ),
        )
        .with_detail(format!(
            "Оставшиеся {unexplained:.0}% нагрузки не принадлежат ни одному процессу — \
             искать виновника в диспетчере задач бесполезно. Чаще всего дело в драйверах \
             сетевого адаптера, звука или накопителя: стоит проверить, нет ли для них \
             обновлений. Назвать конкретный драйвер Bamboo не может: для этого нужен \
             xperf с трассировкой стеков, а это выходит за рамки резидентной утилиты."
        )),
    )
}

/// Уверенность тем выше, чем больше нагрузки остаётся необъяснённой.
fn confidence(input: &DriverInput) -> f32 {
    let unexplained = 1.0 - input.explained();
    let by_driver_time = (input.driver_ratio / 0.30).min(1.0);
    (0.4 + 0.3 * unexplained + 0.3 * by_driver_time).clamp(0.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_driver_problem_is_named_as_such() {
        let observation = analyze(&DriverInput {
            total_busy: 0.35,
            process_sum: 0.10,
            driver_ratio: 0.22,
        })
        .unwrap();

        assert_eq!(observation.kind, ObservationKind::DriverLoad);
        assert!(observation.summary.contains("драйверы"));
        assert!(observation
            .detail
            .as_ref()
            .unwrap()
            .contains("в диспетчере задач бесполезно"));
    }

    #[test]
    fn we_do_not_pretend_to_name_the_driver() {
        // Определение конкретного драйвера требует xperf и в объём не входит.
        // Обещать больше, чем можем, нельзя.
        let observation = analyze(&DriverInput {
            total_busy: 0.40,
            process_sum: 0.05,
            driver_ratio: 0.30,
        })
        .unwrap();
        assert!(observation.detail.unwrap().contains("не может"));
    }

    #[test]
    fn a_load_fully_explained_by_processes_is_not_a_driver_problem() {
        assert!(analyze(&DriverInput {
            total_busy: 0.80,
            process_sum: 0.78,
            driver_ratio: 0.15,
        })
        .is_none());
    }

    #[test]
    fn an_idle_machine_says_nothing() {
        assert!(analyze(&DriverInput {
            total_busy: 0.03,
            process_sum: 0.0,
            driver_ratio: 0.5,
        })
        .is_none());
    }

    #[test]
    fn unexplained_load_without_dpc_time_is_not_blamed_on_drivers() {
        // Нагрузка есть, процессы её не объясняют, но и в DPC время
        // не уходит. Значит, дело в чём-то другом — например, в процессах,
        // которые успели завершиться между опросами. Молчим.
        assert!(analyze(&DriverInput {
            total_busy: 0.40,
            process_sum: 0.05,
            driver_ratio: 0.02,
        })
        .is_none());
    }

    #[test]
    fn confidence_grows_with_unexplained_load() {
        let weak = analyze(&DriverInput {
            total_busy: 0.20,
            process_sum: 0.11,
            driver_ratio: 0.10,
        })
        .unwrap()
        .confidence;

        let strong = analyze(&DriverInput {
            total_busy: 0.60,
            process_sum: 0.02,
            driver_ratio: 0.45,
        })
        .unwrap()
        .confidence;

        assert!(strong > weak);
        assert!((0.0..=1.0).contains(&strong));
    }

    #[test]
    fn empty_input_does_not_divide_by_zero() {
        assert!(analyze(&DriverInput::default()).is_none());
    }
}
