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
mod history;
#[cfg(windows)]
mod mainwin;
#[cfg(windows)]
mod selfwatch;
#[cfg(windows)]
mod session;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod update;

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

    // Язык интерфейса — до создания окон: строки берутся при построении,
    // и переключать их потом придётся перезапуском.
    let language = bamboo_sys::language();
    // Тексты, приходящие из Rust — объяснения анализаторов, — берут язык
    // отсюда же. Иначе интерфейс был бы английским, а объяснения в нём
    // русскими, и это хуже, чем один язык целиком.
    bamboo_core::set_language(bamboo_core::Language::parse(&language));
    if language != "ru" {
        // Русский встроен в сами файлы интерфейса и переводом не является:
        // выбирать надо только всё остальное.
        if let Err(error) = slint::select_bundled_translation(&language) {
            eprintln!("язык интерфейса не переключился: {error}");
        }
    }

    // Утилита, которая учит систему экономить, начинает с себя.
    let _ = bamboo_sys::apply_self_limits();

    // Сессии трассировки, оставшиеся от прошлого запуска. Убитый процесс
    // не выполняет Drop, а сессия ETW переживает его и продолжает работать
    // сама по себе — снять её потом можно только вручную. Выяснилось
    // на прогоне, а не в рассуждении.
    let _ = bamboo_etw::stop_stale();
    let _ = bamboo_etw::stop_stale_investigation();

    // Файл, оставшийся от прошлого обновления. К этому моменту его уже
    // никто не держит, а раньше убрать было нельзя.
    update::clean_up_after_update();

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
    main_window.set_app_version(SharedString::from(update::CURRENT));
    main_window.set_language(SharedString::from(language.clone()));
    main_window.set_update_status(SharedString::from("Проверяю обновления…"));
    main_window.set_show_widget_on_start(bamboo_sys::show_widget_on_start());
    main_window.set_processes(main_processes.clone());
    main_window.set_drives(drives_model.clone());
    main_window.set_wakes(wakes_model.clone());
    main_window.set_journal(journal_model.clone());
    main_window.set_extensions(ModelRc::new(VecModel::from(Vec::<ExtensionRow>::new())));
    main_window.set_targets(ModelRc::new(VecModel::from(Vec::<TargetRow>::new())));
    main_window.set_boots(ModelRc::new(VecModel::from(Vec::<BootRow>::new())));
    main_window.set_budget(ModelRc::new(VecModel::from(Vec::<BudgetRow>::new())));
    main_window.set_startup(ModelRc::new(VecModel::from(Vec::<StartupRow>::new())));
    main_window.set_record_cpu(ModelRc::new(VecModel::from(Vec::<f32>::new())));
    main_window.set_record_memory(ModelRc::new(VecModel::from(Vec::<f32>::new())));
    main_window.set_record_gpu(ModelRc::new(VecModel::from(Vec::<f32>::new())));
    main_window.set_record_disk(ModelRc::new(VecModel::from(Vec::<f32>::new())));

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

    // Самозамер бюджета. ТЗ требует суточного прогона «в CI на выделенной
    // машине»; машины нет, и она не нужна: резидентную утилиту правильнее
    // мерить ею самой на живой машине под настоящей нагрузкой. Стерильный
    // раннер такого не покажет, да и задание там убивают на шести часах.
    let mut selfwatch = selfwatch::SelfWatch::new();
    let written_at_start = bamboo_sys::budget::own_memory()
        .map(|_| own_written())
        .unwrap_or(0);

    // История наблюдений на диске. Без неё выводы о росте памяти
    // начинались заново после каждого перезапуска, а недельный отчёт
    // строить было не из чего.
    let mut history = match history::History::open(0) {
        Ok(history) => Some(history),
        Err(error) => {
            // Наблюдение продолжается и без истории, но молчать нельзя.
            eprintln!("история наблюдений недоступна: {error}");
            None
        }
    };

    // Уведомления. Своя скрытая иконка: чужую, которую держит трей,
    // трогать нельзя, а вторая видимая панда человеку не нужна.
    // Отсутствие иконки не беда — тогда о подвисании скажет виджет.
    let notifier = bamboo_sys::Notifier::new().ok();
    // О чём уже говорили: одно подвисание не должно всплывать дважды.
    let mut last_notified = String::new();

    // Отказы человека. Читаются с диска при запуске: без этого
    // «больше не предлагать никогда» жило бы до выхода из программы,
    // и то же предложение возвращалось бы после каждой перезагрузки.
    let rejections: std::rc::Rc<std::cell::RefCell<bamboo_policy::Rejections>> =
        std::rc::Rc::new(std::cell::RefCell::new(actions::load_rejections()));

    // Установленные игры: читаются с диска при открытии раздела «Анализ»
    // и живут до закрытия программы. Перечитывать их каждый тик значило бы
    // ходить на диск без нужды.
    let known_games: std::sync::Arc<std::sync::Mutex<Vec<bamboo_sys::Game>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    // Идущая запись наблюдения за программой.
    let recording: std::rc::Rc<std::cell::RefCell<Option<session::Session>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));

    // Найденное обновление. Держим здесь, потому что кнопка «Обновить»
    // ставит именно то, о чём человеку сказали, а не спрашивает GitHub
    // заново: между сообщением и нажатием выпуск мог смениться.
    let pending_update: std::sync::Arc<std::sync::Mutex<Option<update::Release>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));

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

    // Переключение языка интерфейса.
    {
        let weak = main_window.as_weak();
        main_window.on_toggle_language(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };

            // Меняем на другой из двух: третьего пока нет, и городить
            // список ради двух пунктов незачем.
            let wanted = if win.get_language() == "ru" { "en" } else { "ru" };
            let note = match bamboo_sys::set_language(wanted) {
                // Строки интерфейса берутся при построении окна, поэтому
                // на лету они не сменятся. Сказать об этом надо прямо:
                // человек нажал и вправе понимать, почему ничего
                // не изменилось.
                Ok(()) => {
                    win.set_language(SharedString::from(wanted));
                    if wanted == "en" {
                        "Язык переключён на английский. Он вступит в силу                          после перезапуска Bamboo — закройте его из трея                          и запустите снова."
                    } else {
                        "Язык переключён на русский. Он вступит в силу после                          перезапуска Bamboo."
                    }
                    .to_string()
                }
                Err(error) => format!("Язык сохранить не удалось: {error}"),
            };
            win.set_action_note(SharedString::from(note));
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

    // Отказ от предложения. Дважды — и оно замолкает навсегда.
    {
        let weak = main_window.as_weak();
        let rejections = rejections.clone();
        let snapshot = last_snapshot.clone();
        main_window.on_reject_suggestion(move |pid, code| {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let Ok(pid) = pid.parse::<u32>() else {
                return;
            };

            // Отказ запоминаем по имени программы, а не по номеру процесса:
            // номер сменится при следующем запуске, и «никогда» продержалось
            // бы ровно до перезапуска программы.
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

            let what = remedy_name(code);
            let now = bamboo_core::SampleTime::wall_clock_now();
            let learned = rejections.borrow_mut().reject(&name, what, now);

            let note = match learned {
                bamboo_policy::Learned::Silenced => format!(
                    "{name}: больше не предложу «{what}». Передумаете — удалите строку                      из файла отказов рядом с журналом."
                ),
                bamboo_policy::Learned::Counted { .. } => format!(
                    "{name}: понял, «{what}» пока пропускаю. Откажетесь ещё раз —                      перестану предлагать совсем."
                ),
                // Тот же самый отказ пришёл дважды: считать его вторым
                // нельзя, иначе одно нажатие замолчит предложение навсегда.
                bamboo_policy::Learned::Duplicate => return,
            };

            if let Err(error) = actions::save_rejections(&rejections.borrow()) {
                win.set_action_note(SharedString::from(format!(
                    "Отказ учтён, но сохранить его не удалось: {error}.                      После перезапуска предложение вернётся."
                )));
                return;
            }
            win.set_action_note(SharedString::from(note));
        });
    }

    // Включение и выключение записи автозагрузки.
    {
        let weak = main_window.as_weak();
        main_window.on_toggle_startup(move |name, wanted| {
            let Some(win) = weak.upgrade() else {
                return;
            };

            let note = match bamboo_sys::set_startup_enabled(&name, wanted) {
                // Отвечаем по факту, а не по намерению: Windows может
                // и отказать, и человек должен видеть, что вышло.
                Ok(true) if wanted => format!("{name}: будет запускаться вместе с Windows."),
                Ok(true) => format!(
                    "{name}: больше не запускается сам. Программа осталась                      установленной — запустить её можно как обычно."
                ),
                Ok(false) => format!("{name}: состояние не изменилось."),
                Err(error) => format!("{name}: изменить не удалось — {error}"),
            };
            win.set_action_note(SharedString::from(note));
        });
    }

    // Сборка недельного отчёта.
    {
        let weak = main_window.as_weak();
        let snapshot = last_snapshot.clone();
        main_window.on_build_report(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let format = win.get_report_format();

            let empty = collector::Snapshot::default();
            let borrowed = snapshot.borrow();
            let snapshot = borrowed.as_ref().unwrap_or(&empty);

            let (text, note) = mainwin::weekly_report(snapshot, format);
            win.set_report_text(SharedString::from(text));
            win.set_report_note(SharedString::from(note));
        });
    }

    // Сохранение отчёта в файл.
    {
        let weak = main_window.as_weak();
        main_window.on_save_report(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let text = win.get_report_text().to_string();
            if text.is_empty() {
                win.set_report_note(SharedString::from(
                    "Сохранять нечего: сначала соберите отчёт.",
                ));
                return;
            }

            let note = match mainwin::save_report(&text, win.get_report_format()) {
                Ok(path) => format!("Отчёт сохранён: {path}"),
                Err(error) => format!("Сохранить не удалось: {error}"),
            };
            win.set_report_note(SharedString::from(note));
        });
    }

    // Запись наблюдения: начать и остановить.
    {
        let weak = main_window.as_weak();
        let recording = recording.clone();
        main_window.on_toggle_recording(move |app| {
            let Some(win) = weak.upgrade() else {
                return;
            };

            let mut recording = recording.borrow_mut();
            if recording.is_some() {
                // Остановка: запись остаётся на экране. Стирать её сразу
                // значило бы выбросить то, ради чего человек и записывал.
                *recording = None;
                win.set_recording(false);
                win.set_record_status(SharedString::from(
                    "Запись остановлена. График и разбор ниже — они никуда                      не денутся, пока вы не начнёте новую запись.",
                ));
                return;
            }

            let app = app.trim().to_string();
            if app.is_empty() {
                win.set_record_status(SharedString::from(
                    "Впишите имя программы — так, как оно стоит в списке процессов:                      например, game.exe.",
                ));
                return;
            }

            *recording = Some(session::Session::start(&app));
            win.set_recording(true);
            win.set_record_verdict(SharedString::from(""));
            win.set_record_status(SharedString::from(format!(
                "Записываю {app}. Переключайтесь в программу и работайте как                  обычно — Bamboo замеряет раз в секунду. Разбор появится примерно                  через десять секунд: на меньшем отрезке любой вывод был бы                  гаданием."
            )));
        });
    }

    // Проверка обновлений по кнопке. Идёт в отдельном потоке: запрос
    // к чужому серверу занимает секунды, и всё это время окно не разбирало
    // бы сообщения — интерфейс замер бы, а нажатия ушли в никуда.
    {
        let weak = main_window.as_weak();
        let found = pending_update.clone();
        main_window.on_check_update(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            win.set_update_busy(true);
            win.set_update_status(SharedString::from("Спрашиваю у GitHub…"));

            let back = weak.clone();
            let found = found.clone();
            std::thread::spawn(move || {
                let state = update::check();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = back.upgrade() {
                        show_update_state(&win, &found, state);
                    }
                });
            });
        });
    }

    // Установка обновления. Тоже в отдельном потоке: качается несколько
    // мегабайт.
    {
        let weak = main_window.as_weak();
        let found = pending_update.clone();
        main_window.on_install_update(move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let Some(release) = found.lock().expect("найденный выпуск").clone()
            else {
                return;
            };

            win.set_update_busy(true);
            win.set_update_status(SharedString::from("Скачиваю…"));

            let back = weak.clone();
            std::thread::spawn(move || {
                let (note, installed) = update::install(&release);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = back.upgrade() {
                        win.set_update_busy(false);
                        win.set_update_status(SharedString::from(note.clone()));
                        win.set_action_note(SharedString::from(note));
                        if installed {
                            // Полосу убираем: обновляться больше не на что,
                            // осталось перезапустить.
                            win.set_update_version(SharedString::from(""));
                        }
                    }
                });
            });
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
    // Переход с виджета к виновникам подвисания. Прочитать имена мало:
    // искать их потом руками в списке из трёхсот строк — работа, которую
    // человек делать не должен.
    {
        let main_for_culprits = main_window.as_weak();
        widget.on_show_culprits(move |names| {
            let Some(main) = main_for_culprits.upgrade() else {
                return;
            };
            main.set_filter(names.clone());
            main.set_section(1);
            main.show().ok();
            main.invoke_apply_filter();
            main.invoke_refresh();
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
        let refresh_games = known_games.clone();
        let refresh_rejections = rejections.clone();
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
                        &refresh_games,
                        &refresh_rejections.borrow(),
                    );
                }
                return;
            }

            let note = match section {
                2 => "Читаю накопители…",
                3 => "Читаю журнал пробуждений…",
                4 => "Открываю журнал действий…",
                7 => "Читаю профили браузеров…",
                8 => "Ищу установленные игры…",
                9 => "Читаю журнал загрузок…",
                10 => "Читаю автозагрузку…",
                _ => "",
            };
            match section {
                2 => win.set_disk_note(SharedString::from(note)),
                3 => win.set_power_note(SharedString::from(note)),
                4 => win.set_journal_note(SharedString::from(note)),
                7 => win.set_extensions_note(SharedString::from(note)),
                8 => win.set_targets_note(SharedString::from(note)),
                9 => win.set_boot_note(SharedString::from(note)),
                10 => win.set_startup_note(SharedString::from(note)),
                // Отчёт собирается по кнопке, а не при открытии раздела:
                // он читает журнал, и делать это на каждом переключении
                // впустую незачем.
                11 => return,
                _ => return,
            }

            // В поток уходит только слабая ссылка на окно: модели Slint
            // живут в потоке интерфейса и через границу потока не проходят.
            // Забираем их из самого окна, уже вернувшись в его поток.
            let back = weak.clone();
            // Клонируем на каждый заход: замыкание вызывается многократно,
            // а поток забирает своё владение навсегда.
            let refresh_games = refresh_games.clone();

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
                8 => {
                    // Библиотеки магазинов читаются с диска, поэтому в потоке
                    // и только при открытии раздела. Найденное складываем
                    // в общий список, а в строки его превратит ближайший тик:
                    // там есть снимок с запущенными программами.
                    let found = bamboo_sys::installed_games().unwrap_or_default();
                    let count = found.len();
                    if let Ok(mut known) = refresh_games.lock() {
                        *known = found;
                    }
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(win) = back.upgrade() {
                            win.set_targets_note(SharedString::from(format!(
                                "Игр найдено: {count}. Список ниже — обновляется сам."
                            )));
                        }
                    });
                }
                9 => {
                    let (rows, note) = mainwin::boot_rows();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(win) = back.upgrade() {
                            replace(
                                &win.get_boots(),
                                rows.into_iter()
                                    .map(|row| BootRow {
                                        when: SharedString::from(row.when),
                                        total: SharedString::from(row.total),
                                        phases: SharedString::from(row.phases),
                                        slower: SharedString::from(row.slower),
                                        degraded: row.degraded,
                                    })
                                    .collect(),
                            );
                            win.set_boot_note(SharedString::from(note));
                        }
                    });
                }
                10 => {
                    let (rows, note) = mainwin::startup_rows();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(win) = back.upgrade() {
                            replace(
                                &win.get_startup(),
                                rows.into_iter()
                                    .map(|row| StartupRow {
                                        name: SharedString::from(row.name),
                                        command: SharedString::from(row.command),
                                        enabled: row.enabled,
                                    })
                                    .collect(),
                            );
                            win.set_startup_note(SharedString::from(note));
                        }
                    });
                }
                7 => {
                    let (rows, note) = mainwin::extension_rows();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(win) = back.upgrade() {
                            replace(
                                &win.get_extensions(),
                                rows.into_iter()
                                    .map(|row| ExtensionRow {
                                        name: SharedString::from(row.name),
                                        browser: SharedString::from(row.browser),
                                    })
                                    .collect(),
                            );
                            win.set_extensions_note(SharedString::from(note));
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
    // Обновления: спрашиваем при запуске и потом раз в сутки. Первый
    // запрос уходит сразу — человеку, который только что поставил Bamboo,
    // ждать до завтра незачем.
    {
        let weak = main_window.as_weak();
        let found = pending_update.clone();
        std::thread::spawn(move || {
            let state = update::check();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    show_update_state(&win, &found, state);
                }
            });
        });
    }
    let mut checked_at = std::time::Instant::now();
    let update_weak = main_window.as_weak();
    let update_found = pending_update.clone();

    let tick_games = known_games.clone();
    let tick_rejections = rejections.clone();
    let tick_recording = recording.clone();
    let started = std::time::Instant::now();
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

            // Раз в сутки спрашиваем, не вышло ли новой версии. Чаще
            // незачем: выпуски выходят не по часам, а лишние запросы
            // к чужому серверу — это трафик человека.
            if checked_at.elapsed() >= Duration::from_millis(update::CHECK_EVERY_MS) {
                checked_at = std::time::Instant::now();
                let back = update_weak.clone();
                let found = update_found.clone();
                std::thread::spawn(move || {
                    let state = update::check();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(win) = back.upgrade() {
                            show_update_state(&win, &found, state);
                        }
                    });
                });
            }

            if let Some(snapshot) = latest {
                apply_snapshot(&widget, &snapshot, &processes, &spark, &spark_cpu);

                // Самозамер: сколько Bamboo стоит машине. Меряем каждым
                // тиком, судим — только когда наблюдения хватит.
                if let Ok(own) = bamboo_sys::budget::own_memory() {
                    let me = std::process::id();
                    let cpu = snapshot
                        .top
                        .iter()
                        .find(|line| line.pid == me)
                        .map(|line| f64::from(line.cpu_percent))
                        .unwrap_or(0.0);

                    selfwatch.observe(
                        own.working_set.as_u64(),
                        own.private_bytes.as_u64(),
                        cpu,
                        own_written().saturating_sub(written_at_start),
                        started.elapsed().as_millis() as u64,
                    );
                }

                // История: копим каждый тик, пишем раз в несколько часов.
                if let Some(history) = &mut history {
                    let now = started.elapsed().as_millis() as u64;
                    history.observe(&snapshot, now);
                    if history.due(now) {
                        if let Err(error) = history.flush(now) {
                            eprintln!("историю записать не удалось: {error}");
                        }
                    }

                    // Свой расход показываем наравне с чужим.
                    if let Some(main) = main_weak.upgrade() {
                        if main.window().is_visible() && main.get_section() == 5 {
                            fill_budget(&main, &selfwatch);
                            main.set_history_note(SharedString::from(format!(
                                "База наблюдений занимает {}. Запись идёт раз в                                  несколько часов пачкой, а не постоянно: Bamboo считает                                  чужой износ накопителя и не имеет права изнашивать его                                  сам. Ждёт записи программ: {}.",
                                history.size(),
                                history.pending_apps(),
                            )));
                        }
                    }
                }

                // Подвисание всплывает уведомлением. Не всегда: пока
                // человек играет или показывает презентацию, всплывать
                // поверх игры — ровно то поведение, за которое такие
                // программы и не любят.
                if let (Some(notifier), Some(freeze)) = (&notifier, &snapshot.freeze) {
                    if *freeze != last_notified && bamboo_sys::notification_state().may_notify() {
                        last_notified = freeze.clone();
                        // Текст обрезаем сами: в подсказку влезает
                        // немного, и обрывать её на полуслове нельзя.
                        let short = shorten(freeze, 240);
                        let _ = notifier.show(
                            "Bamboo: система подвисала",
                            &short,
                            bamboo_sys::Importance::Warning,
                        );
                    }
                }

                // Запись наблюдения кормится из того же снимка: отдельного
                // опроса системы ей не нужно, данные уже есть.
                if let Some(recording) = tick_recording.borrow_mut().as_mut() {
                    recording.observe(&snapshot, started.elapsed().as_millis() as u64);
                    if let Some(main) = main_weak.upgrade() {
                        if main.window().is_visible() {
                            show_recording(&main, recording);
                        }
                    }
                }

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
                            &tick_games,
                            &tick_rejections.borrow(),
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

/// Обрезает текст по границе слова.
///
/// В подсказку области уведомлений влезает немного, а обрыв на полуслове
/// читается как поломка. Режем по пробелу и ставим многоточие — тогда
/// видно, что текст продолжается, и где именно.
#[cfg(windows)]
fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }

    let cut: String = text.chars().take(limit).collect();
    let at = cut.rfind(' ').unwrap_or(cut.len());
    format!("{}…", cut[..at].trim_end_matches([',', '.', ' ', '—']))
}

