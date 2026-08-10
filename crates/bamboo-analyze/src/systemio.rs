//! Объяснение дисковой активности процесса System (ТЗ, раздел 9.3).
//!
//! Самая частая и самая бесполезная картина в диспетчере задач: диск
//! загружен на сто процентов, а виновником числится `System`. Дальше
//! диспетчер молчит, и человек остаётся один на один с надписью.
//!
//! Разобраться можно, потому что `System` — это не программа, а ядро.
//! Своих дел у него нет: он пишет и читает по поручению других. Кто
//! поручил, снаружи видно не всегда, но у каждой типичной причины есть
//! спутники — служба, процесс или состояние системы, которые видны рядом.
//! По ним и опознаём.
//!
//! Врать при этом нельзя. Если спутников не нашлось, так и говорим:
//! «работает ядро, источник назвать не могу» — это честнее, чем назначить
//! виновным первый попавшийся процесс.

/// Что, судя по всему, стоит за работой ядра с диском.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemIoCause {
    /// Защитник или другой антивирус сканирует файлы.
    Antivirus,
    /// Служба поиска перестраивает индекс.
    SearchIndex,
    /// Идёт установка обновлений Windows.
    WindowsUpdate,
    /// SysMain готовит файлы к запуску программ.
    Prefetch,
    /// Windows сжимает память и уводит её в подкачку — памяти не хватает.
    MemoryPressure,
    /// Идёт оптимизация или дефрагментация накопителя.
    Defrag,
    /// Копирование или распаковка большого объёма пользователем.
    BulkFileWork,
    /// Спутников не нашлось.
    Unknown,
}

impl SystemIoCause {
    /// Короткое название причины.
    pub fn name(self) -> &'static str {
        match self {
            SystemIoCause::Antivirus => "проверка антивирусом",
            SystemIoCause::SearchIndex => "индексация поиска",
            SystemIoCause::WindowsUpdate => "обновление Windows",
            SystemIoCause::Prefetch => "подготовка файлов SysMain",
            SystemIoCause::MemoryPressure => "нехватка оперативной памяти",
            SystemIoCause::Defrag => "оптимизация накопителя",
            SystemIoCause::BulkFileWork => "работа с большими файлами",
            SystemIoCause::Unknown => "источник не определён",
        }
    }

    /// Что с этим делать. Совет обязан быть выполнимым и честным:
    /// «перезагрузите компьютер» советом не считается.
    pub fn advice(self) -> &'static str {
        match self {
            SystemIoCause::Antivirus => {
                "Это разовая работа: проверка закончится сама. Если она идёт \
                 часами каждый день, в настройках антивируса стоит перенести \
                 полную проверку на ночь."
            }
            SystemIoCause::SearchIndex => {
                "Индекс перестраивается после крупных изменений на диске и \
                 затихает сам. Если это повторяется постоянно, из индексации \
                 стоит исключить папки со сборками и кэшем — они меняются \
                 непрерывно."
            }
            SystemIoCause::WindowsUpdate => {
                "Обновление дописывает файлы и остановится само. Прерывать \
                 его на середине не стоит: следующая попытка начнётся заново."
            }
            SystemIoCause::Prefetch => {
                "SysMain заранее подтягивает то, что вы запускаете часто. \
                 На SSD пользы от этого немного, и службу можно перевести \
                 на отложенный запуск — но выигрыш будет скромным."
            }
            SystemIoCause::MemoryPressure => {
                "Памяти не хватает, и Windows сжимает её и пишет в подкачку. \
                 Диск здесь следствие, а не причина: закройте лишние вкладки \
                 и программы — дисковая активность прекратится сама."
            }
            SystemIoCause::Defrag => {
                "Плановая оптимизация. У SSD это не дефрагментация, а команда \
                 TRIM — она быстрая и полезная, ей лучше дать закончиться."
            }
            SystemIoCause::BulkFileWork => {
                "Копирование или распаковка идёт через ядро, поэтому в списке \
                 виден System. Это ваша же операция — она закончится."
            }
            SystemIoCause::Unknown => {
                "Назвать источник по внешним признакам не получилось. \
                 Понять точнее можно записью трассы ETW: она покажет, какой \
                 процесс инициировал обращения к диску."
            }
        }
    }
}

