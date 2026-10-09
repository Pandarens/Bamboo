//! Автозагрузка пользователя (ТЗ, разделы 5.6 и 11.1, уровень 2).
//!
//! Пока только куст текущего пользователя: HKCU доступен агенту без прав
//! администратора. Машинная автозагрузка в HKLM появится вместе с брокером.
//!
//! Отключение — через `StartupApproved`, как это делает диспетчер задач:
//! сама запись в `Run` не удаляется, а помечается выключенной. Это и есть
//! обратимость: включить обратно — значит поменять один байт, а не
//! восстанавливать удалённую команду по памяти.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_SZ,
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const APPROVED_KEY: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";

/// Первый байт блоба StartupApproved: чётный — включено, нечётный — выключено.
/// Диспетчер задач пишет 0x02 и 0x03 соответственно.
const ENABLED_MARKER: u8 = 0x02;
const DISABLED_MARKER: u8 = 0x03;

/// Элемент автозагрузки.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupItem {
    pub name: String,
    pub command: String,
    pub enabled: bool,
}

struct Key(HKEY);

impl Key {
    /// Открывает ключ, создавая его при отсутствии.
    ///
    /// Нужен для записи в `StartupApproved`: у человека, который ни разу
    /// не выключал автозагрузку — ни диспетчером задач, ни чем-то ещё, —
    /// этого раздела попросту нет, и открытие без создания падает
    /// с «кодом 2». Вскрылось на свежем раннере CI, но касается и живых
    /// машин: кнопка «Выключить» в разделе «Автозапуск» падала бы так же.
    fn open_or_create(path: &str, access: u32) -> Result<Key> {
        let wide: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
        let mut handle: HKEY = core::ptr::null_mut();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide.as_ptr(),
                0,
                core::ptr::null(),
                0,
                access,
                core::ptr::null(),
                &mut handle,
                core::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(Error::Win32 {
                call: "RegCreateKeyExW",
                code: status,
            });
        }
        Ok(Key(handle))
    }

    fn open(path: &str, access: u32) -> Result<Key> {
        let wide: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
        let mut handle: HKEY = core::ptr::null_mut();
        let status =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide.as_ptr(), 0, access, &mut handle) };
        if status != ERROR_SUCCESS {
            return Err(Error::Win32 {
                call: "RegOpenKeyExW",
                code: status,
            });
        }
        Ok(Key(handle))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

/// Перечисляет автозагрузку текущего пользователя.
pub fn user_startup_items() -> Result<Vec<StartupItem>> {
    // Раздела может не быть вовсе — у свежего профиля, где ни одна программа
    // ещё не прописалась в автозапуск. Это пустой список, а не ошибка.
    // Вскрылось на чистых машинах CI: тесты автозапуска падали через раз,
    // смотря на какую машину попадёт прогон, — на одних раздел был,
    // на других нет.
    let run = match Key::open(RUN_KEY, KEY_READ) {
        Ok(run) => run,
        Err(Error::Win32 {
            code: ERROR_FILE_NOT_FOUND,
            ..
        }) => return Ok(Vec::new()),
        Err(other) => return Err(other),
    };
    let mut items = Vec::new();

    for index in 0.. {
        let mut name = [0u16; 512];
        let mut name_len = name.len() as u32;
        let mut data = [0u8; 4096];
        let mut data_len = data.len() as u32;

        let status = unsafe {
            RegEnumValueW(
                run.0,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                data.as_mut_ptr(),
                &mut data_len,
            )
        };
        if status != ERROR_SUCCESS {
            break; // ERROR_NO_MORE_ITEMS или что-то серьёзное — список кончился.
        }

        let name = String::from_utf16_lossy(&name[..name_len as usize]);
        // Команда лежит как REG_SZ: UTF-16 с нулём на конце.
        let command: Vec<u16> = data[..data_len as usize]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect();

        items.push(StartupItem {
            enabled: is_enabled(&name)?,
            name,
            command: String::from_utf16_lossy(&command),
        });
    }

    Ok(items)
}