/// Сколько Bamboo записал на диск за свою жизнь.
///
/// Счётчик самой Windows, а не наш подсчёт: считать самому значило бы
/// доверять своей же бухгалтерии в вопросе, ради которого замер и делается.
#[cfg(windows)]
fn own_written() -> u64 {
    bamboo_sys::budget::own_write_bytes().unwrap_or(0)
}

/// Показывает отчёт о собственном бюджете.
#[cfg(windows)]
fn fill_budget(main: &MainWindow, watch: &selfwatch::SelfWatch) {
    let rows: Vec<BudgetRow> = watch
        .report()
        .into_iter()
        .map(|line| BudgetRow {
            metric: SharedString::from(line.metric),
            measured: SharedString::from(line.measured),
            limit: SharedString::from(line.limit),
            // Три состояния, а не два: «уложились», «вышли» и «судить рано».
            // Последнее нельзя показывать как успех.
            state: match line.within {
                Some(true) => 1,
                Some(false) => 2,
                None => 0,
            },
        })
        .collect();

    replace(&main.get_budget(), rows);
    main.set_budget_verdict(SharedString::from(watch.verdict()));
}

/// Короткое имя средства для списка отказов.
///
/// Отказ запоминается по паре «программа + средство»: отказ придержать диск
/// браузеру не означает отказа от экономичного режима для него же.
#[cfg(windows)]
fn remedy_name(action: i32) -> &'static str {
    match action {
        0 => "эконом",
        1 => "память",
        2 => "диск",
        _ => "прочее",
    }
}

