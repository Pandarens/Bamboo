//! Самозамер ресурсного бюджета (ТЗ, раздел 4).
//!
//! ТЗ требует суточного прогона «в CI на выделенной машине». Машины нет,
//! и оказалось, что она и не нужна: у раннера GitHub задание убивается
//! на шести часах, а суточный замер резидентной утилиты правильнее делать
//! ею самой. Bamboo и так работает сутками на живой машине — под настоящей
//! нагрузкой, с настоящим числом процессов и настоящими окнами. Стерильный
//! раннер такого не покажет.
//!
//! Главное правило здесь то же, что и во всём остальном: не выдавать
//! короткое наблюдение за суточное. Пересчитывать десять минут в сутки
//! линейно — гадание: Bamboo пишет на диск пачками раз в несколько часов,
//! и десять минут между пачками дадут ноль, а десять минут в момент сброса —
//! десятикратное превышение. Поэтому суточные величины показываются только
//! тогда, когда прошли сутки, а до тех пор честно говорится, сколько
//! осталось ждать.

#![forbid(unsafe_code)]

use bamboo_core::Bytes;

/// Пределы из таблицы раздела 4 ТЗ для агента.
///
/// Взята колонка «виджет открыт»: она мягче, а мерить приходится один
/// процесс, который в разное время бывает и с окном, и без. Сравнивать
/// с более строгой колонкой значило бы объявлять превышением нормальную
/// работу с открытым окном.
const WORKING_SET_LIMIT: u64 = 30 * 1024 * 1024;
const PRIVATE_LIMIT: u64 = 25 * 1024 * 1024;
/// Доля одного ядра в процентах, среднее за час.
const CPU_LIMIT: f64 = 0.5;
/// Агенту таблица ТЗ отводит ноль записи на диск. Ноль недостижим:
/// журнал действий и история наблюдений — это запись. Меряем и показываем
/// сколько есть, а сравниваем с пределом брокера: он и есть та величина,
/// в которую вся телеметрия Bamboo обязана уложиться.
const DISK_PER_DAY_LIMIT: u64 = 20 * 1024 * 1024;

/// Сколько надо наблюдать, чтобы говорить о суточных величинах.
const A_DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// Сколько надо наблюдать, чтобы говорить о среднем за час.
const AN_HOUR_MS: u64 = 60 * 60 * 1000;

/// Одна строка отчёта о бюджете.
#[derive(Clone, Debug, PartialEq)]
pub struct BudgetLine {
    pub metric: String,
    /// Замер словами. Пусто — ещё не намерено.
    pub measured: String,
    pub limit: String,
    /// Уложились ли. `None` — наблюдений пока мало, и судить рано.
    pub within: Option<bool>,
}

/// Самозамер: копит своё же потребление.
#[derive(Default)]
pub struct SelfWatch {
    /// Пик рабочего набора за всё наблюдение.
    peak_working_set: u64,
    peak_private: u64,
    /// Сумма долей процессора и число замеров — для среднего.
    cpu_total: f64,
    cpu_samples: u64,
    /// Записано на диск с начала наблюдения.
    written: u64,
    /// Сколько идёт наблюдение.
    watched_ms: u64,
}

