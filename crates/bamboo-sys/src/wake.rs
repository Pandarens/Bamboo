//! Пробуждения из сна (ТЗ, раздел 9.6).
//!
//! Прямой ответ на вопрос «почему рюкзак с ноутбуком был горячим».
//! Данные лежат в журнале System и доступны без прав администратора.
//!
//! Читаем провайдер `Microsoft-Windows-Power-Troubleshooter`, а не вывод
//! `powercfg /lastwake`: у первого есть история, а второй показывает только
//! последнее пробуждение и требует разбора текста, зависящего от локали.

use bamboo_core::Result;

use crate::eventlog::{query, EventXml};

const CHANNEL: &str = "System";
const PROVIDER: &str = "Microsoft-Windows-Power-Troubleshooter";

/// Что разбудило систему.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WakeSource {
    /// Таймер пробуждения, обычно задача планировщика.
    Timer(String),
    /// Сетевой адаптер: Wake-on-LAN или magic packet.
    Network,
    /// Клавиатура, мышь, другое устройство ввода.
    Input,
    /// Кнопка питания или крышка ноутбука.
    PowerButton,
    /// Часы реального времени.
    RealTimeClock,
    /// Другое устройство. Строка — то, что сообщила система.
    Device(String),
    /// Система источник не назвала. Не догадываемся.
    Unknown,
}

impl WakeSource {
    pub fn describe(&self) -> String {
        match self {
            WakeSource::Timer(what) if what.is_empty() => "таймер пробуждения".to_string(),
            WakeSource::Timer(what) => format!("таймер пробуждения: {what}"),
            WakeSource::Network => "сетевой адаптер".to_string(),
            WakeSource::Input => "устройство ввода".to_string(),
            WakeSource::PowerButton => "кнопка питания или крышка".to_string(),
            WakeSource::RealTimeClock => "часы реального времени".to_string(),
            WakeSource::Device(what) => format!("устройство: {what}"),
            WakeSource::Unknown => "система не назвала источник".to_string(),
        }
    }

    /// Можно ли отключить этот источник обратимым действием.
    ///
    /// Для таймера — снять галочку у задачи планировщика, для адаптера —
    /// «разрешить этому устройству выводить компьютер из ждущего режима».
    /// Остальное трогать нельзя: кнопкой питания пользуется человек.
    pub fn is_actionable(&self) -> bool {
        matches!(self, WakeSource::Timer(_) | WakeSource::Network)
    }
}

/// Одно пробуждение.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeEvent {
    pub at_unix_ms: i64,
    /// Когда система уснула.
    pub slept_at_unix_ms: Option<i64>,
    pub source: WakeSource,
}

impl WakeEvent {
    /// Сколько система проспала до этого пробуждения.
    pub fn sleep_duration_ms(&self) -> Option<i64> {
        let slept = self.slept_at_unix_ms?;
        (self.at_unix_ms > slept).then_some(self.at_unix_ms - slept)
    }
}

/// История пробуждений, от свежих к старым.
pub fn wake_history(limit: usize) -> Result<Vec<WakeEvent>> {
    let xpath = format!("*[System[Provider[@Name='{PROVIDER}']]]");
    let events = query(CHANNEL, &xpath, limit)?;
    Ok(events.iter().filter_map(parse_wake).collect())
}

fn parse_wake(event: &EventXml) -> Option<WakeEvent> {
    let raw_source = ["WakeSourceText", "WakeSource", "SourceText", "Source"]
        .iter()
        .find_map(|field| event.data(field))
        .unwrap_or("");

    Some(WakeEvent {
        at_unix_ms: event
            .data("WakeTime")
            .and_then(bamboo_core::time::parse_iso8601_utc_ms)
            .or_else(|| event.time_ms())?,
        slept_at_unix_ms: event
            .data("SleepTime")
            .and_then(bamboo_core::time::parse_iso8601_utc_ms),
        source: classify(raw_source),
    })
}

