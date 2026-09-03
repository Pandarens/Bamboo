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
//! кончилась и всё ушло в подкачку. Все три различимы снаружи — в отличие
//! от «компьютер тормозит», которое ничего не значит.
//!
//! Но у списка причин есть общая слабость: каждую надо предугадать. Причина,
//! о которой не подумали, не попадает ни в один признак, и подвисание
//! проходит мимо. Ровно так и вышло на живой машине — подвисания шли
//! от нехватки памяти, а признак был настроен на другой порог, и журнал
//! оставался пустым, пока человек своими глазами видел рывки.
//!
//! Поэтому есть и четвёртая проверка, устроенная наоборот: она меряет
//! не причину, а сам простой. Bamboo просит поспать положенный интервал
//! и смотрит, сколько прошло на самом деле. Всё сверх запрошенного — время,
//! которое система не отдавала управление. Угадывать тут нечего, и ловится
//! в том числе то, чего в списке причин нет.

use bamboo_core::Bytes;

/// Доля времени в DPC и прерываниях, после которой отзывчивость страдает.
///
/// Это работа драйверов, и она вытесняет всё остальное, включая обработку
/// ввода. Десять процентов — та граница, где человек начинает замечать
/// рывки курсора.
const DRIVER_TIME: f64 = 0.10;

/// Длина очереди к накопителю, при которой запросы уже ждут ощутимо.
///
/// Признак слабый, и держится он только как запасной. Длина очереди
/// сама по себе мало что значит: у NVMe очередей десятки по тысяче команд,
/// и глубокая очередь там означает хорошую пропускную способность,
/// а не беду. Настоящий признак — задержка ниже.
const DISK_QUEUE: u32 = 8;

/// Задержка одной операции, при которой ожидание становится заметным.
///
/// Пятьдесят миллисекунд. Опора — то, на что накопители способны:
/// твердотельный отвечает быстрее миллисекунды, механический тратит
/// от пяти до пятнадцати на подвод головки. Пятьдесят означает, что запрос
/// стоял в очереди за другими, а не обслуживался, — и всё, что ждёт диска,
/// стоит вместе с ним.
const DISK_SLOW_MS: f64 = 50.0;

/// Занятость, ниже которой длина очереди ничего не значит.
const DISK_BUSY_ENOUGH: f64 = 0.80;

/// Насколько занятой должна быть память, чтобы Windows начала вытеснять
/// страницы в подкачку и всё замерло.
const MEMORY_TIGHT: f64 = 0.92;

/// Сколько страниц в секунду поднимается с диска, чтобы это стало заметно
/// человеку.
///
/// Тридцать два мегабайта в секунду, то есть восемь тысяч страниц.
///
/// Первая редакция ставила сюда тысячу страниц — четыре мегабайта, — и это
/// оказалось грубой ошибкой, которую видно только на длинном ряду. Порог
/// брался из одного сорокапятисекундного замера: фон около двухсот страниц,
/// всплеск до 6519. За тридцать один час работы картина другая: медиана
/// **девять** мегабайт в секунду, девяностый процентиль пятьдесят два.
/// То есть порог стоял ниже медианы обычной работы машины и срабатывал
/// на ровном месте — 308 записей за сутки с небольшим.
///
/// Тридцать два мегабайта — это верхняя пятая часть замеров. Урок
/// на будущее: порог по одному короткому замеру — это не измерение,
/// а угадывание с видом измерения.
const PAGING_STALL: f64 = 8192.0;

/// Занятость памяти, ниже которой чтение из подкачки — не толкотня.
///
/// Само по себе чтение с диска ничего не доказывает: запуск программы тоже
/// читает страницы её образа, и это здоровое поведение, а не нехватка
/// памяти. Признаком толкотни чтение становится только тогда, когда память
/// уже плотно занята и вытеснять приходится живое.
const MEMORY_STRAINED: f64 = 0.80;

/// Насколько система должна опоздать, чтобы это было подвисанием.
///
/// Полсекунды. Ниже человек не назовёт это подвисанием, а выше — назовёт
/// обязательно. Порог не гадательный: замер на живой машине дал перелёт
/// сна в 0,3 мс по медиане и 1,3 мс в худшем случае из шестидесяти. Между
/// шумом и порогом — почти четыре сотни раз, так что ложных срабатываний
/// здесь не будет.
const STALL_NOTICED_MS: u64 = 500;

/// Загрузка процессора, при которой очереди на выполнение уже мешают.
///
/// Одной этой загрузки мало, и это принципиально. Сборка проекта занимает
/// сто процентов процессора и подвисанием не является: работа идёт, машина
/// отвечает. Поэтому загрузка называется причиной только вместе
/// с измеренным простоем — тогда видно, что процессор не работал,
/// а не пускал.
const CPU_CROWDED: f64 = 0.90;