/// Включён ли элемент по данным StartupApproved.
///
/// Отсутствие записи означает «включён»: система создаёт блоб только
/// после первого отключения.
fn is_enabled(name: &str) -> Result<bool> {
    let approved = match Key::open(APPROVED_KEY, KEY_READ) {
        Ok(key) => key,
        // Куста может не быть вовсе — тогда всё включено.
        Err(_) => return Ok(true),
    };

    let wide: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();
    let mut data = [0u8; 32];
    let mut len = data.len() as u32;

    let status = unsafe {
        RegQueryValueExW(
            approved.0,
            wide.as_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            data.as_mut_ptr(),
            &mut len,
        )
    };

    if status != ERROR_SUCCESS || len == 0 {
        return Ok(true);
    }
    Ok(data[0] % 2 == 0)
}

/// Включает или выключает элемент автозагрузки текущего пользователя.
///
/// Возвращает прежнее состояние — оно уходит в журнал как состояние «до».
pub fn set_startup_enabled(name: &str, enabled: bool) -> Result<bool> {
    let before = is_enabled(name)?;

    // Создаём раздел, если его нет: у человека, ни разу не выключавшего
    // автозагрузку, StartupApproved не существует.
    let approved = Key::open_or_create(APPROVED_KEY, KEY_SET_VALUE)?;
    let wide: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();

    // 12 байт, как пишет диспетчер задач: маркер и время отключения.
    // Время оставляем нулевым — оно информационное.
    let mut blob = [0u8; 12];
    blob[0] = if enabled {
        ENABLED_MARKER
    } else {
        DISABLED_MARKER
    };

    let status = unsafe {
        RegSetValueExW(
            approved.0,
            wide.as_ptr(),
            0,
            REG_BINARY,
            blob.as_ptr(),
            blob.len() as u32,
        )
    };

    if status != ERROR_SUCCESS {
        return Err(Error::Win32 {
            call: "RegSetValueExW(StartupApproved)",
            code: status,
        });
    }
    Ok(before)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_startup_list_is_readable_without_elevation() {
        // Список может быть и пустым — важно, что чтение не падает
        // и каждый элемент имеет имя.
        let items = user_startup_items().expect("автозагрузка не прочиталась");
        for item in &items {
            assert!(!item.name.is_empty());
        }
    }

    #[test]
    fn an_absent_approved_entry_means_enabled() {
        // Записи с таким именем заведомо нет.
        assert!(is_enabled("bamboo-теста-такого-нет").unwrap());
    }

    #[test]
    fn toggling_round_trips_and_restores() {
        // Работаем только с собственной, специально созданной записью:
        // чужие элементы автозагрузки тестам трогать нельзя.
        let name = "bamboo-self-test";

        let before = set_startup_enabled(name, false).unwrap();
        assert!(!is_enabled(name).unwrap(), "элемент не выключился");

        set_startup_enabled(name, true).unwrap();
        assert!(is_enabled(name).unwrap(), "элемент не включился обратно");

        // Возвращаем как было (для несуществующей записи before = true).
        set_startup_enabled(name, before).unwrap();
    }
}

/// Имя, под которым Bamboo прописывается в автозапуск.
pub const STARTUP_NAME: &str = "Bamboo";

/// Строка в кодировке Windows с завершающим нулём.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Стоит ли Bamboo в автозапуске.
pub fn is_in_startup() -> bool {
    startup_command(STARTUP_NAME).is_some()
}

/// Что записано в автозапуске под этим именем.
///
/// Нужно не только для проверки. Тест, который правит настоящий автозапуск,
/// обязан вернуть **прежнее значение**, а не просто «запись была». Разница
/// не теоретическая: прошлая редакция теста помнила один лишь признак
/// и восстанавливала запись вызовом `add_to_startup()` — то есть подставляла
/// путь к самому тестовому бинарнику. Настоящая запись пользователя
/// затиралась, а тестовый файл при следующей сборке исчезал, и автозапуск
/// переставал работать молча.
pub fn startup_command(name: &str) -> Option<String> {
    let run = Key::open(RUN_KEY, KEY_READ).ok()?;
    read_value(&run, name)
}

/// Добавляет Bamboo в автозапуск текущего пользователя.
///
/// Пишем в раздел пользователя, а не машины: для машинного нужны права
/// администратора, а наблюдатель за системой должен ставиться без них.
/// Путь берём собственный — тот, откуда программа запущена сейчас.
pub fn add_to_startup() -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|_| Error::Unsupported("не удалось определить путь к себе"))?;
    // Путь в кавычках: без них пробел в пути превратит команду в две.
    let command = format!("\"{}\"", exe.to_string_lossy());
    set_startup_command(STARTUP_NAME, &command)
}

