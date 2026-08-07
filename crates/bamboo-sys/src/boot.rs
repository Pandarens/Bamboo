//! Время загрузки и её деградации (ТЗ, раздел 9.8).
//!
//! Windows измеряет каждую загрузку сама, раскладывает её по этапам и даже
//! называет виновных — приложения, службы и драйверы, из-за которых стало
//! дольше. И не показывает пользователю ничего из этого: канал
//! `Diagnostics-Performance` доступен только администратору и не имеет
//! интерфейса в системе.

use bamboo_core::{Error, Result};

use crate::eventlog::{query, EventXml};

/// Канал с диагностикой производительности.
const CHANNEL: &str = "Microsoft-Windows-Diagnostics-Performance/Operational";

/// Одна загрузка системы.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootRecord {
    /// Когда система загрузилась, миллисекунды эпохи Unix.
    pub at_unix_ms: i64,
    /// Полное время загрузки.
    pub total_ms: u64,
    /// Основная фаза: до появления рабочего стола.
    pub main_path_ms: u64,
    /// Фаза после появления рабочего стола — автозагрузка и отложенные службы.
    pub post_boot_ms: u64,
    /// Насколько эта загрузка оказалась дольше обычной, по мнению Windows.
    pub degradation_ms: Option<u64>,
}

/// Кто именно замедлил загрузку.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Culprit {
    Application,
    Driver,
    Service,
    Device,
    Other,
}

impl Culprit {
    fn from_event_id(id: u32) -> Culprit {
        match id {
            101 => Culprit::Application,
            102 => Culprit::Driver,
            103 => Culprit::Service,
            109 => Culprit::Device,
            _ => Culprit::Other,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Culprit::Application => "приложение",
            Culprit::Driver => "драйвер",
            Culprit::Service => "служба",
            Culprit::Device => "устройство",
            Culprit::Other => "компонент",
        }
    }
}

/// Замедливший загрузку компонент.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootCulprit {
    pub at_unix_ms: i64,
    pub kind: Culprit,
    pub name: String,
    /// Сколько всего заняло.
    pub total_ms: u64,
    /// Насколько это дольше обычного для него же.
    pub degradation_ms: Option<u64>,
}

/// История загрузок, от свежих к старым.
pub fn boot_history(limit: usize) -> Result<Vec<BootRecord>> {
    let events = query(CHANNEL, "*[System[(EventID=100)]]", limit)?;
    Ok(events.iter().filter_map(parse_boot).collect())
}

/// Компоненты, замедлившие загрузку.
pub fn boot_culprits(limit: usize) -> Result<Vec<BootCulprit>> {
    let events = query(
        CHANNEL,
        "*[System[(EventID=101 or EventID=102 or EventID=103 or EventID=109)]]",
        limit,
    )?;
    Ok(events.iter().filter_map(parse_culprit).collect())
}

fn parse_boot(event: &EventXml) -> Option<BootRecord> {
    Some(BootRecord {
        at_unix_ms: event.time_ms()?,
        total_ms: event.data_u64("BootTime")?,
        main_path_ms: event.data_u64("MainPathBootTime").unwrap_or(0),
        post_boot_ms: event.data_u64("BootPostBootTime").unwrap_or(0),
        // Ноль означает «деградации не было», а не «неизвестно».
        degradation_ms: event.data_u64("BootDegradationTime").filter(|ms| *ms > 0),
    })
}

fn parse_culprit(event: &EventXml) -> Option<BootCulprit> {
    let id = event.event_id()?;

    // Имя лежит под разными полями в зависимости от типа виновника,
    // поэтому перебираем кандидатов по очереди.
    let name = ["Name", "FriendlyName", "ServiceName", "FileName", "Path"]
        .iter()
        .find_map(|field| event.data(field))
        .filter(|value| !value.is_empty())?
        .to_string();

    Some(BootCulprit {
        at_unix_ms: event.time_ms()?,
        kind: Culprit::from_event_id(id),
        name,
        total_ms: event
            .data_u64("TotalTime")
            .or_else(|| event.data_u64("Time"))
            .unwrap_or(0),
        degradation_ms: event.data_u64("DegradationTime").filter(|ms| *ms > 0),
    })
}

