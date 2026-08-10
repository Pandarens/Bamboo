//! Действия над процессами из главного окна (ТЗ, разделы 11.1 и 14.6).
//!
//! Здесь только те действия, которые агент вправе выполнить сам: уровень
//! риска 1, прав администратора не требуют, применяются к процессам своей
//! сессии. Всё остальное — через брокер.
//!
//! Чего здесь нет и не будет: «очистки памяти». `EmptyWorkingSet` не
//! освобождает память, а выталкивает рабочий набор в подкачку, после чего
//! приложение считывает его обратно за секунды — пользователь получает
//! красивую цифру и подтормаживание вместо пользы. Это прямой отказ из
//! раздела 11.5 ТЗ, а не недоделка.

#![forbid(unsafe_code)]

use bamboo_actuate::{Executor, Outcome, SystemBackend};
use bamboo_journal::{Actor, Journal, Target};
use bamboo_policy::{Action, AutonomyMode, Context, ProcessFacts, Profile, UserWhitelist};

/// Что можно сделать с процессом из списка.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    /// Экономичный режим: планировщик уводит потоки на энергоэффективные ядра.
    EcoQos,
    /// Понизить приоритет памяти.
    LowerMemory,
}

impl RowAction {
    /// Действие по коду из интерфейса.
    pub fn from_index(index: i32) -> Option<RowAction> {
        match index {
            0 => Some(RowAction::EcoQos),
            1 => Some(RowAction::LowerMemory),
            _ => None,
        }
    }

    fn action(self) -> Action {
        match self {
            RowAction::EcoQos => Action::EnableEcoQos,
            RowAction::LowerMemory => Action::LowerMemoryPriority,
        }
    }
}

fn journal_path() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(base)
        .join("Bamboo")
        .join("journal.db")
}

/// Применяет действие от имени автоматики и возвращает номер записи журнала.
///
/// Отличий от ручного применения два, и оба существенные. Первое: в журнале
/// стоит `Auto`, и человек всегда видит, что это сделал не он. Второе:
/// возвращается номер записи — без него автоматика не смогла бы вернуть
/// как было, а право вмешиваться она имеет ровно потому, что умеет вернуть.
///
/// `None` — политика отказала либо действие не удалось. Это штатный исход:
/// молча настаивать автоматика не будет.
pub fn apply_automatically(pid: u32, image_name: &str, what: RowAction) -> Option<i64> {
    let journal = open_journal()?;
    let executor = Executor::new(&journal, SystemBackend);
    let whitelist = UserWhitelist::new();

    let context = Context {
        action: what.action(),
        process: ProcessFacts {
            image_name,
            session_id: 1,
            ..Default::default()
        },
        app_key: image_name,
        app_class: None,
        profile: Profile::Normal,
        mode: AutonomyMode::Assist,
        learning: false,
        whitelist: &whitelist,
    };
    let target = Target {
        app_key: image_name.to_string(),
        pid: Some(pid),
        ..Default::default()
    };

    let now = bamboo_core::SampleTime::wall_clock_now();
    match executor.apply(now, &context, &target, Actor::Auto, false) {
        Outcome::Applied { journal_id } => Some(journal_id),
        _ => None,
    }
}

/// Возвращает как было по номеру записи журнала.
///
/// Возвращает `true`, если откат состоялся. Неудача здесь означает, что
/// процесса уже нет — тогда и возвращать нечего.
pub fn revert_automatically(journal_id: i64, reason: &str) -> bool {
    let Some(journal) = open_journal() else {
        return false;
    };
    Executor::new(&journal, SystemBackend)
        .revert(journal_id, reason)
        .is_ok()
}

/// Открывает журнал, создавая папку под него.
fn open_journal() -> Option<Journal> {
    let path = journal_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    Journal::open(&path).ok()
}

