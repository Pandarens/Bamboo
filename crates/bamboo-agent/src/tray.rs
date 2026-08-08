//! Иконка в трее.

#![forbid(unsafe_code)]

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Что пользователь сделал с иконкой.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    ToggleWidget,
    OpenWindow,
    Quit,
}

pub struct Tray {
    /// Дескриптор надо держать: при его удалении иконка исчезает из трея.
    _icon: TrayIcon,
    show_id: MenuId,
    window_id: MenuId,
    quit_id: MenuId,
}

impl Tray {
    pub fn new() -> Result<Tray, Box<dyn std::error::Error>> {
        let menu = Menu::new();
        let show = MenuItem::new("Показать виджет", true, None);
        let window = MenuItem::new("Открыть окно", true, None);
        let quit = MenuItem::new("Выход", true, None);
        menu.append(&show)?;
        menu.append(&window)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit)?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Bamboo — наблюдает за системой")
            .with_icon(bamboo_icon())
            .build()?;

        Ok(Tray {
            _icon: icon,
            show_id: show.id().clone(),
            window_id: window.id().clone(),
            quit_id: quit.id().clone(),
        })
    }

    /// Забирает накопившиеся события. Вызывается из таймера интерфейса:
    /// отдельный поток здесь не нужен и стоил бы пробуждений.
    pub fn poll(&self) -> Vec<TrayAction> {
        let mut actions = Vec::new();

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.show_id {
                actions.push(TrayAction::ToggleWidget);
            } else if event.id == self.window_id {
                actions.push(TrayAction::OpenWindow);
            } else if event.id == self.quit_id {
                actions.push(TrayAction::Quit);
            }
        }

        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            // Реагируем только на отпускание левой кнопки: нажатие приходит
            // отдельным событием, и на паре «нажал-отпустил» виджет мигнул бы.
            if let TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            } = event
            {
                actions.push(TrayAction::ToggleWidget);
            }
        }

        actions
    }
}

/// Рисует иконку кодом.
///
/// Стебель бамбука с перемычками: узнаваемо и не тянет за собой ни файл
/// ресурса, ни библиотеку разбора изображений ради одной картинки 32x32.
fn bamboo_icon() -> Icon {
    const SIZE: u32 = 32;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];

    let stalk = (10u32, 22u32);
    let joints = [8u32, 16, 24];

    for y in 0..SIZE {
        for x in 0..SIZE {
            let index = ((y * SIZE + x) * 4) as usize;

            let inside = x >= stalk.0 && x < stalk.1 && (2..30).contains(&y);
            if !inside {
                continue;
            }

            let on_joint = joints.iter().any(|joint| y.abs_diff(*joint) < 2);
            let (r, g, b) = if on_joint {
                (36, 84, 58)
            } else {
                (63, 143, 102)
            };

            rgba[index] = r;
            rgba[index + 1] = g;
            rgba[index + 2] = b;
            rgba[index + 3] = 255;
        }
    }

    Icon::from_rgba(rgba, SIZE, SIZE).expect("иконка собрана неверно")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_is_built_without_a_resource_file() {
        // from_rgba паникует на неверном размере буфера — этот вызов
        // и есть проверка.
        let _ = bamboo_icon();
    }
}
