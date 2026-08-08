//! Отладочная утилита Bamboo.
//!
//! На этапе 0.1 это единственный интерфейс к наблюдателю: трей и виджет
//! появятся позже. Ничего в системе не меняет — только смотрит.

#![forbid(unsafe_code)]

#[cfg(windows)]
mod optimize;
#[cfg(windows)]
mod render;

#[cfg(not(windows))]
fn main() {
    eprintln!("Bamboo работает только на Windows.");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("snapshot");

    let result = match command {
        "snapshot" => commands::snapshot(),
        "watch" => commands::watch(&args[1..]),
        "budget" => commands::budget(&args[1..]),
        "disk" => commands::disk(),
        "boot" => commands::boot(),
        "power" => commands::power(),
        "trace" => commands::trace(&args[1..]),
        "record" => commands::record(&args[1..]),
        "optimize" => optimize::plan(args.iter().any(|arg| arg == "--apply")),
        "journal" => optimize::show_journal(),
        "revert" => optimize::revert(&args[1..]),
        "help" | "--help" | "-h" => {
            usage();
            Ok(())
        }
        other => {
            eprintln!("неизвестная команда: {other}\n");
            usage();
            std::process::exit(2);
        }
    };

    if let Err(error) = result {
        eprintln!("ошибка: {error}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn usage() {
    println!(
        "\
Bamboo {} — наблюдатель за системой

  bamboo snapshot            снимок: загрузка, память, топ потребителей
  bamboo watch [--every N]   непрерывное наблюдение, выход по Ctrl+C
  bamboo disk                накопители: здоровье, износ, ресурс
  bamboo boot                время загрузки и что его удлиняет
  bamboo power               пробуждения из сна и их причины
  bamboo trace [--for N]     короткоживущие процессы, невидимые для опроса
  bamboo record --out F [--for N]  записать трассу в файл для анализа
  bamboo budget [--for N]    измерить собственное потребление за N секунд

  bamboo optimize            показать, что было бы сделано (ничего не меняет)
  bamboo optimize --apply    применить действия, записав их в журнал
  bamboo journal             журнал действий
  bamboo revert --id N       откатить одно действие
  bamboo revert --all        откатить всё

Всё, кроме optimize --apply, только читает систему.
Любое применённое действие обратимо.",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(windows)]
mod commands {
    use std::time::Duration;

    use bamboo_collect::Collector;
    use bamboo_core::Result;

    use crate::render;

    /// Интервал между двумя тиками снимка. Первый тик задаёт базу для дельт,
    /// второй уже даёт настоящую загрузку.
    /// Меньше секунды брать нельзя: процессорное время учитывается квантами
    /// примерно по 15 мс, и на коротком интервале загрузка получается
    /// ступенчатой — 2.5%, 5%, 7.5% и ничего между.
    const WARMUP: Duration = Duration::from_millis(1000);

    pub fn snapshot() -> Result<()> {
        let mut collector = Collector::new();
        collector.tick()?;
        std::thread::sleep(WARMUP);
        let tick = collector.tick()?;

        render::system(&tick);
        println!();
        render::top_cpu(collector.table(), 10);
        println!();
        render::top_memory(collector.table(), 5);
        println!();
        render::top_write(collector.table(), 5);
        Ok(())
    }

    pub fn watch(args: &[String]) -> Result<()> {
        let every = flag_secs(args, "--every");
        let mut collector = Collector::new();
        collector.tick()?;

        loop {
            let pause = every.unwrap_or_else(|| collector.next_interval());
            std::thread::sleep(pause);

            let tick = collector.tick()?;
            render::system(&tick);
            render::top_cpu(collector.table(), 8);
            render::changes(&tick);
            println!();
        }
    }

    pub fn disk() -> Result<()> {
        let drives = bamboo_sys::enumerate_drives();
        if drives.is_empty() {
            println!("Ни одного накопителя не удалось открыть.");
            return Ok(());
        }

        for (index, info) in drives.iter().enumerate() {
            if index > 0 {
                println!();
            }
            render::drive(info, bamboo_sys::read_smart(info));
        }
        Ok(())
    }

    pub fn boot() -> Result<()> {
        // Канал диагностики закрыт для обычного пользователя. В продукте
        // его читает брокер под SYSTEM, здесь честно говорим, чего не хватает.
        let history = match bamboo_sys::boot_history(40) {
            Ok(history) => history,
            Err(error) => {
                println!("Историю загрузок прочитать не удалось: {error}");
                return Ok(());
            }
        };
        let culprits = bamboo_sys::boot_culprits(40).unwrap_or_default();
        render::boot(&history, &culprits);
        Ok(())
    }

    pub fn power() -> Result<()> {
        render::wakeups(&bamboo_sys::wake_history(30)?);
        Ok(())
    }

    pub fn trace(args: &[String]) -> Result<()> {
        use bamboo_etw::{ProcessTracker, Recorder};

        let duration = flag_secs(args, "--for").unwrap_or(Duration::from_secs(30));

        // Держим сессию живой всё время чтения: её Drop останавливает
        // трассировку, а сессия ETW переживает процесс и без остановки
        // осталась бы висеть в системе.
        let _session = match bamboo_etw::ProcessTrace::start() {
            Ok(session) => session,
            Err(error) => {
                println!("Трассировку запустить не удалось: {error}");
                return Ok(());
            }
        };

        println!(
            "Слушаю запуски и завершения процессов {} с.\n\
             Опрос такие процессы не видит: они успевают умереть между снимками.\n",
            duration.as_secs()
        );

        // Остановка сессии из другого потока — это и есть способ прервать
        // чтение: consume блокируется до конца сессии.
        std::thread::spawn(move || {
            std::thread::sleep(duration);
            let _ = bamboo_etw::stop_stale();
        });

        let mut tracker = ProcessTracker::new();
        let mut recorder = Recorder::new();
        let mut total = 0u32;

        bamboo_etw::ProcessTrace::consume(&mut |event| {
            tracker.observe(&event);
            recorder.record(event);
            total += 1;
            true
        })?;

        render::trace(&tracker, &recorder, total, duration);
        Ok(())
    }

    pub fn record(args: &[String]) -> Result<()> {
        use bamboo_store::{Trace, TraceFrame, TraceProcess};

        let out = flag_value(args, "--out").unwrap_or_else(|| "bamboo.trace".to_string());
        let duration = flag_secs(args, "--for").unwrap_or(Duration::from_secs(60));

        let mut collector = Collector::new();
        collector.tick()?; // первый тик задаёт базу для дельт
        let mut trace = Trace::new(5000);

        let started = std::time::Instant::now();
        println!("Записываю трассу {} с в {out}...", duration.as_secs());

        while started.elapsed() < duration {
            std::thread::sleep(Duration::from_secs(5));
            let tick = collector.tick()?;

            let processes: Vec<TraceProcess> = collector
                .table()
                .iter()
                .map(|process| TraceProcess {
                    app_key: process.app_key.to_string(),
                    image_name: process.image_name.to_string(),
                    cpu_ms: process.last_point().cpu_ms,
                    private_kib: process.last_point().private_kib,
                    working_set_kib: process.last_point().working_set_private_kib,
                    read_kib: process.last_point().read_kib,
                    write_kib: process.last_point().write_kib,
                    handles: process.handles,
                    threads: process.threads,
                })
                .collect();

            trace.push(TraceFrame {
                at_unix_ms: tick.at.unix_ms,
                processes,
            });
        }

        let file = std::fs::File::create(&out)
            .map_err(|_| bamboo_core::Error::Unsupported("не удалось создать файл трассы"))?;
        trace
            .write_to(std::io::BufWriter::new(file))
            .map_err(|_| bamboo_core::Error::Unsupported("не удалось записать трассу"))?;

        println!(
            "Готово: {} кадров. Прогнать анализаторы: trace-replay {out}",
            trace.frame_count()
        );
        Ok(())
    }

    fn flag_value(args: &[String], name: &str) -> Option<String> {
        let index = args.iter().position(|a| a == name)?;
        args.get(index + 1).cloned()
    }

    pub fn budget(args: &[String]) -> Result<()> {
        let duration = flag_secs(args, "--for").unwrap_or(Duration::from_secs(60));
        render::budget_header(duration);

        let mut collector = Collector::new();
        let started = std::time::Instant::now();
        let mut ticks = 0u32;
        let mut peak = bamboo_sys::own_memory()?;

        while started.elapsed() < duration {
            collector.tick()?;
            ticks += 1;

            let now = bamboo_sys::own_memory()?;
            peak.working_set = peak.working_set.max(now.working_set);
            peak.private_bytes = peak.private_bytes.max(now.private_bytes);

            std::thread::sleep(collector.next_interval());
        }

        render::budget_report(peak, ticks, started.elapsed(), collector.table());
        Ok(())
    }

    fn flag_secs(args: &[String], name: &str) -> Option<Duration> {
        let index = args.iter().position(|a| a == name)?;
        let value: u64 = args.get(index + 1)?.parse().ok()?;
        Some(Duration::from_secs(value.max(1)))
    }
}