/// Применяет действие к процессу и возвращает строку для показа пользователю.
///
/// Ничего не делает молча: и успех, и отказ объясняются словами. Отказ
/// политики — штатный исход, а не ошибка: например, системные процессы
/// защищены неизменяемым белым списком.
pub fn apply(pid: u32, image_name: &str, what: RowAction) -> String {
    let path = journal_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let journal = match Journal::open(&path) {
        Ok(journal) => journal,
        Err(error) => return format!("Журнал недоступен, действие отменено: {error}"),
    };

    let executor = Executor::new(&journal, SystemBackend);
    let whitelist = UserWhitelist::new();
    let action = what.action();

    let context = Context {
        action,
        process: ProcessFacts {
            image_name,
            session_id: 1,
            ..Default::default()
        },
        app_key: image_name,
        app_class: None,
        profile: Profile::Normal,
        // Действие запустил человек, а не автоматика: это его решение.
        mode: AutonomyMode::Assist,
        learning: false,
        whitelist: &whitelist,
    };
    let target = Target {
        app_key: image_name.to_string(),
        pid: Some(pid),
        ..Default::default()
    };

    let now = bamboo_core::SampleTime::wall_clock_now();
    match executor.apply(now, &context, &target, Actor::Manual, false) {
        Outcome::Applied { journal_id } => format!(
            "{}: {} — применено, запись №{journal_id}. Откатить можно в журнале.",
            image_name,
            action.name()
        ),
        Outcome::Simulated { would_do } => format!("{image_name}: {would_do}"),
        Outcome::Refused { reason } => format!("{image_name}: отказ — {reason}"),
        Outcome::Failed { error, .. } => format!("{image_name}: не удалось — {error}"),
    }
}

/// Что Bamboo уже сделал с процессами, по номерам процессов.
///
/// Пользователь должен видеть в списке, что он уже нажимал: иначе легко
/// применить экономичный режим второй раз или забыть, что процесс уже
/// придержан. Берём это из журнала — того же, по которому идёт откат.
#[derive(Default)]
pub struct AppliedActions {
    by_pid: std::collections::HashMap<u32, Vec<&'static str>>,
}

impl AppliedActions {
    /// Читает действующие записи журнала.
    ///
    /// Ошибку чтения глотаем молча и намеренно: журнал недоступен —
    /// значит меток не будет, но список процессов человек всё равно
    /// увидит. Ронять из-за подписи весь интерфейс незачем.
    pub fn load() -> Self {
        let mut by_pid: std::collections::HashMap<u32, Vec<&'static str>> =
            std::collections::HashMap::new();

        if let Ok(journal) = Journal::open(journal_path()) {
            if let Ok(entries) = journal.active() {
                for entry in entries {
                    let Some(pid) = entry.target.pid else {
                        continue;
                    };
                    let label = match entry.action {
                        Action::EnableEcoQos => "эконом",
                        Action::LowerMemoryPriority => "память ↓",
                        Action::DelayServiceStart => "отложенный старт",
                        Action::FreezeProcess => "заморожен",
                        _ => continue,
                    };
                    let marks = by_pid.entry(pid).or_default();
                    if !marks.contains(&label) {
                        marks.push(label);
                    }
                }
            }
        }

        AppliedActions { by_pid }
    }

    /// Метки для процесса: «эконом, память ↓». Пусто — ничего не делали.
    pub fn marks(&self, pid: u32, throttled: bool) -> String {
        let mut parts: Vec<&str> = self
            .by_pid
            .get(&pid)
            .map(|marks| marks.to_vec())
            .unwrap_or_default();

        // Придерживание диска в журнал не пишется: оно живёт в памяти
        // агента, пока он запущен. Добавляем его отдельно.
        if throttled {
            parts.push("диск ↓");
        }
        parts.join(", ")
    }
}

/// Реестр процессов, которым мы придержали диск.
///
/// Ограничение держится job-объектом и живёт ровно столько, сколько живёт
/// его дескриптор. Поэтому дескрипторы надо где-то хранить: выпустим —
/// лимит снимется сам. Это же и защита от забытых изменений — закрылся
/// Bamboo, и чужой процесс работает как раньше.
#[derive(Default)]
pub struct IoLimits {
    limited: std::collections::HashMap<u32, bamboo_sys::LimitedProcess>,
    /// Номера записей журнала: по ним ограничение помечается снятым.
    journal_ids: std::collections::HashMap<u32, i64>,
}

