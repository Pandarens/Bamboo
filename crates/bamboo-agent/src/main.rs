//! Агент Bamboo: иконка в трее и виджет.
//!
//! Работает в пользовательской сессии без прав администратора. Всё, что
//! требует привилегий, будет делать брокер — его пока нет, поэтому агент
//! только наблюдает.
//!
//! Про `unsafe`. На корне крейта `forbid(unsafe_code)` поставить нельзя:
//! `include_modules!` вставляет сюда сгенерированный Slint код, а он таблицы
//! виртуальных методов строит через `unsafe`. Поэтому запрет стоит на уровне
//! модулей — ровно так, как и написано в ТЗ, раздел 3.4. Собственного
//! `unsafe` в агенте нет ни строки: оконные стили применяет `bamboo-sys`.

// Консольное окно рядом с виджетом не нужно. В отладочной сборке оставляем:
// без него не видно паник и сообщений об ошибках.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(unsafe_code)]

#[cfg(windows)]
mod collector;
#[cfg(windows)]
mod tray;

#[cfg(not(windows))]
fn main() {
    eprintln!("Bamboo работает только на Windows.");
    std::process::exit(2);
}

#[cfg(windows)]
slint::include_modules!();

#[cfg(windows)]
use std::sync::atomic::Ordering;
#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Программный рендерер: GPU-контекст не создаётся вообще (ТЗ, раздел 4.1).
    // Виджету он не нужен, а его создание стоит памяти и держит загруженным
    // драйвер видеокарты.
    std::env::set_var("SLINT_BACKEND", "winit-software");

    // Утилита, которая учит систему экономить, начинает с себя.
    let _ = bamboo_sys::apply_self_limits();

    let (updates, visible) = collector::spawn();
    let widget = Widget::new()?;

    apply_window_look(&widget);

    // Закрепление поверх остальных окон — единственное действие,
    // доступное сейчас из интерфейса.
    {
        let weak = widget.as_weak();
        widget.on_toggle_pin(move || {
            if let Some(widget) = weak.upgrade() {
                let pinned = !widget.get_pinned();
                widget.set_pinned(pinned);
                let _ = bamboo_sys::window::set_topmost(window_handle(&widget), pinned);
            }
        });
    }

    let tray = match tray::Tray::new() {
        Ok(tray) => Some(tray),
        Err(error) => {
            // Без трея виджет всё равно работает — просто закрывается насовсем.
            eprintln!("иконка в трее недоступна: {error}");
            None
        }
    };

    let processes: ModelRc<ProcessRow> = ModelRc::new(VecModel::from(Vec::<ProcessRow>::new()));
    let spark: ModelRc<f32> = ModelRc::new(VecModel::from(Vec::<f32>::new()));
    widget.set_processes(processes.clone());
    widget.set_spark(spark.clone());

    let weak = widget.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        // Опрашиваем канал чаще, чем приходят данные: так виджет реагирует
        // на действия сразу, а пустая проверка канала ничего не стоит.
        Duration::from_millis(200),
        move || {
            let Some(widget) = weak.upgrade() else {
                return;
            };

            if let Some(tray) = &tray {
                for action in tray.poll() {
                    match action {
                        tray::TrayAction::ToggleWidget => toggle(&widget, &visible),
                        tray::TrayAction::Quit => {
                            let _ = slint::quit_event_loop();
                            return;
                        }
                    }
                }
            }

            // Берём последний снимок, промежуточные не рисуем: пока таймер
            // спал, их могло накопиться несколько, и все, кроме свежего,
            // уже неактуальны.
            let mut latest = None;
            while let Ok(snapshot) = updates.try_recv() {
                latest = Some(snapshot);
            }

            if let Some(snapshot) = latest {
                apply_snapshot(&widget, &snapshot, &processes, &spark);
            }
        },
    );

    widget.run()?;
    Ok(())
}

/// Показывает или прячет виджет.
///
/// По ТЗ (раздел 14.2) при скрытии полагается разрушать окно, а не просто
/// прятать: скрытое живое окно продолжает удерживать ресурсы и получать
/// сообщения. Slint своё окно пересоздавать не даёт, поэтому пока
/// ограничиваемся снижением частоты опроса — это основная часть экономии.
#[cfg(windows)]
fn toggle(widget: &Widget, visible: &collector::WidgetVisible) {
    let showing = widget.window().is_visible();
    if showing {
        widget.window().hide().ok();
    } else {
        widget.window().show().ok();
    }
    visible.store(!showing, Ordering::Relaxed);
}

#[cfg(windows)]
fn apply_snapshot(
    widget: &Widget,
    snapshot: &collector::Snapshot,
    processes: &ModelRc<ProcessRow>,
    spark: &ModelRc<f32>,
) {
    widget.set_cpu_value(SharedString::from(format!(
        "{:.0}%",
        snapshot.cpu_busy * 100.0
    )));

    // Условие из раздела 9.3 ТЗ: время в DPC и прерываниях означает, что
    // виновника надо искать в драйверах, а не среди процессов.
    widget.set_cpu_note(SharedString::from(
        if snapshot.driver_ratio > 0.10 && snapshot.cpu_busy > 0.15 {
            format!(
                "{:.0}% в драйверах, не в процессах",
                snapshot.driver_ratio * 100.0
            )
        } else {
            format!("процессов: {}", snapshot.process_count)
        },
    ));

    widget.set_memory_value(SharedString::from(snapshot.memory_used.to_string()));
    widget.set_memory_note(SharedString::from(format!("из {}", snapshot.memory_total)));
    widget.set_status(SharedString::from(snapshot.cadence.clone()));
    widget.set_own_cost(SharedString::from(format!(
        "Bamboo занимает {}",
        snapshot.own_memory
    )));

    let rows: Vec<ProcessRow> = snapshot
        .top
        .iter()
        .map(|line| ProcessRow {
            name: SharedString::from(line.name.clone()),
            cpu: SharedString::from(format!("{:.1}%", line.cpu_percent)),
            memory: SharedString::from(line.memory.to_string()),
            badge: SharedString::from(line.badge.clone()),
        })
        .collect();

    replace(processes, rows);
    replace(spark, snapshot.spark.clone());
}

/// Заменяет содержимое модели целиком.
#[cfg(windows)]
fn replace<T: Clone + 'static>(model: &ModelRc<T>, values: Vec<T>) {
    let Some(vec_model) = model.as_any().downcast_ref::<VecModel<T>>() else {
        return;
    };
    while vec_model.row_count() > 0 {
        vec_model.remove(vec_model.row_count() - 1);
    }
    for value in values {
        vec_model.push(value);
    }
}

/// Дескриптор окна для передачи в `bamboo-sys`.
#[cfg(windows)]
fn window_handle(widget: &Widget) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    widget
        .window()
        .window_handle()
        .window_handle()
        .ok()
        .and_then(|handle| match handle.as_raw() {
            RawWindowHandle::Win32(win32) => Some(win32.hwnd.get()),
            _ => None,
        })
        .unwrap_or(0)
}

#[cfg(windows)]
fn apply_window_look(widget: &Widget) {
    let hwnd = window_handle(widget);
    // Ошибки не фатальны: на Windows 10 часть оформления недоступна,
    // окно просто останется обычным.
    let _ = bamboo_sys::window::apply_widget_styles(hwnd);
    let _ = bamboo_sys::window::apply_windows11_look(hwnd);
}
