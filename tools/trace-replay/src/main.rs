//! Прогон анализаторов на записанной трассе (ТЗ, раздел 16.1).
//!
//! Нельзя проверить «правильно ли детектируется утечка», дёргая живую
//! систему. Вместо этого — записанная трасса на вход и наблюдения на выход.
//! Так анализаторы проверяются на реальных данных в CI и так разработчик
//! воспроизводит проблему пользователя, не имея доступа к его машине.

#![forbid(unsafe_code)]

use std::path::Path;

use bamboo_analyze::growth::{self, GrowthInput};
use bamboo_store::Trace;

fn main() {
    let path = match std::env::args().nth(1) {
        Some(path) => path,
        None => {
            eprintln!(
                "trace-replay — прогон анализаторов на записанной трассе\n\n\
                 Использование: trace-replay <файл.bamboo-trace>"
            );
            std::process::exit(2);
        }
    };

    match replay(Path::new(&path)) {
        Ok(count) => {
            if count == 0 {
                println!("\nНаблюдений нет. Для многих трасс это правильный ответ.");
            }
        }
        Err(error) => {
            eprintln!("не удалось прочитать трассу: {error}");
            std::process::exit(1);
        }
    }
}

/// Читает трассу и прогоняет анализаторы. Возвращает число наблюдений.
fn replay(path: &Path) -> std::io::Result<usize> {
    let file = std::fs::File::open(path)?;
    let trace = Trace::read_from(std::io::BufReader::new(file))?;

    println!(
        "Трасса: {} кадров, интервал {} мс, приложений {}.",
        trace.frame_count(),
        trace.interval_ms,
        trace.app_keys().len()
    );

    let observations = analyze(&trace);
    if !observations.is_empty() {
        println!("\nНаблюдения:");
        for observation in &observations {
            println!("  [{:?}] {}", observation.severity, observation.summary);
            if let Some(detail) = &observation.detail {
                println!("      {detail}");
            }
        }
    }
    Ok(observations.len())
}

/// Прогоняет доступные анализаторы по каждому приложению трассы.
///
/// Пока это анализатор роста памяти: он единственный работает по одному
/// приватному ряду, который трасса восстанавливает напрямую. Остальные
/// требуют данных, которых в трассе этого формата ещё нет (простой
/// пользователя, состояние окон), — они подключатся по мере расширения
/// формата.
fn analyze(trace: &Trace) -> Vec<bamboo_analyze::Observation> {
    let mut observations = Vec::new();

    for app_key in trace.app_keys() {
        let series: Vec<growth::Point> = trace.private_series(&app_key);
        if series.len() < 3 {
            continue;
        }

        // Время жизни считаем по протяжённости ряда: сколько наблюдали,
        // столько и жил под нашим взглядом.
        let lifetime_ms = series
            .last()
            .map(|(t, _)| *t)
            .unwrap_or(0)
            .saturating_sub(series.first().map(|(t, _)| *t).unwrap_or(0));

        let input = GrowthInput {
            process_name: image_name_for(trace, &app_key),
            lifetime_ms,
            private_bytes: &series,
            handles: &[],
            gdi_objects: &[],
        };

        observations.extend(growth::analyze(&input));
    }

    observations
}

/// Имя образа приложения из трассы. Берём его из кадра, а не из ключа:
/// в ключе путь содержит двоеточие диска, и разбор по нему ненадёжен,
/// а имя образа уже лежит отдельным полем.
fn image_name_for<'a>(trace: &'a Trace, app_key: &str) -> &'a str {
    // find не промахнётся: app_key взят из самой трассы. Статичный
    // запасной вариант нужен лишь чтобы удовлетворить проверку времён жизни.
    trace
        .frames
        .iter()
        .flat_map(|frame| frame.processes.iter())
        .find(|process| process.app_key == app_key)
        .map(|process| process.image_name.as_str())
        .unwrap_or("процесс")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bamboo_store::{TraceFrame, TraceProcess};

    const HOUR: i64 = 3_600_000;
    const MB_KIB: u32 = 1024;

    /// Собирает трассу с монотонным ростом памяти у одного приложения.
    fn leaking_trace() -> Trace {
        let mut trace = Trace::new(60_000);
        // Семь часов, кадр в минуту, рост 40 МБ в час. Семь, а не пять:
        // анализатор роста не судит процессы моложе шести часов.
        for minute in 0..(7 * 60) {
            let grown = 300 * MB_KIB + (minute as u32 * 40 * MB_KIB / 60);
            trace.push(TraceFrame {
                at_unix_ms: minute * 60_000,
                processes: vec![TraceProcess {
                    app_key: "c:\\app\\teams.exe:?".into(),
                    image_name: "teams.exe".into(),
                    private_kib: grown,
                    ..Default::default()
                }],
            });
        }
        trace
    }

    #[test]
    fn a_leak_trace_produces_a_memory_growth_observation() {
        // Ровно сценарий leak-teams-4h из таблицы фикстур ТЗ.
        let observations = analyze(&leaking_trace());
        assert_eq!(observations.len(), 1);
        assert!(observations[0].summary.contains("teams.exe"));
        assert!(observations[0].confidence > 0.8);
    }

    #[test]
    fn a_flat_trace_produces_nothing() {
        // active-work / idle-postgres: рост отсутствует, наблюдений быть
        // не должно.
        let mut trace = Trace::new(60_000);
        for minute in 0..(6 * 60) {
            trace.push(TraceFrame {
                at_unix_ms: minute * 60_000,
                processes: vec![TraceProcess {
                    app_key: "c:\\srv\\postgres.exe:?".into(),
                    image_name: "postgres.exe".into(),
                    private_kib: 300 * MB_KIB,
                    ..Default::default()
                }],
            });
        }
        assert!(analyze(&trace).is_empty());
    }

    #[test]
    fn a_leak_trace_survives_a_file_round_trip_and_still_detects() {
        // Ключевая проверка: трасса пишется, читается и даёт тот же вывод.
        // Именно так она поедет в CI и в issue.
        let trace = leaking_trace();
        let mut buffer = Vec::new();
        trace.write_to(&mut buffer).unwrap();
        let restored = Trace::read_from(buffer.as_slice()).unwrap();

        assert_eq!(analyze(&restored).len(), 1);
        let _ = HOUR;
    }

    #[test]
    fn the_image_name_comes_from_the_trace_not_the_key() {
        // В ключе путь содержит двоеточие диска — имя берём из поля образа.
        let trace = leaking_trace();
        assert_eq!(image_name_for(&trace, "c:\\app\\teams.exe:?"), "teams.exe");
    }
}