/// Записывает произвольную команду в автозапуск под данным именем.
///
/// Отдельно от `add_to_startup`, чтобы тесты могли пользоваться **своим**
/// именем и не трогать запись пользователя вовсе. Это не удобство,
/// а исправление настоящей поломки: тесты, писавшие в боевое имя, ломали
/// автозапуск на машине разработчика.
pub fn set_startup_command(name: &str, command: &str) -> Result<()> {
    // Создать, если нет: на свежем профиле раздела автозапуска ещё нет,
    // и без этого Bamboo не смог бы прописать себя в автозапуск вовсе.
    let run = Key::open_or_create(RUN_KEY, KEY_SET_VALUE)?;
    let name = wide(name);
    let value = wide(command);

    let status = unsafe {
        RegSetValueExW(
            run.0,
            name.as_ptr(),
            0,
            REG_SZ,
            value.as_ptr().cast(),
            (value.len() * 2) as u32,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(Error::Win32 {
            call: "RegSetValueExW(автозапуск)",
            code: status,
        });
    }
    Ok(())
}

/// Убирает Bamboo из автозапуска.
pub fn remove_from_startup() -> Result<()> {
    remove_startup_command(STARTUP_NAME)
}

/// Убирает запись автозапуска по имени.
pub fn remove_startup_command(name: &str) -> Result<()> {
    use windows_sys::Win32::System::Registry::RegDeleteValueW;

    let run = match Key::open(RUN_KEY, KEY_SET_VALUE) {
        Ok(run) => run,
        // Нет раздела — нет и записи: удалять нечего, цель достигнута.
        Err(Error::Win32 {
            code: ERROR_FILE_NOT_FOUND,
            ..
        }) => return Ok(()),
        Err(other) => return Err(other),
    };
    let name = wide(name);

    let status = unsafe { RegDeleteValueW(run.0, name.as_ptr()) };
    // Значения не было — цель достигнута, это не ошибка.
    if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
        return Err(Error::Win32 {
            call: "RegDeleteValueW(автозапуск)",
            code: status,
        });
    }
    Ok(())
}

/// Читает строковое значение из ключа.
fn read_value(key: &Key, name: &str) -> Option<String> {
    let wide_name = wide(name);
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;

    let status = unsafe {
        RegQueryValueExW(
            key.0,
            wide_name.as_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS || size < 2 {
        return None;
    }
    let chars = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buffer[..chars]))
}

#[cfg(test)]
mod autostart_tests {
    use super::*;
    use std::sync::Mutex;

    /// Оба теста правят один и тот же ключ реестра. Параллельно они
    /// гонялись бы за него и мешали друг другу, поэтому сериализуем.
    static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

    /// Имя, под которым пишут тесты.
    ///
    /// Своё, а не боевое, и это исправление настоящей поломки. Прежняя
    /// редакция писала в боевое имя и «восстанавливала» запись вызовом
    /// `add_to_startup()` — то есть подставляла путь к тестовому бинарнику
    /// вместо пути пользователя. Тестовый файл при следующей сборке
    /// исчезал, и автозапуск переставал работать молча: запись есть,
    /// а запускать нечего.
    const TEST_NAME: &str = "Bamboo-проверка";

    #[test]
    fn adding_and_removing_autostart_works() {
        let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        set_startup_command(TEST_NAME, "\"C:\\нет\\такого.exe\"").expect("запись");
        assert!(startup_command(TEST_NAME).is_some(), "запись должна быть");

        remove_startup_command(TEST_NAME).expect("удаление");
        assert!(
            startup_command(TEST_NAME).is_none(),
            "записи быть не должно"
        );
    }

    #[test]
    fn removing_what_is_not_there_is_not_an_error() {
        let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        remove_startup_command(TEST_NAME).expect("первое удаление");
        // Повторное удаление — цель уже достигнута.
        remove_startup_command(TEST_NAME).expect("повторное удаление не должно падать");
    }

    #[test]
    fn tests_never_touch_the_real_autostart_entry() {
        // Сторож от той самой поломки. Настоящая запись пользователя
        // после прогона тестов обязана остаться нетронутой — и по факту
        // наличия, и по содержимому.
        let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = startup_command(STARTUP_NAME);

        set_startup_command(TEST_NAME, "\"C:\\нет\\такого.exe\"").ok();
        remove_startup_command(TEST_NAME).ok();

        assert_eq!(
            startup_command(STARTUP_NAME),
            before,
            "тест изменил настоящую запись автозапуска"
        );
    }

