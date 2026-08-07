//! Оформление окна виджета (ТЗ, раздел 14.2).
//!
//! Живёт здесь, а не в агенте, из-за инварианта проекта: весь `unsafe`
//! только в `bamboo-sys`. Агент получает дескриптор окна от Slint
//! и передаёт его сюда.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_NOTOPMOST, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

/// Применяет стили виджета к окну.
///
/// `WS_EX_TOOLWINDOW` убирает окно из панели задач и из Alt+Tab: виджет —
/// не приложение, ему там не место. `WS_EX_NOACTIVATE` не даёт красть фокус
/// при появлении — человек в этот момент печатает в другом окне.
pub fn apply_widget_styles(hwnd: isize) -> Result<()> {
    if hwnd == 0 {
        return Err(Error::Unsupported("окно ещё не создано"));
    }
    let hwnd = hwnd as HWND;

    let current = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let wanted = current | (WS_EX_TOOLWINDOW as isize) | (WS_EX_NOACTIVATE as isize);
    unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted) };

    Ok(())
}

/// Скругление углов и подложка в стиле Windows 11.
///
/// Отрисовку выполняет композитор, для приложения это бесплатно.
/// На Windows 10 вызовы просто возвращают ошибку — тогда окно остаётся
/// прямоугольным, и ничего страшного не происходит.
pub fn apply_windows11_look(hwnd: isize) -> Result<()> {
    if hwnd == 0 {
        return Err(Error::Unsupported("окно ещё не создано"));
    }
    let hwnd = hwnd as HWND;

    let corner = DWMWCP_ROUND;
    let corner_ok = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&corner as *const i32).cast(),
            size_of_i32(),
        )
    };

    let backdrop = DWMSBT_TRANSIENTWINDOW;
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE as u32,
            (&backdrop as *const i32).cast(),
            size_of_i32(),
        )
    };

    if corner_ok != 0 {
        // HRESULT не ноль — атрибут не поддерживается этой версией Windows.
        return Err(Error::Unsupported(
            "скругление углов доступно начиная с Windows 11",
        ));
    }
    Ok(())
}

/// Закрепляет окно поверх остальных или снимает закрепление.
pub fn set_topmost(hwnd: isize, topmost: bool) -> Result<()> {
    if hwnd == 0 {
        return Err(Error::Unsupported("окно ещё не создано"));
    }

    let ok = unsafe {
        SetWindowPos(
            hwnd as HWND,
            if topmost {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            },
            0,
            0,
            0,
            0,
            // Позицию и размер не трогаем, фокус не отбираем.
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    };

    if ok == 0 {
        return Err(Error::Win32 {
            call: "SetWindowPos",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }
    Ok(())
}

const fn size_of_i32() -> u32 {
    core::mem::size_of::<i32>() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_handle_is_rejected_not_dereferenced() {
        assert!(apply_widget_styles(0).is_err());
        assert!(apply_windows11_look(0).is_err());
        assert!(set_topmost(0, true).is_err());
    }
}
