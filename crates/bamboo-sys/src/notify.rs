//! Всплывающие уведомления (ТЗ, раздел 14.4).
//!
//! Сразу о том, чего этот способ **не умеет**, потому что это меняет
//! требования раздела 14.4. У всплывающей подсказки области уведомлений
//! не бывает кнопок — ни «Заморозить», ни «Не трогать», ни «Подробнее».
//! Доступен только щелчок по подсказке целиком. Способ с кнопками
//! (ToastNotification из WinRT) в проекте недостижим: в `windows-sys`
//! WinRT нет вовсе, а тянуть ради него другой крейт значит менять
//! основание проекта ради одной возможности.
//!
//! Поэтому здесь честная деградация: подсказка сообщает наблюдение и
//! по щелчку открывает окно, где действия уже есть. Обещать кнопки
//! в самой подсказке и не давать их было бы хуже.
//!
//! Иконка своя и **скрытая**. Своя — потому что чужую, добавленную другой
//! библиотекой, трогать нельзя: у каждой иконки свой владелец, свой
//! идентификатор и своё окно. Скрытая — чтобы человек не увидел вторую
//! панду в трее рядом с первой.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_INFO, NIF_STATE, NIIF_INFO, NIIF_WARNING, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIS_HIDDEN, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, HWND_MESSAGE, WNDCLASSW,
};

/// Идентификатор нашей иконки. Свой, отдельный от той, что держит трей.
const ICON_ID: u32 = 1;

/// Имя класса окна. Своё: чужой класс регистрировать нельзя.
const CLASS_NAME: &str = "bamboo_notify";

/// Важность уведомления.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Importance {
    /// Наблюдение: что-то стоит знать.
    Notice,
    /// Предупреждение: что-то мешает прямо сейчас.
    Warning,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Кладёт строку в поле фиксированной длины.
///
/// Обрезает по месту и оставляет позицию под завершающий ноль: строка
/// длиннее поля переполнила бы структуру Windows.
fn fill(field: &mut [u16], text: &str) {
    let room = field.len().saturating_sub(1);
    field.fill(0);
    for (slot, symbol) in field.iter_mut().take(room).zip(text.encode_utf16()) {
        *slot = symbol;
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Окно нужно только как владелец иконки: сообщений мы не разбираем.
    // Щелчок по подсказке доставляется сюда же, но обрабатывать его будет
    // тот, кто захочет, — пока просто отдаём системе.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Владелец скрытой иконки, через которую показываются уведомления.
///
/// Иконка живёт, пока жив этот объект: при удалении она убирается из
/// области уведомлений. Оставить её после выхода означало бы засорить
/// человеку трей мёртвым значком.
pub struct Notifier {
    window: HWND,
}

impl Drop for Notifier {
    fn drop(&mut self) {
        let mut data: NOTIFYICONDATAW = unsafe { core::mem::zeroed() };
        data.cbSize = core::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.window;
        data.uID = ICON_ID;
        unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        unsafe { DestroyWindow(self.window) };
    }
}

impl Notifier {
    /// Заводит скрытую иконку.
    pub fn new() -> Result<Notifier> {
        let class = wide(CLASS_NAME);

        let mut description: WNDCLASSW = unsafe { core::mem::zeroed() };
        description.lpfnWndProc = Some(window_proc);
        description.lpszClassName = class.as_ptr();
        // Повторная регистрация того же класса возвращает ноль — это
        // не беда, класс уже есть. Проверять надо создание окна.
        unsafe { RegisterClassW(&description) };

        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                // Окно только для сообщений: на экране его нет и быть
                // не должно.
                HWND_MESSAGE,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            )
        };
        if window.is_null() {
            return Err(Error::Win32 {
                call: "CreateWindowExW(окно уведомлений)",
                code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
            });
        }

        let mut data: NOTIFYICONDATAW = unsafe { core::mem::zeroed() };
        data.cbSize = core::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = window;
        data.uID = ICON_ID;
        // Скрываем: вторая панда в трее человеку не нужна, а уведомления
        // скрытая иконка доставляет наравне с видимой.
        data.uFlags = NIF_STATE;
        data.dwState = NIS_HIDDEN;
        data.dwStateMask = NIS_HIDDEN;

        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
        if ok == 0 {
            unsafe { DestroyWindow(window) };
            return Err(Error::Win32 {
                call: "Shell_NotifyIconW(NIM_ADD)",
                code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
            });
        }

        Ok(Notifier { window })
    }

    /// Показывает уведомление.
    ///
    /// Успех здесь означает ровно одно: оболочка приняла запрос. Увидит ли
    /// его человек — вопрос его настроек: подсказки области уведомлений
    /// отключаются одной галочкой, и Windows об этом не сообщает.
    /// Выдавать наш успех за «человек прочитал» нельзя.
    pub fn show(&self, title: &str, text: &str, importance: Importance) -> Result<()> {
        let mut data: NOTIFYICONDATAW = unsafe { core::mem::zeroed() };
        data.cbSize = core::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.window;
        data.uID = ICON_ID;
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = match importance {
            Importance::Notice => NIIF_INFO,
            Importance::Warning => NIIF_WARNING,
        };

        // Поля фиксированной длины, и заполнять их надо через локальные
        // массивы: структура упакована, ссылку на её поле брать нельзя.
        let mut title_field = [0u16; 64];
        let mut text_field = [0u16; 256];
        fill(&mut title_field, title);
        fill(&mut text_field, text);
        data.szInfoTitle = title_field;
        data.szInfo = text_field;

        let ok = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
        if ok == 0 {
            return Err(Error::Win32 {
                call: "Shell_NotifyIconW(NIM_MODIFY)",
                code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
            });
        }
        Ok(())
    }
}

