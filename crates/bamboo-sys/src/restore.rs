//! Точки восстановления системы (ТЗ, раздел 12.2).
//!
//! Перед действиями уровня 5–6 — остановкой и отключением служб — Bamboo
//! обязан оставить путь назад. Свой откат у него есть, но откат чинит то,
//! что Bamboo сделал сам; точка восстановления страхует случай, когда
//! сломалось что-то ещё.
//!
//! Вся сложность здесь в одном: **вызвать не значит создать**. Windows
//! придерживает создание точек — если свежая точка уже есть, она молча
//! пропускает новую. На этой машине в журнале шесть записей «точка создана»
//! против пятидесяти четырёх «пропускаю, свежая уже есть». Программа,
//! которая считает свой вызов успехом, выдаёт ровно ту самую плацебо-функцию
//! из раздела 11.5 ТЗ: отчиталась о защите, которой нет.
//!
//! Поэтому здесь проверяется не возврат вызова, а результат: появилась ли
//! в журнале запись о созданной точке. Факт вместо намерения.

use bamboo_core::Result;
use windows_sys::Win32::System::Restore::{
    SRSetRestorePointW, APPLICATION_INSTALL, BEGIN_SYSTEM_CHANGE, RESTOREPOINTINFOW, STATEMGRSTATUS,
};

/// Длина поля описания в структуре Windows, вместе с завершающим нулём.
const DESCRIPTION_LENGTH: usize = 256;

/// Чем кончилась попытка создать точку.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// Точка создана.
    Created,
    /// Windows пропустила создание: свежая точка уже есть. Это не отказ —
    /// защита на месте, просто её обеспечила предыдущая точка.
    AlreadyRecent,
    /// Защита системы выключена. Точку создать нельзя, и это повод
    /// отказаться от действия, а не продолжить без страховки.
    ProtectionOff,
}

impl RestoreOutcome {
    /// Есть ли путь назад после этой попытки.
    pub fn is_protected(self) -> bool {
        matches!(
            self,
            RestoreOutcome::Created | RestoreOutcome::AlreadyRecent
        )
    }

    /// Что сказать человеку.
    pub fn explain(self) -> &'static str {
        match self {
            RestoreOutcome::Created => {
                "Точка восстановления создана — если что-то пойдёт не так, \
                 систему можно вернуть к этому моменту средствами Windows."
            }
            RestoreOutcome::AlreadyRecent => {
                "Новую точку восстановления Windows не создала: свежая уже есть, \
                 и она моложе суток. Это не сбой — путь назад на месте, его \
                 обеспечивает предыдущая точка."
            }
            RestoreOutcome::ProtectionOff => {
                "Защита системы выключена, и точку восстановления создать нельзя. \
                 Действие отменено: рискованные изменения без пути назад Bamboo \
                 не делает. Включить защиту можно в свойствах системы, раздел \
                 «Защита системы»."
            }
        }
    }
}

/// Когда в последний раз создавалась точка восстановления.
///
/// Читаем журнал, а не спрашиваем службу: событие 8194 пишет сама Windows
/// при каждом успешном создании, кем бы оно ни было затеяно — обновлением,
/// установкой драйвера или нами.
pub fn last_restore_point_ms() -> Result<Option<i64>> {
    let events = crate::eventlog::query(
        "Application",
        // Событие 8194 — «точка восстановления создана». Его пишет сама
        // Windows при каждом успешном создании.
        "*[System[Provider[@Name='System Restore'] and (EventID=8194)]]",
        1,
    )?;
    Ok(events.first().and_then(|event| event.time_ms()))
}

