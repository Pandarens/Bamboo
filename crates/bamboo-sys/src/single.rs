//! Один агент на сеанс.
//!
//! Написано после того, как на живой машине разом оказалось два агента:
//! два запуска ждали подтверждения прав, дождались оба и подняли по копии.
//! Стоит это дорого — две иконки в трее, двойной расход, а главное два
//! писателя в одну базу наблюдений. База такое переживает, но повторять
//! не хочется: она уже была повреждена однажды.
//!
//! Защита — именованный мьютекс. Не файл-замок: файл переживает падение
//! процесса и после него врёт, будто агент ещё работает, а мьютекс ядро
//! закрывает само вместе с процессом, как бы тот ни завершился.
//!
//! Пространство имён `Local\` — на сеанс входа. Именно то, что нужно:
//! у разных пользователей за одной машиной агенты свои и мешать друг другу
//! не должны, а вот запуск от администратора и обычный внутри одного
//! сеанса — это те самые двое, которых надо развести.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Занятое место единственного агента.
///
/// Держится живым, пока жив процесс: значение надо сохранить, а не бросить
/// сразу же. Брошенное освободит место в тот же миг, и защита исчезнет.
pub struct SingleInstance {
    handle: HANDLE,
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

impl SingleInstance {
    /// Занимает место. `Ok(None)` — место уже занято другим агентом.
    ///
    /// Не ошибка: второй запуск — обычное дело, человек дважды нажал
    /// на ярлык. Ошибкой это делать нельзя, иначе к обычному действию
    /// прилагалось бы сообщение о сбое.
    pub fn acquire(name: &str) -> Result<Option<SingleInstance>> {
        // SAFETY: имя завершено нулём, атрибуты по умолчанию.
        let handle = unsafe { CreateMutexW(core::ptr::null(), 1, wide(name).as_ptr()) };
        if handle.is_null() {
            return Err(Error::Win32 {
                call: "CreateMutexW",
                code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
            });
        }

        // Мьютекс уже был — значит агент работает. Описатель закрываем
        // сразу: он наш, но место не наше, и держать его незачем.
        let taken =
            unsafe { windows_sys::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS;
        if taken {
            unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        Ok(Some(SingleInstance { handle }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_second_holder_is_turned_away_and_the_place_frees_on_drop() {
        // Имя своё, чтобы не столкнуться с работающим агентом.
        let name = format!(r"Local\Bamboo-тест-{}", std::process::id());

        let first = SingleInstance::acquire(&name)
            .expect("занять место должно получаться")
            .expect("место свободно — первый обязан его занять");
        assert!(
            SingleInstance::acquire(&name).unwrap().is_none(),
            "второй занял уже занятое место"
        );

        // Место освобождается вместе с владельцем, а не когда-нибудь потом.
        drop(first);
        assert!(
            SingleInstance::acquire(&name).unwrap().is_some(),
            "место не освободилось после ухода владельца"
        );
    }
}
