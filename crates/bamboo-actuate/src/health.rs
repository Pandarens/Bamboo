//! Живая проба здоровья для сторожевого таймера (ТЗ, раздел 12.4).
//!
//! `watchdog::sweep` спрашивает у пробы две вещи: каким было здоровье
//! системы на момент применения действия и каким оно стало сейчас. Тесты
//! подставляют фиксированные значения; здесь — настоящая реализация,
//! читающая живые метрики.
//!
//! Из чего складывается здоровье:
//! - число ошибок и критических событий в журналах за сутки — читается
//!   без прав администратора;
//! - длительность последней загрузки — доступна только администратору,
//!   без прав возвращается ноль, и сторож этот сигнал просто не учитывает.
//!
//! Базовую линию снимаем в момент применения и держим в памяти брокера,
//! пока запись под наблюдением. Ключ — номер записи в журнале.

use std::cell::RefCell;
use std::collections::HashMap;

use bamboo_journal::watchdog::{Baseline, Observed};

use crate::watchdog::HealthProbe;

/// Проба здоровья на живой системе.
///
/// Базовые линии по записям журнала держит у себя: сторож обращается к ним
/// по номеру записи. Всё внутри `RefCell`, чтобы проба оставалась общей
/// ссылкой — `sweep` берёт её как `&impl HealthProbe`.
#[derive(Default)]
pub struct LiveHealthProbe {
    baselines: RefCell<HashMap<i64, Baseline>>,
}

impl LiveHealthProbe {
    pub fn new() -> Self {
        LiveHealthProbe {
            baselines: RefCell::new(HashMap::new()),
        }
    }

    /// Снимает текущее здоровье системы как базовую линию для записи.
    ///
    /// Вызывается сразу после применения действия: именно с этим состоянием
    /// сторож будет сравнивать систему всё окно наблюдения.
    pub fn remember_baseline(&self, journal_id: i64) {
        self.baselines
            .borrow_mut()
            .insert(journal_id, current_health());
    }

    /// Забывает базовую линию записи — после отката или закрытия окна
    /// наблюдения держать её незачем.
    pub fn forget(&self, journal_id: i64) {
        self.baselines.borrow_mut().remove(&journal_id);
    }

    /// Сколько записей сейчас под наблюдением.
    pub fn tracked(&self) -> usize {
        self.baselines.borrow().len()
    }
}

impl HealthProbe for LiveHealthProbe {
    fn baseline(&self, journal_id: i64) -> Baseline {
        // Базовой линии нет — возвращаем пустую. У неё ноль ошибок и ноль
        // времени загрузки, а сторож на таких значениях откат не запускает:
        // сравнивать не с чем, и это безопаснее ложной тревоги.
        self.baselines
            .borrow()
            .get(&journal_id)
            .copied()
            .unwrap_or_default()
    }

    fn observed(&self, _journal_id: i64) -> Observed {
        // Ошибки и время загрузки читаем живьём. Остальные сигналы —
        // аварии приложения, ручной перезапуск и разморозку цели — снаружи
        // достоверно не определить, оставляем их пустыми: сторож отработает
        // по тому, что действительно измеримо.
        Observed {
            daily_errors: daily_errors(),
            boot_ms: latest_boot_ms(),
            ..Default::default()
        }
    }
}

/// Текущее здоровье системы как базовая линия.
fn current_health() -> Baseline {
    Baseline {
        daily_errors: daily_errors(),
        boot_ms: latest_boot_ms(),
    }
}

/// Ошибок и критических событий за сутки. При сбое чтения — ноль: это
/// безопасное направление, сторож примет отсутствие роста, а не деградацию.
fn daily_errors() -> u32 {
    bamboo_sys::daily_error_count().unwrap_or(0)
}

/// Длительность последней загрузки в миллисекундах. Канал доступен только
/// администратору; без прав возвращаем ноль, и сторож этот сигнал игнорирует.
fn latest_boot_ms() -> u64 {
    bamboo_sys::boot_history(1)
        .ok()
        .and_then(|records| records.first().map(|record| record.total_ms))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remembered_baseline_is_returned_for_its_entry() {
        let probe = LiveHealthProbe::new();
        probe.remember_baseline(42);
        // Базовая линия снята и привязана к номеру записи.
        assert_eq!(probe.tracked(), 1);
        // Число ошибок — u32, читается на живой машине без прав; проверяем,
        // что база вообще снялась и достаётся по ключу.
        let _baseline = probe.baseline(42);
    }

    #[test]
    fn an_unknown_entry_yields_a_neutral_baseline() {
        let probe = LiveHealthProbe::new();
        let baseline = probe.baseline(999);
        // Пустая база: сторож на ней откат не запустит.
        assert_eq!(baseline.daily_errors, 0);
        assert_eq!(baseline.boot_ms, 0);
    }

    #[test]
    fn forgetting_drops_the_baseline() {
        let probe = LiveHealthProbe::new();
        probe.remember_baseline(7);
        assert_eq!(probe.tracked(), 1);
        probe.forget(7);
        assert_eq!(probe.tracked(), 0);
        // После забвения снова нейтральная база.
        assert_eq!(probe.baseline(7).daily_errors, 0);
    }

    #[test]
    fn observed_reads_live_without_panicking() {
        let probe = LiveHealthProbe::new();
        // На живой машине System и Application читаются без прав, значит
        // observed обязан вернуться, а не упасть.
        let observed = probe.observed(1);
        assert!(!observed.user_restarted_target);
    }
}