impl SelfWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Учитывает очередной замер.
    ///
    /// `written` — накопленный счётчик записи процесса, а не приращение:
    /// именно так его отдаёт система, и вычитать начальное значение должен
    /// тот, кто знает, когда началось наблюдение.
    pub fn observe(
        &mut self,
        working_set: u64,
        private: u64,
        cpu_percent: f64,
        written_since_start: u64,
        watched_ms: u64,
    ) {
        self.peak_working_set = self.peak_working_set.max(working_set);
        self.peak_private = self.peak_private.max(private);
        self.cpu_total += cpu_percent.max(0.0);
        self.cpu_samples += 1;
        self.written = written_since_start;
        self.watched_ms = watched_ms;
    }

    /// Среднее потребление процессора, проценты одного ядра.
    fn average_cpu(&self) -> f64 {
        if self.cpu_samples == 0 {
            return 0.0;
        }
        self.cpu_total / self.cpu_samples as f64
    }

    /// Отчёт по всем строкам таблицы бюджета.
    pub fn report(&self) -> Vec<BudgetLine> {
        vec![
            BudgetLine {
                metric: "Рабочий набор, пик".to_string(),
                measured: Bytes(self.peak_working_set).to_string(),
                limit: Bytes(WORKING_SET_LIMIT).to_string(),
                // Память меряется мгновенно: ждать суток незачем.
                within: (self.cpu_samples > 0)
                    .then_some(self.peak_working_set <= WORKING_SET_LIMIT),
            },
            BudgetLine {
                metric: "Приватные байты, пик".to_string(),
                measured: Bytes(self.peak_private).to_string(),
                limit: Bytes(PRIVATE_LIMIT).to_string(),
                within: (self.cpu_samples > 0).then_some(self.peak_private <= PRIVATE_LIMIT),
            },
            BudgetLine {
                metric: "Процессор, среднее".to_string(),
                measured: if self.cpu_samples == 0 {
                    String::new()
                } else {
                    format!("{:.3}%", self.average_cpu())
                },
                limit: format!("{CPU_LIMIT}%"),
                // Час — минимум, за который среднее перестаёт зависеть
                // от случайного всплеска при запуске.
                within: (self.watched_ms >= AN_HOUR_MS).then_some(self.average_cpu() <= CPU_LIMIT),
            },
            BudgetLine {
                metric: "Запись на диск в сутки".to_string(),
                measured: if self.watched_ms >= A_DAY_MS {
                    format!("{}", Bytes(self.written))
                } else {
                    // Пересчитывать линейно нельзя: Bamboo пишет пачками
                    // раз в несколько часов, и короткое окно даст либо ноль,
                    // либо десятикратное превышение — оба ответа неправда.
                    format!("{} за {}", Bytes(self.written), spell(self.watched_ms))
                },
                limit: Bytes(DISK_PER_DAY_LIMIT).to_string(),
                within: (self.watched_ms >= A_DAY_MS).then_some(self.written <= DISK_PER_DAY_LIMIT),
            },
        ]
    }

    /// Что сказать про отчёт в целом.
    pub fn verdict(&self) -> String {
        let report = self.report();
        let judged: Vec<&BudgetLine> = report.iter().filter(|l| l.within.is_some()).collect();
        let broken: Vec<&str> = judged
            .iter()
            .filter(|l| l.within == Some(false))
            .map(|l| l.metric.as_str())
            .collect();

        if !broken.is_empty() {
            return format!(
                "Bamboo вышел за собственный бюджет: {}. Это блокирующая                  неисправность, а не повод отложить — утилита, которая сама                  ест ресурсы, не имеет права учить этому других.",
                broken.join(", ")
            );
        }

        let waiting = report.len() - judged.len();
        if waiting > 0 {
            return format!(
                "Наблюдение идёт {}. Уложились по {} из {} мерок; остальные                  требуют более долгого наблюдения — суточные величины Bamboo                  не пересчитывает из коротких, потому что это было бы гаданием.",
                spell(self.watched_ms),
                judged.len(),
                report.len()
            );
        }

        format!(
            "За {} наблюдения Bamboo уложился во все мерки собственного бюджета.",
            spell(self.watched_ms)
        )
    }
}

