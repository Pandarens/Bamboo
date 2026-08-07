//! Еженедельный отчёт (ТЗ, раздел 14.5).
//!
//! Четыре блока, и четвёртый обязателен: что Bamboo сделал за неделю
//! и какой измеримый эффект это дало. Он замыкает петлю обратной связи
//! и не даёт продукту превратиться в набор непроверяемых твиков.
//!
//! Markdown, а не только HTML: такой отчёт пользователь может приложить
//! к issue разработчику тормозящего приложения — и Bamboo из «оптимизатора»
//! превращается в диагностический инструмент.

use bamboo_core::Bytes;

use crate::observation::Observation;

/// Данные за неделю.
#[derive(Default)]
pub struct WeeklyData<'a> {
    /// Записано на диск за неделю.
    pub written: Bytes,
    /// Проекция ресурса в годах.
    pub years_left: Option<f64>,
    /// Кто писал больше всех: имя и объём.
    pub top_writers: &'a [(String, Bytes)],

    /// Фоновые всплески процессора.
    pub cpu_spikes: &'a [Observation],
    /// Тренды роста памяти.
    pub memory_trends: &'a [Observation],

    /// Что Bamboo сделал: описание действия и его результат.
    pub actions: &'a [ActionEffect],
    /// Сколько действий откачено сторожевым таймером.
    pub auto_reverted: usize,
}

/// Действие и его измеренный эффект.
pub struct ActionEffect {
    pub description: String,
    /// Измеримый результат. `None`, если измерить не удалось —
    /// и тогда так и написано.
    pub effect: Option<String>,
}

/// Собирает отчёт в Markdown.
pub fn weekly_markdown(data: &WeeklyData<'_>) -> String {
    let mut out = String::from("# Bamboo: отчёт за неделю\n");

    out.push_str("\n## Расход ресурса накопителя\n\n");
    out.push_str(&format!("За неделю записано **{}**.\n", data.written));
    match data.years_left {
        Some(years) if years > 50.0 => out.push_str(
            "При таком темпе ресурса накопителя хватит на десятилетия — \
             беспокоиться не о чем.\n",
        ),
        Some(years) => out.push_str(&format!(
            "При таком темпе ресурса хватит примерно на {years:.0} лет.\n"
        )),
        None => out.push_str(
            "Проекцию ресурса построить не удалось: накопитель не сообщает \
             нужных данных.\n",
        ),
    }
    if !data.top_writers.is_empty() {
        out.push_str("\nБольше всех писали:\n\n");
        for (name, bytes) in data.top_writers.iter().take(5) {
            out.push_str(&format!("- {name} — {bytes}\n"));
        }
    }

    out.push_str("\n## Фоновая нагрузка на процессор\n\n");
    push_observations(&mut out, data.cpu_spikes, "Всплесков в фоне не было.");

    out.push_str("\n## Рост памяти\n\n");
    push_observations(
        &mut out,
        data.memory_trends,
        "Приложений с монотонным ростом памяти не замечено.",
    );

    // Четвёртый блок обязателен и не пропускается никогда.
    out.push_str("\n## Что сделал Bamboo\n\n");
    if data.actions.is_empty() {
        out.push_str("За неделю Bamboo ничего не менял в системе.\n");
    } else {
        for action in data.actions {
            match &action.effect {
                Some(effect) => out.push_str(&format!("- {} — {effect}\n", action.description)),
                // Честно: сделали, а померить не смогли.
                None => out.push_str(&format!(
                    "- {} — измеримого эффекта зафиксировать не удалось\n",
                    action.description
                )),
            }
        }
    }
    if data.auto_reverted > 0 {
        out.push_str(&format!(
            "\nОткачено автоматически: {}. Сторожевой таймер заметил ухудшение \
             и вернул систему к прежнему состоянию.\n",
            data.auto_reverted
        ));
    }

    out
}

fn push_observations(out: &mut String, observations: &[Observation], when_empty: &str) {
    if observations.is_empty() {
        out.push_str(when_empty);
        out.push('\n');
        return;
    }
    for observation in observations {
        out.push_str(&format!("- {}\n", observation.summary));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{ObservationKind, Severity};

    fn observation(text: &str) -> Observation {
        Observation::new(
            ObservationKind::BackgroundCpu,
            Severity::Notice,
            0.8,
            text.to_string(),
        )
    }

    #[test]
    fn a_quiet_week_produces_a_calm_report() {
        let report = weekly_markdown(&WeeklyData {
            written: Bytes(120 * 1_000_000_000),
            years_left: Some(120.0),
            ..Default::default()
        });

        assert!(report.contains("хватит на десятилетия"));
        assert!(report.contains("Всплесков в фоне не было"));
        assert!(report.contains("ничего не менял в системе"));
    }

    #[test]
    fn the_fourth_block_is_always_present() {
        // Он замыкает петлю обратной связи и пропускаться не может.
        let report = weekly_markdown(&WeeklyData::default());
        assert!(report.contains("## Что сделал Bamboo"));
    }

    #[test]
    fn actions_are_reported_with_their_effect() {
        let actions = vec![ActionEffect {
            description: "Slack переведён в экономичный режим".into(),
            effect: Some("фоновое потребление процессора упало вдвое".into()),
        }];
        let report = weekly_markdown(&WeeklyData {
            actions: &actions,
            ..Default::default()
        });

        assert!(report.contains("фоновое потребление процессора упало вдвое"));
    }

    #[test]
    fn an_unmeasurable_effect_is_admitted_not_invented() {
        let actions = vec![ActionEffect {
            description: "Задача планировщика отключена".into(),
            effect: None,
        }];
        let report = weekly_markdown(&WeeklyData {
            actions: &actions,
            ..Default::default()
        });

        assert!(report.contains("измеримого эффекта зафиксировать не удалось"));
    }

    #[test]
    fn automatic_reverts_are_reported_honestly() {
        let report = weekly_markdown(&WeeklyData {
            auto_reverted: 2,
            ..Default::default()
        });
        assert!(report.contains("Откачено автоматически: 2"));
    }

    #[test]
    fn a_drive_that_hides_its_numbers_gets_no_invented_projection() {
        let report = weekly_markdown(&WeeklyData {
            years_left: None,
            ..Default::default()
        });
        assert!(report.contains("не сообщает"));
    }

    #[test]
    fn observations_land_in_their_sections() {
        let spikes = vec![observation("CompatTelRunner.exe: 4 мин нагрузки ночью")];
        let growth = vec![observation("teams.exe: память растёт на 40 МБ в час")];

        let report = weekly_markdown(&WeeklyData {
            cpu_spikes: &spikes,
            memory_trends: &growth,
            ..Default::default()
        });

        let cpu_section = report.find("## Фоновая нагрузка").unwrap();
        let memory_section = report.find("## Рост памяти").unwrap();
        let spike = report.find("CompatTelRunner").unwrap();
        let trend = report.find("teams.exe").unwrap();

        assert!(cpu_section < spike && spike < memory_section);
        assert!(memory_section < trend);
    }

    #[test]
    fn the_report_is_valid_markdown_with_all_four_blocks() {
        let report = weekly_markdown(&WeeklyData::default());
        for heading in [
            "# Bamboo: отчёт за неделю",
            "## Расход ресурса накопителя",
            "## Фоновая нагрузка на процессор",
            "## Рост памяти",
            "## Что сделал Bamboo",
        ] {
            assert!(report.contains(heading), "не хватает блока: {heading}");
        }
    }
}
