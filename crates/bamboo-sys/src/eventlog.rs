//! Чтение журналов событий Windows.
//!
//! Windows сама собирает массу диагностики — время загрузки с разбивкой
//! по компонентам, причины пробуждений, деградации — и не показывает
//! пользователю ничего из этого. Здесь мы её забираем.
//!
//! Работаем через `EvtQuery`, а не через разбор вывода `wevtutil`
//! или `powercfg`: парсить чужой текстовый вывод — гарантированная поломка
//! на следующем обновлении или другой локали.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ACCESS_DENIED, ERROR_NO_MORE_ITEMS};
use windows_sys::Win32::System::EventLog::{
    EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection, EvtRender,
    EvtRenderEventXml, EVT_HANDLE,
};

/// Одно событие, отрисованное в XML.
pub struct EventXml(String);

impl EventXml {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Собирает событие из готового XML. Нужно тестам разборщиков:
    /// прогонять их на живом журнале нельзя — там нет нужных событий,
    /// а на машине разработчика они появятся не раньше следующего сбоя.
    #[cfg(test)]
    pub(crate) fn from_xml_for_tests(xml: String) -> EventXml {
        EventXml(xml)
    }

    /// Значение поля `<Data Name='...'>` из секции `EventData`.
    pub fn data(&self, name: &str) -> Option<&str> {
        // EvtRender отдаёт атрибуты в одинарных кавычках, но тот же XML,
        // пропущенный через любой нормализатор, приезжает в двойных.
        // Принимаем оба варианта.
        let start = [format!("Name='{name}'>"), format!("Name=\"{name}\">")]
            .iter()
            .find_map(|key| self.0.find(key.as_str()).map(|at| at + key.len()))?;
        let end = self.0[start..].find("</Data>")? + start;
        Some(self.0[start..end].trim())
    }

    /// Числовое поле `EventData`.
    pub fn data_u64(&self, name: &str) -> Option<u64> {
        self.data(name)?.parse().ok()
    }

    /// Идентификатор события.
    pub fn event_id(&self) -> Option<u32> {
        between(&self.0, "<EventID", "</EventID>")
            .and_then(|text| text.rsplit('>').next().map(str::to_string))
            .and_then(|text| text.trim().parse().ok())
    }

    /// Время события в миллисекундах эпохи Unix.
    pub fn time_ms(&self) -> Option<i64> {
        let raw = attribute(&self.0, "SystemTime=")?;
        bamboo_core::time::parse_iso8601_utc_ms(raw)
    }
}

fn between<'a>(haystack: &'a str, from: &str, to: &str) -> Option<&'a str> {
    let start = haystack.find(from)?;
    let end = haystack[start..].find(to)? + start;
    Some(&haystack[start..end])
}

/// Значение XML-атрибута по его имени вместе со знаком равенства.
/// Кавычки могут быть любыми — читаем ту, что стоит первой.
fn attribute<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let after = &text[text.find(key)? + key.len()..];
    let quote = after.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let end = after[1..].find(quote)? + 1;
    Some(&after[1..end])
}

/// Дескриптор запроса к журналу. Закрывается автоматически.
struct QueryHandle(EVT_HANDLE);

impl Drop for QueryHandle {
    fn drop(&mut self) {
        unsafe { EvtClose(self.0) };
    }
}

/// Читает события канала, начиная с самых свежих.
///
/// `xpath` — стандартный фильтр журнала событий, например
/// `*[System[(EventID=100)]]`. Пустая строка означает «все события».
pub fn query(channel: &str, xpath: &str, limit: usize) -> Result<Vec<EventXml>> {
    let channel_w: Vec<u16> = channel.encode_utf16().chain(core::iter::once(0)).collect();
    let query_w: Vec<u16> = if xpath.is_empty() { "*" } else { xpath }
        .encode_utf16()
        .chain(core::iter::once(0))
        .collect();

    let handle = unsafe {
        EvtQuery(
            0,
            channel_w.as_ptr(),
            query_w.as_ptr(),
            // Обратный порядок: свежие события интереснее старых, а забирать
            // весь журнал ради последних десяти записей незачем.
            EvtQueryChannelPath | EvtQueryReverseDirection,
        )
    };

    if handle == 0 {
        let code = unsafe { GetLastError() };
        // Часть каналов, в том числе Diagnostics-Performance, читается
        // только администратором. Для агента без прав это штатная ситуация,
        // и сообщить о ней надо понятно.
        if code == ERROR_ACCESS_DENIED {
            return Err(Error::Unsupported(
                "этот журнал событий доступен только с правами администратора",
            ));
        }
        return Err(Error::Win32 {
            call: "EvtQuery",
            code,
        });
    }
    let handle = QueryHandle(handle);

    let mut events = Vec::new();
    let mut batch = [0isize; 16];

    while events.len() < limit {
        let want = batch.len().min(limit - events.len()) as u32;
        let mut returned: u32 = 0;
        let ok = unsafe { EvtNext(handle.0, want, batch.as_mut_ptr(), 0, 0, &mut returned) };

        if ok == 0 {
            let code = unsafe { GetLastError() };
            if code == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(Error::Win32 {
                call: "EvtNext",
                code,
            });
        }

        for event in batch.iter().take(returned as usize) {
            if let Ok(xml) = render(*event) {
                events.push(EventXml(xml));
            }
            unsafe { EvtClose(*event) };
        }

        if returned == 0 {
            break;
        }
    }

    Ok(events)
}