/// Наполняет список того, за чем можно понаблюдать.
///
/// Список берётся из последнего снимка, а игры — с диска. Снимка может
/// ещё не быть: раздел открывают сразу после запуска, а первый снимок
/// приходит через секунду. Тогда покажем одни игры, а программы появятся
/// со следующим тиком.
#[cfg(windows)]
fn fill_targets(main: &MainWindow, snapshot: &collector::Snapshot, games: &[bamboo_sys::Game]) {
    let (rows, note) = mainwin::target_rows(snapshot, games);
    replace(
        &main.get_targets(),
        rows.into_iter()
            .map(|row| TargetRow {
                label: SharedString::from(row.label),
                exe: SharedString::from(row.exe),
                source: SharedString::from(row.source),
            })
            .collect(),
    );
    main.set_targets_note(SharedString::from(note));
}

/// Рисует запись наблюдения: графики и разбор.
#[cfg(windows)]
fn show_recording(main: &MainWindow, recording: &session::Session) {
    use bamboo_analyze::record::to_chart;

    /// Сколько столбиков помещается в график, чтобы они оставались
    /// различимыми. Точек в записи бывают тысячи.
    const POINTS: usize = 120;

    let samples = recording.samples();
    let cpu: Vec<f64> = samples
        .iter()
        .map(|sample| f64::from(sample.cpu_percent))
        .collect();
    let memory: Vec<f64> = samples
        .iter()
        .map(|sample| sample.memory.as_u64() as f64)
        .collect();
    let disk: Vec<f64> = samples
        .iter()
        .map(|sample| sample.disk_per_second as f64)
        .collect();
    let gpu: Vec<f64> = samples
        .iter()
        .filter_map(|sample| sample.gpu_percent.map(f64::from))
        .collect();

    // Шкалу берём по своему же пику, а не по ста процентам: программа,
    // которая держится на пяти процентах, на шкале до ста выглядела бы
    // ровной чертой, и просадка в ней потерялась бы.
    let cpu_top = cpu.iter().copied().fold(1.0_f64, f64::max);
    let memory_top = memory.iter().copied().fold(1.0_f64, f64::max);
    let disk_top = disk.iter().copied().fold(1.0_f64, f64::max);

    replace_floats(&main.get_record_cpu(), to_chart(&cpu, POINTS, cpu_top));
    replace_floats(
        &main.get_record_memory(),
        to_chart(&memory, POINTS, memory_top),
    );
    replace_floats(&main.get_record_disk(), to_chart(&disk, POINTS, disk_top));
    replace_floats(&main.get_record_gpu(), to_chart(&gpu, POINTS, 100.0));
    main.set_record_has_gpu(!gpu.is_empty());

    main.set_record_scale_cpu(SharedString::from(format!("пик {cpu_top:.0}%")));
    main.set_record_scale_memory(SharedString::from(format!(
        "пик {}",
        bamboo_core::Bytes(memory_top as u64)
    )));
    main.set_record_scale_disk(SharedString::from(format!(
        "пик {}/с",
        bamboo_core::Bytes(disk_top as u64)
    )));

    main.set_record_status(SharedString::from(format!(
        "Записываю {} — {} наблюдения, {} замеров.",
        recording.app(),
        recording.spell_length(),
        recording.len(),
    )));

    match bamboo_analyze::analyse_recording(samples) {
        Some(verdict) => main.set_record_verdict(SharedString::from(format!(
            "{}: {}",
            verdict.bottleneck.name(),
            verdict.summary
        ))),
        // Меньше десяти секунд — вывода ещё нет, и выдумывать его нельзя.
        None => main.set_record_verdict(SharedString::from(
            "Наблюдений пока мало. Вывод появится, когда наберётся десять             секунд: на меньшем отрезке он был бы гаданием.",
        )),
    }
}

