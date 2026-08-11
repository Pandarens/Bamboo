//! Имя задачи планировщика, запустившей процесс (ТЗ, раздел 7.2).
//!
//! Отвечает на вопрос, которым кончается всякое расследование внезапной
//! нагрузки: «откуда взялся этот процесс». Родитель у таких процессов —
//! `svchost.exe`, и цепочка родителей упирается в него же, ничего не
//! объясняя. А планировщик при каждом запуске пишет в свой журнал событие
//! 129 с именем задачи и номером запущенного процесса.
//!
//! Главная опасность здесь — не в разборе, а в молчании. Канал планировщика
//! в Windows по умолчанию **выключен**, а запрос к выключенному каналу
//! ошибки не возвращает: он отдаёт пустой список, неотличимый от «задач
//! не запускалось». Программа, которая на это купится, будет уверенно
//! молчать там, где на самом деле ничего не знает, — плацебо из раздела
//! 11.5 ТЗ в чистом виде. Поэтому состояние канала проверяется до каждого
//! чтения.

use bamboo_core::{Error, Result};
use windows_sys::Win32::System::EventLog::{
    EvtChannelConfigEnabled, EvtClose, EvtGetChannelConfigProperty, EvtOpenChannelConfig,
    EVT_VARIANT,
};

/// Канал планировщика заданий.
pub const CHANNEL: &str = "Microsoft-Windows-TaskScheduler/Operational";

/// Событие «задача запустила процесс»: несёт имя задачи и номер процесса.
const TASK_STARTED: u32 = 129;

/// Задача планировщика, запустившая процесс.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartedByTask {
    /// Полное имя задачи, как его показывает планировщик.
    pub task: String,
    /// Что она запустила.
    pub pid: u32,
}

/// Включён ли канал журнала.
///
/// Отдельной проверкой, а не по факту пустого ответа: пустой ответ
/// от выключенного канала выглядит точно так же, как «событий не было».
pub fn channel_enabled(channel: &str) -> Result<bool> {
    let wide: Vec<u16> = channel.encode_utf16().chain(core::iter::once(0)).collect();

    let config = unsafe { EvtOpenChannelConfig(0, wide.as_ptr(), 0) };
    if config == 0 {
        return Err(Error::Win32 {
            call: "EvtOpenChannelConfig",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }

    let mut value: EVT_VARIANT = unsafe { core::mem::zeroed() };
    let mut used: u32 = 0;
    let ok = unsafe {
        EvtGetChannelConfigProperty(
            config,
            EvtChannelConfigEnabled,
            0,
            core::mem::size_of::<EVT_VARIANT>() as u32,
            &mut value,
            &mut used,
        )
    };
    unsafe { EvtClose(config) };

    if ok == 0 {
        return Err(Error::Win32 {
            call: "EvtGetChannelConfigProperty",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }
    Ok(unsafe { value.Anonymous.BooleanVal } != 0)
}

/// Ищет, какая задача запустила процесс.
///
/// `Ok(None)` означает ровно одно: канал включён, события читались, и
/// среди них этого процесса нет — значит запустила его не задача.
/// Если канал выключен, возвращается ошибка, а не пустота: молчать там,
/// где мы ничего не знаем, нельзя.
pub fn started_by_task(pid: u32, look_back: usize) -> Result<Option<StartedByTask>> {
    if !channel_enabled(CHANNEL)? {
        return Err(Error::Unsupported(
            "журнал планировщика заданий выключен: узнать имя задачи нельзя.              Включить его можно в просмотре событий, ветка Microsoft-Windows-TaskScheduler",
        ));
    }

    let events = crate::eventlog::query(
        CHANNEL,
        &format!("*[System[(EventID={TASK_STARTED})]]"),
        look_back,
    )?;

    for event in events {
        // В событии 129 номер процесса лежит в поле ProcessID данных
        // события — не путать с ProcessID системной части заголовка,
        // где стоит сам планировщик.
        if event.data_u64("ProcessID") != Some(u64::from(pid)) {
            continue;
        }
        let Some(task) = event.data("TaskName") else {
            continue;
        };
        return Ok(Some(StartedByTask {
            task: task.to_string(),
            pid,
        }));
    }
    Ok(None)
}

/// Все задачи, запускавшие процессы за последнее время.
///
/// Пригодится для разбора всплеска: чаще интересно не «кто запустил вот
/// этот процесс», а «что вообще запускалось, пока машина тормозила».
pub fn recent_task_starts(limit: usize) -> Result<Vec<StartedByTask>> {
    if !channel_enabled(CHANNEL)? {
        return Err(Error::Unsupported(
            "журнал планировщика заданий выключен: список запусков недоступен",
        ));
    }

    let events = crate::eventlog::query(
        CHANNEL,
        &format!("*[System[(EventID={TASK_STARTED})]]"),
        limit,
    )?;

    Ok(events
        .into_iter()
        .filter_map(|event| {
            Some(StartedByTask {
                task: event.data("TaskName")?.to_string(),
                pid: event.data_u64("ProcessID")? as u32,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_channel_state_is_answered_without_error() {
        // Живая проверка. Канал может быть и включён, и выключен —
        // важно, что мы это знаем, а не гадаем.
        let state = channel_enabled(CHANNEL);
        assert!(
            state.is_ok(),
            "состояние канала обязано читаться: {state:?}"
        );
    }

    #[test]
    fn a_disabled_channel_is_an_error_not_an_empty_answer() {
        // Главное правило этого модуля. Запрос к выключенному каналу
        // отдаёт пустой список, неотличимый от «задач не запускалось».
        // Молчать там, где мы ничего не знаем, — плацебо.
        let Ok(enabled) = channel_enabled(CHANNEL) else {
            return; // Состояние не прочиталось — проверять нечего.
        };

        let answer = started_by_task(4, 50);
        if enabled {
            // Канал включён: ответ обязан быть определённым.
            assert!(answer.is_ok(), "{answer:?}");
        } else {
            let error = answer.expect_err("выключенный канал обязан быть ошибкой");
            let text = error.to_string();
            assert!(text.contains("выключен"), "{text}");
            // И должен сказать, что с этим делать.
            assert!(text.contains("Включить"), "{text}");
        }
    }

    #[test]
    fn an_unknown_channel_fails_cleanly() {
        let error = channel_enabled("Нет-Такого-Канала/Operational");
        assert!(error.is_err());
    }

    #[test]
    fn the_kernel_process_was_not_started_by_a_task() {
        // Процесс с номером 4 — ядро, его не запускает никакая задача.
        // Если канал включён, ответ обязан быть «нет», а не выдумка.
        if channel_enabled(CHANNEL).unwrap_or(false) {
            assert_eq!(started_by_task(4, 50).unwrap(), None);
        }
    }
}