impl IoLimits {
    pub fn new() -> Self {
        Self::default()
    }

    /// Придержан ли процесс прямо сейчас.
    pub fn is_limited(&self, pid: u32) -> bool {
        self.limited.contains_key(&pid)
    }

    /// Переключает ограничение и объясняет результат словами.
    ///
    /// Отдельно оговорка про «запретить». Запрета ввода-вывода в Windows
    /// нет, и он был бы вреден: процесс, которому отказали в чтении файла,
    /// не подождёт вежливо — он упадёт. Поэтому придерживаем скорость.
    pub fn toggle(&mut self, pid: u32, image_name: &str) -> String {
        use bamboo_policy::ProcessFacts;

        if self.limited.remove(&pid).is_some() {
            if let Some(id) = self.journal_ids.remove(&pid) {
                record_limit_lifted(id);
            }
            return format!("{image_name}: ограничение диска снято.");
        }

        let facts = ProcessFacts {
            image_name,
            session_id: 1,
            ..Default::default()
        };
        if let Some(reason) = bamboo_policy::immutable_reason(&facts) {
            return format!("{image_name}: придерживать нельзя — {reason}");
        }

        // Запись открываем до попытки: иначе неудачная попытка не оставила бы
        // следа, и в журнале было бы видно только то, что получилось.
        // Журнал, показывающий одни успехи, — плохой журнал.
        let entry = begin_limit_entry(pid, image_name);

        let limit = bamboo_sys::IoLimit::Background;
        match bamboo_sys::LimitedProcess::throttle(pid, limit) {
            Ok(limited) => {
                self.limited.insert(pid, limited);
                if let Some(id) = entry {
                    record_limit_applied(id);
                    self.journal_ids.insert(pid, id);
                }
                let held = self.count();
                let others = if held > 1 {
                    format!(" Сейчас придержано процессов: {held}.")
                } else {
                    String::new()
                };
                format!(
                    "{image_name}: диск придержан — {}. Полного запрета не бывает:                      процесс, которому отказали в чтении, просто упал бы.                      Ограничение снимется само, когда Bamboo закроется.{others}",
                    limit.describe()
                )
            }
            Err(error) => {
                let reason = error.to_string();
                if let Some(id) = entry {
                    record_limit_failed(id, &reason);
                }
                format!("{image_name}: придержать не удалось — {reason}")
            }
        }
    }

    /// Сколько процессов сейчас придержано.
    pub fn count(&self) -> usize {
        self.limited.len()
    }
}