/// Что видно рядом с работающим ядром.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bystanders {
    /// Активен процесс защитника или стороннего антивируса.
    pub antivirus_busy: bool,
    /// Работает служба индексации поиска.
    pub indexer_busy: bool,
    /// Идёт установка обновлений.
    pub update_busy: bool,
    /// Работает SysMain.
    pub sysmain_busy: bool,
    /// Виден процесс сжатия памяти либо подкачка заметно занята.
    pub memory_compression_busy: bool,
    /// Доля занятой оперативной памяти, 0..1.
    pub memory_used_share: f64,
    /// Работает оптимизация диска.
    pub defrag_busy: bool,
    /// Пользовательский процесс сам активно работает с диском.
    pub user_bulk_io: bool,
}

/// Объяснение работы ядра с диском.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemIoVerdict {
    pub cause: SystemIoCause,
    /// Почему решили именно так — теми же словами, что и человеку.
    pub because: String,
}

/// Определяет причину дисковой активности ядра.
///
/// Порядок проверки — от самого частого и однозначного к менее явному.
/// Нехватка памяти идёт первой не случайно: это единственный случай, где
/// диск лишь следствие, и лечится он совсем не там, где виден.
pub fn explain(bystanders: Bystanders) -> SystemIoVerdict {
    // Память проверяем первой: если её мало, всё остальное вторично.
    // Windows начинает сжимать страницы и писать их в подкачку, и
    // именно ядро выполняет эту запись.
    if bystanders.memory_compression_busy && bystanders.memory_used_share >= 0.85 {
        return SystemIoVerdict {
            cause: SystemIoCause::MemoryPressure,
            because: format!(
                "занято {:.0}% оперативной памяти, и работает сжатие памяти — \
                 Windows вытесняет страницы в подкачку, а пишет их ядро",
                bystanders.memory_used_share * 100.0
            ),
        };
    }

    if bystanders.antivirus_busy {
        return SystemIoVerdict {
            cause: SystemIoCause::Antivirus,
            because: "рядом работает антивирус: файлы он проверяет через \
                      драйвер-фильтр, и обращения к диску идут от ядра"
                .to_string(),
        };
    }

    if bystanders.update_busy {
        return SystemIoVerdict {
            cause: SystemIoCause::WindowsUpdate,
            because: "работает установщик обновлений Windows".to_string(),
        };
    }

    if bystanders.indexer_busy {
        return SystemIoVerdict {
            cause: SystemIoCause::SearchIndex,
            because: "работает служба индексации поиска".to_string(),
        };
    }

    if bystanders.defrag_busy {
        return SystemIoVerdict {
            cause: SystemIoCause::Defrag,
            because: "работает плановая оптимизация накопителя".to_string(),
        };
    }

    if bystanders.sysmain_busy {
        return SystemIoVerdict {
            cause: SystemIoCause::Prefetch,
            because: "работает SysMain — служба предзагрузки часто \
                      запускаемых программ"
                .to_string(),
        };
    }

    if bystanders.user_bulk_io {
        return SystemIoVerdict {
            cause: SystemIoCause::BulkFileWork,
            because: "рядом идёт активная работа с файлами из пользовательской \
                      программы"
                .to_string(),
        };
    }

    SystemIoVerdict {
        cause: SystemIoCause::Unknown,
        because: "рядом с ядром ничего примечательного не видно".to_string(),
    }
}

/// Опознаёт спутника по имени процесса.
///
/// Имена собраны из того, что реально встречается на живых машинах.
/// Список неполон по определению — антивирусов много, — поэтому
/// неизвестное имя просто не опознаётся, а не записывается в виновные.
pub fn classify(image_name: &str) -> Option<Bystander> {
    let name = image_name.to_lowercase();
    let name = name.trim_end_matches(".exe");

    Some(match name {
        "msmpeng" | "nissrv" | "avp" | "avastsvc" | "avgsvc" | "ekrn" | "bdagent" | "mcshield"
        | "nortonsecurity" => Bystander::Antivirus,
        "searchindexer" | "searchprotocolhost" | "searchfilterhost" => Bystander::Indexer,
        "tiworker" | "trustedinstaller" | "usoclient" | "mousocoreworker" | "wuauclt" => {
            Bystander::Update
        }
        "memory compression" | "memcompression" => Bystander::MemoryCompression,
        "defrag" | "dfrgui" => Bystander::Defrag,
        _ => return None,
    })
}

