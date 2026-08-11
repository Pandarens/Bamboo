//! Запись наблюдения за одной программой (ТЗ, раздел 10.4).
//!
//! Отвечает на вопрос, на который обычный диспетчер задач не отвечает
//! никак: «в игре проседает — чего не хватает». Мгновенные числа тут
//! бесполезны, потому что просадка длится секунду, а к моменту, когда
//! человек переключится в диспетчер, всё уже прошло. Нужна запись за всё
//! время игры и разбор постфактум.
//!
//! Разбор строится на одном простом наблюдении: узкое место — это то, что
//! упёрлось в потолок, пока остальное простаивало. Видеокарта под сотню при
//! свободном процессоре означает одно, обратное — совсем другое, а нехватка
//! памяти с вытеснением в подкачку не похожа ни на то, ни на другое.
//!
//! Чего здесь принципиально нет: попыток назвать «оценку производительности»
//! одним числом. Такое число ничего не значит и ни к чему не ведёт.

use bamboo_core::Bytes;

/// Один замер.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    /// Сколько прошло от начала записи.
    pub at_ms: u64,
    /// Доля процессора всей машиной, проценты.
    pub cpu_percent: f32,
    /// Память программы.
    pub memory: Bytes,
    /// Чтение и запись вместе, байт в секунду.
    pub disk_per_second: u64,
    /// Загрузка видеокарты программой, проценты. `None` — счётчики
    /// недоступны на этой машине.
    pub gpu_percent: Option<f32>,
    /// Занятость памяти всей машины, 0..1: просадки от нехватки памяти
    /// видны только по системе целиком, а не по одной программе.
    pub memory_pressure: f64,
}

/// Что мешало.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bottleneck {
    /// Упирается в видеокарту.
    Gpu,
    /// Упирается в процессор.
    Cpu,
    /// Не хватает оперативной памяти.
    Memory,
    /// Ждёт диска.
    Disk,
    /// Ничто не упиралось: запас был везде.
    Nothing,
}

impl Bottleneck {
    pub fn name(self) -> &'static str {
        use bamboo_core::pick;
        match self {
            Bottleneck::Gpu => pick("упирается в видеокарту", "limited by the graphics card"),
            Bottleneck::Cpu => pick("упирается в процессор", "limited by the processor"),
            Bottleneck::Memory => pick("не хватает оперативной памяти", "not enough memory"),
            Bottleneck::Disk => pick("ждёт диска", "waiting on the disk"),
            Bottleneck::Nothing => pick("запас был везде", "everything had headroom"),
        }
    }

    /// Что это значит и что с этим делать.
    pub fn advice(self) -> &'static str {
        use bamboo_core::pick;
        match self {
            Bottleneck::Gpu => pick(
                "Видеокарта почти всё время была загружена под предел, а процессор \
                 простаивал. Это обычное и здоровое состояние для игры: она берёт \
                 от видеокарты всё, что та даёт. Кадров прибавит только снижение \
                 настроек графики или разрешения — закрывать программы бесполезно, \
                 они тут ни при чём.",
                "The graphics card was at its limit almost the whole time while the \
                 processor idled. For a game that is normal and healthy: it takes \
                 everything the card will give. Only lowering graphics settings or \
                 resolution will add frames — closing programs is pointless, they \
                 have nothing to do with it.",
            ),
            Bottleneck::Cpu => pick(
                "Процессор был загружен, а видеокарта недогружена — значит она \
                 ждала, пока ей подготовят кадр. Вот здесь закрытие лишних программ \
                 действительно помогает, потому что они отбирают то самое \
                 процессорное время. Снижение настроек графики, наоборот, почти \
                 ничего не даст.",
                "The processor was loaded while the graphics card was not — meaning \
                 the card was waiting for a frame to be prepared for it. Here closing \
                 unnecessary programs genuinely helps, because they take away that \
                 very processor time. Lowering graphics settings, on the contrary, \
                 will give almost nothing.",
            ),
            Bottleneck::Memory => pick(
                "Памяти не хватало, и Windows вытесняла страницы на диск. Просадки \
                 при этом рваные: программа замирает, пока её данные возвращаются \
                 с накопителя. Помогает только освобождение памяти — закрыть \
                 вкладки браузера и всё лишнее до запуска.",
                "There was not enough memory, and Windows was pushing pages out to \
                 disk. The stutters are ragged because of it: the program freezes \
                 while its data comes back from the drive. Only freeing memory helps — \
                 close browser tabs and anything unnecessary before starting.",
            ),
            Bottleneck::Disk => pick(
                "Программа заметную часть времени ждала накопитель. У игр так \
                 выглядит подгрузка уровня, у остального — работа с крупными \
                 файлами. Если это повторяется не только при загрузке, стоит \
                 посмотреть в разделе «Диск», кто ещё его занимает.",
                "The program spent a noticeable share of the time waiting on the \
                 drive. In games that is what level streaming looks like; elsewhere \
                 it is work with large files. If it repeats outside loading screens, \
                 look in the «Disk» section at what else is using it.",
            ),
            Bottleneck::Nothing => pick(
                "Ничто не упиралось в потолок: и процессор, и видеокарта, и память \
                 имели запас. Если просадки при этом были, дело не в нехватке \
                 ресурсов — так выглядят ограничение кадров, работа драйвера или \
                 сама программа.",
                "Nothing hit a ceiling: the processor, the graphics card and memory \
                 all had headroom. If there were stutters anyway, it is not a shortage \
                 of resources — that is what a frame cap, a driver, or the program \
                 itself looks like.",
            ),
        }
    }
}