/// Создаёт точку восстановления перед рискованным действием.
///
/// Возвращает не «получилось ли вызвать», а что на самом деле произошло.
/// Требует прав администратора: без них вызов не удастся, и это честная
/// ошибка, а не повод сделать вид, что точка есть.
pub fn create_restore_point(description: &str) -> Result<RestoreOutcome> {
    // Запоминаем, что было до: только так можно отличить созданную точку
    // от придержанной. Возврат вызова этого не различает.
    let before = last_restore_point_ms()?;

    let mut info: RESTOREPOINTINFOW = unsafe { core::mem::zeroed() };
    info.dwEventType = BEGIN_SYSTEM_CHANGE;
    // «Установка приложения» — ближайший по смыслу тип из тех, что
    // предлагает Windows: мы меняем состав и настройки служб.
    info.dwRestorePtType = APPLICATION_INSTALL;
    info.llSequenceNumber = 0;

    // Описание собираем отдельно и кладём целиком: структура упакована,
    // и ссылку на её поле брать нельзя — она может оказаться невыровненной.
    // Строку обрезаем по длине поля: обрезать её должен тот, кто знает
    // предел, а не вызывающий.
    let mut description_field = [0u16; DESCRIPTION_LENGTH];
    for (slot, symbol) in description_field
        .iter_mut()
        .take(DESCRIPTION_LENGTH - 1)
        .zip(description.encode_utf16())
    {
        *slot = symbol;
    }
    info.szDescription = description_field;

    let mut status: STATEMGRSTATUS = unsafe { core::mem::zeroed() };
    let ok = unsafe { SRSetRestorePointW(&info, &mut status) };

    if ok == 0 {
        // Отказ вызова означает почти всегда одно: защита системы выключена
        // либо не хватает прав. Различить их по коду нельзя, а вот проверить
        // журнал — можно.
        return Ok(RestoreOutcome::ProtectionOff);
    }

    // Вызов удался — но создалась ли точка? Смотрим журнал: если появилась
    // свежая запись, точка наша. Если нет, Windows придержала создание.
    let after = last_restore_point_ms()?;
    let created = match (before, after) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(was), Some(now)) => now > was,
    };

    if created {
        return Ok(RestoreOutcome::Created);
    }

    // Точки не появилось. Если свежая всё-таки есть, защита на месте.
    let recent = after.is_some_and(|when| {
        let now = bamboo_core::SampleTime::wall_clock_now();
        now.saturating_sub(when) < 24 * 60 * 60 * 1000
    });
    Ok(if recent {
        RestoreOutcome::AlreadyRecent
    } else {
        RestoreOutcome::ProtectionOff
    })
}

/// Свежая ли точка есть прямо сейчас, без попытки создать новую.
///
/// Дешёвая проверка для случая, когда действие ещё только предлагается:
/// спрашивать Windows о создании точки ради подсказки в интерфейсе незачем.
pub fn has_recent_restore_point(within_ms: i64) -> Result<bool> {
    let Some(when) = last_restore_point_ms()? else {
        return Ok(false);
    };
    let now = bamboo_core::SampleTime::wall_clock_now();
    Ok(now.saturating_sub(when) < within_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_created_point_and_a_recent_one_both_mean_protected() {
        // Придержанное создание — не провал: путь назад обеспечивает
        // предыдущая точка, и отменять из-за этого действие незачем.
        assert!(RestoreOutcome::Created.is_protected());
        assert!(RestoreOutcome::AlreadyRecent.is_protected());
        assert!(!RestoreOutcome::ProtectionOff.is_protected());
    }

    #[test]
    fn a_skipped_point_is_not_reported_as_created() {
        // Главная ловушка этого раздела. Windows придерживает создание,
        // и программа, считающая свой вызов успехом, отчитывается о защите,
        // которой нет, — плацебо из раздела 11.5.
        let skipped = RestoreOutcome::AlreadyRecent.explain();
        assert!(skipped.contains("не создала"), "{skipped}");
        assert!(skipped.contains("не сбой"), "{skipped}");
    }

    #[test]
    fn protection_off_cancels_the_action_and_says_how_to_fix_it() {
        let off = RestoreOutcome::ProtectionOff.explain();
        assert!(off.contains("отменено"), "{off}");
        // Совет обязан быть выполнимым: куда идти и что включать.
        assert!(off.contains("Защита системы"), "{off}");
    }

    #[test]
    fn every_outcome_explains_itself() {
        for outcome in [
            RestoreOutcome::Created,
            RestoreOutcome::AlreadyRecent,
            RestoreOutcome::ProtectionOff,
        ] {
            assert!(outcome.explain().len() > 60, "{outcome:?}");
        }
    }

    #[test]
    fn a_long_description_does_not_overflow_the_field() {
        // Поле фиксированной длины. Описание длиннее обрезается на нашей
        // стороне: переполнить структуру Windows нельзя.
        let long = "я".repeat(1000);
        let mut field = [0u16; DESCRIPTION_LENGTH];
        for (slot, symbol) in field
            .iter_mut()
            .take(DESCRIPTION_LENGTH - 1)
            .zip(long.encode_utf16())
        {
            *slot = symbol;
        }
        // Последнее место обязано остаться под завершающий ноль.
        assert_eq!(field[DESCRIPTION_LENGTH - 1], 0);
    }

    #[test]
    fn the_journal_answers_about_the_last_point() {
        // Живая проверка чтения. Точек может не быть вовсе — это законный
        // ответ, а не ошибка.
        match last_restore_point_ms() {
            Ok(Some(when)) => {
                assert!(when > 0);
                // Точка из будущего означала бы, что мы читаем не то поле.
                let now = bamboo_core::SampleTime::wall_clock_now();
                assert!(when <= now + 60_000, "точка из будущего: {when} > {now}");
            }
            Ok(None) => {}
            // Журнал приложений может быть недоступен без прав — штатно.
            Err(_) => {}
        }
    }
}
