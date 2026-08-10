//! Детектор подвисаний системы (ТЗ, разделы 9.2 и 9.3).
//!
//! «Иногда всё замирает на пару секунд, и непонятно почему» — самая
//! частая жалоба и самая трудная для разбора: к тому моменту, когда
//! человек открывает диспетчер задач, всё уже прошло. Смотреть надо
//! в момент подвисания, а он короткий.
//!
//! Поэтому Bamboo смотрит непрерывно и запоминает моменты, когда система
//! была не в себе, вместе с обстановкой вокруг. Разбирать их можно потом,
//! спокойно.
//!
//! Что считается подвисанием, определяется тем, из-за чего оно бывает
//! на самом деле: диск не успевает, драйверы съели процессор, память
//! кончилась и всё ушло в подкачку. Все три причины различимы снаружи —
//! в отличие от «компьютер тормозит», которое ничего не значит.

use bamboo_core::Bytes;

/// Доля времени в DPC и прерываниях, после которой отзывчивость страдает.
///
/// Это работа драйверов, и она вытесняет всё остальное, включая обработку
/// ввода. Десять процентов — та граница, где человек начинает замечать
/// рывки курсора.
const DRIVER_TIME: f64 = 0.10;

/// Длина очереди к накопителю, при которой запросы уже ждут ощутимо.
const DISK_QUEUE: u32 = 8;

/// Насколько занятой должна быть память, чтобы Windows начала вытеснять
/// страницы в подкачку и всё замерло.
const MEMORY_TIGHT: f64 = 0.92;

/// Из-за чего подвисло.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreezeCause {
    /// Накопитель не успевает, запросы стоят в очереди.
    DiskQueue,
    /// Драйверы съели процессорное время.
    DriverTime,
    /// Память кончилась, идёт вытеснение в подкачку.
    MemoryPressure,
}

impl FreezeCause {
    pub fn name(self) -> &'static str {
        match self {
            FreezeCause::DiskQueue => "накопитель не успевает",
            FreezeCause::DriverTime => "драйверы заняли процессор",
            FreezeCause::MemoryPressure => "закончилась оперативная память",
        }
    }

    /// Что с этим делать. Совет обязан быть выполнимым.
    pub fn advice(self) -> &'static str {
        match self {
            FreezeCause::DiskQueue => {
                "Запросы к диску встали в очередь, и всё, что ждёт диска, замирает. \
                 Если виновник назван выше, его можно придержать: правая кнопка \
                 на строке процесса, «Придержать диск». Если не назван, диск занят \
                 изнутри — антивирусом, индексатором поиска или самим накопителем, \
                 и остаётся только переждать."
            }
            FreezeCause::DriverTime => {
                "Процессор ушёл в обработку прерываний — это работа драйверов, \
                 а не программ. Чаще всего виноват драйвер сети, звука или \
                 видеокарты. Среди процессов виновника искать бесполезно: \
                 помогает обновление драйверов, а не закрытие программ."
            }
            FreezeCause::MemoryPressure => {
                "Свободной памяти не осталось, и Windows вытесняет страницы \
                 на диск. Отсюда и рывки: программа ждёт, пока её данные \
                 вернутся с накопителя. Закройте лишние вкладки и программы — \
                 это единственное, что помогает по-настоящему."
            }
        }
    }
}

/// Обстановка на момент замера.
#[derive(Clone, Copy, Debug, Default)]
pub struct Moment {
    /// Доля времени в DPC и прерываниях, 0..1.
    pub driver_ratio: f64,
    /// Наибольшая длина очереди среди накопителей.
    pub disk_queue: u32,
    /// Занятость самого нагруженного накопителя, 0..1.
    pub disk_busy: f64,
    /// Доля занятой оперативной памяти, 0..1.
    pub memory_used_share: f64,
    /// Идёт ли вытеснение в подкачку прямо сейчас.
    pub compressing_memory: bool,
}

/// Кто чем занимался в этот момент.
///
/// Ради этого всё и затевалось. Совет «посмотрите, кто занимает диск»
/// бесполезен: пока человек откроет раздел, подвисание кончится и виновник
/// разойдётся. Поэтому список снимается в тот же миг, что и само
/// подвисание, и хранится вместе с ним.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bystanders<'a> {
    /// Кто работал с диском: имя и байт в секунду.
    pub disk: &'a [(String, u64)],
    /// Кто держал память: имя и байты.
    pub memory: &'a [(String, u64)],
}

