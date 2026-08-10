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
use windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SendMessageW, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE,
    HTCAPTION, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_MINIMIZE,
    WM_NCLBUTTONDOWN, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
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

/// Сворачивает окно в панель задач.
///
/// Нужно окну без системной рамки: кнопку сворачивания рисуем сами, и она
/// должна делать ровно то же, что делала системная.
pub fn minimize(hwnd: isize) -> Result<()> {
    if hwnd == 0 {
        return Err(Error::Unsupported("окно ещё не создано"));
    }
    unsafe { ShowWindow(hwnd as HWND, SW_MINIMIZE) };
    Ok(())
}

/// Начинает перетаскивание окна за шапку.
///
/// Тащить окно, пересчитывая координаты на каждое движение мыши, — плохая
/// идея: при быстром движении курсор обгоняет окно и «срывается» с него.
/// Вместо этого отпускаем захват мыши и говорим Windows, что нажатие
/// пришлось на заголовок, — дальше окно тащит сама система, ровно так же,
/// как обычное окно с рамкой.
pub fn begin_drag(hwnd: isize) -> Result<()> {
    if hwnd == 0 {
        return Err(Error::Unsupported("окно ещё не создано"));
    }
    unsafe {
        ReleaseCapture();
        SendMessageW(hwnd as HWND, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
    }
    Ok(())
}

/// За какой край тянут окно.
///
/// Значения совпадают с кодами зон окна из Windows (`HTLEFT` и соседние):
/// мы их прямо и передаём системе, поэтому переводить ничего не нужно.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeEdge {
    Left,
    Right,
    Top,
    TopLeft,
    TopRight,
    Bottom,
    BottomLeft,
    BottomRight,
}

impl ResizeEdge {
    /// Код края по номеру из интерфейса.
    pub fn from_index(index: i32) -> Option<ResizeEdge> {
        match index {
            0 => Some(ResizeEdge::Left),
            1 => Some(ResizeEdge::Right),
            2 => Some(ResizeEdge::Top),
            3 => Some(ResizeEdge::TopLeft),
            4 => Some(ResizeEdge::TopRight),
            5 => Some(ResizeEdge::Bottom),
            6 => Some(ResizeEdge::BottomLeft),
            7 => Some(ResizeEdge::BottomRight),
            _ => None,
        }
    }

    fn hit_code(self) -> usize {
        // Значения из WinUser.h: HTLEFT = 10 и далее по порядку.
        match self {
            ResizeEdge::Left => 10,
            ResizeEdge::Right => 11,
            ResizeEdge::Top => 12,
            ResizeEdge::TopLeft => 13,
            ResizeEdge::TopRight => 14,
            ResizeEdge::Bottom => 15,
            ResizeEdge::BottomLeft => 16,
            ResizeEdge::BottomRight => 17,
        }
    }
}

/// Начинает изменение размера окна за указанный край.
///
/// У окна без системной рамки нет и системных границ, за которые его тянут.
/// Возвращаем их тем же приёмом, что и перетаскивание за шапку: говорим
/// Windows, что нажатие пришлось на край окна, и дальше размер меняет сама
/// система — с правильным курсором, привязкой к краям экрана и всем прочим.
pub fn begin_resize(hwnd: isize, edge: ResizeEdge) -> Result<()> {
    if hwnd == 0 {
        return Err(Error::Unsupported("окно ещё не создано"));
    }
    unsafe {
        ReleaseCapture();
        SendMessageW(hwnd as HWND, WM_NCLBUTTONDOWN, edge.hit_code(), 0);
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

#[cfg(test)]
mod resize_tests {
    use super::*;

    #[test]
    fn edge_codes_match_windows_hit_test_zones() {
        // Коды должны совпадать с WinUser.h: HTLEFT = 10 и далее подряд.
        assert_eq!(ResizeEdge::Left.hit_code(), 10);
        assert_eq!(ResizeEdge::Right.hit_code(), 11);
        assert_eq!(ResizeEdge::Top.hit_code(), 12);
        assert_eq!(ResizeEdge::BottomRight.hit_code(), 17);
    }

    #[test]
    fn every_index_maps_to_a_distinct_edge() {
        let mut codes: Vec<usize> = (0..8)
            .map(|index| {
                ResizeEdge::from_index(index)
                    .expect("край должен разбираться")
                    .hit_code()
            })
            .collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 8, "два края получили один код");
    }

    #[test]
    fn an_unknown_index_is_rejected() {
        assert_eq!(ResizeEdge::from_index(99), None);
    }

    #[test]
    fn resizing_a_missing_window_is_refused() {
        assert!(begin_resize(0, ResizeEdge::Right).is_err());
    }
}