/// Доля номинальной частоты, ниже которой процессор явно придержан.
///
/// Семьдесят процентов. Опора — замер на живой машине: под обычной
/// нагрузкой процессор шёл на 150–165% номинала, то есть в ускорении.
/// Падение ниже семидесяти — это уже не «не разогнался», а «придержан»:
/// так бывает при перегреве, упоре в предел питания или схеме
/// электропитания, выставленной в экономию.
const FREQUENCY_HELD_BACK: f64 = 0.70;

/// Загрузка, ниже которой о сброшенной частоте говорить нельзя.
///
/// На простое процессор снижает частоту нарочно, ради экономии, и это
/// здоровое поведение. Беда — когда частота низкая, а процессор при этом
/// просят работать.
const CPU_WORKING: f64 = 0.50;

/// Из-за чего подвисло.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreezeCause {
    /// Накопитель не успевает, запросы стоят в очереди.
    DiskQueue,
    /// Драйверы съели процессорное время.
    DriverTime,
    /// Память кончилась, идёт вытеснение в подкачку.
    MemoryPressure,
    /// Процессор занят целиком, и очередь на выполнение мешает.
    CpuSaturated,
    /// Процессор работает на пониженной частоте.
    Throttled,
    /// Система не отвечала, а причина не опознана.
    ///
    /// Единственная причина, которая измеряется, а не выводится. Остальные
    /// три — это признаки, по которым подвисание предполагается: очередь
    /// к диску, время в драйверах, чтение из подкачки. Каждый признак
    /// приходится угадывать заранее, и подвисание от причины, которую
    /// не предусмотрели, проходит мимо всех трёх.
    ///
    /// Здесь меряется сам простой: Bamboo просит поспать секунду и смотрит,
    /// сколько прошло на самом деле. Проспали три — система две секунды
    /// не отвечала, и неважно почему. Это ловит и то, чего в списке нет:
    /// зависший драйвер, проверку антивируса, тепловой сброс частоты,
    /// торможение виртуальной машины.
    Unresponsive,
}