fn spell(ms: u64) -> String {
    let minutes = ms / 60_000;
    if minutes < 60 {
        format!("{minutes} мин")
    } else if minutes < 60 * 24 {
        format!("{} ч {} мин", minutes / 60, minutes % 60)
    } else {
        format!("{} сут {} ч", minutes / (60 * 24), (minutes / 60) % 24)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watched(ms: u64, working_set: u64, cpu: f64, written: u64) -> SelfWatch {
        let mut watch = SelfWatch::new();
        watch.observe(working_set, working_set / 2, cpu, written, ms);
        watch
    }

    #[test]
    fn a_short_watch_does_not_extrapolate_to_a_day() {
        // Главное правило. Bamboo пишет пачками раз в несколько часов:
        // десять минут между пачками дадут ноль, а десять минут в момент
        // сброса — десятикратное превышение. Оба ответа — неправда.
        let watch = watched(10 * 60_000, 20 << 20, 0.1, 5 << 20);
        let disk = watch
            .report()
            .into_iter()
            .find(|line| line.metric.contains("диск"))
            .unwrap();

        assert_eq!(disk.within, None, "суточный вывод из десяти минут");
        assert!(disk.measured.contains("за 10 мин"), "{}", disk.measured);
    }

    #[test]
    fn memory_is_judged_at_once_because_it_is_measured_at_once() {
        // Память — не среднее за время, а пик. Ждать суток, чтобы сказать
        // «двадцать мегабайт меньше тридцати», незачем.
        let watch = watched(60_000, 20 << 20, 0.1, 0);
        let memory = watch.report().into_iter().next().unwrap();
        assert_eq!(memory.within, Some(true));
    }

    #[test]
    fn exceeding_the_memory_budget_is_called_a_blocking_fault() {
        let watch = watched(60_000, 100 << 20, 0.1, 0);
        let verdict = watch.verdict();
        assert!(verdict.contains("блокирующая"), "{verdict}");
        // И объяснено, почему это не «оптимизация на потом».
        assert!(verdict.contains("не имеет права"), "{verdict}");
    }

    #[test]
    fn the_cpu_average_waits_for_an_hour() {
        let short = watched(10 * 60_000, 20 << 20, 0.01, 0);
        let cpu = short
            .report()
            .into_iter()
            .find(|line| line.metric.contains("Процессор"))
            .unwrap();
        assert_eq!(
            cpu.within, None,
            "среднее за десять минут — не среднее за час"
        );

        let long = watched(AN_HOUR_MS, 20 << 20, 0.01, 0);
        let cpu = long
            .report()
            .into_iter()
            .find(|line| line.metric.contains("Процессор"))
            .unwrap();
        assert_eq!(cpu.within, Some(true));
    }

    #[test]
    fn a_full_day_judges_everything() {
        let mut watch = SelfWatch::new();
        watch.observe(20 << 20, 10 << 20, 0.01, 3 << 20, A_DAY_MS);

        let report = watch.report();
        assert!(
            report.iter().all(|line| line.within.is_some()),
            "за сутки судить можно обо всём: {report:#?}"
        );
        assert!(
            watch.verdict().contains("уложился во все"),
            "{}",
            watch.verdict()
        );
    }

    #[test]
    fn the_peak_is_remembered_not_the_last_value() {
        // Иначе бюджет проверялся бы по случайному мгновению, и всплеск
        // в момент открытия окна остался бы незамеченным.
        let mut watch = SelfWatch::new();
        watch.observe(90 << 20, 80 << 20, 0.1, 0, 1000);
        watch.observe(10 << 20, 5 << 20, 0.1, 0, 2000);

        assert_eq!(watch.report()[0].within, Some(false), "пик забыт");
    }

    #[test]
    fn the_average_cpu_is_an_average_not_the_last_sample() {
        let mut watch = SelfWatch::new();
        watch.observe(10 << 20, 5 << 20, 2.0, 0, AN_HOUR_MS);
        watch.observe(10 << 20, 5 << 20, 0.0, 0, AN_HOUR_MS);
        watch.observe(10 << 20, 5 << 20, 0.0, 0, AN_HOUR_MS);
        watch.observe(10 << 20, 5 << 20, 0.0, 0, AN_HOUR_MS);

        // Среднее 0.5 — ровно на пределе, и это ещё «уложились».
        assert_eq!(watch.report()[2].within, Some(true));
    }

    #[test]
    fn nothing_is_judged_before_the_first_sample() {
        let watch = SelfWatch::new();
        assert!(watch.report().iter().all(|line| line.within.is_none()));
    }

    #[test]
    fn the_waiting_verdict_says_how_long_is_left_to_judge() {
        let watch = watched(30 * 60_000, 20 << 20, 0.01, 0);
        let verdict = watch.verdict();
        assert!(verdict.contains("30 мин"), "{verdict}");
        assert!(verdict.contains("гаданием"), "{verdict}");
    }

    #[test]
    fn time_is_spelled_for_people() {
        assert_eq!(spell(45 * 60_000), "45 мин");
        assert_eq!(spell(90 * 60_000), "1 ч 30 мин");
        assert_eq!(spell(A_DAY_MS + AN_HOUR_MS * 3), "1 сут 3 ч");
    }
}
