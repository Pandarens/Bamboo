//! Перепись драйверов и задач планировщика (ТЗ, раздел 5.4).
//!
//! Нужна для системного диффа: чтобы сказать «после установки этой
//! программы у вас появилось три драйвера и семь задач», надо уметь
//! перечислить и то, и другое.
//!
//! Оба списка берутся обходным путём, и оба раза — потому что прямой
//! не работает.
//!
//! Драйверы. `EnumDeviceDrivers` под обычным пользователем возвращает
//! адреса, но не резолвит имена — проверено. Зато список драйверов целиком
//! лежит в реестре, в той же ветке, что и службы: драйвер в Windows —
//! это служба с типом 1 или 2. Читается без всяких прав.
//!
//! Задачи. Через COM к `ITaskService` их можно перечислить, но COM тянет
//! за собой инициализацию, апартаменты и целый пласт кода ради одного
//! списка имён. А сами задачи лежат файлами в `System32\Tasks`, по файлу
//! на задачу, и имя файла — это имя задачи. Читать их можно только
//! с правами администратора, и это честное ограничение: без прав список
//! будет пуст, о чём и надо сказать, а не выдавать пустоту за «задач нет».

use bamboo_core::{Error, Result};

/// Драйвер в системе.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Driver {
    /// Имя службы драйвера — оно же ключ в реестре.
    pub name: String,
    /// Загружается ли при старте системы.
    pub boot_start: bool,
}

/// Тип службы: драйвер уровня ядра.
const SERVICE_KERNEL_DRIVER: u32 = 1;
/// Тип службы: драйвер файловой системы.
const SERVICE_FILE_SYSTEM_DRIVER: u32 = 2;
/// Тип запуска: загрузка вместе с ядром.
const START_BOOT: u32 = 0;

/// Перечисляет драйверы.
///
/// Читаем реестр, а не спрашиваем `EnumDeviceDrivers`: та возвращает
/// адреса загрузки и под обычным пользователем не даёт имён.
pub fn drivers() -> Result<Vec<Driver>> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    };

    let path: Vec<u16> = r"SYSTEM\CurrentControlSet\Services"
        .encode_utf16()
        .chain(core::iter::once(0))
        .collect();

    let mut root: HKEY = core::ptr::null_mut();
    let status =
        unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, path.as_ptr(), 0, KEY_READ, &mut root) };
    if status != 0 {
        return Err(Error::Win32 {
            call: "RegOpenKeyExW(службы)",
            code: status,
        });
    }

    let mut found = Vec::new();
    let mut index = 0u32;
    loop {
        // Имена ключей коротки; предел защищает от бесконечного обхода,
        // если перечисление вдруг перестанет продвигаться.
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;

        let status = unsafe {
            RegEnumKeyExW(
                root,
                index,
                name.as_mut_ptr(),
                &mut length,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            )
        };
        if status != 0 {
            break;
        }
        index += 1;

        let key = String::from_utf16_lossy(&name[..length as usize]);
        let Some(kind) = crate::settings::registry_u32("HKLM", &subkey(&key), "Type") else {
            continue;
        };
        if kind != SERVICE_KERNEL_DRIVER && kind != SERVICE_FILE_SYSTEM_DRIVER {
            continue;
        }

        found.push(Driver {
            boot_start: crate::settings::registry_u32("HKLM", &subkey(&key), "Start")
                == Some(START_BOOT),
            name: key,
        });
    }
    unsafe { RegCloseKey(root) };

    found.sort_by_key(|driver| driver.name.to_lowercase());
    Ok(found)
}

fn subkey(name: &str) -> String {
    format!(r"SYSTEM\CurrentControlSet\Services\{name}")
}

/// Задача планировщика.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledTask {
    /// Полный путь задачи, как его показывает планировщик:
    /// `\Microsoft\Windows\UpdateOrchestrator\Reboot`.
    pub path: String,
}

/// Перечисляет задачи планировщика.
///
/// Требует прав администратора: папка задач закрыта для чтения обычному
/// пользователю. Без прав возвращается ошибка, а **не пустой список** —
/// пустой список неотличим от «задач нет», и выдавать одно за другое
/// нельзя (ТЗ 5.7).
pub fn scheduled_tasks() -> Result<Vec<ScheduledTask>> {
    let root = std::path::Path::new(r"C:\Windows\System32\Tasks");

    // Пробуем открыть саму папку: отказ по правам должен стать ошибкой
    // здесь, а не превратиться в пустой обход ниже.
    std::fs::read_dir(root).map_err(|_| {
        Error::Unsupported(
            "папка задач планировщика закрыта: перечислить задачи можно только              с правами администратора",
        )
    })?;

    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort_by_key(|task| task.path.to_lowercase());
    Ok(found)
}

/// Обходит дерево задач. Папки внутри — это разделы планировщика.
fn walk(root: &std::path::Path, folder: &std::path::Path, found: &mut Vec<ScheduledTask>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, found);
            continue;
        }
        // Имя файла и есть имя задачи, а путь от корня — её раздел.
        if let Ok(relative) = path.strip_prefix(root) {
            let name = relative.to_string_lossy().replace('/', "\\");
            if !name.is_empty() {
                found.push(ScheduledTask {
                    path: format!("\\{name}"),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drivers_are_listed_without_administrator_rights() {
        // Ради этого и выбран реестр вместо EnumDeviceDrivers: та под
        // обычным пользователем имён не даёт.
        let list = drivers().expect("список драйверов");
        assert!(
            list.len() > 50,
            "драйверов подозрительно мало: {}",
            list.len()
        );
        for driver in &list {
            assert!(!driver.name.trim().is_empty());
        }
    }

    #[test]
    fn some_drivers_load_with_the_kernel() {
        // Загружаемые вместе с ядром есть на любой машине: без них
        // не поднимется диск и файловая система.
        let list = drivers().unwrap_or_default();
        assert!(list.iter().any(|driver| driver.boot_start));
    }

    #[test]
    fn the_driver_list_is_stable_between_calls() {
        // Дифф сравнивает списки: порядок, зависящий от вызова, дал бы
        // ложные «появилось» и «исчезло» на ровном месте.
        let one = drivers().unwrap_or_default();
        let other = drivers().unwrap_or_default();
        assert_eq!(one, other);
    }

    #[test]
    fn tasks_are_refused_without_rights_not_reported_as_absent() {
        // Пустой список неотличим от «задач нет». Отдавать его вместо
        // отказа значило бы сказать неправду о системе.
        match scheduled_tasks() {
            Ok(list) => {
                // Права есть — список обязан быть непустым и осмысленным.
                assert!(!list.is_empty(), "с правами задач не может не быть");
                for task in &list {
                    assert!(task.path.starts_with('\\'), "{}", task.path);
                }
            }
            Err(error) => {
                let text = error.to_string();
                assert!(text.contains("администратора"), "{text}");
            }
        }
    }

    #[test]
    fn task_paths_look_like_the_scheduler_shows_them() {
        let Ok(list) = scheduled_tasks() else {
            return;
        };
        // У Windows свои задачи лежат в разделе Microsoft — если их нет,
        // значит обход дерева не работает.
        assert!(
            list.iter()
                .any(|task| task.path.starts_with("\\Microsoft\\")),
            "вложенные разделы не обошлись"
        );
    }
}