/// Выше этого считаем, что упёрлось в потолок.
///
/// Не сто процентов: до ровной сотни загрузка не доходит почти никогда,
/// а девяносто уже означает, что запаса нет.
const AT_THE_LIMIT: f32 = 90.0;

/// Ниже этого считаем, что запас был.
const HAS_ROOM: f32 = 70.0;

/// Занятость памяти, после которой начинается вытеснение.
const MEMORY_TIGHT: f64 = 0.92;

/// Сколько времени надо ждать диска, чтобы это стало заметно, — доля
/// замеров, в которых диск работал заметно.
const DISK_SHARE: f64 = 0.30;

/// Заметная работа с диском, байт в секунду.
const DISK_BUSY: u64 = 20 * 1024 * 1024;

/// Итог записи.
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub bottleneck: Bottleneck,
    /// Пересказ числами: то же, что на графике, только словами.
    pub summary: String,
}

/// Сводка по одной величине.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Range {
    pub average: f64,
    pub peak: f64,
}

fn summarise(values: impl Iterator<Item = f64>) -> Range {
    let mut count = 0usize;
    let mut total = 0.0;
    let mut peak = 0.0_f64;

    for value in values {
        count += 1;
        total += value;
        peak = peak.max(value);
    }

    Range {
        average: if count == 0 {
            0.0
        } else {
            total / count as f64
        },
        peak,
    }
}

/// Разбирает запись и называет узкое место.
///
/// Порядок проверки не случаен: нехватка памяти идёт первой, потому что она
/// портит картину всем остальным — процессор при вытеснении простаивает,
/// а диск, наоборот, занят, и без этой проверки виновником назвался бы диск.
pub fn analyse(samples: &[Sample]) -> Option<Verdict> {
    // Меньше десяти замеров — это меньше десяти секунд наблюдения.
    // На таком отрезке любой вывод был бы гаданием.
    if samples.len() < 10 {
        return None;
    }

    let cpu = summarise(samples.iter().map(|s| f64::from(s.cpu_percent)));
    let memory = summarise(samples.iter().map(|s| s.memory.as_u64() as f64));
    let disk = summarise(samples.iter().map(|s| s.disk_per_second as f64));
    let gpu_known: Vec<f64> = samples
        .iter()
        .filter_map(|s| s.gpu_percent.map(f64::from))
        .collect();
    let gpu = (!gpu_known.is_empty()).then(|| summarise(gpu_known.iter().copied()));

    let squeezed = samples
        .iter()
        .filter(|s| s.memory_pressure >= MEMORY_TIGHT)
        .count() as f64
        / samples.len() as f64;
    let waiting_disk = samples
        .iter()
        .filter(|s| s.disk_per_second >= DISK_BUSY)
        .count() as f64
        / samples.len() as f64;

    let bottleneck = pick(cpu, gpu, squeezed, waiting_disk);

    let mut parts = vec![
        format!(
            "процессор: в среднем {:.0}%, пик {:.0}%",
            cpu.average, cpu.peak
        ),
        format!(
            "память: в среднем {}, пик {}",
            Bytes(memory.average as u64),
            Bytes(memory.peak as u64)
        ),
    ];
    match gpu {
        Some(gpu) => parts.push(format!(
            "видеокарта: в среднем {:.0}%, пик {:.0}%",
            gpu.average, gpu.peak
        )),
        // Молчать нельзя: человек ждал графика видеокарты и должен знать,
        // почему его нет.
        None => parts.push(
            "видеокарта: счётчики недоступны в этой системе, поэтому её нагрузку              Bamboo не измерял"
                .to_string(),
        ),
    }
    if disk.peak > 0.0 {
        parts.push(format!("диск: пик {}/с", Bytes(disk.peak as u64)));
    }

    Some(Verdict {
        bottleneck,
        summary: format!(
            "За {} наблюдения — {}. {}",
            spell_duration(samples.last().map_or(0, |s| s.at_ms)),
            parts.join(", "),
            bottleneck.advice(),
        ),
    })
}