/// Кто именно опознан рядом с ядром.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bystander {
    Antivirus,
    Indexer,
    Update,
    MemoryCompression,
    Defrag,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_pressure_wins_over_everything_else() {
        // Тот самый случай, где диск — следствие, а лечится в другом месте.
        // Поэтому проверяется первым, даже если рядом работает антивирус.
        let verdict = explain(Bystanders {
            memory_compression_busy: true,
            memory_used_share: 0.93,
            antivirus_busy: true,
            ..Default::default()
        });
        assert_eq!(verdict.cause, SystemIoCause::MemoryPressure);
        assert!(verdict.because.contains("93%"), "{}", verdict.because);
        assert!(verdict.cause.advice().contains("следствие"));
    }

    #[test]
    fn compression_with_plenty_of_memory_is_not_pressure() {
        // Сжатие памяти работает всегда: само по себе оно ничего
        // не означает, важна занятость.
        let verdict = explain(Bystanders {
            memory_compression_busy: true,
            memory_used_share: 0.40,
            ..Default::default()
        });
        assert_eq!(verdict.cause, SystemIoCause::Unknown);
    }

    #[test]
    fn an_antivirus_nearby_explains_the_kernel() {
        let verdict = explain(Bystanders {
            antivirus_busy: true,
            ..Default::default()
        });
        assert_eq!(verdict.cause, SystemIoCause::Antivirus);
        assert!(verdict.because.contains("драйвер-фильтр"));
    }

    #[test]
    fn without_bystanders_we_admit_we_do_not_know() {
        // Главное свойство: не назначать виновного, когда его не видно.
        let verdict = explain(Bystanders::default());
        assert_eq!(verdict.cause, SystemIoCause::Unknown);
        assert!(verdict.cause.advice().contains("ETW"), "нужен путь дальше");
    }

    #[test]
    fn every_cause_has_a_name_and_workable_advice() {
        for cause in [
            SystemIoCause::Antivirus,
            SystemIoCause::SearchIndex,
            SystemIoCause::WindowsUpdate,
            SystemIoCause::Prefetch,
            SystemIoCause::MemoryPressure,
            SystemIoCause::Defrag,
            SystemIoCause::BulkFileWork,
            SystemIoCause::Unknown,
        ] {
            assert!(!cause.name().is_empty());
            let advice = cause.advice();
            assert!(advice.len() > 40, "совет слишком короткий: {advice}");
            // Совета «перезагрузите компьютер» здесь быть не должно.
            assert!(
                !advice.to_lowercase().contains("перезагруз"),
                "бесполезный совет: {advice}"
            );
        }
    }

    #[test]
    fn known_bystanders_are_recognised_by_name() {
        assert_eq!(classify("MsMpEng.exe"), Some(Bystander::Antivirus));
        assert_eq!(classify("SearchIndexer.exe"), Some(Bystander::Indexer));
        assert_eq!(classify("TiWorker.exe"), Some(Bystander::Update));
        assert_eq!(
            classify("Memory Compression"),
            Some(Bystander::MemoryCompression)
        );
    }

    #[test]
    fn an_unknown_process_is_not_blamed() {
        // Список антивирусов неполон по определению, и это не повод
        // записывать неизвестное имя в виновные.
        assert_eq!(classify("chrome.exe"), None);
        assert_eq!(classify("моя-программа.exe"), None);
    }

    #[test]
    fn the_order_of_checks_is_stable() {
        // Обновление важнее индексации: оно объясняет и её тоже.
        let verdict = explain(Bystanders {
            update_busy: true,
            indexer_busy: true,
            ..Default::default()
        });
        assert_eq!(verdict.cause, SystemIoCause::WindowsUpdate);
    }
}
