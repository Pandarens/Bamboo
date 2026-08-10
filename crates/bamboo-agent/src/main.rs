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
mod autopilot;
mod collector;
#[cfg(windows)]
mod gamemode;
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

/// Разделитель имён развёрнутых групп.
///
/// Символ с кодом 1 в именах процессов не встречается, поэтому годится
/// как разделитель и не требует экранирования.
#[cfg(windows)]
const GROUP_SEPARATOR: char = '\u{1}';

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

    // Окно создаём сразу — иначе стили и иконку применять некуда, — но
    // показываем только если человек этого хочет. По умолчанию Bamboo
    // запускается в трей и на экран не лезет.
    widget.show()?;
    let show_widget = bamboo_sys::show_widget_on_start();
    if !show_widget {
        widget.window().hide().ok();
    }
    visible.store(show_widget, Ordering::Relaxed);

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
        .set_size(slint::PhysicalSize::new(1420, 720));
    // Модели дашборда: без них replace не найдёт VecModel и молча ничего
    // не сделает — список останется пустым.
    main_window.set_disk_load(ModelRc::new(VecModel::from(Vec::<DiskLoadRow>::new())));
    main_window.set_pagefiles(ModelRc::new(VecModel::from(Vec::<PagefileRow>::new())));
    main_window.set_volumes(ModelRc::new(VecModel::from(Vec::<VolumeRow>::new())));
    main_window.set_suggestions(ModelRc::new(VecModel::from(Vec::<SuggestionRow>::new())));
    main_window.set_autostart(bamboo_sys::is_in_startup());
    main_window.set_show_widget_on_start(bamboo_sys::show_widget_on_start());
    main_window.set_processes(main_processes.clone());
    main_window.set_drives(drives_model.clone());
    main_window.set_wakes(wakes_model.clone());
    main_window.set_journal(journal_model.clone());

    // Последний снимок держим под рукой: по клику на заголовок столбца
    // таблицу надо пересортировать сразу, а не ждать следующего тика.
    let last_snapshot: std::rc::Rc<std::cell::RefCell<Option<collector::Snapshot>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));

    // Придержанные процессы: реестр держит job-объекты, а без них
    // ограничение снялось бы сразу после установки.
    let io_limits: std::rc::Rc<std::cell::RefCell<actions::IoLimits>> =
        std::rc::Rc::new(std::cell::RefCell::new(actions::IoLimits::new()));

    // Автоматика и её память: что придержано и какой записью журнала это
    // откатывать. Без номера записи вернуть как было было бы нечем.
    let autopilot: std::rc::Rc<std::cell::RefCell<autopilot::Autopilot>> = {
        let mut pilot = autopilot::Autopilot::new();
        pilot.set_enabled(bamboo_sys::autopilot_enabled());
        std::rc::Rc::new(std::cell::RefCell::new(pilot))
    };
    let autopilot_holds: std::rc::Rc<
        std::cell::RefCell<std::collections::HashMap<(u32, &'static str), i64>>,
    > = std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));

    // Игровой режим помнит, что и кому менял: без этого вернуть прежние
    // настройки было бы нечем.
    let game_mode: std::rc::Rc<std::cell::RefCell<gamemode::GameMode>> =
        std::rc::Rc::new(std::cell::RefCell::new(gamemode::GameMode::new()));

    // Завершённые процессы: следим, не вернулись ли они. Возвращение
    // означает, что кто-то их поднимает, и человеку полезно знать кто.
    let terminated: std::rc::Rc<std::cell::RefCell<actions::Terminated>> =
        std::rc::Rc::new(std::cell::RefCell::new(actions::Terminated::new()));

    // Сортировка по столбцу. Повторный щелчок по тому же столбцу
    // разворачивает порядок — привычное поведение любой таблицы.
    {
        let weak = main_window.as_weak();
        let rows = main_processes.clone();
        let snapshot = last_snapshot.clone();
        let limits = io_limits.clone();
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
                fill_processes(&win, snapshot, &rows, &limits.borrow());
            }
        });
    }

    // Разрешение на самостоятельную оптимизацию.
    {
        let weak = main_window.as_weak();
        let pilot = autopilot.clone();
        let holds = autopilot_holds.clone();
        main_window.on_toggle_autopilot(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };

            let mut pilot = pilot.borrow_mut();
            let now_on = !pilot.enabled();
            // Выключение обязано вернуть всё придержанное: автоматика,
            // оставившая после себя следы, — худшее из возможного.
            let returned = pilot.set_enabled(now_on);
            for held in &returned {
                let key = (held.pid, remedy_key(held.remedy));
                if let Some(journal_id) = holds.borrow_mut().remove(&key) {
                    actions::revert_automatically(journal_id, "автоматика выключена");
                }
            }

            let note = match bamboo_sys::set_autopilot_enabled(now_on) {
                Ok(()) if now_on => "Автоматика включена. Пока вас нет, Bamboo придержит                     фоновую работу, а как только вы тронете мышь — вернёт всё                     на место. Каждое действие попадёт в журнал."
                    .to_string(),
                Ok(()) => format!(
                    "Автоматика выключена, вернулось процессов: {}. Bamboo снова                      только предлагает.",
                    returned.len()
                ),
                Err(error) => format!("Настройку сохранить не удалось: {error}"),
            };
            win.set_autopilot(pilot.enabled());
            win.set_action_note(SharedString::from(note));
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
        let limits = io_limits.clone();
        let killed = terminated.clone();
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
            killed.borrow_mut().remember(&name);

            // Список сразу перестраиваем: строки завершённого процесса
            // в таблице быть не должно, а следующего тика ждать секунду.
            if let Some(snapshot) = snapshot.borrow_mut().as_mut() {
                snapshot.top.retain(|line| line.pid != pid);
            }
            if let Some(snapshot) = snapshot.borrow().as_ref() {
                fill_processes(&win, snapshot, &rows, &limits.borrow());
            }
        });
    }

    // Игровой режим.
    {
        let weak = main_window.as_weak();
        let snapshot = last_snapshot.clone();
        let mode = game_mode.clone();
        main_window.on_toggle_game_mode(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };

            let note = if mode.borrow().is_on() {
                mode.borrow_mut().turn_off()
            } else {
                let borrowed = snapshot.borrow();
                let Some(snapshot) = borrowed.as_ref() else {
                    return;
                };
                // Игру ищем по переднему окну: программа, занимающая экран,
                // и есть та, чем человек сейчас занят.
                let foreground = bamboo_sys::window::foreground_pid();
                let candidates: Vec<gamemode::Candidate<'_>> = snapshot
                    .top
                    .iter()
                    .map(|line| gamemode::Candidate {
                        pid: line.pid,
                        name: &line.name,
                        cpu_percent: line.cpu_percent,
                        is_foreground: line.pid == foreground,
                        protected: bamboo_policy::immutable_reason(&bamboo_policy::ProcessFacts {
                            image_name: &line.name,
                            session_id: 1,
                            ..Default::default()
                        })
                        .is_some(),
                    })
                    .collect();
                mode.borrow_mut().turn_on(&candidates)
            };

            win.set_game_mode(mode.borrow().is_on());
            win.set_action_note(SharedString::from(note));
        });
    }

    // Развернуть или свернуть группу.
    {
        let weak = main_window.as_weak();
        let snapshot = last_snapshot.clone();
        let limits = io_limits.clone();
        let rows = main_processes.clone();
        main_window.on_toggle_group(move |name| {
            let Some(win) = weak.upgrade() else {
                return;
            };
            // Имя приходит чистым: стрелку и отступ рисует интерфейс.
            let name = name.to_string();

            let current = win.get_expanded_groups().to_string();
            let mut open: Vec<String> = current
                .split(GROUP_SEPARATOR)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect();

            let now_open = match open.iter().position(|entry| *entry == name) {
                Some(at) => {
                    open.remove(at);
                    false
                }
                None => {
                    open.push(name.clone());
                    true
                }
            };
            win.set_expanded_groups(SharedString::from(open.join(&GROUP_SEPARATOR.to_string())));

            if let Some(snapshot) = snapshot.borrow().as_ref() {
                fill_processes(&win, snapshot, &rows, &limits.borrow());

                // Развернули — объясняем, из чего складывается память
                // программы. Это ответ на «почему браузер занимает восемь
                // гигабайт», который иначе приходится искать самому.
                let note = if now_open {
                    // Список расширений читается с диска, поэтому только
                    // в момент разворачивания группы и только для браузера.
                    let extensions = if mainwin::is_browser(&name) {
                        bamboo_sys::installed_extensions().unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    mainwin::explain_group_memory(snapshot, &name, &extensions).unwrap_or_default()
                } else {
                    String::new()
                };
                win.set_action_note(SharedString::from(note));
            }
        });
    }

    // Завершить всю группу процессов разом.
    {
        let weak = main_window.as_weak();
        let snapshot = last_snapshot.clone();
        let limits = io_limits.clone();
        let rows = main_processes.clone();
        let killed = terminated.clone();
        main_window.on_terminate_group(move |pids| {
            let Some(win) = weak.upgrade() else {
                return;
            };

            let wanted: Vec<u32> = pids
                .split(',')
                .filter_map(|pid| pid.trim().parse::<u32>().ok())
                .collect();
            if wanted.is_empty() {
                return;
            }

            // Имя берём у первого найденного: у всех в группе оно одно.
            let name = snapshot
                .borrow()
                .as_ref()
                .and_then(|snapshot| {
                    snapshot
                        .top
                        .iter()
                        .find(|line| wanted.contains(&line.pid))
                        .map(|line| line.name.clone())
                })
                .unwrap_or_default();

            let mut done = 0usize;
            let mut first_failure: Option<String> = None;
            for pid in &wanted {
                let note = actions::terminate(*pid, &name);
                if note.contains("завершён") {
                    done += 1;
                } else if first_failure.is_none() {
                    first_failure = Some(note);
                }
            }

            let note = match first_failure {
                None => format!("{name}: завершено процессов — {done}. Это необратимо."),
                Some(failure) => format!("{name}: завершено {done} из {}. {failure}", wanted.len()),
            };
            win.set_action_note(SharedString::from(note));
            killed.borrow_mut().remember(&name);

            if let Some(snapshot) = snapshot.borrow_mut().as_mut() {
                snapshot.top.retain(|line| !wanted.contains(&line.pid));
            }
            if let Some(snapshot) = snapshot.borrow().as_ref() {
                fill_processes(&win, snapshot, &rows, &limits.borrow());
            }
        });
    }

    // Автозапуск из раздела настроек: та же операция, что и галочка в трее.
    {
        let weak = main_window.as_weak();
        main_window.on_toggle_autostart(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
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
            // Состояние читаем из реестра: пользователь мог поменять его
            // и мимо нас, через диспетчер задач.
            win.set_autostart(bamboo_sys::is_in_startup());
            win.set_action_note(SharedString::from(note));
        });
    }

    // Остановка службы, которая возвращает завершённый процесс.
    {
        let weak = main_window.as_weak();
        let killed = terminated.clone();
        main_window.on_stop_culprit_service(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let Some(service) = killed.borrow().culprit_service() else {
                return;
            };

            win.set_action_note(SharedString::from(actions::stop_service(&service)));
            // Предложение одноразовое: служба либо остановлена, либо нет,
            // и держать кнопку висящей незачем.
            win.set_culprit_service(SharedString::from(""));
        });
    }

    // Показывать ли виджет при запуске.
    {
        let weak = main_window.as_weak();
        main_window.on_toggle_widget_on_start(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let wanted = !win.get_show_widget_on_start();
            let note = match bamboo_sys::set_show_widget_on_start(wanted) {
                Ok(()) if wanted => "Виджет будет появляться сразу при запуске.",
                Ok(()) => "Bamboo будет запускаться в трее, без виджета.",
                Err(_) => "Настройку сохранить не удалось.",
            };
            win.set_show_widget_on_start(bamboo_sys::show_widget_on_start());
            win.set_action_note(SharedString::from(note));
        });
    }

    // Фильтр по имени процесса.
    {
        let weak = main_window.as_weak();
        let snapshot = last_snapshot.clone();
        let limits = io_limits.clone();
        let rows = main_processes.clone();
        main_window.on_apply_filter(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            if let Some(snapshot) = snapshot.borrow().as_ref() {
                fill_processes(&win, snapshot, &rows, &limits.borrow());
            }
        });
    }

    // Переключение «по программам» / «по процессам».
    {
        let weak = main_window.as_weak();
        let snapshot = last_snapshot.clone();
        let limits = io_limits.clone();
        let rows = main_processes.clone();
        main_window.on_toggle_grouping(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            win.set_grouped(!win.get_grouped());
            if let Some(snapshot) = snapshot.borrow().as_ref() {
                fill_processes(&win, snapshot, &rows, &limits.borrow());
            }
        });
    }

    // Придержать или отпустить диск процесса.
    {
        let weak = main_window.as_weak();
        let snapshot = last_snapshot.clone();
        let limits = io_limits.clone();
        let rows = main_processes.clone();
        main_window.on_toggle_io_limit(move |pid| {
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

            let note = limits.borrow_mut().toggle(pid, &name);
            win.set_action_note(SharedString::from(note));

            // Перерисовываем список, чтобы кнопка сразу сменила подпись.
            if let Some(snapshot) = snapshot.borrow().as_ref() {
                fill_processes(&win, snapshot, &rows, &limits.borrow());
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
        let refresh_snapshot = last_snapshot.clone();
        let refresh_limits = io_limits.clone();
        let refresh_rows = main_processes.clone();
        let refresh_pilot = autopilot.clone();
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
            // Предложения считаются из снимка мгновенно — фонового чтения
            // им не нужно, поэтому обновляем прямо здесь.
            if section == 6 {
                if let Some(snapshot) = refresh_snapshot.borrow().as_ref() {
                    apply_overview(
                        &win,
                        snapshot,
                        &refresh_rows,
                        &refresh_limits.borrow(),
                        &refresh_pilot,
                    );
                }
                return;
            }

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
    let tick_pilot = autopilot.clone();
    let tick_holds = autopilot_holds.clone();
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
    let mut main_styled = false;
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

                // Логотип окнам: без него Windows рисует в панели задач
                // пустой квадрат, хотя в трее иконка уже есть.
                if styled {
                    let logo = tray::logo_rgba();
                    let _ = bamboo_sys::window::set_icon(
                        window_handle(&widget),
                        &logo,
                        tray::LOGO_SIZE,
                    );
                }
            }

            // Главное окно создаётся позже виджета: его нативное окно
            // появляется только при первом показе, и до этого момента
            // ставить иконку некуда.
            if !main_styled {
                if let Some(main) = main_weak.upgrade() {
                    let handle = main_window_handle(&main);
                    if handle != 0 {
                        let _ = bamboo_sys::window::set_icon(
                            handle,
                            &tray::logo_rgba(),
                            tray::LOGO_SIZE,
                        );
                        main_styled = true;
                    }
                }
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
                        tray::TrayAction::ToggleAutostart => {
                            let note = tray.toggle_autostart();
                            if let Some(main) = main_weak.upgrade() {
                                main.set_autostart(tray.autostart_enabled());
                                main.set_action_note(SharedString::from(note));
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

                // Автоматика работает независимо от окна: когда человека
                // нет, окна тоже нет, а придерживать фоновую работу надо
                // именно тогда.
                {
                    let applied = actions::AppliedActions::load();
                    let limits = io_limits.borrow();
                    let raw = mainwin::suggestions_for(&snapshot, &|pid| {
                        limits.is_limited(pid) || !applied.marks(pid, false).is_empty()
                    });
                    drop(limits);

                    let mut pilot = tick_pilot.borrow_mut();
                    let plan = pilot.decide(snapshot.user_idle_ms, &raw);
                    if !plan.is_empty() {
                        apply_autopilot_plan(&mut pilot, plan, &tick_holds);
                    }
                }

                // Обновляем главное окно, только если оно на экране.
                if let Some(main) = main_weak.upgrade() {
                    if main.window().is_visible() {
                        apply_overview(
                            &main,
                            &snapshot,
                            &main_processes,
                            &io_limits.borrow(),
                            &tick_pilot,
                        );
                    }
                }
                // Не вернулся ли кто-то из завершённых? Если вернулся,
                // называем того, кто его поднял.
                let processes: Vec<(String, u32, u32)> = snapshot
                    .top
                    .iter()
                    .map(|line| (line.name.clone(), line.pid, line.parent_pid))
                    .collect();
                let parents: std::collections::HashMap<u32, String> = snapshot
                    .top
                    .iter()
                    .map(|line| (line.pid, line.name.clone()))
                    .collect();

                let returned = terminated
                    .borrow_mut()
                    .check_returns(&processes, &|pid| parents.get(&pid).cloned());
                if let Some(note) = returned {
                    if let Some(main) = main_weak.upgrade() {
                        main.set_action_note(SharedString::from(note));
                        // Служба-источник: по ней рисуется кнопка остановки.
                        let culprit = terminated
                            .borrow()
                            .culprit_service()
                            .map(|service| service.display)
                            .unwrap_or_default();
                        main.set_culprit_service(SharedString::from(culprit));
                    }
                }

                // Держим свежий снимок для пересортировки по клику и для
                // поиска имени процесса при действиях.
                *last_snapshot.borrow_mut() = Some(snapshot);
            }
        },
    );

    // Не `widget.run()`: тот завершает цикл, когда закрыто последнее окно.
    // Bamboo — фоновый наблюдатель, он живёт в трее и обязан пережить
    // закрытие виджета. Раньше вместе с виджетом исчезала и иконка.
    slint::run_event_loop_until_quit()?;
    Ok(())
}

/// Наполняет разделы «Обзор» и «Процессы» из снимка.
/// Выполняет решение автоматики: применяет и снимает.
///
/// Номер записи журнала хранится рядом с процессом — им и откатываем.
/// Если номера нет, снимать нечего: значит применить и не вышло.
#[cfg(windows)]
fn apply_autopilot_plan(
    pilot: &mut autopilot::Autopilot,
    plan: autopilot::Plan,
    holds: &std::cell::RefCell<std::collections::HashMap<(u32, &'static str), i64>>,
) {
    use bamboo_analyze::suggest::Remedy;

    for held in plan.release {
        let key = (held.pid, remedy_key(held.remedy));
        if let Some(journal_id) = holds.borrow_mut().remove(&key) {
            actions::revert_automatically(journal_id, "человек вернулся за компьютер");
        }
    }

    for held in plan.apply {
        let what = match held.remedy {
            Remedy::EcoQos => actions::RowAction::EcoQos,
            Remedy::LowerMemory => actions::RowAction::LowerMemory,
            // Придержать диск умеет только ограничитель, и он снимается
            // сам вместе с дескриптором — журнал ему не нужен.
            _ => {
                pilot.forget(held.pid);
                continue;
            }
        };

        match actions::apply_automatically(held.pid, &held.name, what) {
            Some(journal_id) => {
                holds
                    .borrow_mut()
                    .insert((held.pid, remedy_key(held.remedy)), journal_id);
            }
            // Политика отказала или процесс исчез — забываем, иначе будем
            // показывать человеку неправду о числе придержанных.
            None => pilot.forget(held.pid),
        }
    }
}

/// Короткое имя средства: по нему находим запись журнала для отката.
#[cfg(windows)]
fn remedy_key(remedy: bamboo_analyze::suggest::Remedy) -> &'static str {
    use bamboo_analyze::suggest::Remedy;
    match remedy {
        Remedy::EcoQos => "эконом",
        Remedy::LowerMemory => "память",
        Remedy::ThrottleDisk => "диск",
        Remedy::JustSaying => "ничего",
    }
}

#[cfg(windows)]
fn apply_overview(
    main: &MainWindow,
    snapshot: &collector::Snapshot,
    processes: &ModelRc<MainProcessRow>,
    limits: &actions::IoLimits,
    autopilot: &std::cell::RefCell<autopilot::Autopilot>,
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

    let volumes: Vec<VolumeRow> = snapshot
        .volumes
        .iter()
        .map(|volume| VolumeRow {
            title: SharedString::from(volume.title.clone()),
            space: SharedString::from(format!("свободно {} из {}", volume.free, volume.total)),
            fill: volume.usage as f32,
            cramped: volume.cramped,
        })
        .collect();
    replace(&main.get_volumes(), volumes);

    // Предложения считаем из того же снимка. Уже применённое исключаем:
    // предлагать второй раз то же самое — назойливость.
    let applied = actions::AppliedActions::load();
    let (suggestions, note) = mainwin::suggestion_rows(snapshot, &|pid| {
        limits.is_limited(pid) || !applied.marks(pid, false).is_empty()
    });
    let rows: Vec<SuggestionRow> = suggestions
        .into_iter()
        .map(|row| SuggestionRow {
            pid: SharedString::from(row.pid),
            title: SharedString::from(row.title),
            reason: SharedString::from(row.reason),
            effect: SharedString::from(row.effect),
            button: SharedString::from(row.button),
            action: row.action,
        })
        .collect();
    replace(&main.get_suggestions(), rows);
    main.set_suggestions_note(SharedString::from(note));

    // Автоматика сама работает в цикле — здесь только показываем, что она
    // сейчас делает.
    let pilot = autopilot.borrow();
    main.set_autopilot(pilot.enabled());
    main.set_autopilot_status(SharedString::from(pilot.status(snapshot.user_idle_ms)));
    main.set_disk_pressure(SharedString::from(
        snapshot.disk_pressure.clone().unwrap_or_default(),
    ));
    main.set_watch_status(SharedString::from(mainwin::watch_status(snapshot)));
    main.set_system_io(SharedString::from(
        snapshot.system_io.clone().unwrap_or_default(),
    ));
    main.set_freeze(SharedString::from(
        snapshot.freeze.clone().unwrap_or_default(),
    ));

    fill_processes(main, snapshot, processes, limits);
}

/// Перестраивает таблицу процессов по текущей сортировке окна.
#[cfg(windows)]
fn fill_processes(
    main: &MainWindow,
    snapshot: &collector::Snapshot,
    processes: &ModelRc<MainProcessRow>,
    limits: &actions::IoLimits,
) {
    let sort = mainwin::SortColumn::from_index(main.get_sort_column());
    // Режим групп и режим процессов дают одинаковые строки, поэтому
    // дальше таблица о разнице не знает.
    let filter = main.get_filter().to_string();
    // Метки прошлых действий читаем один раз на перестроение таблицы,
    // а не на каждую строку: это обращение к базе журнала.
    let applied = actions::AppliedActions::load();
    let prepared = if main.get_grouped() {
        // Развёрнутые группы держим одной строкой имён: список короткий,
        // а заводить ради него отдельную модель в окне Slint неудобно.
        let open = main.get_expanded_groups().to_string();
        mainwin::grouped_rows(
            snapshot,
            sort,
            main.get_sort_descending(),
            &|name| open.split(GROUP_SEPARATOR).any(|entry| entry == name),
            &filter,
        )
    } else {
        mainwin::process_rows(snapshot, sort, main.get_sort_descending(), &filter)
    };
    let rows: Vec<MainProcessRow> = prepared
        .into_iter()
        .map(|row| {
            let pid = row.pid.parse::<u32>().ok();
            let throttled = pid.is_some_and(|pid| limits.is_limited(pid));
            let marks = pid
                .map(|pid| applied.marks(pid, throttled))
                .unwrap_or_default();
            MainProcessRow {
                name: SharedString::from(row.name),
                cpu: SharedString::from(row.cpu),
                memory: SharedString::from(row.memory),
                threads: SharedString::from(row.threads),
                badge: SharedString::from(row.badge),
                growth: SharedString::from(row.growth),
                leak: row.leak,
                state: SharedString::from(row.state),
                hung: row.hung,
                disk: SharedString::from(row.disk),
                throttled,
                applied: SharedString::from(marks),
                is_group: row.is_group,
                expanded: row.expanded,
                is_member: row.is_member,
                member_pids: SharedString::from(row.member_pids),
                pid: SharedString::from(row.pid),
            }
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
