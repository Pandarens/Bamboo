//! Иконка в трее.

#![forbid(unsafe_code)]

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Что пользователь сделал с иконкой.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    ToggleWidget,
    OpenWindow,
    /// Переключить запуск вместе с Windows.
    ToggleAutostart,
    Quit,
}

pub struct Tray {
    /// Дескриптор надо держать: при его удалении иконка исчезает из трея.
    _icon: TrayIcon,
    show_id: MenuId,
    window_id: MenuId,
    autostart_id: MenuId,
    quit_id: MenuId,
    /// Пункт с галочкой: его состояние надо обновлять после переключения.
    autostart: CheckMenuItem,
    /// Одиночный щелчок, реакция на который отложена.
    ///
    /// Двойной щелчок Windows присылает как пару одиночных плюс отдельное
    /// событие. Если реагировать на первый же щелчок, виджет успеет мигнуть
    /// прежде, чем откроется окно. Поэтому одиночный щелчок ждёт: не придёт
    /// ли следом второй.
    pending_click: std::cell::Cell<Option<std::time::Instant>>,
}

impl Tray {
    pub fn new() -> Result<Tray, Box<dyn std::error::Error>> {
        let menu = Menu::new();
        let show = MenuItem::new("Показать виджет", true, None);
        let window = MenuItem::new("Открыть окно", true, None);
        // Галочка показывает нынешнее состояние: читаем его из реестра,
        // а не помним у себя — пользователь мог убрать запись руками.
        let autostart = CheckMenuItem::new(
            "Запускать вместе с Windows",
            true,
            bamboo_sys::is_in_startup(),
            None,
        );
        let quit = MenuItem::new("Выход", true, None);
        menu.append(&show)?;
        menu.append(&window)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&autostart)?;
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
            autostart_id: autostart.id().clone(),
            quit_id: quit.id().clone(),
            autostart,
            pending_click: std::cell::Cell::new(None),
        })
    }

    /// Переключает запуск вместе с Windows и возвращает объяснение.
    ///
    /// Состояние берём из реестра, а не из галочки: пользователь мог
    /// убрать запись руками через диспетчер задач, и тогда галочка врёт.
    pub fn toggle_autostart(&self) -> String {
        let note = if bamboo_sys::is_in_startup() {
            match bamboo_sys::remove_from_startup() {
                Ok(()) => "Bamboo больше не будет запускаться вместе с Windows.",
                Err(_) => "Убрать из автозапуска не удалось.",
            }
        } else {
            match bamboo_sys::add_to_startup() {
                Ok(()) => "Bamboo будет запускаться вместе с Windows.",
                Err(_) => "Добавить в автозапуск не удалось.",
            }
        };

        // Галочку выставляем по факту, а не по намерению.
        self.autostart.set_checked(bamboo_sys::is_in_startup());
        note.to_string()
    }

    /// Стоит ли Bamboo в автозапуске.
    pub fn autostart_enabled(&self) -> bool {
        bamboo_sys::is_in_startup()
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
            } else if event.id == self.autostart_id {
                actions.push(TrayAction::ToggleAutostart);
            } else if event.id == self.quit_id {
                actions.push(TrayAction::Quit);
            }
        }

        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            match event {
                // Двойной щелчок открывает окно. Проверяем его первым:
                // одиночный, который уже ждал своей очереди, отменяется.
                TrayIconEvent::DoubleClick {
                    button: tray_icon::MouseButton::Left,
                    ..
                } => {
                    self.pending_click.set(None);
                    actions.push(TrayAction::OpenWindow);
                }
                // Реагируем только на отпускание левой кнопки: нажатие
                // приходит отдельным событием, и на паре «нажал-отпустил»
                // виджет мигнул бы.
                // Не отменяем уже открытое окно вторым щелчком пары: после
                // двойного щелчка Windows пришлёт ещё одно отпускание, и оно
                // не должно ничего значить.
                TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Left,
                    button_state: tray_icon::MouseButtonState::Up,
                    ..
                } if !actions.contains(&TrayAction::OpenWindow) => {
                    self.pending_click.set(Some(std::time::Instant::now()));
                }
                _ => {}
            }
        }

        // Одиночный щелчок дождался своего: второго не последовало.
        if let Some(at) = self.pending_click.get() {
            if at.elapsed() >= double_click_time() {
                self.pending_click.set(None);
                actions.push(TrayAction::ToggleWidget);
            }
        }

        actions
    }
}