/// Отрисовывает событие в XML.
fn render(event: EVT_HANDLE) -> Result<String> {
    let mut needed: u32 = 0;
    let mut properties: u32 = 0;

    // Первый вызов только узнаёт нужный размер: он всегда завершается
    // неуспехом с ERROR_INSUFFICIENT_BUFFER.
    unsafe {
        EvtRender(
            0,
            event,
            EvtRenderEventXml,
            0,
            core::ptr::null_mut(),
            &mut needed,
            &mut properties,
        )
    };

    if needed == 0 {
        return Err(Error::Malformed("событие отрисовалось в пустой буфер"));
    }

    let mut buffer = vec![0u16; needed as usize / 2 + 1];
    let ok = unsafe {
        EvtRender(
            0,
            event,
            EvtRenderEventXml,
            needed,
            buffer.as_mut_ptr().cast(),
            &mut needed,
            &mut properties,
        )
    };

    if ok == 0 {
        return Err(Error::Win32 {
            call: "EvtRender",
            code: unsafe { GetLastError() },
        });
    }

    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Так выглядит вывод EvtRender: атрибуты в одинарных кавычках.
    const SAMPLE: &str = "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'>\
<System><Provider Name='Microsoft-Windows-Diagnostics-Performance'/>\
<EventID>100</EventID><Version>2</Version>\
<TimeCreated SystemTime='2026-08-07T09:12:33.1234567Z'/>\
</System>\
<EventData><Data Name='BootTime'>31240</Data><Data Name='MainPathBootTime'>12100</Data>\
<Data Name='Name'>Пример</Data></EventData></Event>";

    #[test]
    fn fields_are_extracted_from_xml() {
        let event = EventXml(SAMPLE.to_string());
        assert_eq!(event.event_id(), Some(100));
        assert_eq!(event.data_u64("BootTime"), Some(31_240));
        assert_eq!(event.data_u64("MainPathBootTime"), Some(12_100));
        assert_eq!(event.data("Name"), Some("Пример"));
        assert_eq!(event.data("НетТакого"), None);
        assert_eq!(event.time_ms(), Some(1_786_093_953_123));
    }

    #[test]
    fn double_quoted_xml_is_understood_too() {
        let event = EventXml(SAMPLE.replace('\'', "\""));
        assert_eq!(event.event_id(), Some(100));
        assert_eq!(event.data_u64("BootTime"), Some(31_240));
        assert_eq!(event.time_ms(), Some(1_786_093_953_123));
    }

    #[test]
    fn event_id_with_attributes_is_read() {
        // У части провайдеров тег с атрибутом: <EventID Qualifiers='16384'>7040
        let event = EventXml("<EventID Qualifiers='16384'>7040</EventID>".to_string());
        assert_eq!(event.event_id(), Some(7040));
    }

    #[test]
    fn malformed_xml_does_not_panic() {
        let event = EventXml("<Event><EventID>".to_string());
        assert_eq!(event.event_id(), None);
        assert_eq!(event.time_ms(), None);
        assert_eq!(event.data("что-нибудь"), None);
    }

    #[test]
    fn system_log_is_readable_without_elevation() {
        // Журнал System доступен обычному пользователю. Если это сломается,
        // отвалится анализ пробуждений у агента.
        let events = query("System", "", 5).expect("журнал System не читается");
        assert!(!events.is_empty());
        for event in &events {
            assert!(event.event_id().is_some());
            assert!(event.time_ms().is_some());
        }
    }

    #[test]
    fn newest_events_come_first() {
        let events = query("System", "", 10).unwrap();
        let times: Vec<i64> = events.iter().filter_map(|e| e.time_ms()).collect();
        assert!(times.windows(2).all(|pair| pair[0] >= pair[1]));
    }

    #[test]
    fn xpath_filter_works() {
        let events = query(
            "System",
            "*[System[Provider[@Name='Microsoft-Windows-Kernel-Power']]]",
            5,
        )
        .unwrap();
        assert!(events.iter().all(|e| e.as_str().contains("Kernel-Power")));
    }

    #[test]
    fn a_missing_channel_reports_an_error() {
        assert!(query("Такого-Канала-Нет", "", 1).is_err());
    }
}
