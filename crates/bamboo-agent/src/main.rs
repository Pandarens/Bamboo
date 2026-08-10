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
mod actions;
#[cfg(windows)]
mod collector;
#[cfg(windows)]
mod mainwin;
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

    widget.show()?;

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
    let spark_cpu: ModelRc<f32> = ModelRc::new(VecModel::from(Vec::<f32>::new()));
    widget.set_processes(processes.clone());
    widget.set_spark(spark.clone());
    widget.set_spark_cpu(spark_cpu.clone());

    // Главное окно создаётся заранее и держится скрытым: открытие из трея
    // должно быть мгновенным. Его секции наполняются по требованию.
    let main_window = MainWindow::new()?;
    let main_processes: ModelRc<MainProcessRow> = ModelRc::new(VecModel::from(Vec::new()));
    let drives_model: ModelRc<DriveRow> = ModelRc::new(VecModel::from(Vec::new()));
    let wakes_model: ModelRc<WakeRow> = ModelRc::new(VecModel::from(Vec::new()));
    let journal_model: ModelRc<JournalRow> = ModelRc::new(VecModel::from(Vec::new()));
    // Размер задаём явно: у окна без системной рамки Slint берёт минимальный,
    // а не предпочтительный, и таблица процессов оказывается сжатой.
    main_window
        .window()
        .set_size(slint::PhysicalSize::new(1080, 640));
    // Модели дашборда: без них replace не найдёт VecModel и молча ничего
    // не сделает — список останется пустым.
    main_window.set_disk_load(ModelRc::new(VecModel::from(Vec::<DiskLoadRow>::new())));
    main_window.set_pagefiles(ModelRc::new(VecModel::from(Vec::<PagefileRow>::new())));
    main_window.set_processes(main_processes.clone());
    main_window.set_drives(drives_model.clone());
    main_window.set_wakes(wakes_model.clone());
    main_window.set_journal(journal_model.clone());

    // Последний снимок держим под рукой: по клику на заголовок столбца
    // таблицу надо пересортировать сразу, а не ждать следующего тика.
    let last_snapshot: std::rc::Rc<std::cell::RefCell<Option<collector::Snapshot>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));

    // Сортировка по столбцу. Повторный щелчок по тому же столбцу
    // разворачивает порядок — привычное поведение любой таблицы.
    {
        let weak = main_window.as_weak();
        let rows = main_processes.clone();
        let snapshot = last_snapshot.clone();
        main_window.on_sort_by(move |column| {
            let Some(win) = weak.upgrade() else {
                return;
            };
            if win.get_sort_column() == column {
                win.set_sort_descending(!win.get_sort_descending());
            } else {
                win.set_sort_column(column);
                // Имя удобнее читать по алфавиту, числа — от большего.
                win.set_sort_descending(column != 0);
            }
            if let Some(snapshot) = snapshot.borrow().as_ref() {
                fill_processes(&win, snapshot, &rows);
            }
        });
    }

    // Действия над процессом из списка.
    {
        let weak = main_window.as_weak();
        let snapshot = last_snapshot.clone();
        main_window.on_apply_action(move |pid, code| {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let Some(what) = actions::RowAction::from_index(code) else {
                return;
            };
            let Ok(pid) = pid.parse::<u32>() else {
                return;
            };

            // Имя процесса берём из снимка: политике оно нужно, чтобы узнать
            // системный процесс, а PID сам по себе ей ничего не говорит.
            let name = snapshot
                .borrow()
                .as_ref()
                .and_then(|snapshot| {
                    snapshot
                        .top
                        .iter()
                        .find(|line| line.pid == pid)
                        .map(|line| line.name.clone())
                })
                .unwrap_or_default();
            if name.is_empty() {
                win.set_action_note(SharedString::from(
                    "Процесс уже завершился — действие отменено.",
                ));
                return;
            }

            win.set_action_note(SharedString::from(actions::apply(pid, &name, what)));
        });
    }

    // Завершение процесса. Необратимо, поэтому интерфейс спрашивает дважды,
    // а сама операция идёт мимо журнала действий: откатывать тут нечего.
    {
        let weak = main_window.as_weak();
        let snapshot = last_snapshot.clone();
        let rows = main_processes.clone();
        main_window.on_terminate_process(move |pid| {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let Ok(pid) = pid.parse::<u32>() else {
                return;
            };

            let name = snapshot
                .borrow()
                .as_ref()
                .and_then(|snapshot| {
                    snapshot
                        .top
                        .iter()
                        .find(|line| line.pid == pid)
                        .map(|line| line.name.clone())
                })
                .unwrap_or_default();
            if name.is_empty() {
                win.set_action_note(SharedString::from("Процесс уже завершился."));
                return;
            }

            win.set_action_note(SharedString::from(actions::terminate(pid, &name)));

            // Список сразу перестраиваем: строки завершённого процесса
            // в таблице быть не должно, а следующего тика ждать секунду.
            if let Some(snapshot) = snapshot.borrow_mut().as_mut() {
                snapshot.top.retain(|line| line.pid != pid);
            }
            if let Some(snapshot) = snapshot.borrow().as_ref() {
                fill_processes(&win, snapshot, &rows);
            }
        });
    }

    // Своя рамка окна: сворачивание, закрытие и перетаскивание за шапку.
    {
        let weak = main_window.as_weak();
        main_window.on_minimize_window(move || {
            if let Some(win) = weak.upgrade() {
                let _ = bamboo_sys::window::minimize(main_window_handle(&win));
            }
        });
    }
    {
        let weak = main_window.as_weak();
        main_window.on_close_window(move || {
            // Закрываем только окно: агент живёт в трее и продолжает
            // наблюдать. Выход — пункт «Выход» в меню трея.
            if let Some(win) = weak.upgrade() {
                win.hide().ok();
            }
        });
    }
    {
        let weak = main_window.as_weak();
        main_window.on_drag_window(move || {
            if let Some(win) = weak.upgrade() {
                let _ = bamboo_sys::window::begin_drag(main_window_handle(&win));
            }
        });
    }
    {
        let weak = main_window.as_weak();
        main_window.on_resize_window(move |edge| {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let Some(edge) = bamboo_sys::window::ResizeEdge::from_index(edge) else {
                return;
            };
            let _ = bamboo_sys::window::begin_resize(main_window_handle(&win), edge);
        });
    }
    // Виджет тоже без рамки — его тянут за строку состояния.
    {
        let weak = widget.as_weak();
        widget.on_drag_window(move || {
            if let Some(widget) = weak.upgrade() {
                let _ = bamboo_sys::window::begin_drag(window_handle(&widget));
            }
        });
    }
    // Крестик на виджете. Прячет окно и снижает частоту опроса, но Bamboo
    // продолжает работать: вернуть виджет можно из трея, выйти — оттуда же.
    {
        let weak = widget.as_weak();
        let visible = visible.clone();
        widget.on_close_widget(move || {
            if let Some(widget) = weak.upgrade() {
                widget.window().hide().ok();
                visible.store(false, Ordering::Relaxed);
            }
        });
    }

    // Загрузка разделов по требованию: диск, питание и журнал — разовые
    // запросы, держать их в фоне незачем.
    {
        let weak = main_window.as_weak();
        main_window.on_refresh(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let section = win.get_section();

            // Раздел грузим в фоне, а не здесь. Чтение SMART идёт через IOCTL
            // к накопителю, история пробуждений — запросом к журналу событий,
            // журнал действий — обращением к базе. На живой машине любое
            // из этих чтений занимает от долей секунды до нескольких секунд,
            // и всё это время окно не разбирало бы сообщения: интерфейс
            // замирал бы, а нажатия уходили в никуда.
            let note = match section {
                2 => "Читаю накопители…",
                3 => "Читаю журнал пробуждений…",
                4 => "Открываю журнал действий…",
                _ => "",
            };
            match section {
                2 => win.set_disk_note(SharedString::from(note)),
                3 => win.set_power_note(SharedString::from(note)),
                4 => win.set_journal_note(SharedString::from(note)),
                _ => return,
            }

            // В поток уходит только слабая ссылка на окно: модели Slint
            // живут в потоке интерфейса и через границу потока не проходят.
            // Забираем их из самого окна, уже вернувшись в его поток.
            let back = weak.clone();

            std::thread::spawn(move || match section {
                2 => {
                    let (rows, note) = mainwin::drive_rows();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(win) = back.upgrade() {
                            replace(
                                &win.get_drives(),
                                rows.into_iter().map(to_drive_row).collect(),
                            );
                            win.set_disk_note(SharedString::from(note));
                        }
                    });
                }
                3 => {
                    let (rows, note) = mainwin::wake_rows();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(win) = back.upgrade() {
                            replace(
                                &win.get_wakes(),
                                rows.into_iter().map(to_wake_row).collect(),
                            );
                            win.set_power_note(SharedString::from(note));
                        }
                    });
                }
                4 => {
                    let (rows, note) = mainwin::journal_rows();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(win) = back.upgrade() {
                            replace(
                                &win.get_journal(),
                                rows.into_iter().map(to_journal_row).collect(),
                            );
                            win.set_journal_note(SharedString::from(note));
                        }
                    });
                }
                _ => {}
            });
        });
    }

    // Отладочный ход: открыть главное окно сразу, чтобы проверить отрисовку
    // без иконки в трее.
    if let Ok(section) = std::env::var("BAMBOO_OPEN_WINDOW") {
        if let Ok(index) = section.parse::<i32>() {
            main_window.set_section(index);
        }
        main_window.show().ok();
        main_window.invoke_refresh();
    }

    let weak = widget.as_weak();
    let main_weak = main_window.as_weak();
    let timer = slint::Timer::default();
    // Состояние автоскрытия в полноэкранном режиме (ТЗ 14.2). Реагируем на
    // переходы, а не на само состояние: иначе, если пользователь вручную
    // вернёт виджет поверх игры, мы бы прятали его снова каждый тик.
    let mut was_fullscreen = false;
    let mut hidden_for_fullscreen = false;
    // Стили окна применяем из цикла событий, а не сразу после создания.
    // Slint делает нативное окно лениво, и до первого прохода цикла
    // дескриптора ещё нет: все настройки уходили в пустоту, поэтому виджет
    // так и оставался обычным окном с кнопкой в панели задач.
    let mut styled = false;
    timer.start(
        slint::TimerMode::Repeated,
        // Опрашиваем канал чаще, чем приходят данные: так виджет реагирует
        // на действия сразу, а пустая проверка канала ничего не стоит.
        Duration::from_millis(200),
        move || {
            let Some(widget) = weak.upgrade() else {
                return;
            };

            if !styled {
                apply_window_look(&widget);
                styled = window_handle(&widget) != 0;
            }

            // Полноэкранное приложение (игра, презентация, видео) не должно
            // перекрываться виджетом. Прячем на входе в полный экран и
            // возвращаем на выходе — но только если прятали сами.
            let fullscreen = bamboo_sys::notification_state().is_fullscreen();
            match fullscreen_action(
                fullscreen,
                was_fullscreen,
                widget.window().is_visible(),
                hidden_for_fullscreen,
            ) {
                FullscreenAction::Hide => {
                    widget.window().hide().ok();
                    visible.store(false, Ordering::Relaxed);
                    hidden_for_fullscreen = true;
                }
                FullscreenAction::Restore => {
                    widget.window().show().ok();
                    visible.store(true, Ordering::Relaxed);
                    hidden_for_fullscreen = false;
                }
                FullscreenAction::None => {}
            }
            was_fullscreen = fullscreen;

            if let Some(tray) = &tray {
                for action in tray.poll() {
                    match action {
                        tray::TrayAction::ToggleWidget => toggle(&widget, &visible),
                        tray::TrayAction::OpenWindow => {
                            if let Some(main) = main_weak.upgrade() {
                                main.show().ok();
                                main.invoke_refresh();
                            }
                        }
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
                apply_snapshot(&widget, &snapshot, &processes, &spark, &spark_cpu);
                // Обновляем главное окно, только если оно на экране.
                if let Some(main) = main_weak.upgrade() {
                    if main.window().is_visible() {
                        apply_overview(&main, &snapshot, &main_processes);
                    }
                }
                // Держим свежий снимок для пересортировки по клику и для
                // поиска имени процесса при действиях.
                *last_snapshot.borrow_mut() = Some(snapshot);
            }
        },
    );

    widget.run()?;
    Ok(())
}

/// Наполняет разделы «Обзор» и «Процессы» из снимка.
#[cfg(windows)]
fn apply_overview(
    main: &MainWindow,
    snapshot: &collector::Snapshot,
    processes: &ModelRc<MainProcessRow>,
) {
    let overview = mainwin::overview(snapshot);
    main.set_cpu_summary(SharedString::from(overview.cpu));
    main.set_memory_summary(SharedString::from(overview.memory));
    main.set_process_summary(SharedString::from(overview.processes));

    // Накопители и подкачка: дашборд в обзоре отвечает на вопрос «что
    // именно грузит диск», который иначе приходится выяснять на ощупь.
    let disks: Vec<DiskLoadRow> = snapshot
        .disks
        .iter()
        .map(|disk| DiskLoadRow {
            name: SharedString::from(disk.name.clone()),
            busy: SharedString::from(format!("занят {:.0}%", disk.busy * 100.0)),
            speed: SharedString::from(format!(
                "чтение {}/с, запись {}/с",
                disk.read_per_second, disk.write_per_second
            )),
            queue: SharedString::from(disk.queue_depth.to_string()),
            saturated: disk.saturated,
            fill: disk.busy as f32,
        })
        .collect();
    replace(&main.get_disk_load(), disks);

    let pagefiles: Vec<PagefileRow> = snapshot
        .pagefiles
        .iter()
        .map(|file| PagefileRow {
            r#where: SharedString::from(file.where_.clone()),
            used: SharedString::from(format!(
                "{} из {} ({:.0}%)",
                file.in_use,
                file.total,
                file.usage * 100.0
            )),
            peak: SharedString::from(format!("наибольшая занятость с загрузки: {}", file.peak)),
            fill: file.usage as f32,
        })
        .collect();
    replace(&main.get_pagefiles(), pagefiles);
    main.set_disk_pressure(SharedString::from(
        snapshot.disk_pressure.clone().unwrap_or_default(),
    ));

    fill_processes(main, snapshot, processes);
}

/// Перестраивает таблицу процессов по текущей сортировке окна.
#[cfg(windows)]
fn fill_processes(
    main: &MainWindow,
    snapshot: &collector::Snapshot,
    processes: &ModelRc<MainProcessRow>,
) {
    let sort = mainwin::SortColumn::from_index(main.get_sort_column());
    let rows: Vec<MainProcessRow> =
        mainwin::process_rows(snapshot, sort, main.get_sort_descending())
            .into_iter()
            .map(|row| MainProcessRow {
                name: SharedString::from(row.name),
                pid: SharedString::from(row.pid),
                cpu: SharedString::from(row.cpu),
                memory: SharedString::from(row.memory),
                threads: SharedString::from(row.threads),
                badge: SharedString::from(row.badge),
                growth: SharedString::from(row.growth),
                leak: row.leak,
                state: SharedString::from(row.state),
                hung: row.hung,
                disk: SharedString::from(row.disk),
            })
            .collect();
    replace(processes, rows);
}

#[cfg(windows)]
fn to_drive_row(row: mainwin::DriveRow) -> DriveRow {
    DriveRow {
        title: SharedString::from(row.title),
        facts: SharedString::from(row.facts),
        verdict: SharedString::from(row.verdict),
    }
}

#[cfg(windows)]
fn to_wake_row(row: mainwin::WakeRow) -> WakeRow {
    WakeRow {
        when: SharedString::from(row.when),
        source: SharedString::from(row.source),
    }
}

#[cfg(windows)]
fn to_journal_row(row: mainwin::JournalRow) -> JournalRow {
    JournalRow {
        when: SharedString::from(row.when),
        action: SharedString::from(row.action),
        target: SharedString::from(row.target),
        status: SharedString::from(row.status),
    }
}

/// Что сделать с виджетом на переходе полноэкранного режима (ТЗ 14.2).
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FullscreenAction {
    /// Спрятать: вошли в полный экран, а виджет на экране.
    Hide,
    /// Вернуть: вышли из полного экрана, и прятали его мы.
    Restore,
    /// Ничего не делать.
    None,
}

/// Решение об автоскрытии по фронту полноэкранного режима.
///
/// Реагируем именно на переход, а не на само состояние: если пользователь
/// вручную вернул виджет поверх полноэкранного приложения, дёргать его
/// каждый тик нельзя. И возвращаем окно только если прятали сами —
/// самостоятельно скрытый пользователем виджет всплывать не должен.
#[cfg(windows)]
fn fullscreen_action(
    fullscreen: bool,
    was_fullscreen: bool,
    widget_visible: bool,
    hidden_by_us: bool,
) -> FullscreenAction {
    if fullscreen && !was_fullscreen && widget_visible {
        FullscreenAction::Hide
    } else if !fullscreen && was_fullscreen && hidden_by_us {
        FullscreenAction::Restore
    } else {
        FullscreenAction::None
    }
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
    spark_cpu: &ModelRc<f32>,
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

    // Диск: показываем самый занятый — если их несколько, интересен тот,
    // который упирается в предел, а не средняя температура по больнице.
    match snapshot
        .disks
        .iter()
        .max_by(|a, b| a.busy.total_cmp(&b.busy))
    {
        Some(disk) => {
            widget.set_disk_value(SharedString::from(format!("{:.0}%", disk.busy * 100.0)));
            widget.set_disk_note(SharedString::from(format!(
                "чтение {}/с
запись {}/с",
                disk.read_per_second, disk.write_per_second
            )));
        }
        None => {
            widget.set_disk_value(SharedString::from("—"));
            widget.set_disk_note(SharedString::from("замеряю…"));
        }
    }
    widget.set_disk_pressure(SharedString::from(
        snapshot.disk_pressure.clone().unwrap_or_default(),
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
    replace(spark_cpu, snapshot.spark_cpu.clone());
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

/// Дескриптор главного окна. Отдельно от виджета: типы окон разные,
/// а общий трейт Slint для них не выведен.
#[cfg(windows)]
fn main_window_handle(main: &MainWindow) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    main.window()
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

#[cfg(all(windows, test))]
mod tests {
    use super::{fullscreen_action, FullscreenAction};

    #[test]
    fn entering_fullscreen_hides_a_visible_widget() {
        // Вошли в полный экран (переход false→true), виджет на экране.
        assert_eq!(
            fullscreen_action(true, false, true, false),
            FullscreenAction::Hide
        );
    }

    #[test]
    fn a_hidden_widget_is_not_touched_on_entry() {
        // Виджет и так скрыт — прятать нечего.
        assert_eq!(
            fullscreen_action(true, false, false, false),
            FullscreenAction::None
        );
    }

    #[test]
    fn leaving_fullscreen_restores_only_what_we_hid() {
        // Выход из полного экрана, прятали сами — возвращаем.
        assert_eq!(
            fullscreen_action(false, true, false, true),
            FullscreenAction::Restore
        );
        // Выход, но прятали не мы (пользователь сам закрыл) — не всплываем.
        assert_eq!(
            fullscreen_action(false, true, false, false),
            FullscreenAction::None
        );
    }

    #[test]
    fn staying_fullscreen_does_nothing_even_if_user_reopens() {
        // Полный экран продолжается (true→true). Пользователь вернул виджет
        // вручную — трогать его на каждом тике нельзя, реагируем на переход.
        assert_eq!(
            fullscreen_action(true, true, true, true),
            FullscreenAction::None
        );
    }

    #[test]
    fn staying_windowed_does_nothing() {
        assert_eq!(
            fullscreen_action(false, false, true, false),
            FullscreenAction::None
        );
    }
}