impl FreezeCause {
    /// Устойчивый ключ для хранения на диске.
    ///
    /// Отдельно от `name`, и это не дублирование. Название переводится
    /// и переписывается, а по ключу считают, сколько подвисаний какой
    /// причины было за месяц. Совпади они — смена языка разбила бы
    /// подсчёт надвое, а правка формулировки потеряла бы прошлое.
    pub fn storage_key(self) -> &'static str {
        match self {
            FreezeCause::DiskQueue => "disk",
            FreezeCause::DriverTime => "drivers",
            FreezeCause::MemoryPressure => "memory",
            FreezeCause::CpuSaturated => "cpu",
            FreezeCause::Throttled => "throttle",
            FreezeCause::Unresponsive => "unknown",
        }
    }

    /// Разбирает ключ обратно. Незнакомый — `None`: молча подменять
    /// нехватку памяти нехваткой диска нельзя.
    pub fn from_storage_key(key: &str) -> Option<FreezeCause> {
        Some(match key {
            "disk" => FreezeCause::DiskQueue,
            "drivers" => FreezeCause::DriverTime,
            "memory" => FreezeCause::MemoryPressure,
            "cpu" => FreezeCause::CpuSaturated,
            "throttle" => FreezeCause::Throttled,
            "unknown" => FreezeCause::Unresponsive,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        use bamboo_core::pick;
        match self {
            FreezeCause::DiskQueue => pick("накопитель не успевает", "the drive cannot keep up"),
            FreezeCause::DriverTime => {
                pick("драйверы заняли процессор", "drivers took the processor")
            }
            FreezeCause::MemoryPressure => pick("закончилась оперативная память", "memory ran out"),
            FreezeCause::CpuSaturated => {
                pick("процессор занят целиком", "the processor is fully busy")
            }
            FreezeCause::Throttled => pick(
                "процессор работает на пониженной частоте",
                "the processor is running at a reduced clock",
            ),
            FreezeCause::Unresponsive => pick(
                "система не отвечала, причина не определилась",
                "the system stopped responding, cause undetermined",
            ),
        }
    }

    /// Что с этим делать. Совет обязан быть выполнимым.
    pub fn advice(self) -> &'static str {
        use bamboo_core::pick;
        match self {
            FreezeCause::DiskQueue => pick(
                "Запросы к диску встали в очередь, и всё, что ждёт диска, замирает. \
                 Если виновник назван выше, его можно придержать: правая кнопка \
                 на строке процесса, «Придержать диск». Если не назван, диск занят \
                 изнутри — антивирусом, индексатором поиска или самим накопителем, \
                 и остаётся только переждать.",
                "Disk requests queued up, and everything waiting on the disk freezes. \
                 If a culprit is named above, it can be held back: right-click its \
                 row and pick «Hold back the disk». If none is named, the disk is \
                 busy from the inside — antivirus, the search indexer, or the drive \
                 itself — and all you can do is wait it out.",
            ),
            FreezeCause::DriverTime => pick(
                "Процессор ушёл в обработку прерываний — это работа драйверов, \
                 а не программ. Чаще всего виноват драйвер сети, звука или \
                 видеокарты. Среди процессов виновника искать бесполезно: \
                 помогает обновление драйверов, а не закрытие программ.",
                "The processor went into interrupt handling — that is drivers at \
                 work, not programs. Usually it is the network, sound or graphics \
                 driver. Looking for a culprit among processes is pointless: \
                 updating drivers helps, closing programs does not.",
            ),
            FreezeCause::MemoryPressure => pick(
                "Свободной памяти не осталось, и Windows вытесняет страницы \
                 на диск. Отсюда и рывки: программа ждёт, пока её данные \
                 вернутся с накопителя. Закройте лишние вкладки и программы — \
                 это единственное, что помогает по-настоящему.",
                "There is no free memory left, and Windows is pushing pages out \
                 to disk. Hence the jerkiness: a program waits while its data \
                 comes back from the drive. Close spare tabs and programs — that \
                 is the only thing that genuinely helps.",
            ),
            FreezeCause::CpuSaturated => pick(
                "Процессор занят целиком, и очередь на выполнение дошла                  до того, что обработка ввода стала ждать. Само по себе это                  не беда — сборка проекта тоже занимает процессор полностью,                  и никто не страдает. Здесь беда: простой измерен. Если                  виновник назван выше и его работа не срочная, помогает                  «Экономичный режим» по правой кнопке на строке процесса:                  он уступит процессор тому, что на переднем плане.",
                "The processor is fully busy, and the run queue has grown to                  where input handling waits. That alone is not trouble —                  building a project also takes the whole processor and nobody                  suffers. Here it is trouble: the stall was measured. If                  a culprit is named above and its work is not urgent, «Eco                  mode» from the row's right-click menu helps: it will yield                  the processor to whatever is in the foreground.",
            ),
            FreezeCause::Throttled => pick(
                "Процессор работает на пониженной частоте: его просят                  работать, а он не может. Причин три, и все проверяемые.                  Перегрев — самая частая: помогает чистка от пыли и замена                  термопасты. Схема электропитания, выставленная в экономию, —                  проверяется в параметрах питания Windows. Упор в предел                  питания — на ноутбуке от слабого блока, на настольном                  от блока не по мощности. Программы тут ни при чём, и                  закрывать их бесполезно.",
                "The processor is running at a reduced clock: it is being                  asked to work and cannot. There are three causes, all                  checkable. Overheating is the most common: dust removal and                  fresh thermal paste help. A power plan set to saving — check                  Windows power options. Hitting a power limit — on a laptop                  from a weak adapter, on a desktop from an underrated supply.                  Programs have nothing to do with it, and closing them will                  not help.",
            ),
            FreezeCause::Unresponsive => pick(
                "Простой измерен секундомером: система не отвечала столько, \
                 сколько написано выше. А вот причина не опознана — ни диск, \
                 ни драйверы, ни память в этот момент за порог не вышли. \
                 Такое бывает от проверки антивирусом, зависшего драйвера, \
                 сброса частоты по нагреву. Если это повторяется, помогает \
                 запись сеанса: она пишет нагрузку раз в секунду, и по ней \
                 видно, что менялось вокруг подвисания.",
                "The stall was measured with a stopwatch: the system did not \
                 respond for as long as shown above. The cause, though, is not \
                 identified — neither the disk, nor drivers, nor memory crossed \
                 a threshold at that moment. This happens with antivirus scans, \
                 a hung driver, or thermal throttling. If it repeats, record \
                 a session: it samples the load every second, and shows what \
                 was changing around the stall.",
            ),
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
    /// Сколько занимает одна операция на самом медленном накопителе, мс.
    /// Ноль — операций не было; это не «мгновенно», а «нечего мерить».
    pub disk_latency_ms: f64,
    /// Загрузка процессора всей машины, 0..1.
    pub cpu_busy: f64,
    /// Доля номинальной частоты процессора: 1.0 — номинал, больше —
    /// ускорение. `None` — счётчика в системе нет, и тогда о сбросе
    /// частоты мы просто молчим, а не выдумываем.
    pub cpu_frequency: Option<f64>,
    /// Работает ли машина от батареи. На батарее пониженная частота —
    /// это не поломка, а ровно то, чего человек и просил.
    pub on_battery: bool,
    /// Доля занятой оперативной памяти, 0..1.
    pub memory_used_share: f64,
    /// Идёт ли вытеснение в подкачку прямо сейчас.
    pub compressing_memory: bool,
    /// Сколько страниц в секунду поднимается с диска.
    ///
    /// Главный признак нехватки памяти, и добавлен он потому, что доля
    /// занятой памяти оказалась плохим признаком. На машине, где текст
    /// появлялся с задержкой, в момент чтения 6519 страниц за секунду было
    /// занято 87% — ниже прежнего порога в 92%, так что подвисание, которое
    /// человек видел глазами, не попадало в журнал вовсе.
    pub paging_rate: f64,
    /// На сколько система опоздала отдать управление, миллисекунды.
    ///
    /// Единственное число здесь, которое не признак, а само подвисание:
    /// сколько времени сверх запрошенного заняло ожидание. Остальные поля —
    /// причины, которые пришлось предугадать; это — следствие, и оно ловит
    /// в том числе причины, которых нет в списке.
    pub stall_ms: u64,
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
    /// Кто занимал процессор: имя и целые проценты.
    pub cpu: &'a [(String, u64)],
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
    //
    // Признака два, и они про разное. Чтение из подкачки — это само
    // подвисание, измеренное напрямую: страницы поднимаются с диска,
    // и программа стоит, пока они не придут. Вытеснение при почти полной
    // памяти — состояние, в котором подвисания неизбежны, даже если прямо
    // в этот миг чтения нет.
    let thrashing =
        moment.paging_rate >= PAGING_STALL && moment.memory_used_share >= MEMORY_STRAINED;
    let squeezed = moment.memory_used_share >= MEMORY_TIGHT && moment.compressing_memory;
    if thrashing || squeezed {
        return Some(Freeze {
            cause: FreezeCause::MemoryPressure,
            // Говорим то, что намерили. Когда есть чтение с диска, называем
            // именно его: «занято 87%» человека не убеждает, а «поднято
            // 25 МБ из подкачки за секунду» объясняет застывший курсор.
            detail: if thrashing {
                format!(
                    "занято {:.0}% памяти, с диска поднято {:.0} МБ за секунду",
                    moment.memory_used_share * 100.0,
                    moment.paging_rate * 4.0 / 1024.0
                )
            } else {
                format!(
                    "занято {:.0}% памяти, идёт вытеснение в подкачку",
                    moment.memory_used_share * 100.0
                )
            },
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

    // Диск: смотрим, сколько занимает одна операция, и лишь во вторую
    // очередь — длину очереди. Занятый диск сам по себе норма, глубокая
    // очередь на быстром накопителе тоже, а вот операция на пятьдесят
    // миллисекунд означает, что запрос стоял, а не обслуживался.
    // Очередь считается только при занятом накопителе. Из журнала живой
    // машины: «8 запросов в очереди при занятости 5%» — четыре такие записи
    // из девятнадцати. Длина очереди снимается мгновенным замером, и на почти
    // свободном диске она означает всего лишь, что в этот миг прилетела пачка
    // запросов, которую он тут же и разобрал.
    let slow_disk = moment.disk_latency_ms >= DISK_SLOW_MS;
    let queued = moment.disk_queue >= DISK_QUEUE && moment.disk_busy >= DISK_BUSY_ENOUGH;
    if slow_disk || queued {
        return Some(Freeze {
            cause: FreezeCause::DiskQueue,
            detail: if slow_disk {
                format!(
                    "одна операция к накопителю занимала {:.0} мс при занятости {:.0}%",
                    moment.disk_latency_ms,
                    moment.disk_busy * 100.0
                )
            } else {
                format!(
                    "{} запросов в очереди к накопителю при занятости {:.0}%",
                    moment.disk_queue,
                    moment.disk_busy * 100.0
                )
            },
            culprits: name_them("В этот момент диск занимали", who.disk, &per_second),
            culprit_names: names_of(who.disk),
        });
    }

    // Процессор занят целиком — но только вместе с измеренным простоем.
    // Без простоя это просто работа: сборка проекта занимает процессор
    // на сто процентов, и подвисанием это не является. Пара «занят
    // и при этом не пускал» — другое дело.
    if moment.cpu_busy >= CPU_CROWDED && moment.stall_ms >= STALL_NOTICED_MS {
        return Some(Freeze {
            cause: FreezeCause::CpuSaturated,
            detail: format!(
                "процессор занят на {:.0}%, система не отвечала {}",
                moment.cpu_busy * 100.0,
                spell_stall(moment.stall_ms)
            ),
            culprits: name_them("Больше всех процессора занимали", who.cpu, &percent),
            culprit_names: names_of(who.cpu),
        });
    }

    // Сброшенная частота. Три оговорки, и без них признак врал бы
    // постоянно: на простое частота падает нарочно, на батарее — тоже
    // нарочно, а без счётчика говорить не о чем.
    if let Some(frequency) = moment.cpu_frequency {
        if frequency < FREQUENCY_HELD_BACK
            && frequency > 0.0
            && moment.cpu_busy >= CPU_WORKING
            && !moment.on_battery
        {
            return Some(Freeze {
                cause: FreezeCause::Throttled,
                detail: format!(
                    "процессор работает на {:.0}% номинальной частоты при загрузке {:.0}%",
                    frequency * 100.0,
                    moment.cpu_busy * 100.0
                ),
                // Виновника нет: частоту сбрасывает не программа,
                // а сам процессор — от нагрева, предела питания или
                // настройки электропитания. Назвать кого-то из списка
                // значило бы отправить человека закрывать невиновных.
                culprits: String::new(),
                culprit_names: Vec::new(),
            });
        }
    }

    // Последним — сам простой. Он идёт после всех признаков намеренно:
    // когда причина опознана, называть надо её, а не пересказывать факт
    // подвисания, который человек и без нас заметил.
    //
    // Но если ни один признак не сработал, а система всё-таки не отвечала,
    // молчать нельзя. Молчание здесь — худшее из возможного: человек видел
    // подвисание своими глазами, открывает Bamboo и находит пустоту. Ровно
    // так и вышло на машине, где подвисания оказались от нехватки памяти:
    // признак был настроен на другое, и журнал остался пустым.
    if moment.stall_ms >= STALL_NOTICED_MS {
        return Some(Freeze {
            cause: FreezeCause::Unresponsive,
            detail: format!("система не отвечала {}", spell_stall(moment.stall_ms)),
            // Виновника не называем. Мы знаем, что простой был, и не знаем,
            // из-за кого: назвать первого попавшегося из списка памяти
            // значило бы выдать догадку за измерение.
            culprits: String::new(),
            culprit_names: Vec::new(),
        });
    }

    None
}

/// Простой словами: секунды с десятыми, пока их немного.
fn spell_stall(ms: u64) -> String {
    let unit = bamboo_core::pick("с", "s");
    if ms < 10_000 {
        format!("{:.1} {unit}", ms as f64 / 1000.0)
    } else {
        format!("{} {unit}", ms / 1000)
    }
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

fn percent(value: u64) -> String {
    format!("{value}%")
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
    /// Возвращает подвисание, если оно только что записано, — и `None`,
    /// если замер обычный либо продолжает уже записанное. Возврат нужен
    /// тем, кто складывает подвисания на диск: писать каждый замер значило
    /// бы записать одно событие сотню раз.
    ///
    /// Прежняя редакция обещала то же самое сигнатурой, а возвращала `None`
    /// всегда — из-за заимствования, которое мешало отдать ссылку на только
    /// что добавленную запись. Отдаём копию: она невелика, а молчаливо
    /// сломанный возврат стоил того, что о новых подвисаниях снаружи
    /// узнать было нельзя.
    ///
    /// Одно и то же подвисание длится несколько замеров подряд, и писать
    /// каждый из них незачем: считаем это одним событием, пока причина
    /// не сменилась и не прошло время.
    pub fn observe(&mut self, moment: Moment, who: Bystanders<'_>, at_ms: u64) -> Option<Freeze> {
        // Событие и состояние повторяются по-разному.
        //
        // Событие — это когда система действительно встала: простой измерен
        // секундомером. Два таких подряд — два разных случая, и записывать
        // надо оба, разделив их полминутой.
        //
        // Состояние — когда сработал признак, а простоя не было. Тогда мы
        // говорим не «подвисло», а «условия такие, что подвиснет»: память
        // в дефиците, диск еле тянет. Условие держится часами, и повторять
        // его каждые полминуты бессмысленно.
        //
        // Разница обнаружилась на живой машине: за 31 час набралось 327
        // записей, и **ни в одной** простой не превысил двух миллисекунд
        // при пороге заметности в пятьсот. То есть весь журнал состоял
        // из состояния, записанного триста раз подряд одними и теми же
        // словами и с теми же виновниками. Настоящее подвисание в таком
        // журнале было бы не найти.
        const SAME_EVENT_MS: u64 = 30_000;
        const SAME_CONDITION_MS: u64 = 60 * 60 * 1000;

        let freeze = detect_with(moment, who)?;
        let repeat_after = if moment.stall_ms >= STALL_NOTICED_MS {
            SAME_EVENT_MS
        } else {
            SAME_CONDITION_MS
        };

        if let Some((cause, when, _, _, _)) = self.entries.last() {
            if *cause == freeze.cause && at_ms.saturating_sub(*when) < repeat_after {
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
        Some(freeze)
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

/// Доля занятой памяти, 0..1.
///
/// Отдельной функцией, потому что ноль в знаменателе здесь не гипотетика:
/// на ранних тиках размер памяти иногда ещё не прочитан, и деление дало бы
/// «не число», которое затем прошло бы все сравнения как ложь и молча
/// отключило проверку памяти.
pub fn used_share(memory_used: Bytes, memory_total: Bytes) -> f64 {
    if memory_total.as_u64() == 0 {
        0.0
    } else {
        memory_used.as_u64() as f64 / memory_total.as_u64() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Состояние не записывается сотней одинаковых строк.
    ///
    /// Случай с живой машины: за 31 час набралось 327 записей, и ни в одной
    /// простой не превысил двух миллисекунд при пороге заметности в пятьсот.
    /// Журнал целиком состоял из одного состояния, записанного снова и снова.
    #[test]
    fn a_standing_condition_is_recorded_once_not_every_half_minute() {
        let mut log = FreezeLog::new();
        // Память в дефиците и подкачка молотит — но система отвечает.
        let condition = Moment {
            memory_used_share: 0.89,
            paging_rate: 20_000.0,
            stall_ms: 0,
            ..Default::default()
        };

        let mut recorded = 0;
        // Полчаса наблюдения с шагом в полминуты.
        for tick in 0..60 {
            if log
                .observe(condition, Bystanders::default(), tick * 30_000)
                .is_some()
            {
                recorded += 1;
            }
        }
        assert_eq!(
            recorded, 1,
            "состояние записано {recorded} раз вместо одного"
        );
    }

    /// А настоящие подвисания записываются каждое.
    #[test]
    fn real_stalls_are_each_recorded() {
        let mut log = FreezeLog::new();
        // Тот же дефицит памяти, но система при этом встала.
        let event = Moment {
            memory_used_share: 0.89,
            paging_rate: 20_000.0,
            stall_ms: 1500,
            ..Default::default()
        };

        let mut recorded = 0;
        for tick in 0..5 {
            // Минута между случаями: это разные события, а не одно.
            if log
                .observe(event, Bystanders::default(), tick * 60_000)
                .is_some()
            {
                recorded += 1;
            }
        }
        assert_eq!(recorded, 5, "настоящие подвисания потерялись");
    }

    /// Очередь к почти свободному диску — не признак.
    #[test]
    fn a_queue_on_an_idle_drive_means_nothing() {
        // Из журнала живой машины: «8 запросов в очереди при занятости 5%».
        // Длина очереди снимается мгновенным замером, и на свободном диске
        // означает лишь пачку запросов, которую он тут же и разобрал.
        let blip = Moment {
            disk_queue: 12,
            disk_busy: 0.05,
            ..Default::default()
        };
        assert_eq!(detect(blip), None);
    }

    /// Ключи хранения переживают круг и не совпадают между собой.
    #[test]
    fn every_cause_survives_a_round_trip_through_its_key() {
        const ALL: [FreezeCause; 6] = [
            FreezeCause::DiskQueue,
            FreezeCause::DriverTime,
            FreezeCause::MemoryPressure,
            FreezeCause::CpuSaturated,
            FreezeCause::Throttled,
            FreezeCause::Unresponsive,
        ];
        let mut seen = std::collections::HashSet::new();
        for cause in ALL {
            assert_eq!(
                FreezeCause::from_storage_key(cause.storage_key()),
                Some(cause)
            );
            assert!(
                seen.insert(cause.storage_key()),
                "две причины делят один ключ: {}",
                cause.storage_key()
            );
        }
        assert_eq!(FreezeCause::from_storage_key("чего-то-нет"), None);
    }

    /// Полный процессор без простоя — это работа, а не подвисание.
    #[test]
    fn a_busy_processor_alone_is_work_not_a_freeze() {
        // Сборка проекта, обработка видео, распаковка архива — всё это
        // занимает процессор целиком, и никто не страдает. Правило,
        // которое кричит на такое, обесценивает все остальные.
        let compiling = Moment {
            cpu_busy: 1.0,
            stall_ms: 0,
            ..Default::default()
        };
        assert_eq!(detect(compiling), None);
    }

    /// Тот же процессор, но система при этом не отвечала.
    #[test]
    fn a_busy_processor_with_a_measured_stall_is_a_freeze() {
        let crowded = Moment {
            cpu_busy: 0.97,
            stall_ms: 900,
            ..Default::default()
        };
        let freeze = detect(crowded).expect("занят и не пускал — это подвисание");
        assert_eq!(freeze.cause, FreezeCause::CpuSaturated);
        assert!(freeze.detail.contains("97%"), "{}", freeze.detail);
    }

    /// Сброс частоты под нагрузкой.
    #[test]
    fn a_processor_held_below_its_clock_under_load_is_reported() {
        let hot = Moment {
            cpu_busy: 0.80,
            cpu_frequency: Some(0.45),
            on_battery: false,
            ..Default::default()
        };
        let freeze = detect(hot).expect("придержанный процессор — это причина");
        assert_eq!(freeze.cause, FreezeCause::Throttled);
        assert!(freeze.detail.contains("45%"), "{}", freeze.detail);
        assert!(
            freeze.culprit_names.is_empty(),
            "частоту сбрасывает не программа — называть кого-то нельзя"
        );
    }

    /// Низкая частота на простое — это экономия, а не беда.
    #[test]
    fn a_low_clock_at_idle_is_saving_power_not_throttling() {
        let idle = Moment {
            cpu_busy: 0.03,
            cpu_frequency: Some(0.30),
            ..Default::default()
        };
        assert_eq!(detect(idle), None);
    }

    /// На батарее пониженная частота — это то, чего человек и просил.
    #[test]
    fn a_low_clock_on_battery_is_not_a_complaint() {
        let saving = Moment {
            cpu_busy: 0.90,
            cpu_frequency: Some(0.40),
            on_battery: true,
            // Простоя нет: иначе сработало бы правило занятого процессора,
            // и мы проверили бы не то.
            stall_ms: 0,
            ..Default::default()
        };
        assert_eq!(detect(saving), None);
    }

    /// Без счётчика частоты о сбросе молчим, а не выдумываем.
    #[test]
    fn a_missing_frequency_counter_produces_silence() {
        let unknown = Moment {
            cpu_busy: 0.80,
            cpu_frequency: None,
            ..Default::default()
        };
        assert_eq!(detect(unknown), None);
    }

    /// Медленный накопитель ловится по задержке, а не по очереди.
    #[test]
    fn a_slow_drive_is_caught_even_with_a_short_queue() {
        // Очередь короткая — по прежнему признаку было бы тихо. А операция
        // на 120 мс означает, что всё, обратившееся к диску, стоит.
        let crawling = Moment {
            disk_queue: 2,
            disk_busy: 0.95,
            disk_latency_ms: 120.0,
            ..Default::default()
        };
        let freeze = detect(crawling).expect("медленный диск — это подвисание");
        assert_eq!(freeze.cause, FreezeCause::DiskQueue);
        assert!(freeze.detail.contains("120 мс"), "{}", freeze.detail);
    }

    /// Быстрый накопитель с глубокой очередью — это норма.
    #[test]
    fn a_deep_queue_on_a_fast_drive_is_not_a_freeze() {
        // Ровно то, из-за чего признак очереди и понадобилось подпереть
        // задержкой: у NVMe глубокая очередь означает хорошую пропускную
        // способность. Порог очереди мы не убрали, поэтому проверяем
        // случай ниже него — но с задержкой, которой человек не заметит.
        let busy_but_fast = Moment {
            disk_queue: 7,
            disk_busy: 1.0,
            disk_latency_ms: 0.4,
            ..Default::default()
        };
        assert_eq!(detect(busy_but_fast), None);
    }

    /// Подвисание без опознанной причины всё равно попадает в журнал.
    ///
    /// Ради этого случая проверка и заведена. Все прочие признаки надо
    /// предугадать заранее, и подвисание от причины, которой нет в списке,
    /// проходит мимо всех. А простой измерен секундомером и не зависит
    /// от того, догадались мы о причине или нет.
    #[test]
    fn a_measured_stall_is_reported_even_with_no_cause_in_sight() {
        let stalled = Moment {
            driver_ratio: 0.01,
            disk_queue: 0,
            disk_busy: 0.05,
            disk_latency_ms: 0.0,
            memory_used_share: 0.40,
            compressing_memory: false,
            paging_rate: 0.0,
            cpu_busy: 0.0,
            cpu_frequency: None,
            on_battery: false,
            stall_ms: 2300,
        };
        let freeze = detect(stalled).expect("измеренный простой — это подвисание");
        assert_eq!(freeze.cause, FreezeCause::Unresponsive);
        assert!(freeze.detail.contains("2.3"), "{}", freeze.detail);
        assert!(
            freeze.culprit_names.is_empty(),
            "виновника не знаем — называть кого-то значило бы гадать"
        );
    }

    /// Опознанная причина важнее голого факта простоя.
    #[test]
    fn a_known_cause_wins_over_the_bare_fact_of_a_stall() {
        // Простой есть, но и причина известна. Сказать «система не отвечала»
        // вместо «кончилась память» — значит потерять единственное, что
        // человеку пригодится: что с этим делать.
        let both = Moment {
            memory_used_share: 0.88,
            paging_rate: 20_000.0,
            cpu_busy: 0.0,
            cpu_frequency: None,
            on_battery: false,
            stall_ms: 2300,
            ..Default::default()
        };
        assert_eq!(detect(both).unwrap().cause, FreezeCause::MemoryPressure);
    }

    /// Обычная задержка планировщика — не подвисание.
    #[test]
    fn ordinary_scheduling_jitter_is_not_a_freeze() {
        // Замер на живой машине: перелёт сна 0,3 мс по медиане и 1,3 мс
        // в худшем случае из шестидесяти. Сотня миллисекунд — уже сильно
        // выше шума, но человек этого не заметит, и кричать не о чем.
        let jitter = Moment {
            stall_ms: 100,
            ..Default::default()
        };
        assert_eq!(detect(jitter), None);
    }

    /// Настоящая толкотня в подкачке — та, что выделяется на фоне.
    ///
    /// У этого теста поучительная история. Сперва он был написан на числах
    /// одного сорокапятисекундного замера: 6519 страниц в секунду, занято
    /// 87%. Тогда это выглядело всплеском. Тридцать один час наблюдения
    /// показал, что на этой машине 6519 — обычное дело: медиана девять
    /// мегабайт в секунду, а 6519 страниц это двадцать пять. То есть тест
    /// закреплял срабатывание на будничной работе, и порог, который он
    /// защищал, дал 308 ложных записей за сутки.
    ///
    /// Теперь здесь наибольшее, что вообще наблюдалось за эти часы, —
    /// 128 МБ в секунду. Урок: число из одного короткого замера нельзя
    /// брать за порог, даже когда оно выглядит выдающимся.
    #[test]
    fn a_paging_storm_that_stands_out_from_the_background_is_a_freeze() {
        let thrashing = Moment {
            driver_ratio: 0.05,
            disk_queue: 0,
            disk_busy: 0.02,
            disk_latency_ms: 0.0,
            memory_used_share: 1.0 - 2042.0 / 16169.0,
            compressing_memory: false,
            paging_rate: 32_768.0,
            cpu_busy: 0.0,
            cpu_frequency: None,
            on_battery: false,
            stall_ms: 0,
        };
        let freeze = detect(thrashing).expect("толкотня в подкачке — это подвисание");
        assert_eq!(freeze.cause, FreezeCause::MemoryPressure);
        // В объяснении мегабайты, а не одни проценты: процент занятой
        // памяти человеку ничего не говорит, а «128 МБ за секунду»
        // объясняет застывший курсор.
        assert!(freeze.detail.contains("128 МБ"), "{}", freeze.detail);
    }

    /// А будничное чтение из подкачки — не подвисание.
    ///
    /// Ровно те числа, на которых порог срабатывал зря: медиана этой
    /// машины. Тест сторожит, чтобы порог не съехал обратно вниз.
    #[test]
    fn everyday_paging_on_a_tight_machine_is_not_a_freeze() {
        let ordinary = Moment {
            memory_used_share: 0.88,
            // Девять мегабайт в секунду — медиана за тридцать один час.
            paging_rate: 2304.0,
            ..Default::default()
        };
        assert_eq!(detect(ordinary), None);
    }

    /// Обратная сторона: чтение с диска само по себе не беда.
    #[test]
    fn paging_with_memory_to_spare_is_just_a_program_starting() {
        // Запуск программы читает страницы её образа сотнями — при этом
        // памяти вдоволь, и ничего не подвисает. Считать это подвисанием
        // значило бы кричать при каждом запуске.
        let launching = Moment {
            memory_used_share: 0.45,
            paging_rate: 5000.0,
            cpu_busy: 0.0,
            cpu_frequency: None,
            on_battery: false,
            ..Default::default()
        };
        assert_eq!(detect(launching), None);
    }

    #[test]
    fn a_healthy_system_reports_nothing() {
        let calm = Moment {
            driver_ratio: 0.02,
            disk_queue: 1,
            disk_busy: 0.30,
            disk_latency_ms: 0.0,
            memory_used_share: 0.60,
            compressing_memory: false,
            paging_rate: 0.0,
            cpu_busy: 0.0,
            cpu_frequency: None,
            on_battery: false,
            stall_ms: 0,
        };
        assert_eq!(detect(calm), None);
    }

    #[test]
    fn a_long_disk_queue_is_a_freeze() {
        let stuck = Moment {
            disk_queue: 25,
            disk_busy: 1.0,
            disk_latency_ms: 0.0,
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
            disk_latency_ms: 0.0,
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
            disk_latency_ms: 0.0,
            memory_used_share: 0.97,
            compressing_memory: true,
            paging_rate: 0.0,
            cpu_busy: 0.0,
            cpu_frequency: None,
            on_battery: false,
            stall_ms: 0,
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
            disk_busy: 1.0,
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
        // Занятость обязательна: очередь к свободному диску ничего
        // не значит. И простой измерен — иначе это одно длящееся условие,
        // а не два случая, и второй раз записывать его незачем.
        let stuck = Moment {
            disk_queue: 20,
            disk_busy: 1.0,
            stall_ms: 800,
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
                disk_latency_ms: 0.0,
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
                disk_latency_ms: 0.0,
                ..Default::default()
            },
            Bystanders {
                disk: &hogs,
                memory: &[],
                cpu: &[],
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
                cpu: &[],
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
                cpu: &[],
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
                disk_busy: 1.0,
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
                disk_busy: 1.0,
                ..Default::default()
            },
            Bystanders {
                disk: &many,
                memory: &[],
                cpu: &[],
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
            disk_busy: 1.0,
            stall_ms: 800,
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
                disk_busy: 1.0,
                ..Default::default()
            },
            Bystanders {
                disk: &many,
                memory: &[],
                cpu: &[],
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
                disk_busy: 1.0,
                ..Default::default()
            },
            Bystanders {
                disk: &hogs,
                memory: &[],
                cpu: &[],
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