/// Разбирает описание источника.
///
/// Система отдаёт свободный текст вида `Device -\_SB.PCI0.RP09.PXSX`
/// или `Timer - Windows will execute 'NT TASK\...'`. Формулировки зависят
/// от версии и локали, поэтому распознаём по устойчивым признакам,
/// а всё нераспознанное честно оставляем как есть.
fn classify(raw: &str) -> WakeSource {
    let text = raw.trim();
    if text.is_empty() {
        return WakeSource::Unknown;
    }
    let lower = text.to_lowercase();

    if lower.starts_with("timer") || lower.contains("таймер") {
        // Из текста таймера полезно только имя задачи в кавычках.
        let task = text
            .split('\'')
            .nth(1)
            .or_else(|| text.split('"').nth(1))
            .unwrap_or("")
            .to_string();
        return WakeSource::Timer(task);
    }
    if lower.contains("power button") || lower.contains("кнопка") || lower.contains("lid") {
        return WakeSource::PowerButton;
    }
    if lower.contains("real time clock") || lower.contains("rtc") {
        return WakeSource::RealTimeClock;
    }
    if lower.contains("ethernet") || lower.contains("network") || lower.contains("wi-fi") {
        return WakeSource::Network;
    }
    if lower.contains("keyboard") || lower.contains("mouse") || lower.contains("hid") {
        return WakeSource::Input;
    }
    if lower.starts_with("unknown") || lower.starts_with("none") {
        return WakeSource::Unknown;
    }

    WakeSource::Device(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(xml: &str) -> EventXml {
        crate::eventlog::EventXml::from_xml_for_tests(xml.to_string())
    }

    fn wake_xml(source: &str) -> String {
        format!(
            "<Event><System><EventID>1</EventID>\
<TimeCreated SystemTime='2026-03-12T03:14:00.0000000Z'/></System>\
<EventData><Data Name='SleepTime'>2026-03-12T01:00:00.0000000Z</Data>\
<Data Name='WakeTime'>2026-03-12T03:14:00.0000000Z</Data>\
<Data Name='WakeSource'>{source}</Data></EventData></Event>"
        )
    }

    #[test]
    fn a_scheduled_task_wake_names_the_task() {
        let xml = wake_xml(
            "Timer - Windows will execute 'NT TASK\\Microsoft\\Windows\\UpdateOrchestrator\\Reboot'",
        );
        let wake = parse_wake(&event(&xml)).unwrap();
        match &wake.source {
            WakeSource::Timer(task) => assert!(task.contains("UpdateOrchestrator")),
            other => panic!("ожидался таймер, получили {other:?}"),
        }
        assert!(wake.source.is_actionable());
    }

    #[test]
    fn sleep_duration_is_computed() {
        let wake = parse_wake(&event(&wake_xml("Timer"))).unwrap();
        // С 01:00 до 03:14 — два часа четырнадцать минут.
        assert_eq!(wake.sleep_duration_ms(), Some((2 * 60 + 14) * 60 * 1000));
    }

    #[test]
    fn network_and_input_are_distinguished() {
        assert_eq!(
            classify("Device -Realtek PCIe GbE Family Controller (Network)"),
            WakeSource::Network
        );
        assert_eq!(classify("Device -HID Keyboard Device"), WakeSource::Input);
    }

    #[test]
    fn the_power_button_is_not_something_to_disable() {
        let source = classify("Power Button");
        assert_eq!(source, WakeSource::PowerButton);
        assert!(!source.is_actionable(), "кнопку питания трогать нельзя");
    }

    #[test]
    fn an_unrecognised_source_is_kept_verbatim() {
        let source = classify("Device -\\_SB.PCI0.RP09.PXSX");
        match &source {
            WakeSource::Device(text) => assert!(text.contains("PXSX")),
            other => panic!("ожидалось устройство, получили {other:?}"),
        }
        // Ничего не выдумываем и действий не предлагаем.
        assert!(!source.is_actionable());
    }

    #[test]
    fn a_silent_system_gets_no_invented_reason() {
        assert_eq!(classify(""), WakeSource::Unknown);
        assert_eq!(classify("Unknown"), WakeSource::Unknown);
        assert!(WakeSource::Unknown.describe().contains("не назвала"));
    }

    #[test]
    fn history_query_works_on_a_live_system() {
        // На стационарной машине пробуждений может не быть вовсе —
        // проверяем, что запрос отрабатывает, а не что данные есть.
        let history = wake_history(10).expect("запрос к журналу System не прошёл");
        for wake in &history {
            assert!(wake.at_unix_ms > 0);
        }
    }
}
