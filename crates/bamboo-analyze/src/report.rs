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

/// Собирает отчёт в HTML для просмотра.
///
/// Один самодостаточный файл без внешних ресурсов: его можно открыть
/// в браузере на любой машине, а не только там, где есть интернет.
pub fn weekly_html(data: &WeeklyData<'_>) -> String {
    let mut body = String::new();

    body.push_str("<h2>Расход ресурса накопителя</h2>");
    body.push_str(&format!(
        "<p>За неделю записано <strong>{}</strong>.</p>",
        escape(&data.written.to_string())
    ));
    match data.years_left {
        Some(years) if years > 50.0 => body
            .push_str("<p>Ресурса накопителя хватит на десятилетия — беспокоиться не о чем.</p>"),
        Some(years) => body.push_str(&format!(
            "<p>При таком темпе ресурса хватит примерно на {years:.0} лет.</p>"
        )),
        None => body.push_str(
            "<p>Проекцию построить не удалось: накопитель не сообщает нужных данных.</p>",
        ),
    }
    if !data.top_writers.is_empty() {
        body.push_str("<p>Больше всех писали:</p><ul>");
        for (name, bytes) in data.top_writers.iter().take(5) {
            body.push_str(&format!(
                "<li>{} — {}</li>",
                escape(name),
                escape(&bytes.to_string())
            ));
        }
        body.push_str("</ul>");
    }

    body.push_str("<h2>Фоновая нагрузка на процессор</h2>");
    push_observations_html(&mut body, data.cpu_spikes, "Всплесков в фоне не было.");

    body.push_str("<h2>Рост памяти</h2>");
    push_observations_html(
        &mut body,
        data.memory_trends,
        "Приложений с монотонным ростом памяти не замечено.",
    );

    body.push_str("<h2>Что сделал Bamboo</h2>");
    if data.actions.is_empty() {
        body.push_str("<p>За неделю Bamboo ничего не менял в системе.</p>");
    } else {
        body.push_str("<ul>");
        for action in data.actions {
            let effect = action
                .effect
                .as_deref()
                .unwrap_or("измеримого эффекта зафиксировать не удалось");
            body.push_str(&format!(
                "<li>{} — {}</li>",
                escape(&action.description),
                escape(effect)
            ));
        }
        body.push_str("</ul>");
    }
    if data.auto_reverted > 0 {
        body.push_str(&format!(
            "<p>Откачено автоматически: {}. Сторожевой таймер заметил ухудшение \
             и вернул систему к прежнему состоянию.</p>",
            data.auto_reverted
        ));
    }

    // Стиль минимальный и встроенный: отчёт должен открываться
    // одинаково где угодно, без загрузки шрифтов и таблиц стилей.
    format!(
        "<!doctype html><html lang=\"ru\"><head><meta charset=\"utf-8\">\
         <title>Bamboo: отчёт за неделю</title>\
         <style>body{{font-family:system-ui,sans-serif;max-width:40rem;margin:2rem auto;\
         padding:0 1rem;line-height:1.5;color:#1a231f}}h1{{font-size:1.5rem}}\
         h2{{font-size:1.1rem;margin-top:1.5rem;color:#2f6f4f}}</style></head>\
         <body><h1>Bamboo: отчёт за неделю</h1>{body}</body></html>"
    )
}

fn push_observations_html(out: &mut String, observations: &[Observation], when_empty: &str) {
    if observations.is_empty() {
        out.push_str(&format!("<p>{}</p>", escape(when_empty)));
        return;
    }
    out.push_str("<ul>");
    for observation in observations {
        out.push_str(&format!("<li>{}</li>", escape(&observation.summary)));
    }
    out.push_str("</ul>");
}