/// Зафиксированное подвисание.
#[derive(Clone, Debug, PartialEq)]
pub struct Freeze {
    pub cause: FreezeCause,
    /// Что именно намерили — теми же числами, что и человеку показываем.
    pub detail: String,
    /// Кто был рядом в этот момент. Пусто, если виновника назвать нельзя.
    pub culprits: String,
    /// Их имена по отдельности: по ним открывается список процессов,
    /// отфильтрованный ровно на этих виновников. Пересказ словами для
    /// перехода не годится — в нём есть числа и знаки препинания.
    pub culprit_names: Vec<String>,
}

/// Определяет, было ли подвисание, и из-за чего.
///
/// `None` — обычное состояние: система работает, придираться не к чему.
/// Порядок проверки важен: причины перечислены от самой заметной для
/// человека к менее заметной, и первая же объясняет остальные.
pub fn detect(moment: Moment) -> Option<Freeze> {
    detect_with(moment, Bystanders::default())
}

/// То же, но с обстановкой вокруг: кто чем был занят в этот миг.
pub fn detect_with(moment: Moment, who: Bystanders<'_>) -> Option<Freeze> {
    // Нехватка памяти идёт первой: она порождает и очередь к диску, и
    // время в драйверах, и лечится совсем не там, где видна.
    if moment.memory_used_share >= MEMORY_TIGHT && moment.compressing_memory {
        return Some(Freeze {
            cause: FreezeCause::MemoryPressure,
            detail: format!(
                "занято {:.0}% памяти, идёт вытеснение в подкачку",
                moment.memory_used_share * 100.0
            ),
            culprits: name_them("Больше всех памяти держали", who.memory, &bytes),
            culprit_names: names_of(who.memory),
        });
    }

    // Драйверы вторые: их время вытесняет обработку ввода, и человек
    // замечает это раньше, чем медленный диск.
    if moment.driver_ratio >= DRIVER_TIME {
        return Some(Freeze {
            cause: FreezeCause::DriverTime,
            detail: format!(
                "{:.0}% процессорного времени ушло в прерывания и отложенные вызовы",
                moment.driver_ratio * 100.0
            ),
            // Виновника здесь нет и быть не может: это время не принадлежит
            // ни одному процессу. Называть кого-то было бы враньём.
            culprits: String::new(),
            culprit_names: Vec::new(),
        });
    }

    // Диск: смотрим именно очередь, а не занятость. Занятый диск — это
    // норма, а вот очередь означает, что запросы уже ждут.
    if moment.disk_queue >= DISK_QUEUE {
        return Some(Freeze {
            cause: FreezeCause::DiskQueue,
            detail: format!(
                "{} запросов в очереди к накопителю при занятости {:.0}%",
                moment.disk_queue,
                moment.disk_busy * 100.0
            ),
            culprits: name_them("В этот момент диск занимали", who.disk, &per_second),
            culprit_names: names_of(who.disk),
        });
    }

    None
}

/// Сколько виновников называем. Больше трёх человек не удержит в голове,
/// а виновник почти всегда первый. Число общее для пересказа и для перехода
/// в список: разойтись им нельзя, иначе отфильтруются не те, кого назвали.
const SHOW: usize = 3;

/// Перечисляет тех, кто был рядом в момент подвисания.
///
/// Пустая строка — обычный исход, и молчание здесь честнее выдумки: бывает,
/// что диск занят самим накопителем или драйвером, и среди процессов
/// виновника попросту нет.
fn name_them(lead: &str, who: &[(String, u64)], show_value: &dyn Fn(u64) -> String) -> String {
    let named: Vec<String> = who
        .iter()
        .take(SHOW)
        .map(|(name, value)| format!("{name} ({})", show_value(*value)))
        .collect();

    if named.is_empty() {
        return String::new();
    }
    format!(" {lead}: {}.", named.join(", "))
}

/// Имена виновников по отдельности — столько же, сколько названо словами.
fn names_of(who: &[(String, u64)]) -> Vec<String> {
    who.iter()
        .take(SHOW)
        .map(|(name, _)| name.clone())
        .collect()
}

fn bytes(value: u64) -> String {
    bamboo_core::Bytes(value).to_string()
}