/// Доступен ли канал диагностики.
///
/// Отдельная проверка нужна, чтобы интерфейс мог заранее сказать
/// «нужны права администратора», а не показывать пустой раздел.
pub fn available() -> Result<()> {
    match query(CHANNEL, "*[System[(EventID=100)]]", 1) {
        Ok(_) => Ok(()),
        Err(Error::Unsupported(reason)) => Err(Error::Unsupported(reason)),
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(xml: &str) -> EventXml {
        // EventXml намеренно не конструируется снаружи крейта;
        // внутри тестов пользуемся тем же разбором, что и в бою.
        crate::eventlog::EventXml::from_xml_for_tests(xml.to_string())
    }

    const BOOT_100: &str = "<Event><System><EventID>100</EventID>\
<TimeCreated SystemTime='2026-03-12T07:01:15.0000000Z'/></System>\
<EventData><Data Name='BootTime'>42150</Data>\
<Data Name='MainPathBootTime'>18300</Data>\
<Data Name='BootPostBootTime'>23850</Data>\
<Data Name='BootDegradationTime'>8100</Data></EventData></Event>";

    const CULPRIT_101: &str = "<Event><System><EventID>101</EventID>\
<TimeCreated SystemTime='2026-03-12T07:01:15.0000000Z'/></System>\
<EventData><Data Name='Name'>Steam Client Bootstrapper</Data>\
<Data Name='TotalTime'>9400</Data>\
<Data Name='DegradationTime'>4200</Data></EventData></Event>";

    #[test]
    fn boot_record_is_parsed() {
        let record = parse_boot(&event(BOOT_100)).unwrap();
        assert_eq!(record.total_ms, 42_150);
        assert_eq!(record.main_path_ms, 18_300);
        assert_eq!(record.post_boot_ms, 23_850);
        assert_eq!(record.degradation_ms, Some(8_100));
        assert!(record.at_unix_ms > 1_700_000_000_000);
    }

    #[test]
    fn zero_degradation_means_none_not_unknown() {
        let xml = BOOT_100.replace(">8100<", ">0<");
        assert_eq!(parse_boot(&event(&xml)).unwrap().degradation_ms, None);
    }

    #[test]
    fn culprit_is_parsed_and_classified() {
        let culprit = parse_culprit(&event(CULPRIT_101)).unwrap();
        assert_eq!(culprit.kind, Culprit::Application);
        assert_eq!(culprit.name, "Steam Client Bootstrapper");
        assert_eq!(culprit.total_ms, 9_400);
        assert_eq!(culprit.degradation_ms, Some(4_200));
    }

    #[test]
    fn service_culprit_uses_its_own_field() {
        let xml = "<Event><System><EventID>103</EventID>\
<TimeCreated SystemTime='2026-03-12T07:01:15.0000000Z'/></System>\
<EventData><Data Name='ServiceName'>SysMain</Data>\
<Data Name='TotalTime'>6200</Data></EventData></Event>";
        let culprit = parse_culprit(&event(xml)).unwrap();
        assert_eq!(culprit.kind, Culprit::Service);
        assert_eq!(culprit.name, "SysMain");
        assert_eq!(culprit.degradation_ms, None);
    }

    #[test]
    fn an_event_without_a_name_is_skipped() {
        let xml = "<Event><System><EventID>101</EventID>\
<TimeCreated SystemTime='2026-03-12T07:01:15.0000000Z'/></System>\
<EventData><Data Name='TotalTime'>100</Data></EventData></Event>";
        assert!(parse_culprit(&event(xml)).is_none());
    }

    #[test]
    fn the_channel_either_opens_or_explains_itself() {
        // Без прав администратора этот канал закрыт, и это нормально:
        // важно, что причина названа, а не что данные есть.
        match available() {
            Ok(()) => {}
            Err(error) => {
                let text = error.to_string();
                assert!(text.contains("администратора") || text.contains("EvtQuery"));
            }
        }
    }
}
