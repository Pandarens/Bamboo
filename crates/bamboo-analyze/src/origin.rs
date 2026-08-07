//! Атрибуция происхождения процесса (ТЗ, раздел 7.2).
//!
//! Требование к отчёту простое: вместо «неизвестный процесс нагружал
//! процессор» — «задача `\Microsoft\Windows\Application Experience\
//! Microsoft Compatibility Appraiser` заняла 4 минуты одного ядра в 03:14».
//!
//! Восстанавливаем источник по цепочке родителей. Функция чистая: на вход
//! имена и командные строки, на выход — вывод о происхождении.

use core::fmt;

/// Описание процесса в цепочке родителей.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessDescriptor {
    pub image_name: String,
    /// Командная строка, если её удалось прочитать.
    pub command_line: Option<String>,
}

impl ProcessDescriptor {
    pub fn new(image_name: &str) -> Self {
        ProcessDescriptor {
            image_name: image_name.to_string(),
            command_line: None,
        }
    }

    pub fn with_command_line(mut self, command_line: &str) -> Self {
        self.command_line = Some(command_line.to_string());
        self
    }

    fn name_matches(&self, expected: &str) -> bool {
        self.image_name.eq_ignore_ascii_case(expected)
    }
}

/// Откуда взялся процесс.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Задача планировщика. Имя задачи известно не всегда.
    ScheduledTask(Option<String>),
    /// Служба Windows.
    Service,
    /// Пользователь запустил сам.
    UserLaunched,
    /// Работа антивируса Defender.
    Defender,
    /// Обслуживание компонентов Windows.
    ComponentServicing,
    /// Центр обновления Windows.
    WindowsUpdate,
    /// Определить не удалось. Не гадаем.
    Unknown,
}

impl Origin {
    /// Формулировка для пользователя.
    pub fn describe(&self) -> String {
        match self {
            Origin::ScheduledTask(Some(task)) => format!("задача планировщика {task}"),
            Origin::ScheduledTask(None) => "задача планировщика".to_string(),
            Origin::Service => "служба Windows".to_string(),
            Origin::UserLaunched => "запущено вами".to_string(),
            Origin::Defender => "проверка Defender".to_string(),
            Origin::ComponentServicing => "обслуживание компонентов Windows".to_string(),
            Origin::WindowsUpdate => "центр обновления Windows".to_string(),
            Origin::Unknown => "происхождение неизвестно".to_string(),
        }
    }

