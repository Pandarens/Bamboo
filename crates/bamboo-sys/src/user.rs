//! Состояние пользователя: простой и режим уведомлений.
//!
//! Нужно и для частоты опроса, и для анализаторов: «процесс грузит ядро»
//! означает совсем разное, когда человек за компьютером и когда он ушёл.

use bamboo_core::{Error, Result};
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows_sys::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_ACCEPTS_NOTIFICATIONS, QUNS_APP, QUNS_BUSY,
    QUNS_NOT_PRESENT, QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME, QUNS_RUNNING_D3D_FULL_SCREEN,
};

/// Сколько времени прошло с последнего ввода, в миллисекундах.
///
/// `GetLastInputInfo` отдаёт 32-битное значение того же счётчика, что
/// и `GetTickCount`, поэтому раз в 49 суток оно переполняется. Вычитание
/// с переносом даёт правильный результат и на переполнении.
pub fn idle_ms() -> Result<u64> {
    let mut info = LASTINPUTINFO {
        cbSize: core::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };

    let ok = unsafe { GetLastInputInfo(&mut info) };
    if ok == 0 {
        return Err(Error::Win32 {
            call: "GetLastInputInfo",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }

    let now = unsafe { GetTickCount() };
    Ok(now.wrapping_sub(info.dwTime) as u64)
}

/// Готовность системы принимать уведомления.
///
/// Именно по этому состоянию решается, можно ли показать тост и нужно ли
/// прятать виджет: оверлей поверх эксклюзивного полноэкранного режима
/// ломает игру.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationState {
    /// Уведомления можно показывать.
    Accepts,
    /// Пользователя нет — экран заблокирован или сессия отключена.
    NotPresent,
    /// Полноэкранное приложение, обычно игра.
    FullScreenD3d,
    /// Режим презентации.
    Presentation,
    /// Пользователь занят, тосты подавлены системой.
    Busy,
    /// Тихие часы после установки Windows.
    QuietTime,
    /// Полноэкранное приложение, не использующее D3D.
    FullScreenApp,
    /// Состояние получить не удалось.
    Unknown,
}

impl NotificationState {
    /// Можно ли сейчас беспокоить пользователя.
    ///
    /// По ТЗ уведомление — дорогая валюта, поэтому любое состояние,
    /// кроме явного согласия, означает молчание.
    pub fn may_notify(self) -> bool {
        self == NotificationState::Accepts
    }

    /// Нужно ли прятать виджет.
    pub fn should_hide_widget(self) -> bool {
        matches!(
            self,
            NotificationState::FullScreenD3d
                | NotificationState::Presentation
                | NotificationState::Busy
                | NotificationState::FullScreenApp
        )
    }

    /// Идёт ли полноэкранное приложение — основание переключиться
    /// в профиль «Игра».
    pub fn is_fullscreen(self) -> bool {
        matches!(
            self,
            NotificationState::FullScreenD3d | NotificationState::FullScreenApp
        )
    }
}

pub fn notification_state() -> NotificationState {
    let mut state = 0i32;
    let hr = unsafe { SHQueryUserNotificationState(&mut state) };
    if hr < 0 {
        // В сессии 0 вызов не работает — это ожидаемо для службы,
        // состояние в таком случае спрашивает агент.
        return NotificationState::Unknown;
    }

    match state {
        x if x == QUNS_ACCEPTS_NOTIFICATIONS => NotificationState::Accepts,
        x if x == QUNS_NOT_PRESENT => NotificationState::NotPresent,
        x if x == QUNS_RUNNING_D3D_FULL_SCREEN => NotificationState::FullScreenD3d,
        x if x == QUNS_PRESENTATION_MODE => NotificationState::Presentation,
        x if x == QUNS_BUSY => NotificationState::Busy,
        x if x == QUNS_QUIET_TIME => NotificationState::QuietTime,
        x if x == QUNS_APP => NotificationState::FullScreenApp,
        _ => NotificationState::Unknown,
    }
}

/// Сколько времени Windows отводит на двойной щелчок.
///
/// Настройка человека, а не наша: темп двойного щелчка выставляется
/// в параметрах мыши, и подменять его своим числом значит мешать тому,
/// кто его настроил.
pub fn double_click_time_ms() -> Option<u32> {
    // Функция не возвращает ошибок и не может не сработать: значение
    // всегда есть, даже если человек его не трогал.
    let ms = unsafe { GetDoubleClickTime() };
    (ms > 0).then_some(ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_time_is_readable() {
        // Верхняя граница — 49 суток, потолок 32-битного счётчика.
        // Значение выше означает, что переполнение обработано неверно.
        let idle = idle_ms().unwrap();
        assert!(idle < 49 * 24 * 3600 * 1000);
    }

    #[test]
    fn notification_state_is_recognised() {
        // На машине сборки может быть что угодно, важно лишь,
        // что состояние распознано, а не свалилось в Unknown из-за
        // перепутанных констант.
        let state = notification_state();
        assert_ne!(state, NotificationState::Unknown);
    }

    #[test]
    fn only_explicit_consent_allows_notifications() {
        assert!(NotificationState::Accepts.may_notify());
        for state in [
            NotificationState::Busy,
            NotificationState::QuietTime,
            NotificationState::NotPresent,
            NotificationState::Unknown,
            NotificationState::FullScreenD3d,
        ] {
            assert!(!state.may_notify(), "{state:?} не должно разрешать тосты");
        }
    }

    #[test]
    fn widget_hides_over_fullscreen() {
        assert!(NotificationState::FullScreenD3d.should_hide_widget());
        assert!(NotificationState::Presentation.should_hide_widget());
        assert!(!NotificationState::Accepts.should_hide_widget());
    }
}