/// Экранирует текст для вставки в HTML.
///
/// Имена процессов и приложений приходят из системы и попадают в отчёт,
/// который пользователь может открыть в браузере или приложить к issue.
/// Без экранирования имя вида `<script>` превратилось бы из данных в код.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Собирает отчёт в JSON для экспорта и машинной обработки.
///
/// Своя сборка без serde: структура отчёта простая и стабильная, а тащить
/// зависимость в крейт, который сознательно держат чистым, ради одной
/// функции не стоит. Строки экранируются по правилам JSON.
pub fn weekly_json(data: &WeeklyData<'_>) -> String {
    let mut out = String::from("{");

    out.push_str(&format!("\"written_bytes\":{},", data.written.as_u64()));
    match data.years_left {
        Some(years) => out.push_str(&format!("\"years_left\":{years:.1},")),
        None => out.push_str("\"years_left\":null,"),
    }

    out.push_str("\"top_writers\":[");
    for (index, (name, bytes)) in data.top_writers.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"name\":{},\"bytes\":{}}}",
            json_string(name),
            bytes.as_u64()
        ));
    }
    out.push_str("],");

    push_json_observations(&mut out, "cpu_spikes", data.cpu_spikes);
    out.push(',');
    push_json_observations(&mut out, "memory_trends", data.memory_trends);
    out.push(',');

    out.push_str("\"actions\":[");
    for (index, action) in data.actions.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let effect = match &action.effect {
            Some(effect) => json_string(effect),
            None => "null".to_string(),
        };
        out.push_str(&format!(
            "{{\"description\":{},\"effect\":{effect}}}",
            json_string(&action.description)
        ));
    }
    out.push_str("],");

    out.push_str(&format!("\"auto_reverted\":{}", data.auto_reverted));
    out.push('}');
    out
}

fn push_json_observations(out: &mut String, key: &str, observations: &[Observation]) {
    out.push_str(&format!("\"{key}\":["));
    for (index, observation) in observations.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&json_string(&observation.summary));
    }
    out.push(']');
}

/// Экранирует строку по правилам JSON.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            other => out.push(other),
        }
    }
    out.push('"');
    out
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
    fn html_escapes_process_names() {
        // Имя процесса приходит из системы и попадает в отчёт. Без
        // экранирования оно превратилось бы из данных в исполняемый код.
        let actions = vec![ActionEffect {
            description: "<script>alert(1)</script> переведён в экорежим".into(),
            effect: None,
        }];
        let html = weekly_html(&WeeklyData {
            actions: &actions,
            ..Default::default()
        });

        assert!(!html.contains("<script>alert"), "имя не экранировано");
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn html_has_all_four_sections_and_is_self_contained() {
        let html = weekly_html(&WeeklyData::default());
        assert!(html.starts_with("<!doctype html>"));
        for heading in [
            "Расход ресурса накопителя",
            "Фоновая нагрузка на процессор",
            "Рост памяти",
            "Что сделал Bamboo",
        ] {
            assert!(html.contains(heading), "не хватает блока: {heading}");
        }
        // Самодостаточность: никаких внешних ссылок.
        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
    }

    #[test]
    fn json_escapes_strings_and_stays_parseable_in_shape() {
        let actions = vec![ActionEffect {
            description: "приложение \"с кавычками\"".into(),
            effect: Some("эффект".into()),
        }];
        let json = weekly_json(&WeeklyData {
            written: Bytes(1000),
            years_left: Some(120.5),
            actions: &actions,
            auto_reverted: 1,
            ..Default::default()
        });

        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(json.contains("\"written_bytes\":1000"));
        assert!(json.contains("\"years_left\":120.5"));
        assert!(
            json.contains("\\\"с кавычками\\\""),
            "кавычки не экранированы"
        );
        assert!(json.contains("\"auto_reverted\":1"));
    }

    #[test]
    fn json_renders_a_missing_projection_as_null() {
        let json = weekly_json(&WeeklyData {
            years_left: None,
            ..Default::default()
        });
        assert!(json.contains("\"years_left\":null"));
    }

    #[test]
    fn the_three_formats_agree_on_emptiness() {
        // Пустая неделя во всех трёх форматах должна оставаться пустой,
        // а не выдумывать содержимое.
        let data = WeeklyData::default();
        assert!(weekly_markdown(&data).contains("ничего не менял"));
        assert!(weekly_html(&data).contains("ничего не менял"));
        assert!(weekly_json(&data).contains("\"actions\":[]"));
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