fn per_second(value: u64) -> String {
    format!("{}/с", bamboo_core::Bytes(value))
}

/// Память подвисаний: копит зафиксированное и умеет пересказать.
#[derive(Default)]
pub struct FreezeLog {
    /// Причина, момент по монотонным часам, подробности и виновники.
    entries: Vec<(FreezeCause, u64, String, String, Vec<String>)>,
}

/// Сколько подвисаний помним. Больше десятка не нужно: важна свежая
/// картина, а не летопись.
const REMEMBER: usize = 10;

impl FreezeLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Учитывает очередной замер.
    ///
    /// Одно и то же подвисание длится несколько замеров подряд, и писать
    /// каждый из них незачем: считаем это одним событием, пока причина
    /// не сменилась и не прошло время.
    pub fn observe(&mut self, moment: Moment, who: Bystanders<'_>, at_ms: u64) -> Option<&Freeze> {
        const SAME_EVENT_MS: u64 = 30_000;

        let freeze = detect_with(moment, who)?;

        if let Some((cause, when, _, _, _)) = self.entries.last() {
            if *cause == freeze.cause && at_ms.saturating_sub(*when) < SAME_EVENT_MS {
                return None;
            }
        }

        self.entries.push((
            freeze.cause,
            at_ms,
            freeze.detail.clone(),
            freeze.culprits.clone(),
            freeze.culprit_names.clone(),
        ));
        if self.entries.len() > REMEMBER {
            self.entries.remove(0);
        }
        None
    }

    /// Имена виновников последнего подвисания.
    ///
    /// Пусто, когда виновника назвать нельзя: у времени в драйверах его нет.
    pub fn last_culprits(&self) -> Vec<String> {
        self.entries
            .last()
            .map(|(_, _, _, _, names)| names.clone())
            .unwrap_or_default()
    }

    /// Сколько подвисаний записано.
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// Пересказ последних подвисаний для человека.
    ///
    /// Пусто — значит система вела себя ровно всё время наблюдения, и это
    /// стоит сказать: отсутствие жалоб тоже сведения.
    pub fn summary(&self, now_ms: u64) -> Option<String> {
        let (cause, when, detail, culprits, _) = self.entries.last()?;

        let ago = now_ms.saturating_sub(*when);
        let ago = if ago < 60_000 {
            format!("{} с назад", ago / 1000)
        } else {
            format!("{} мин назад", ago / 60_000)
        };

        // Если причина повторяется, это важнее одного случая.
        let same = self
            .entries
            .iter()
            .filter(|(other, _, _, _, _)| other == cause)
            .count();
        let repeats = if same > 1 {
            format!(" Такое повторялось {same} раз за время наблюдения.")
        } else {
            String::new()
        };

        Some(format!(
            "Подвисание {ago}: {} — {detail}.{culprits}{repeats} {}",
            cause.name(),
            cause.advice()
        ))
    }
}