fn pick(cpu: Range, gpu: Option<Range>, squeezed: f64, waiting_disk: f64) -> Bottleneck {
    // Память первой: при вытеснении процессор простаивает, а диск занят,
    // и без этой проверки виновником назвался бы диск.
    if squeezed > 0.25 {
        return Bottleneck::Memory;
    }

    if let Some(gpu) = gpu {
        // Видеокарта в потолке при свободном процессоре — самый частый
        // и самый здоровый случай для игры.
        if gpu.average >= f64::from(AT_THE_LIMIT) && cpu.average < f64::from(HAS_ROOM) {
            return Bottleneck::Gpu;
        }
        // Обратное: процессор занят, а видеокарта ждёт от него кадра.
        if cpu.average >= f64::from(AT_THE_LIMIT) && gpu.average < f64::from(HAS_ROOM) {
            return Bottleneck::Cpu;
        }
    } else if cpu.average >= f64::from(AT_THE_LIMIT) {
        // Без данных о видеокарте про процессор всё равно можно сказать.
        return Bottleneck::Cpu;
    }

    if waiting_disk >= DISK_SHARE {
        return Bottleneck::Disk;
    }

    Bottleneck::Nothing
}

fn spell_duration(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds < 60 {
        format!("{seconds} с")
    } else {
        format!("{} мин {} с", seconds / 60, seconds % 60)
    }
}

