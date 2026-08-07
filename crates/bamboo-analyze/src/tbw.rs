//! Паспортный ресурс записи по модели накопителя.
//!
//! Единой открытой базы TBW не существует, таблицу приходится вести вручную
//! (ТЗ, раздел 19 — открытый вопрос). Поэтому важнее самой таблицы флаг
//! «это оценка»: показывать выдуманное число как паспортное нельзя.

use bamboo_core::Bytes;

/// Ресурс записи накопителя.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TbwRating {
    pub total: Bytes,
    /// Модель в таблице не нашлась, значение получено из общего правила.
    pub is_estimate: bool,
}

/// Консервативная оценка при неизвестной модели: 300 ТБ на терабайт ёмкости.
const FALLBACK_TB_PER_TB: f64 = 300.0;

/// Известные семейства: подстрока модели и ресурс в терабайтах
/// на терабайт ёмкости.
///
/// Нормировка на ёмкость не случайна: у одной и той же линейки версия
/// на 2 ТБ выдерживает вдвое больше записей, чем на 1 ТБ.
const KNOWN: &[(&str, f64)] = &[
    // Samsung
    ("990 pro", 600.0),
    ("980 pro", 600.0),
    ("970 evo", 600.0),
    ("980", 300.0),
    ("870 evo", 600.0),
    ("860 evo", 600.0),
    // Western Digital / SanDisk
    ("sn850", 600.0),
    ("sn770", 600.0),
    ("sn570", 600.0),
    // Crucial / Micron
    ("mx500", 360.0),
    ("p5 plus", 600.0),
    ("p3", 220.0),
    ("bx500", 120.0),
    // Kingston
    ("kc3000", 800.0),
    ("nv2", 320.0),
    ("a400", 333.0),
];

/// Ресурс записи для модели и ёмкости.
pub fn rating_for(model: &str, capacity: Bytes) -> TbwRating {
    let model = model.to_lowercase();
    let terabytes = capacity.as_u64() as f64 / 1e12;

    // Совпадения проверяем от длинных подстрок к коротким, иначе «980»
    // перехватит «980 pro».
    let mut candidates: Vec<&(&str, f64)> = KNOWN
        .iter()
        .filter(|(name, _)| model.contains(name))
        .collect();
    candidates.sort_by_key(|(name, _)| core::cmp::Reverse(name.len()));

    match candidates.first() {
        Some((_, per_tb)) => TbwRating {
            total: terabytes_to_bytes(per_tb * terabytes),
            is_estimate: false,
        },
        None => TbwRating {
            total: terabytes_to_bytes(FALLBACK_TB_PER_TB * terabytes),
            is_estimate: true,
        },
    }
}

fn terabytes_to_bytes(tb: f64) -> Bytes {
    Bytes((tb.max(0.0) * 1e12) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gigabytes(gb: u64) -> Bytes {
        Bytes(gb * 1_000_000_000)
    }

    #[test]
    fn known_model_is_not_marked_as_an_estimate() {
        let rating = rating_for("Samsung SSD 980 PRO 1TB", gigabytes(1000));
        assert!(!rating.is_estimate);
        assert!((rating.total.as_u64() as f64 / 1e12 - 600.0).abs() < 1.0);
    }

    #[test]
    fn rating_scales_with_capacity() {
        let one = rating_for("Samsung SSD 990 PRO", gigabytes(1000));
        let two = rating_for("Samsung SSD 990 PRO", gigabytes(2000));
        assert!(two.total.as_u64() > one.total.as_u64() * 19 / 10);
    }

    #[test]
    fn longer_match_wins() {
        // «980» не должна перехватывать «980 PRO»: ресурс у них разный.
        let pro = rating_for("Samsung SSD 980 PRO", gigabytes(1000));
        let plain = rating_for("Samsung SSD 980", gigabytes(1000));
        assert!(pro.total > plain.total);
    }

    #[test]
    fn unknown_model_is_honestly_marked() {
        let rating = rating_for("Apacer AS350 512GB", gigabytes(512));
        assert!(
            rating.is_estimate,
            "неизвестная модель обязана быть помечена"
        );
        assert!((rating.total.as_u64() as f64 / 1e12 - 153.6).abs() < 1.0);
    }

    #[test]
    fn zero_capacity_does_not_panic() {
        let rating = rating_for("что-то", Bytes::ZERO);
        assert_eq!(rating.total, Bytes::ZERO);
    }
}