/// Собирает обстановку из готовых величин.
///
/// Отдельная функция ради одного: превратить разрозненные числа в один
/// снимок, который потом проверяется тестами целиком.
pub fn moment_from(
    driver_ratio: f64,
    disk_queue: u32,
    disk_busy: f64,
    memory_used: Bytes,
    memory_total: Bytes,
    compressing_memory: bool,
) -> Moment {
    Moment {
        driver_ratio,
        disk_queue,
        disk_busy,
        memory_used_share: if memory_total.as_u64() == 0 {
            0.0
        } else {
            memory_used.as_u64() as f64 / memory_total.as_u64() as f64
        },
        compressing_memory,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_healthy_system_reports_nothing() {
        let calm = Moment {
            driver_ratio: 0.02,
            disk_queue: 1,
            disk_busy: 0.30,
            memory_used_share: 0.60,
            compressing_memory: false,
        };
        assert_eq!(detect(calm), None);
    }

    #[test]
    fn a_long_disk_queue_is_a_freeze() {
        let stuck = Moment {
            disk_queue: 25,
            disk_busy: 1.0,
            ..Default::default()
        };
        let freeze = detect(stuck).unwrap();
        assert_eq!(freeze.cause, FreezeCause::DiskQueue);
        assert!(freeze.detail.contains("25 запросов"), "{}", freeze.detail);
    }

    #[test]
    fn a_busy_disk_without_a_queue_is_not_a_freeze() {
        // Занятый диск — обычное дело: он работает. Подвисание начинается
        // тогда, когда запросы к нему выстраиваются в очередь.
        let busy = Moment {
            disk_queue: 1,
            disk_busy: 1.0,
            ..Default::default()
        };
        assert_eq!(detect(busy), None);
    }

    #[test]
    fn driver_time_is_recognised_and_points_away_from_processes() {
        let drivers = Moment {
            driver_ratio: 0.35,
            ..Default::default()
        };
        let freeze = detect(drivers).unwrap();
        assert_eq!(freeze.cause, FreezeCause::DriverTime);
        // Важно: не отправлять человека искать виновника среди программ.
        assert!(freeze.cause.advice().contains("бесполезно"));
    }

    #[test]
    fn memory_pressure_outranks_everything_else() {
        // Нехватка памяти сама порождает и очередь к диску, и время
        // в драйверах. Назвать надо первопричину.
        let squeezed = Moment {
            driver_ratio: 0.40,
            disk_queue: 30,
            disk_busy: 1.0,
            memory_used_share: 0.97,
            compressing_memory: true,
        };
        assert_eq!(detect(squeezed).unwrap().cause, FreezeCause::MemoryPressure);
    }

    #[test]
    fn tight_memory_without_compression_is_not_yet_pressure() {
        // Занятая память — не беда сама по себе: Windows держит в ней кэш.
        // Бедой она становится, когда начинается вытеснение.
        let tight = Moment {
            memory_used_share: 0.95,
            compressing_memory: false,
            ..Default::default()
        };
        assert_eq!(detect(tight), None);
    }

    #[test]
    fn one_long_freeze_is_recorded_once() {
        // Подвисание длится несколько замеров подряд. Десять записей об
        // одном событии превратили бы журнал в шум.
        let mut log = FreezeLog::new();
        let stuck = Moment {
            disk_queue: 20,
            ..Default::default()
        };

        for tick in 0..5 {
            log.observe(stuck, Bystanders::default(), tick * 1000);
        }
        assert_eq!(log.count(), 1);
    }

    #[test]
    fn a_freeze_after_a_pause_is_a_new_event() {
        let mut log = FreezeLog::new();
        let stuck = Moment {
            disk_queue: 20,
            ..Default::default()
        };

        log.observe(stuck, Bystanders::default(), 0);
        log.observe(stuck, Bystanders::default(), 120_000);
        assert_eq!(log.count(), 2);
    }

    #[test]
    fn the_summary_names_time_cause_and_what_to_do() {
        let mut log = FreezeLog::new();
        log.observe(
            Moment {
                disk_queue: 20,
                disk_busy: 0.95,
                ..Default::default()
            },
            Bystanders::default(),
            0,
        );

        let text = log.summary(45_000).unwrap();
        assert!(text.contains("45 с назад"), "{text}");
        assert!(text.contains("накопитель не успевает"), "{text}");
        assert!(
            text.contains("Придержать диск"),
            "должен быть совет: {text}"
        );
    }

    #[test]
    fn the_culprit_is_captured_at_the_moment_not_looked_up_later() {
        // Ради этого всё и затевалось. Отправлять человека смотреть, кто
        // занимает диск, бесполезно: пока он откроет раздел, подвисание
        // кончится и виновник разойдётся.
        let hogs = [
            ("MsMpEng.exe".to_string(), 40 << 20),
            ("SearchIndexer.exe".to_string(), 12 << 20),
        ];
        let freeze = detect_with(
            Moment {
                disk_queue: 20,
                disk_busy: 0.95,
                ..Default::default()
            },
            Bystanders {
                disk: &hogs,
                memory: &[],
            },
        )
        .unwrap();

        assert!(freeze.culprits.contains("MsMpEng.exe"), "{freeze:?}");
        assert!(freeze.culprits.contains("SearchIndexer.exe"), "{freeze:?}");
        // С числами: без них имя ничего не значит.
        assert!(freeze.culprits.contains("/с"), "{freeze:?}");
    }

    #[test]
    fn driver_time_never_blames_a_process() {
        // Это время не принадлежит ни одному процессу. Назвать кого-то
        // рядом стоящего было бы враньём, а человек пошёл бы его закрывать.
        let hogs = [("chrome.exe".to_string(), 40 << 20)];
        let freeze = detect_with(
            Moment {
                driver_ratio: 0.35,
                ..Default::default()
            },
            Bystanders {
                disk: &hogs,
                memory: &hogs,
            },
        )
        .unwrap();

        assert_eq!(freeze.culprits, "", "драйверам виновника не приписываем");
    }

    #[test]
    fn memory_pressure_names_who_held_the_memory() {
        let hogs = [("chrome.exe".to_string(), 6 << 30)];
        let freeze = detect_with(
            Moment {
                memory_used_share: 0.97,
                compressing_memory: true,
                ..Default::default()
            },
            Bystanders {
                disk: &[],
                memory: &hogs,
            },
        )
        .unwrap();

        assert!(freeze.culprits.contains("chrome.exe"), "{freeze:?}");
    }

    #[test]
    fn no_named_culprit_is_better_than_a_made_up_one() {
        // Диск бывает занят изнутри: сборкой мусора у SSD, драйвером.
        // Тогда среди процессов виновника нет, и молчать честнее.
        let freeze = detect_with(
            Moment {
                disk_queue: 20,
                ..Default::default()
            },
            Bystanders::default(),
        )
        .unwrap();

        assert_eq!(freeze.culprits, "");
        // Но совет обязан объяснить и этот случай.
        assert!(freeze.cause.advice().contains("не назван"));
    }

    #[test]
    fn only_a_few_culprits_are_named() {
        let many: Vec<(String, u64)> = (0..10)
            .map(|n| (format!("процесс{n}.exe"), 10 << 20))
            .collect();
        let freeze = detect_with(
            Moment {
                disk_queue: 20,
                ..Default::default()
            },
            Bystanders {
                disk: &many,
                memory: &[],
            },
        )
        .unwrap();

        assert!(freeze.culprits.contains("процесс0.exe"), "{freeze:?}");
        assert!(!freeze.culprits.contains("процесс5.exe"), "{freeze:?}");
    }

    #[test]
    fn repeats_are_counted_because_they_matter_more() {
        let mut log = FreezeLog::new();
        let stuck = Moment {
            disk_queue: 20,
            ..Default::default()
        };
        for round in 0..3 {
            log.observe(stuck, Bystanders::default(), round * 120_000);
        }

        let text = log.summary(400_000).unwrap();
        assert!(text.contains("повторялось 3 раз"), "{text}");
    }

    #[test]
    fn the_named_culprits_match_the_ones_offered_for_filtering() {
        // Расхождение между тем, кого назвали словами, и тем, на кого
        // отфильтруется список, — прямой обман: человек нажимает «показать
        // этих» и видит других.
        let many: Vec<(String, u64)> = (0..10)
            .map(|n| (format!("процесс{n}.exe"), 10 << 20))
            .collect();
        let freeze = detect_with(
            Moment {
                disk_queue: 20,
                ..Default::default()
            },
            Bystanders {
                disk: &many,
                memory: &[],
            },
        )
        .unwrap();

        assert_eq!(freeze.culprit_names.len(), SHOW);
        for name in &freeze.culprit_names {
            assert!(freeze.culprits.contains(name.as_str()), "{freeze:?}");
        }
    }

    #[test]
    fn the_log_hands_over_the_last_culprits_by_name() {
        let mut log = FreezeLog::new();
        let hogs = [("MsMpEng.exe".to_string(), 40 << 20)];
        log.observe(
            Moment {
                disk_queue: 20,
                ..Default::default()
            },
            Bystanders {
                disk: &hogs,
                memory: &[],
            },
            0,
        );

        assert_eq!(log.last_culprits(), vec!["MsMpEng.exe".to_string()]);
    }

    #[test]
    fn there_is_nothing_to_filter_on_before_any_freeze() {
        assert!(FreezeLog::new().last_culprits().is_empty());
    }

    #[test]
    fn an_empty_log_has_nothing_to_say() {
        assert_eq!(FreezeLog::new().summary(1000), None);
    }

    #[test]
    fn every_cause_has_workable_advice() {
        for cause in [
            FreezeCause::DiskQueue,
            FreezeCause::DriverTime,
            FreezeCause::MemoryPressure,
        ] {
            let advice = cause.advice();
            assert!(advice.len() > 60, "совет слишком общий: {advice}");
            assert!(
                !advice.to_lowercase().contains("перезагруз"),
                "бесполезный совет: {advice}"
            );
        }
    }
}