/// Готовит значения для графика: приводит к долям 0..1 и прореживает
/// до нужного числа точек.
///
/// Прореживаем по максимуму в окне, а не по среднему. Среднее сглаживает
/// ровно то, ради чего график и рисуют: короткий всплеск, из-за которого
/// и была просадка, в среднем растворяется без следа.
pub fn to_chart(values: &[f64], points: usize, top: f64) -> Vec<f32> {
    if values.is_empty() || points == 0 {
        return Vec::new();
    }
    let top = if top <= 0.0 { 1.0 } else { top };

    let mut out = Vec::with_capacity(points.min(values.len()));
    let window = values.len().div_ceil(points).max(1);

    for chunk in values.chunks(window) {
        let peak = chunk.iter().copied().fold(0.0_f64, f64::max);
        out.push((peak / top).clamp(0.0, 1.0) as f32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at_s: u64, cpu: f32, gpu: Option<f32>) -> Sample {
        Sample {
            at_ms: at_s * 1000,
            cpu_percent: cpu,
            memory: Bytes(2 << 30),
            disk_per_second: 0,
            gpu_percent: gpu,
            memory_pressure: 0.5,
        }
    }

    fn run(count: u64, cpu: f32, gpu: Option<f32>) -> Vec<Sample> {
        (0..count).map(|n| sample(n, cpu, gpu)).collect()
    }

    #[test]
    fn a_short_recording_yields_no_verdict() {
        // Три секунды наблюдения — это не наблюдение. Вывод на таком
        // отрезке был бы гаданием, а гадать нельзя.
        assert_eq!(analyse(&run(3, 99.0, Some(99.0))), None);
    }

    #[test]
    fn a_loaded_gpu_with_an_idle_cpu_means_the_gpu() {
        let verdict = analyse(&run(60, 30.0, Some(97.0))).unwrap();
        assert_eq!(verdict.bottleneck, Bottleneck::Gpu);
        // Совет обязан отговорить от бесполезного: закрывать программы
        // при упоре в видеокарту незачем.
        assert!(verdict.summary.contains("настроек графики"), "{verdict:?}");
    }

    #[test]
    fn a_loaded_cpu_with_an_idle_gpu_means_the_cpu() {
        let verdict = analyse(&run(60, 96.0, Some(40.0))).unwrap();
        assert_eq!(verdict.bottleneck, Bottleneck::Cpu);
        assert!(verdict.summary.contains("закрытие лишних программ"));
    }

    #[test]
    fn memory_pressure_outranks_everything_else() {
        // При вытеснении процессор простаивает, а диск занят. Без проверки
        // памяти виновником назвался бы диск — и человек пошёл бы чинить
        // не то.
        let squeezed: Vec<Sample> = (0..60)
            .map(|n| Sample {
                memory_pressure: 0.97,
                disk_per_second: 100 << 20,
                ..sample(n, 20.0, Some(30.0))
            })
            .collect();

        let verdict = analyse(&squeezed).unwrap();
        assert_eq!(verdict.bottleneck, Bottleneck::Memory);
    }

    #[test]
    fn steady_disk_work_is_recognised() {
        let busy: Vec<Sample> = (0..60)
            .map(|n| Sample {
                disk_per_second: 50 << 20,
                ..sample(n, 20.0, Some(30.0))
            })
            .collect();
        assert_eq!(analyse(&busy).unwrap().bottleneck, Bottleneck::Disk);
    }

    #[test]
    fn plenty_of_room_everywhere_is_said_plainly() {
        let verdict = analyse(&run(60, 25.0, Some(35.0))).unwrap();
        assert_eq!(verdict.bottleneck, Bottleneck::Nothing);
        // И это тоже ответ: значит, дело не в нехватке ресурсов.
        assert!(verdict.summary.contains("не в нехватке"), "{verdict:?}");
    }

    #[test]
    fn a_missing_gpu_counter_is_admitted_not_hidden() {
        // Человек ждал графика видеокарты. Если его нет, он должен узнать
        // почему, а не решить, что видеокарта простаивала.
        let verdict = analyse(&run(60, 50.0, None)).unwrap();
        assert!(
            verdict.summary.contains("счётчики недоступны"),
            "{verdict:?}"
        );
    }

    #[test]
    fn a_loaded_cpu_is_named_even_without_gpu_data() {
        assert_eq!(
            analyse(&run(60, 96.0, None)).unwrap().bottleneck,
            Bottleneck::Cpu
        );
    }

    #[test]
    fn the_summary_carries_the_numbers_from_the_chart() {
        let verdict = analyse(&run(60, 42.0, Some(88.0))).unwrap();
        assert!(verdict.summary.contains("42%"), "{verdict:?}");
        assert!(verdict.summary.contains("88%"), "{verdict:?}");
        assert!(verdict.summary.contains("59 с"), "{verdict:?}");
    }

    #[test]
    fn a_chart_keeps_the_spikes() {
        // Ради этого график и рисуют. Короткий всплеск — то самое место,
        // где была просадка; усреднение стёрло бы его без следа.
        let mut values = vec![10.0; 100];
        values[42] = 100.0;

        let chart = to_chart(&values, 10, 100.0);
        assert_eq!(chart.len(), 10);
        assert!(
            chart.iter().any(|point| *point > 0.99),
            "всплеск потерян: {chart:?}"
        );
    }

    #[test]
    fn a_chart_of_nothing_is_empty() {
        assert!(to_chart(&[], 10, 100.0).is_empty());
        assert!(to_chart(&[1.0], 0, 100.0).is_empty());
    }

    #[test]
    fn chart_values_stay_within_bounds() {
        // Загрузка видеокарты складывается по движкам и законно бывает
        // больше сотни. Полоска за краем графика — не то, что нужно.
        let chart = to_chart(&[250.0, -5.0], 2, 100.0);
        assert!(
            chart.iter().all(|point| (0.0..=1.0).contains(point)),
            "{chart:?}"
        );
    }

    #[test]
    fn every_bottleneck_has_a_name_and_workable_advice() {
        for what in [
            Bottleneck::Gpu,
            Bottleneck::Cpu,
            Bottleneck::Memory,
            Bottleneck::Disk,
            Bottleneck::Nothing,
        ] {
            assert!(!what.name().is_empty());
            assert!(what.advice().len() > 80, "совет слишком общий: {what:?}");
            assert!(
                !what.advice().to_lowercase().contains("перезагруз"),
                "бесполезный совет: {what:?}"
            );
        }
    }
}
