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
    GetWindowLongPtrW, PostMessageW, SendMessageW, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    GWL_EXSTYLE, HTCAPTION, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_MINIMIZE, WM_LBUTTONUP, WM_NCLBUTTONDOWN, WM_SETICON, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
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
    start_system_move(hwnd, HTCAPTION as usize)
}

/// Просит Windows начать перетаскивание или растягивание окна.
///
/// Отправляем именно `PostMessage`, а не `SendMessage`, и это важно.
/// `SendMessage` не вернётся, пока Windows не закончит свой модальный цикл
/// перетаскивания, — а вызывают нас из обработчика нажатия в интерфейсе.
/// Интерфейс на всё это время замирает, а главное, он так и не узнаёт, что
/// кнопку мыши отпустили: событие «отпустили» съедает модальный цикл. Окно
/// остаётся с намертво «нажатой» кнопкой и перестаёт отзываться на щелчки.
///
/// Поэтому сначала отдаём интерфейсу событие отпускания, чтобы он закрыл
/// нажатие честно, и только потом ставим сообщение в очередь и немедленно
/// возвращаемся.
fn start_system_move(hwnd: isize, hit_code: usize) -> Result<()> {
    if hwnd == 0 {
        return Err(Error::Unsupported("окно ещё не создано"));
    }
    let hwnd = hwnd as HWND;
    unsafe {
        ReleaseCapture();
        // Закрываем нажатие в самом приложении: иначе оно останется
        // «зажатым» и следующие щелчки будут проигнорированы.
        PostMessageW(hwnd, WM_LBUTTONUP, 0, 0);
        PostMessageW(hwnd, WM_NCLBUTTONDOWN, hit_code, 0);
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
    start_system_move(hwnd, edge.hit_code())
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

/// Ставит окну иконку из точек RGBA.
///
/// Без неё Windows рисует в панели задач пустой квадрат: иконка окна
/// берётся не из трея, а из самого окна, и по умолчанию её просто нет.
///
/// Картинку принимаем той же, что рисует агент для трея, — чтобы логотип
/// был один и правился в одном месте.
pub fn set_icon(hwnd: isize, rgba: &[u8], size: u32) -> Result<()> {
    use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, DestroyIcon, ICONINFO, ICON_BIG, ICON_SMALL,
    };

    if hwnd == 0 {
        return Err(Error::Unsupported("окно ещё не создано"));
    }
    if rgba.len() != (size * size * 4) as usize {
        return Err(Error::Malformed("размер картинки не совпал с указанным"));
    }

    // Windows ждёт BGRA, а рисуем мы в RGBA: меняем местами красный
    // и синий. Заодно это единственное преобразование, которое тут нужно —
    // альфа-канал GDI понимает как есть.
    let mut bgra = Vec::with_capacity(rgba.len());
    for point in rgba.chunks_exact(4) {
        bgra.extend_from_slice(&[point[2], point[1], point[0], point[3]]);
    }

    // Цветная картинка и маска. Маска нужна даже при наличии альфы:
    // структура иконки требует оба изображения.
    let colour = unsafe { CreateBitmap(size as i32, size as i32, 1, 32, bgra.as_ptr().cast()) };
    if colour.is_null() {
        return Err(Error::Win32 {
            call: "CreateBitmap(цвет)",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }

    let mask_bytes = vec![0u8; (size * size / 8).max(1) as usize];
    let mask = unsafe { CreateBitmap(size as i32, size as i32, 1, 1, mask_bytes.as_ptr().cast()) };
    if mask.is_null() {
        unsafe { DeleteObject(colour.cast()) };
        return Err(Error::Win32 {
            call: "CreateBitmap(маска)",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }

    let info = ICONINFO {
        fIcon: 1,
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: colour,
    };
    let icon = unsafe { CreateIconIndirect(&info) };

    // Битмапы своё дело сделали: иконка держит собственные копии.
    unsafe {
        DeleteObject(colour.cast());
        DeleteObject(mask.cast());
    }

    if icon.is_null() {
        return Err(Error::Win32 {
            call: "CreateIconIndirect",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
        });
    }

    // Большая иконка идёт в панель задач и Alt+Tab, малая — в заголовок.
    unsafe {
        SendMessageW(hwnd as HWND, WM_SETICON, ICON_BIG as usize, icon as isize);
        SendMessageW(hwnd as HWND, WM_SETICON, ICON_SMALL as usize, icon as isize);
    }

    // Иконку не уничтожаем: окно держит её, пока живёт. Уничтожили бы —
    // в панели задач снова оказался бы пустой квадрат.
    let _ = DestroyIcon;
    Ok(())
}

#[cfg(test)]
mod icon_tests {
    use super::*;

    #[test]
    fn a_null_window_is_refused() {
        let rgba = vec![0u8; 32 * 32 * 4];
        assert!(set_icon(0, &rgba, 32).is_err());
    }

    #[test]
    fn a_mismatched_buffer_is_refused() {
        // Буфер меньше заявленного размера: молча обрезать нельзя,
        // GDI прочитал бы чужую память.
        let rgba = vec![0u8; 16 * 16 * 4];
        assert!(set_icon(1, &rgba, 32).is_err());
    }
}
