//! Когда можно беспокоить пользователя (ТЗ, раздел 14.4).
//!
//! Уведомление — дорогая валюта. Индустрия оптимизаторов построена
//! на генерации тревоги; здесь всё наоборот: молчание — состояние
//! по умолчанию, и каждое условие работает на то, чтобы промолчать.

/// Не чаще одного уведомления в сутки.
pub const MIN_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

/// Пауза после возвращения пользователя.
///
/// Человек сел за компьютер ради своих дел, а не ради советов.
pub const SETTLE_AFTER_RETURN_MS: i64 = 3 * 60 * 1000;

/// Обстановка на момент проверки.
#[derive(Clone, Copy, Debug, Default)]
pub struct NotifyContext {
    pub now_ms: i64,
    /// Когда показали прошлое уведомление.
    pub last_notification_ms: Option<i64>,
    /// Когда пользователь вернулся за компьютер.
    pub returned_at_ms: Option<i64>,
    /// Система готова принимать уведомления.
    pub accepts_notifications: bool,
    /// Кто-то удерживает экран включённым: воспроизведение или
    /// длительная операция.
    pub display_required: bool,
    /// Идёт заметная передача данных по сети.
    pub network_busy: bool,
    /// Устойчивая нагрузка процессора без ввода: сборка или рендер.
    /// Это работа, а не простой.
    pub sustained_cpu_without_input: bool,
    /// Профиль запрещает уведомления.
    pub profile_silences: bool,
}

/// Почему промолчали. Нужно для отладки и для честного ответа
/// на вопрос «почему я ничего не увидел».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Silence {
    ProfileSilences,
    SystemNotAccepting,
    SomethingIsPlaying,
    NetworkBusy,
    UserIsWorking,
    TooSoonAfterReturn,
    DailyLimitReached,
    NothingToSay,
}

impl Silence {
    pub fn describe(self) -> &'static str {
        match self {
            Silence::ProfileSilences => "профиль требует молчания",
            Silence::SystemNotAccepting => "система не принимает уведомления",
            Silence::SomethingIsPlaying => "идёт воспроизведение или длительная операция",
            Silence::NetworkBusy => "идёт передача данных по сети",
            Silence::UserIsWorking => "процессор занят работой, а не простоем",
            Silence::TooSoonAfterReturn => "вы только что вернулись за компьютер",
            Silence::DailyLimitReached => "сегодня уже было уведомление",
            Silence::NothingToSay => "нечего сообщить",
        }
    }
}

/// Решает, можно ли показать уведомление.
///
/// `pending` — сколько наблюдений накопилось. Ноль означает, что говорить
/// не о чем, и это самая частая ситуация.
pub fn may_notify(context: &NotifyContext, pending: usize) -> Result<(), Silence> {
    if pending == 0 {
        return Err(Silence::NothingToSay);
    }
    if context.profile_silences {
        return Err(Silence::ProfileSilences);
    }
    if !context.accepts_notifications {
        return Err(Silence::SystemNotAccepting);
    }
    if context.display_required {
        return Err(Silence::SomethingIsPlaying);
    }
    if context.network_busy {
        return Err(Silence::NetworkBusy);
    }
    if context.sustained_cpu_without_input {
        return Err(Silence::UserIsWorking);
    }

    if let Some(returned) = context.returned_at_ms {
        if context.now_ms - returned < SETTLE_AFTER_RETURN_MS {
            return Err(Silence::TooSoonAfterReturn);
        }
    }

    if let Some(last) = context.last_notification_ms {
        if context.now_ms - last < MIN_INTERVAL_MS {
            return Err(Silence::DailyLimitReached);
        }
    }

    Ok(())
}

/// Схлопывает накопленные наблюдения в одно уведомление.
///
/// Не серия тостов, а одна фраза, которая разворачивается в виджет.
pub fn collapse(pending: usize) -> String {
    match pending {
        0 => String::new(),
        1 => "Пока вас не было, я кое-что заметил".to_string(),
        count => format!(
            "Пока вас не было, я заметил {count} {}",
            plural_things(count)
        ),
    }
}

fn plural_things(count: usize) -> &'static str {
    let last_two = count % 100;
    let last = count % 10;
    if (11..=14).contains(&last_two) {
        return "вещей";
    }
    match last {
        1 => "вещь",
        2..=4 => "вещи",
        _ => "вещей",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;

    fn ready() -> NotifyContext {
        NotifyContext {
            now_ms: 100 * HOUR,
            accepts_notifications: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_quiet_system_says_nothing() {
        assert_eq!(may_notify(&ready(), 0), Err(Silence::NothingToSay));
    }

    #[test]
    fn with_something_to_say_and_nothing_in_the_way_we_speak() {
        assert_eq!(may_notify(&ready(), 3), Ok(()));
    }

    #[test]
    fn playback_keeps_us_quiet() {
        let mut context = ready();
        context.display_required = true;
        assert_eq!(may_notify(&context, 3), Err(Silence::SomethingIsPlaying));
    }

    #[test]
    fn a_build_running_is_work_not_idleness() {
        let mut context = ready();
        context.sustained_cpu_without_input = true;
        assert_eq!(may_notify(&context, 3), Err(Silence::UserIsWorking));
    }

    #[test]
    fn we_wait_a_few_minutes_after_the_user_returns() {
        // Человек сел за компьютер ради своих дел, а не ради советов.
        let mut context = ready();
        context.returned_at_ms = Some(context.now_ms - 60_000);
        assert_eq!(may_notify(&context, 3), Err(Silence::TooSoonAfterReturn));

        context.returned_at_ms = Some(context.now_ms - 5 * 60_000);
        assert_eq!(may_notify(&context, 3), Ok(()));
    }

    #[test]
    fn no_more_than_one_notification_a_day() {
        let mut context = ready();
        context.last_notification_ms = Some(context.now_ms - 5 * HOUR);
        assert_eq!(may_notify(&context, 3), Err(Silence::DailyLimitReached));

        context.last_notification_ms = Some(context.now_ms - 30 * HOUR);
        assert_eq!(may_notify(&context, 3), Ok(()));
    }

    #[test]
    fn a_silent_profile_wins_over_everything() {
        let mut context = ready();
        context.profile_silences = true;
        assert_eq!(may_notify(&context, 10), Err(Silence::ProfileSilences));
    }

    #[test]
    fn observations_collapse_into_one_message() {
        // Не серия тостов, а одна фраза.
        assert_eq!(collapse(1), "Пока вас не было, я кое-что заметил");
        assert!(collapse(3).contains("3 вещи"));
        assert!(collapse(7).contains("7 вещей"));
        assert!(collapse(21).contains("21 вещь"));
        assert!(collapse(0).is_empty());
    }

    #[test]
    fn every_silence_can_explain_itself() {
        for silence in [
            Silence::ProfileSilences,
            Silence::SystemNotAccepting,
            Silence::SomethingIsPlaying,
            Silence::NetworkBusy,
            Silence::UserIsWorking,
            Silence::TooSoonAfterReturn,
            Silence::DailyLimitReached,
            Silence::NothingToSay,
        ] {
            assert!(!silence.describe().is_empty());
        }
    }
}