    #[test]
    fn a_real_entry_would_point_at_an_existing_file() {
        // Запись, указывающая на несуществующий файл, — это молчаливо
        // сломанный автозапуск: Windows ничего не запустит и не скажет.
        // Ровно так и выглядела поломка, оставленная прежними тестами.
        let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some(command) = startup_command(STARTUP_NAME) else {
            return; // Автозапуск не настроен — проверять нечего.
        };

        let path = command.trim_matches('"');
        assert!(
            std::path::Path::new(path).exists(),
            "автозапуск указывает на несуществующий файл: {path}"
        );

        // И на нужный файл. Существующий, но чужой — та же поломка:
        // при входе запустится не то, а человек этого не увидит.
        let file = std::path::Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        assert!(
            file.starts_with("bamboo-agent"),
            "автозапуск указывает не на Bamboo, а на {file}"
        );
    }
}

/// Запущен ли Bamboo с правами администратора.
///
/// Проверять надо, потому что без них он молча делает меньше: не читает
/// журнал загрузок и здоровье накопителя, не поднимает сессии трассировки,
/// не трогает чужие процессы, запущенные от администратора. Программа,
/// которая в таком состоянии показывает пустые разделы и не объясняет
/// почему, выглядит сломанной — а она просто не может.
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = core::ptr::null_mut();
    // TOKEN_QUERY
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), 0x0008, &mut token) };
    if ok == 0 {
        return false;
    }

    let mut elevation: TOKEN_ELEVATION = unsafe { core::mem::zeroed() };
    let mut returned: u32 = 0;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            core::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    unsafe { CloseHandle(token) };

    ok != 0 && elevation.TokenIsElevated != 0
}

/// Что теряется без прав администратора.
///
/// Список конкретный, а не «часть возможностей недоступна»: человек должен
/// понимать, чего именно он не увидит, и решать, стоит ли это повышения.
pub fn what_needs_elevation() -> &'static str {
    bamboo_core::pick(
        "Bamboo запущен без прав администратора. Так он всё равно наблюдает \
         за системой, но не сможет показать историю загрузок, здоровье \
         накопителя и имена задач планировщика, а действия над процессами, \
         запущенными от администратора, будут отклонены. Чтобы получить всё, \
         запускайте его от администратора.",
        "Bamboo is running without administrator rights. It still watches the \
         system, but it cannot show the boot history, drive health or scheduled \
         task names, and actions on processes started as administrator will be \
         refused. To get everything, run it as administrator.",
    )
}

#[cfg(test)]
mod elevation_tests {
    use super::*;

    #[test]
    fn the_elevation_state_is_answered_without_error() {
        // Ответ зависит от того, как запущен тест, — важно, что он есть.
        let _ = is_elevated();
    }

    #[test]
    fn the_explanation_names_what_is_lost() {
        // «Часть возможностей недоступна» — не объяснение. Человек должен
        // понимать, чего именно не увидит.
        let text = what_needs_elevation();
        for word in ["истори", "накопител", "планировщик"] {
            assert!(text.contains(word), "не назван потерянный раздел: {word}");
        }
        // И что с этим делать.
        assert!(text.contains("от администратора"), "{text}");
    }

    #[test]
    fn the_explanation_speaks_both_languages() {
        use bamboo_core::{set_language, Language};
        set_language(Language::English);
        let english = what_needs_elevation();
        set_language(Language::Russian);

        assert!(english.contains("administrator"), "{english}");
        assert!(
            !english
                .chars()
                .any(|c| ('\u{0410}'..='\u{044f}').contains(&c)),
            "в английском тексте осталась кириллица"
        );
    }
}

/// Имя задачи планировщика, запускающей Bamboo при входе.
const TASK_NAME: &str = "Bamboo";