/// Сколько ждать второго щелчка.
///
/// Берём системную настройку, а не своё число: человек мог выставить свой
/// темп в параметрах мыши, и наше «полсекунды» ему бы мешало.
fn double_click_time() -> std::time::Duration {
    // Значение по умолчанию в Windows — 500 мс. Оно же годится, если
    // спросить систему нечем.
    let ms = bamboo_sys::double_click_time_ms().unwrap_or(500);
    std::time::Duration::from_millis(u64::from(ms))
}

/// Рисует иконку кодом: морда панды и стебель бамбука сбоку.
///
/// Рисуем, а не подключаем файл: ради одной картинки 32x32 тащить ресурс
/// и библиотеку разбора изображений незачем, а так иконка живёт вместе
/// с кодом и правится в одном месте.
///
/// Всё построено на кругах: голова, уши, пятна вокруг глаз, глаза, нос.
/// Отсюда и вспомогательная `disc` — она отвечает на единственный вопрос,
/// который тут вообще возникает: попала ли точка в круг.
fn bamboo_icon() -> Icon {
    Icon::from_rgba(logo_rgba(), LOGO_SIZE, LOGO_SIZE).expect("иконка собрана неверно")
}

/// Сторона логотипа в точках.
pub const LOGO_SIZE: u32 = 32;

/// Рисует логотип в буфер RGBA.
///
/// Отдельно от иконки трея, потому что тот же логотип нужен окнам:
/// без иконки Windows рисует в панели задач пустой квадрат.
pub fn logo_rgba() -> Vec<u8> {
    // Координаты деталей подобраны под этот размер: панда на 32 точках —
    // это предел, где ещё различимы глаза и нос.
    const SIZE: u32 = LOGO_SIZE;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];

    // Цвета: панда чёрно-белая, бамбук в тон интерфейсу.
    const WHITE: (u8, u8, u8) = (240, 244, 241);
    const BLACK: (u8, u8, u8) = (26, 32, 29);
    const BAMBOO: (u8, u8, u8) = (63, 143, 102);
    const JOINT: (u8, u8, u8) = (36, 84, 58);

    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut colour: Option<(u8, u8, u8)> = None;

            // Стебель бамбука по правому краю, с перемычками.
            if (25..29).contains(&x) && (3..30).contains(&y) {
                let on_joint = [8u32, 16, 24].iter().any(|joint| y.abs_diff(*joint) < 2);
                colour = Some(if on_joint { JOINT } else { BAMBOO });
            }

            // Уши — раньше головы: голова накроет их края и получится
            // привычный силуэт.
            if disc(x, y, 5, 8, 4) || disc(x, y, 18, 8, 4) {
                colour = Some(BLACK);
            }

            // Голова.
            if disc(x, y, 11, 17, 9) {
                colour = Some(WHITE);
            }

            // Пятна вокруг глаз — то, по чему панду и узнают.
            if disc(x, y, 7, 15, 3) || disc(x, y, 15, 15, 3) {
                colour = Some(BLACK);
            }
            // Зрачки и нос рисуем прямоугольниками, а не кругами: на такой
            // мелочи круг радиусом в пиксель вырождается в ромб, и морда
            // выходит колючей вместо круглой.
            if box_(x, y, 6, 14, 2, 2) || box_(x, y, 14, 14, 2, 2) {
                colour = Some(WHITE);
            }
            if box_(x, y, 10, 20, 3, 2) {
                colour = Some(BLACK);
            }

            if let Some((r, g, b)) = colour {
                let index = ((y * SIZE + x) * 4) as usize;
                rgba[index] = r;
                rgba[index + 1] = g;
                rgba[index + 2] = b;
                rgba[index + 3] = 255;
            }
        }
    }

    rgba
}

/// Попадает ли точка в круг с центром `(cx, cy)` и радиусом `r`.
///
/// Считаем в целых числах и без корня: сравниваем квадраты расстояний.
fn disc(x: u32, y: u32, cx: i32, cy: i32, r: i32) -> bool {
    let dx = x as i32 - cx;
    let dy = y as i32 - cy;
    dx * dx + dy * dy <= r * r
}

/// Попадает ли точка в прямоугольник с левым верхним углом `(left, top)`.
fn box_(x: u32, y: u32, left: u32, top: u32, width: u32, height: u32) -> bool {
    (left..left + width).contains(&x) && (top..top + height).contains(&y)
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