/// Заменяет содержимое списка чисел в модели окна.
#[cfg(windows)]
fn replace_floats(model: &ModelRc<f32>, values: Vec<f32>) {
    if let Some(list) = model.as_any().downcast_ref::<VecModel<f32>>() {
        list.set_vec(values);
    }
}

/// Показывает в окне то, что вернула проверка обновлений.
#[cfg(windows)]
fn show_update_state(
    main: &MainWindow,
    found: &std::sync::Mutex<Option<update::Release>>,
    state: update::UpdateState,
) {
    main.set_update_busy(false);
    main.set_update_status(SharedString::from(state.note));

    match state.available {
        Some(release) => {
            main.set_update_version(SharedString::from(release.version.clone()));
            // Описание выпуска бывает длинным, а полоса должна оставаться
            // полосой: показываем начало, остальное есть на GitHub.
            let notes: String = release.notes.lines().take(3).collect::<Vec<_>>().join(" ");
            main.set_update_notes(SharedString::from(notes));
            *found.lock().expect("список обновлений") = Some(release);
        }
        None => {
            main.set_update_version(SharedString::from(""));
            *found.lock().expect("список обновлений") = None;
        }
    }
}

#[cfg(windows)]
fn apply_overview(
    main: &MainWindow,
    snapshot: &collector::Snapshot,
    processes: &ModelRc<MainProcessRow>,
    limits: &actions::IoLimits,
    autopilot: &std::cell::RefCell<autopilot::Autopilot>,
    known_games: &std::sync::Mutex<Vec<bamboo_sys::Game>>,
    rejections: &bamboo_policy::Rejections,
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
    // То, от чего человек отказался дважды, из списка убираем совсем.
    let suggestions: Vec<mainwin::SuggestionRow> = suggestions
        .into_iter()
        .filter(|row| {
            let name = snapshot
                .top
                .iter()
                .find(|line| line.pid.to_string() == row.pid)
                .map(|line| line.name.as_str())
                .unwrap_or_default();
            !rejections.is_silenced(name, remedy_name(row.action))
        })
        .collect();
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
    main.set_freeze_culprits(SharedString::from(snapshot.freeze_culprits.clone()));

    // Список целей для анализа: собирается из снимка, поэтому запущенные
    // программы в нём появляются и исчезают сами.
    if main.get_section() == 8 {
        if let Ok(games) = known_games.lock() {
            fill_targets(main, snapshot, &games);
        }
    }

    // Кто занимает диск. Считается из того же снимка каждым тиком: список
    // живой, а не снятый в момент открытия раздела.
    let (users, users_note) = mainwin::disk_user_rows(snapshot);
    let users: Vec<DiskUserRow> = users
        .into_iter()
        .map(|row| DiskUserRow {
            name: SharedString::from(row.name),
            rate: SharedString::from(row.rate),
            share: row.share,
        })
        .collect();
    replace(&main.get_disk_users(), users);
    main.set_disk_users_note(SharedString::from(users_note));

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
    // Подвисание на виджете, а не только в окне: окна в этот момент
    // на экране нет, а сказать надо тогда же, когда случилось.
    widget.set_freeze(SharedString::from(
        snapshot.freeze.clone().unwrap_or_default(),
    ));
    widget.set_freeze_culprits(SharedString::from(snapshot.freeze_culprits.clone()));

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