/// Стоит ли Bamboo в автозапуске через задачу планировщика.
pub fn is_scheduled_at_logon() -> bool {
    std::process::Command::new("schtasks.exe")
        .args(["/query", "/tn", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Окно консоли не показывать: иначе при каждом обращении мигало бы
/// чёрное окно.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

use std::os::windows::process::CommandExt;

/// Заводит автозапуск при входе с правами администратора.
///
/// Обычная запись в разделе `Run` этого не умеет: программа, требующая
/// повышения, оттуда просто не стартует, а не требующая — стартует
/// без прав. Задача планировщика с наивысшими правами решает обе беды
/// разом и не спрашивает подтверждения при каждом входе.
///
/// Требует прав администратора **один раз**, при создании.
///
/// Задача описывается целиком, а не ключами schtasks: у тех нет ни
/// повтора, ни снятия лимитов, а умолчания оказались вредными. На живой
/// машине Bamboo тихо исчез на двое суток — его убили вместе с чужим
/// процессом, а поднять было некому: задача срабатывала только при входе,
/// а компьютер не перезагружали три недели. Да и сама задача по умолчанию
/// останавливала Bamboo через 72 часа и при переходе на батарею.
pub fn schedule_at_logon() -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|_| Error::Unsupported("не удалось определить путь к себе"))?;
    let user = current_user();
    let xml = task_xml(&exe.to_string_lossy(), &user);

    // Описание задачи — во временный файл в UTF-16: так его ждёт schtasks.
    let path = std::env::temp_dir().join(format!("bamboo-task-{}.xml", std::process::id()));
    let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(&path, bytes)
        .map_err(|_| Error::Unsupported("не удалось записать описание задачи"))?;

    let output = std::process::Command::new("schtasks.exe")
        .args(["/create", "/tn", TASK_NAME, "/xml"])
        .arg(&path)
        // Перезаписать существующую: повторное включение не должно
        // спотыкаться о прошлую задачу.
        .arg("/f")
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = std::fs::remove_file(&path);
    let output =
        output.map_err(|_| Error::Unsupported("не удалось вызвать планировщик заданий"))?;

    if !output.status.success() {
        return Err(Error::Unsupported(
            "создать задачу не удалось: нужны права администратора",
        ));
    }
    Ok(())
}

/// Как часто задача проверяет, что Bamboo работает. Второй экземпляр
/// уходит сразу же, увидев первый, — проверка стоит доли секунды.
const KEEPER_INTERVAL: &str = "PT15M";

/// Пользователь в виде «КОМПЬЮТЕР\имя» — так его ждёт планировщик.
fn current_user() -> String {
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    let name = std::env::var("USERNAME").unwrap_or_default();
    if domain.is_empty() {
        name
    } else {
        format!("{domain}\\{name}")
    }
}

/// Экранирование для XML: путь к программе может содержать «&».
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Описание задачи автозапуска.
///
/// - при входе — как и прежде;
/// - каждые пятнадцать минут — сторож: умер Bamboo, задача поднимет его;
/// - без предела времени работы (по умолчанию — 72 часа);
/// - без остановки на батарее: наблюдатель, который уходит, когда ноутбук
///   отключили от сети, бесполезен ровно тогда, когда нужнее всего.
pub fn task_xml(exe: &str, user: &str) -> String {
    let exe = xml_escape(exe);
    let user = xml_escape(user);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Bamboo: запуск при входе и проверка раз в 15 минут, что наблюдатель работает.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
    <TimeTrigger>
      <Repetition>
        <Interval>{KEEPER_INTERVAL}</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <StartBoundary>2026-01-01T00:00:00</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// Устарела ли задача: нет сторожа, стоит предел в 72 часа, остановка
/// на батарее или путь не к этой программе.
pub fn task_is_outdated(xml: &str, exe: &str) -> bool {
    !xml.contains(KEEPER_INTERVAL)
        || !xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>")
        || xml.contains("<StopIfGoingOnBatteries>true</StopIfGoingOnBatteries>")
        || !xml.to_lowercase().contains(&xml_escape(exe).to_lowercase())
}

/// Обновляет задачу автозапуска, если она есть и устарела. Возвращает,
/// обновлена ли.
///
/// Задачу, заведённую прежней версией, человек сам пересоздавать не станет —
/// он не знает, что она устарела. Нужны права администратора; без них
/// оставляем как есть.
pub fn refresh_scheduled_task() -> bool {
    let Ok(output) = std::process::Command::new("schtasks.exe")
        .args(["/query", "/tn", TASK_NAME, "/xml"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false; // задачи нет — автозапуск не включали, не навязываем
    }
    let xml = decode_utf16_or_utf8(&output.stdout);
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    task_is_outdated(&xml, &exe.to_string_lossy()) && schedule_at_logon().is_ok()
}

/// Вывод schtasks: в XML-режиме он бывает и в UTF-16, и в кодировке консоли.
fn decode_utf16_or_utf8(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && (bytes[0] == 0xFF && bytes[1] == 0xFE || bytes.get(1) == Some(&0)) {
        let start = if bytes[0] == 0xFF { 2 } else { 0 };
        let units: Vec<u16> = bytes[start..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Убирает задачу автозапуска.
pub fn unschedule_at_logon() -> Result<()> {
    let output = std::process::Command::new("schtasks.exe")
        .args(["/delete", "/tn", TASK_NAME, "/f"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|_| Error::Unsupported("не удалось вызвать планировщик заданий"))?;

    // Задачи не было — цель достигнута.
    if !output.status.success() && is_scheduled_at_logon() {
        return Err(Error::Unsupported(
            "удалить задачу не удалось: нужны права администратора",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod scheduled_startup_tests {
    use super::*;

    /// Принимает ли планировщик Windows такое описание. Заводит пробную
    /// задачу под другим именем и без повышенных прав и сразу удаляет.
    /// Запуск: cargo test -p bamboo-sys probe_task_xml -- --ignored
    #[test]
    #[ignore]
    fn probe_task_xml_is_accepted_by_windows() {
        let xml = task_xml(
            r"C:\Windows\System32
otepad.exe",
            &current_user(),
        )
        .replace("HighestAvailable", "LeastPrivilege");
        let path = std::env::temp_dir().join("bamboo-task-probe.xml");
        let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
        for unit in xml.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&path, bytes).unwrap();
        let created = std::process::Command::new("schtasks.exe")
            .args(["/create", "/tn", "BambooXmlProbe", "/xml"])
            .arg(&path)
            .arg("/f")
            .output()
            .unwrap();
        let _ = std::process::Command::new("schtasks.exe")
            .args(["/delete", "/tn", "BambooXmlProbe", "/f"])
            .output();
        let _ = std::fs::remove_file(&path);
        assert!(
            created.status.success(),
            "планировщик не принял: {}",
            String::from_utf8_lossy(&created.stderr)
        );
    }

    #[test]
    fn the_task_keeps_bamboo_alive() {
        let xml = task_xml(r"C:\Tools & Co\bamboo-agent.exe", r"PC\user");
        assert!(xml.contains("<Interval>PT15M</Interval>"), "нет сторожа");
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>"));
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains(r"C:\Tools &amp; Co\bamboo-agent.exe"), "{xml}");
        assert!(!task_is_outdated(&xml, r"C:\Tools & Co\bamboo-agent.exe"));
    }

    #[test]
    fn the_old_task_is_recognised_as_outdated() {
        // Та, что заводила прежняя версия: только вход, 72 часа, батарея.
        let old = r#"<Triggers><LogonTrigger></LogonTrigger></Triggers>
<Settings><StopIfGoingOnBatteries>true</StopIfGoingOnBatteries>
<ExecutionTimeLimit>PT72H</ExecutionTimeLimit></Settings>
<Command>"C:\b\bamboo-agent.exe"</Command>"#;
        assert!(task_is_outdated(old, r"C:\b\bamboo-agent.exe"));
        // Новая задача, но для другого пути — тоже устарела.
        let moved = task_xml(r"C:\old\bamboo-agent.exe", "PC\\user");
        assert!(task_is_outdated(&moved, r"C:\new\bamboo-agent.exe"));
    }

    #[test]
    fn the_state_of_the_task_is_answerable() {
        // Запрос к планировщику не должен падать оттого, что задачи нет.
        let _ = is_scheduled_at_logon();
    }

    #[test]
    fn removing_a_task_that_is_not_there_is_not_an_error() {
        // Только если её и правда нет: удалять чужую настройку
        // ради теста нельзя.
        if !is_scheduled_at_logon() {
            unschedule_at_logon().expect("удаление отсутствующей задачи");
        }
    }

    #[test]
    fn scheduling_needs_rights_and_says_so() {
        // Без прав создание обязано отказать с объяснением, а не сделать
        // вид, что получилось.
        if is_elevated() || is_scheduled_at_logon() {
            return; // С правами проверять нечего, чужую задачу не трогаем.
        }
        let error = schedule_at_logon().expect_err("без прав задача не создаётся");
        assert!(
            error.to_string().contains("администратора"),
            "отказ обязан назвать причину: {error}"
        );
    }
}