    /// Стоит ли вообще предлагать что-то делать с этим процессом.
    ///
    /// То, что пользователь запустил сам, трогать не надо: он в курсе.
    /// Обновление и обслуживание компонентов тоже не трогаем — они
    /// закончатся сами, а вмешательство ломает систему.
    pub fn is_actionable(&self) -> bool {
        matches!(self, Origin::ScheduledTask(_) | Origin::Service)
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

/// Восстанавливает происхождение по цепочке родителей.
///
/// `chain[0]` — непосредственный родитель, дальше вверх по дереву.
/// Проверки идут от конкретных к общим: `MsMpEng.exe` тоже запущен
/// из `services.exe`, но сказать «служба» вместо «Defender» значит
/// потерять всю полезную часть ответа.
pub fn attribute(chain: &[ProcessDescriptor]) -> Origin {
    for ancestor in chain {
        if ancestor.name_matches("MsMpEng.exe") {
            return Origin::Defender;
        }
        if ancestor.name_matches("TrustedInstaller.exe") || ancestor.name_matches("TiWorker.exe") {
            return Origin::ComponentServicing;
        }
        if ancestor.name_matches("MoUsoCoreWorker.exe")
            || ancestor.name_matches("UsoClient.exe")
            || ancestor.name_matches("wuauclt.exe")
        {
            return Origin::WindowsUpdate;
        }
        if is_scheduler_host(ancestor) {
            return Origin::ScheduledTask(None);
        }
        if ancestor.name_matches("explorer.exe") {
            return Origin::UserLaunched;
        }
        if ancestor.name_matches("services.exe") {
            return Origin::Service;
        }
    }

    Origin::Unknown
}

/// Экземпляр `svchost`, в котором живёт планировщик заданий.
///
/// Отличаем по параметру `-k`: в одном и том же образе svchost.exe работают
/// десятки разных служб, и без разбора командной строки любая из них
/// выглядела бы задачей планировщика.
fn is_scheduler_host(descriptor: &ProcessDescriptor) -> bool {
    if !descriptor.name_matches("svchost.exe") {
        return false;
    }
    let Some(command_line) = &descriptor.command_line else {
        return false;
    };
    let lower = command_line.to_lowercase();
    lower.contains("-k netsvcs") || lower.contains("schedule")
}

/// Уточняет задачу планировщика её именем.
///
/// Имя приходит отдельно — из провайдера `Microsoft-Windows-TaskScheduler`
/// или прямым запросом к планировщику. Без него остаётся общий вывод
/// «задача планировщика», и это лучше выдуманного имени.
pub fn with_task_name(origin: Origin, task: Option<&str>) -> Origin {
    match (origin, task) {
        (Origin::ScheduledTask(_), Some(name)) if !name.is_empty() => {
            Origin::ScheduledTask(Some(name.to_string()))
        }
        (other, _) => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(names: &[&str]) -> Vec<ProcessDescriptor> {
        names
            .iter()
            .map(|name| ProcessDescriptor::new(name))
            .collect()
    }

    #[test]
    fn explorer_means_the_user_started_it() {
        assert_eq!(
            attribute(&chain(&["explorer.exe", "winlogon.exe"])),
            Origin::UserLaunched
        );
    }

    #[test]
    fn services_parent_means_a_service() {
        assert_eq!(
            attribute(&chain(&["services.exe", "wininit.exe"])),
            Origin::Service
        );
    }

    #[test]
    fn defender_wins_over_the_generic_service_answer() {
        // MsMpEng.exe тоже потомок services.exe, но ответ «служба»
        // не сообщает ничего полезного.
        let chain = vec![
            ProcessDescriptor::new("MsMpEng.exe"),
            ProcessDescriptor::new("services.exe"),
        ];
        assert_eq!(attribute(&chain), Origin::Defender);
    }

    #[test]
    fn component_servicing_is_recognised() {
        assert_eq!(
            attribute(&chain(&["TiWorker.exe", "svchost.exe"])),
            Origin::ComponentServicing
        );
        assert_eq!(
            attribute(&chain(&["TrustedInstaller.exe", "services.exe"])),
            Origin::ComponentServicing
        );
    }

    #[test]
    fn windows_update_is_recognised() {
        assert_eq!(
            attribute(&chain(&["MoUsoCoreWorker.exe", "svchost.exe"])),
            Origin::WindowsUpdate
        );
    }

    #[test]
    fn the_scheduler_host_is_told_apart_by_its_command_line() {
        let scheduler = vec![ProcessDescriptor::new("svchost.exe")
            .with_command_line("C:\\Windows\\system32\\svchost.exe -k netsvcs -p")];
        assert_eq!(attribute(&scheduler), Origin::ScheduledTask(None));
    }

    #[test]
    fn another_svchost_is_not_mistaken_for_the_scheduler() {
        // В svchost.exe живут десятки служб. Без разбора командной строки
        // любая из них выглядела бы задачей планировщика.
        let audio = vec![ProcessDescriptor::new("svchost.exe").with_command_line(
            "C:\\Windows\\system32\\svchost.exe -k LocalServiceNetworkRestricted",
        )];
        assert_ne!(attribute(&audio), Origin::ScheduledTask(None));
    }

    #[test]
    fn svchost_without_a_command_line_is_not_guessed() {
        let unknown = vec![ProcessDescriptor::new("svchost.exe")];
        assert_ne!(attribute(&unknown), Origin::ScheduledTask(None));
    }

    #[test]
    fn an_empty_chain_gives_no_answer() {
        assert_eq!(attribute(&[]), Origin::Unknown);
        assert_eq!(attribute(&chain(&["чтототакое.exe"])), Origin::Unknown);
    }

    #[test]
    fn the_task_name_makes_the_answer_useful() {
        let origin = with_task_name(
            Origin::ScheduledTask(None),
            Some("\\Microsoft\\Windows\\Application Experience\\Microsoft Compatibility Appraiser"),
        );
        assert!(origin.describe().contains("Compatibility Appraiser"));
    }

    #[test]
    fn a_missing_task_name_does_not_invent_one() {
        let origin = with_task_name(Origin::ScheduledTask(None), None);
        assert_eq!(origin, Origin::ScheduledTask(None));

        let empty = with_task_name(Origin::ScheduledTask(None), Some(""));
        assert_eq!(empty, Origin::ScheduledTask(None));
    }

    #[test]
    fn what_the_user_started_is_left_alone() {
        assert!(!Origin::UserLaunched.is_actionable());
        assert!(!Origin::WindowsUpdate.is_actionable());
        assert!(!Origin::ComponentServicing.is_actionable());
        assert!(!Origin::Unknown.is_actionable());

        assert!(Origin::ScheduledTask(None).is_actionable());
        assert!(Origin::Service.is_actionable());
    }
}