/// Завершает процесс по требованию пользователя.
///
/// Стоит особняком от `apply` и намеренно не идёт через журнал действий:
/// журнал существует ради отката, а поднять завершённый процесс нельзя.
/// Это единственная необратимая операция в Bamboo, и подаётся она именно
/// так — с подтверждением в интерфейсе и предупреждением о потере
/// несохранённых данных.
///
/// Системные процессы защищены тем же неизменяемым списком, что и все
/// остальные действия: убить `lsass.exe` через Bamboo не выйдет.
pub fn terminate(pid: u32, image_name: &str) -> String {
    let facts = ProcessFacts {
        image_name,
        session_id: 1,
        ..Default::default()
    };
    if let Some(reason) = bamboo_policy::immutable_reason(&facts) {
        return format!("{image_name}: завершать нельзя — {reason}");
    }

    match bamboo_sys::terminate_process(pid) {
        Ok(()) => format!("{image_name} (PID {pid}) завершён. Это действие необратимо."),
        Err(error) => format!("{image_name}: завершить не удалось — {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiting_a_system_process_is_refused() {
        let mut limits = IoLimits::new();
        let note = limits.toggle(4, "lsass.exe");
        assert!(note.contains("нельзя"), "получили: {note}");
        assert_eq!(limits.count(), 0);
    }

    #[test]
    fn toggling_a_missing_process_reports_failure() {
        let mut limits = IoLimits::new();
        let note = limits.toggle(0xFFFF_FFF0, "нет-такого.exe");
        assert!(note.contains("не удалось"), "получили: {note}");
        assert!(!limits.is_limited(0xFFFF_FFF0));
    }

    #[test]
    fn a_limit_can_be_applied_and_lifted() {
        let child = std::process::Command::new("cmd.exe")
            .args(["/c", "ping -n 5 127.0.0.1 > nul"])
            .spawn();
        let Ok(mut child) = child else { return };

        let mut limits = IoLimits::new();
        let note = limits.toggle(child.id(), "cmd.exe");

        if limits.is_limited(child.id()) {
            assert!(note.contains("придержан"), "{note}");
            // Второе нажатие снимает ограничение.
            let off = limits.toggle(child.id(), "cmd.exe");
            assert!(off.contains("снято"), "{off}");
            assert_eq!(limits.count(), 0);
        }

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_system_process_cannot_be_terminated() {
        // Неизменяемый список обязан отсечь запрос до системного вызова.
        let note = terminate(4, "lsass.exe");
        assert!(note.contains("завершать нельзя"), "получили: {note}");
    }

    #[test]
    fn terminating_reports_failure_instead_of_pretending() {
        // Процесса с таким PID нет — операция обязана честно не удаться.
        let note = terminate(0xFFFF_FFF0, "нет-такого.exe");
        assert!(note.contains("не удалось"), "получили: {note}");
    }

    #[test]
    fn action_codes_map_to_real_actions() {
        assert_eq!(RowAction::from_index(0), Some(RowAction::EcoQos));
        assert_eq!(RowAction::from_index(1), Some(RowAction::LowerMemory));
        // Неизвестный код не превращается молча в какое-нибудь действие.
        assert_eq!(RowAction::from_index(7), None);
    }

    #[test]
    fn both_actions_are_the_lowest_risk_level() {
        // Агент вправе делать сам только уровень 1. Если сюда однажды
        // добавят действие рискованнее, тест это заметит.
        for what in [RowAction::EcoQos, RowAction::LowerMemory] {
            assert_eq!(what.action().risk(), 1, "{:?}", what.action());
        }
    }

    #[test]
    fn a_protected_process_is_refused_with_an_explanation() {
        // lsass.exe в неизменяемом белом списке: политика обязана отказать,
        // и отказ обязан быть объяснён.
        let note = apply(4, "lsass.exe", RowAction::EcoQos);
        assert!(note.contains("отказ"), "получили: {note}");
        assert!(
            note.len() > "lsass.exe: отказ — ".len(),
            "отказ без причины"
        );
    }
}

/// Слежение за процессами, которые мы завершили.
///
/// Некоторые процессы возвращаются: их поднимает служба, планировщик или
/// родительская программа. Человек нажимает «Завершить», процесс исчезает
/// и через секунду появляется снова — и выглядит это как будто Bamboo
/// ничего не сделал. На самом деле сделал, просто его тут же переиграли.
///
/// Поэтому запоминаем завершённое и, увидев возвращение, называем того,
/// кто его вернул.
#[derive(Default)]
pub struct Terminated {
    /// Имя процесса и момент, когда мы его завершили.
    recent: Vec<(String, std::time::Instant)>,
    /// Служба, которая вернула процесс в последний раз.
    culprit: Option<bamboo_sys::ServiceOwner>,
}

/// Сколько ждём возвращения. Дольше нескольких минут связь между нашим
/// действием и появлением процесса становится надуманной.
const WATCH_FOR_RETURN: std::time::Duration = std::time::Duration::from_secs(180);

impl Terminated {
    pub fn new() -> Self {
        Self::default()
    }

    /// Запоминает, что процесс с таким именем был завершён нами.
    pub fn remember(&mut self, image_name: &str) {
        self.recent
            .push((image_name.to_lowercase(), std::time::Instant::now()));
        self.forget_old();
    }

    fn forget_old(&mut self) {
        self.recent
            .retain(|(_, at)| at.elapsed() < WATCH_FOR_RETURN);
    }

    /// Служба, поднявшая вернувшийся процесс, если это была служба.
    ///
    /// Держим её отдельно от текста: по ней интерфейс предлагает кнопку
    /// «остановить источник», а из строки имя службы не выковырять.
    pub fn culprit_service(&self) -> Option<bamboo_sys::ServiceOwner> {
        self.culprit.clone()
    }

    /// Ищет вернувшиеся процессы и объясняет, кто их поднял.
    ///
    /// `processes` — свежий список, `parent_name` — как узнать имя
    /// родителя по его номеру.
    pub fn check_returns(
        &mut self,
        processes: &[(String, u32, u32)],
        parent_name: &dyn Fn(u32) -> Option<String>,
    ) -> Option<String> {
        self.forget_old();
        if self.recent.is_empty() {
            return None;
        }

        for (name, pid, parent_pid) in processes {
            let lowered = name.to_lowercase();
            let Some(at) = self
                .recent
                .iter()
                .position(|(remembered, _)| *remembered == lowered)
            else {
                continue;
            };

            let (_, when) = self.recent.remove(at);
            let seconds = when.elapsed().as_secs();

            // Если поднявший — служба, её можно остановить, и тогда
            // процесс перестанет возвращаться. Это и есть настоящий ответ
            // на «завершаю, а он опять появляется».
            self.culprit = bamboo_sys::service_by_pid(*parent_pid);
            if let Some(service) = &self.culprit {
                return Some(format!(
                    "{name} вернулся через {seconds} с (PID {pid}). Его поднимает служба                      «{}». Завершать процесс повторно бесполезно — он будет возвращаться,                      пока служба работает. Остановить её можно кнопкой ниже: это                      действие уровня 5, и служба поднимется обратно при перезагрузке.",
                    service.display
                ));
            }

            // Родитель мог уже завершиться — так бывает, когда программу
            // поднял разовый запуск. Тогда честно говорим, что не знаем.
            return Some(match parent_name(*parent_pid) {
                Some(parent) => format!(
                    "{name} вернулся через {seconds} с (PID {pid}). Его запустил {parent}. \
                     Такое поднимают службы, планировщик заданий или сама программа — \
                     завершать его повторно бесполезно, надо разбираться с тем, кто \
                     его вызывает."
                ),
                None => format!(
                    "{name} вернулся через {seconds} с (PID {pid}). Кто его запустил, \
                     сказать не могу: родительский процесс уже закрылся. Так ведут \
                     себя разовые запуски из планировщика заданий."
                ),
            });
        }

        None
    }
}

#[cfg(test)]
mod return_tests {
    use super::*;

    #[test]
    fn a_returning_process_names_its_parent() {
        let mut watch = Terminated::new();
        watch.remember("updater.exe");

        let processes = vec![("updater.exe".to_string(), 500u32, 300u32)];
        let note = watch
            .check_returns(&processes, &|pid| {
                (pid == 300).then(|| "services.exe".to_string())
            })
            .expect("возвращение должно быть замечено");

        assert!(note.contains("вернулся"), "{note}");
        assert!(note.contains("services.exe"), "{note}");
        // Совет по делу: повторно завершать бесполезно.
        assert!(note.contains("бесполезно"), "{note}");
    }

    #[test]
    fn an_unknown_parent_is_admitted_not_invented() {
        let mut watch = Terminated::new();
        watch.remember("task.exe");

        let processes = vec![("task.exe".to_string(), 500u32, 999u32)];
        let note = watch.check_returns(&processes, &|_| None).unwrap();
        assert!(note.contains("сказать не могу"), "{note}");
    }

    #[test]
    fn a_process_we_did_not_kill_is_not_reported() {
        let mut watch = Terminated::new();
        let processes = vec![("chrome.exe".to_string(), 500u32, 300u32)];
        assert_eq!(watch.check_returns(&processes, &|_| None), None);
    }

    #[test]
    fn the_same_return_is_reported_only_once() {
        // Иначе сообщение висело бы вечно и раздражало.
        let mut watch = Terminated::new();
        watch.remember("updater.exe");

        let processes = vec![("updater.exe".to_string(), 500u32, 300u32)];
        assert!(watch.check_returns(&processes, &|_| None).is_some());
        assert_eq!(watch.check_returns(&processes, &|_| None), None);
    }
}

/// Останавливает службу, которая возвращает завершённый процесс.
///
/// Отдельная и осознанно неудобная операция: это уровень риска 5 из
/// иерархии ТЗ. Остановка службы способна сломать то, что от неё зависит,
/// поэтому вызывается только по прямому требованию человека и только
/// после того, как он увидел, о какой службе речь.
pub fn stop_service(service: &bamboo_sys::ServiceOwner) -> String {
    // Службы Windows, без которых система не работает, не трогаем даже
    // по прямой просьбе: это тот случай, когда «пользователь сам попросил»
    // не оправдание.
    const NEVER: &[&str] = &[
        "rpcss",
        "dcomlaunch",
        "lsm",
        "plugplay",
        "power",
        "winlogon",
        "csrss",
        "eventlog",
        "schedule",
        "wuauserv",
    ];

    let lowered = service.name.to_lowercase();
    if NEVER.contains(&lowered.as_str()) {
        return format!(
            "Службу «{}» Bamboo не остановит: без неё Windows работает неправильно. \
             Это тот случай, когда «я сам попросил» — не повод.",
            service.display
        );
    }

    match bamboo_sys::stop_service(&service.name) {
        Ok(()) => format!(
            "Служба «{}» остановлена. Процесс больше не должен возвращаться. \
             При перезагрузке служба запустится снова — чтобы этого не было, \
             её нужно перевести в отключённые, а это отдельное решение.",
            service.display
        ),
        Err(error) => format!("Остановить «{}» не удалось: {error}", service.display),
    }
}

#[cfg(test)]
mod stop_service_tests {
    use super::*;

    fn service(name: &str) -> bamboo_sys::ServiceOwner {
        bamboo_sys::ServiceOwner {
            name: name.to_string(),
            display: name.to_string(),
        }
    }

    #[test]
    fn critical_services_are_refused_even_when_asked() {
        // Пользователь может попросить остановить RPCSS. Выполнить эту
        // просьбу — значит сломать ему систему.
        for name in ["RpcSs", "DcomLaunch", "Winlogon", "Schedule"] {
            let note = stop_service(&service(name));
            assert!(note.contains("не остановит"), "{name}: {note}");
        }
    }

    #[test]
    fn a_missing_service_reports_the_failure() {
        let note = stop_service(&service("НетТакойСлужбыBamboo"));
        assert!(note.contains("не удалось"), "{note}");
    }
}

/// Открывает запись журнала о придержании диска.
///
/// Придержание не проходит через исполнитель: ограничение живёт ровно
/// столько, сколько жив дескриптор job-объекта, а исполнитель не хранит
/// состояния между вызовами и закрыл бы его сразу. Поэтому запись ведём
/// здесь — но ведём обязательно: действие, которого нет в журнале, нельзя
/// ни проверить, ни откатить.
fn begin_limit_entry(pid: u32, image_name: &str) -> Option<i64> {
    let journal = open_journal()?;
    let target = Target {
        app_key: image_name.to_string(),
        pid: Some(pid),
        ..Default::default()
    };

    journal
        .begin(&bamboo_journal::NewEntry {
            at_unix_ms: bamboo_core::SampleTime::wall_clock_now(),
            actor: Actor::Manual,
            profile: "обычный",
            target: &target,
            action: Action::LimitDiskRate,
            // Прежнее состояние здесь простое: ограничения не было. Скорость
            // до придержания не записываем — она меняется каждую секунду
            // и возвращать её не требуется.
            prior_state: "io_limit=нет",
            observation: Some("человек попросил придержать диск"),
        })
        .ok()
}

fn record_limit_applied(journal_id: i64) {
    if let Some(journal) = open_journal() {
        let _ = journal.confirm(journal_id);
    }
}

fn record_limit_failed(journal_id: i64, reason: &str) {
    if let Some(journal) = open_journal() {
        let _ = journal.fail(journal_id, reason);
    }
}

fn record_limit_lifted(journal_id: i64) {
    if let Some(journal) = open_journal() {
        let _ = journal.mark_reverted(journal_id, "человек снял ограничение");
    }
}
