//! Части памяти системы, которых нет в списке процессов.
//!
//! Диспетчер задач показывает у процессов только их личную память в ОЗУ.
//! На живой машине это 7,3 ГБ из 12,8 ГБ занятых, а остальное — ядро,
//! драйверы, файловый кэш, память видеокарты — строки в списке процессов
//! не имеет. Человек смотрит на двенадцать гигабайт «занято», складывает
//! глазами столбец и спрашивает, куда делись ещё пять.
//!
//! Все счётчики берутся одним запросом за тик: это мгновенные величины,
//! их значение готово после первого же опроса.

use bamboo_core::{Bytes, Result};

use crate::pdh::CounterSet;

/// Имена английские — см. `pdh`: на локализованной Windows счётчики
/// переименованы.
const PATHS: [&str; 7] = [
    r"\Memory\Pool Nonpaged Bytes",
    r"\Memory\Pool Paged Resident Bytes",
    r"\Memory\System Cache Resident Bytes",
    r"\Memory\System Code Resident Bytes",
    r"\Memory\System Driver Resident Bytes",
    r"\Memory\Modified Page List Bytes",
    r"\Memory\Free & Zero Page List Bytes",
];

/// Системные части памяти. `None` — счётчика в этой системе нет,
/// и тогда о части молчим, а не рисуем ноль.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemMemory {
    /// Невыгружаемый пул ядра — в основном драйверы. Растёт день ото дня —
    /// течёт драйвер, и в Диспетчере задач этого не видно вовсе.
    pub pool_nonpaged: Option<Bytes>,
    /// Выгружаемый пул ядра, та его часть, что лежит в ОЗУ.
    pub pool_paged: Option<Bytes>,
    /// Файлы, с которыми система работает прямо сейчас.
    pub file_cache: Option<Bytes>,
    /// Код ядра и драйверов в памяти.
    pub kernel_code: Option<Bytes>,
    /// Изменённые страницы, ждущие записи на диск.
    pub modified: Option<Bytes>,
    /// Совсем пустые страницы.
    pub free_zero: Option<Bytes>,
}

/// Открытый набор счётчиков.
pub struct SystemMemoryCounter(CounterSet);

impl SystemMemoryCounter {
    pub fn open() -> Result<SystemMemoryCounter> {
        CounterSet::open(&PATHS).map(SystemMemoryCounter)
    }

    pub fn read(&mut self) -> SystemMemory {
        let values = self.0.read();
        let bytes = |at: usize| {
            values
                .get(at)
                .copied()
                .flatten()
                .map(|value| Bytes(value.max(0.0) as u64))
        };
        // Код ядра и код драйверов — два счётчика, человеку это одно.
        let kernel_code = match (bytes(3), bytes(4)) {
            (Some(code), Some(drivers)) => Some(Bytes(code.as_u64() + drivers.as_u64())),
            (one, other) => one.or(other),
        };
        SystemMemory {
            pool_nonpaged: bytes(0),
            pool_paged: bytes(1),
            file_cache: bytes(2),
            kernel_code,
            modified: bytes(5),
            free_zero: bytes(6),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_parts_are_readable_and_believable() {
        // Набор «Memory» есть в любой Windows, поэтому здесь не пропускаем
        // тест при ошибке открытия, а честно падаем.
        let mut counter = SystemMemoryCounter::open().expect("счётчики памяти обязаны открываться");
        let parts = counter.read();

        let pool = parts
            .pool_nonpaged
            .expect("невыгружаемый пул обязан читаться");
        // Ядро без драйверов не бывает меньше мегабайта, а больше
        // шестнадцати гигабайт — значит перепутаны единицы.
        assert!(pool > Bytes::from_mib(1), "пул ядра {pool}");
        assert!(pool < Bytes::from_mib(16 * 1024), "пул ядра {pool}");
        assert!(
            parts.free_zero.is_some(),
            "пустые страницы обязаны читаться"
        );
    }
}