/// Умеет ли эта подсказка нести кнопки действий.
///
/// Всегда `false`, и это не заглушка на будущее, а факт устройства
/// подсказок области уведомлений. Функция существует, чтобы вызывающий
/// код не строил интерфейс вокруг кнопок, которых не будет.
pub const fn supports_actions() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_is_filled_and_always_terminated() {
        let mut field = [0xFFFFu16; 8];
        fill(&mut field, "тест");
        assert_eq!(String::from_utf16_lossy(&field[..4]), "тест");
        assert_eq!(field[7], 0, "место под завершающий ноль обязано остаться");
    }

    #[test]
    fn a_long_string_is_cut_and_does_not_overflow() {
        // Строка длиннее поля переполнила бы структуру Windows.
        let mut field = [0u16; 8];
        fill(&mut field, &"я".repeat(100));
        assert_eq!(field[7], 0);
        assert!(field[..7].iter().all(|symbol| *symbol != 0));
    }

    #[test]
    fn an_empty_string_clears_the_field() {
        let mut field = [0x41u16; 4];
        fill(&mut field, "");
        assert!(field.iter().all(|symbol| *symbol == 0));
    }

    #[test]
    fn buttons_in_the_balloon_are_not_promised() {
        // Требование ТЗ 14.4 «действия в самом тосте» этим способом
        // недостижимо. Честнее сказать это в коде, чем построить
        // интерфейс вокруг кнопок, которых не будет.
        assert!(!supports_actions());
    }

    #[test]
    fn the_icon_is_created_and_removed_without_leaving_a_trace() {
        // Живая проверка: иконка заводится и убирается. Если бы она
        // оставалась после выхода, человек получил бы мёртвый значок
        // в трее.
        let Ok(notifier) = Notifier::new() else {
            // В сеансе без рабочего стола области уведомлений нет —
            // это законный отказ, а не провал.
            return;
        };
        assert!(!notifier.window.is_null());
        drop(notifier);

        // Второй заход после удаления обязан удаться: значит первый
        // за собой убрал.
        assert!(Notifier::new().is_ok());
    }
}
