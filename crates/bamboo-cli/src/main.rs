//! Отладочная утилита Bamboo.
//!
//! На этапе 0.1 это единственный интерфейс к наблюдателю: трей и виджет
//! появятся позже. Ничего в системе не меняет — только смотрит.

#![forbid(unsafe_code)]

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
  bamboo budget [--for N]    измерить собственное потребление за N секунд

Ничего в системе не изменяется.",
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
