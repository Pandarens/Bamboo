//! Белые списки (ТЗ, раздел 11.3).
//!
//! Неизменяемый список — главный предохранитель проекта. Ошибка здесь
//! означает не «неоптимально», а «система не загрузилась» или «пропал звук».
//! Поэтому список зашит в код, не редактируется пользователем и даже
//! не показывается как кандидат.

use std::collections::{HashMap, HashSet};

/// Ядро системы. Трогать нельзя ничего из этого.
const SYSTEM_CORE: &[&str] = &[
    "System",
    "Registry",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "svchost.exe",
    "fontdrvhost.exe",
    "dwm.exe",
    "sihost.exe",
    "ctfmon.exe",
    "explorer.exe",
    "runtimebroker.exe",
    "shellexperiencehost.exe",
];

/// Средства безопасности.
const SECURITY: &[&str] = &[
    "msmpeng.exe",
    "nissrv.exe",
    "securityhealthservice.exe",
    "securityhealthsystray.exe",
    "mpdefendercoreservice.exe",
];

/// Инфраструктура: звук, сеть, видеодрайверы, обслуживание компонентов.
const INFRASTRUCTURE: &[&str] = &[
    "audiodg.exe",
    "spoolsv.exe",
    "trustedinstaller.exe",
    "tiworker.exe",
    "nvdisplay.container.exe",
    "nvcontainer.exe",
    "amdow.exe",
    "atieclxx.exe",
    "igfxcuiservice.exe",
    "igfxem.exe",
];

/// Что известно о процессе для проверки по неизменяемому списку.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessFacts<'a> {
    pub image_name: &'a str,
    pub session_id: u32,
    /// Уровень целостности System.
    pub system_integrity: bool,
    /// Защищённый процесс (Protected Process Light).
    pub protected_process: bool,
    /// Подписан поставщиком средств защиты.
    pub security_vendor: bool,
    /// В процесс загружен ELAM-драйвер.
    pub elam_driver: bool,
}

/// Проверяет процесс по неизменяемому списку.
///
/// Возвращает причину запрета или `None`, если действия допустимы.
/// Причина возвращается текстом, чтобы интерфейс мог объяснить отказ,
/// а не молча спрятать кандидата.
pub fn immutable_reason(facts: &ProcessFacts<'_>) -> Option<&'static str> {
    use bamboo_core::pick;

    let name = facts.image_name.to_lowercase();

    if SYSTEM_CORE.iter().any(|entry| entry.to_lowercase() == name) {
        return Some(pick(
            "процесс входит в ядро системы",
            "the process is part of the system core",
        ));
    }
    if SECURITY.iter().any(|entry| *entry == name) {
        return Some(pick("средство защиты системы", "a system protection tool"));
    }
    if INFRASTRUCTURE.iter().any(|entry| *entry == name) {
        return Some(pick(
            "системная инфраструктура: звук, печать, видео или обслуживание",
            "system infrastructure: sound, printing, video or servicing",
        ));
    }

    if facts.elam_driver {
        return Some(pick(
            "в процесс загружен драйвер раннего запуска антивируса",
            "an early-launch antivirus driver is loaded into the process",
        ));
    }
    if facts.security_vendor {
        return Some(pick(
            "процесс поставщика средств защиты",
            "a security vendor process",
        ));
    }
    if facts.protected_process {
        return Some(pick(
            "защищённый процесс, вмешательство запрещено системой",
            "a protected process: the system forbids interference",
        ));
    }
    // Любой процесс в сессии 0 с уровнем целостности System — часть
    // системной инфраструктуры, даже если имя нам незнакомо.
    if facts.session_id == 0 && facts.system_integrity {
        return Some(pick(
            "системный процесс в сессии 0",
            "a system process in session 0",
        ));
    }

    None
}

/// Класс приложений, который пользователь может исключить целиком.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppClass {
    Games,
    DevTools,
    Virtualization,
    Databases,
}

impl AppClass {
    pub fn name(self) -> &'static str {
        match self {
            AppClass::Games => "игры",
            AppClass::DevTools => "средства разработки",
            AppClass::Virtualization => "средства виртуализации",
            AppClass::Databases => "СУБД",
        }
    }
}

/// Списки, которыми управляет пользователь.
#[derive(Debug, Default)]
pub struct UserWhitelist {
    entries: HashSet<String>,
    classes: HashSet<AppClass>,
    /// Сколько раз пользователь отклонил предложение по приложению.
    rejections: HashMap<String, u8>,
}

/// После скольких отказов приложение уходит в белый список навсегда.
const REJECTIONS_TO_SILENCE: u8 = 2;

impl UserWhitelist {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, app_key: &str) {
        self.entries.insert(app_key.to_string());
    }

    pub fn remove(&mut self, app_key: &str) {
        self.entries.remove(app_key);
        self.rejections.remove(app_key);
    }

    pub fn exclude_class(&mut self, class: AppClass) {
        self.classes.insert(class);
    }

    pub fn contains(&self, app_key: &str, class: Option<AppClass>) -> bool {
        if self.entries.contains(app_key) {
            return true;
        }
        class.is_some_and(|class| self.classes.contains(&class))
    }

    /// Пользователь отклонил предложение.
    ///
    /// Возвращает `true`, если приложение с этого момента в белом списке
    /// навсегда. По ТЗ дважды отклонённое предложение больше не повторяется
    /// никогда — и добавление происходит молча, без ещё одного вопроса.
    pub fn reject(&mut self, app_key: &str) -> bool {
        let count = self.rejections.entry(app_key.to_string()).or_insert(0);
        *count = count.saturating_add(1);

        if *count >= REJECTIONS_TO_SILENCE {
            self.entries.insert(app_key.to_string());
            return true;
        }
        false
    }

    pub fn rejection_count(&self, app_key: &str) -> u8 {
        self.rejections.get(app_key).copied().unwrap_or(0)
    }

    pub fn entries(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(name: &str) -> ProcessFacts<'_> {
        ProcessFacts {
            image_name: name,
            ..Default::default()
        }
    }

    #[test]
    fn the_system_core_is_untouchable() {
        for name in [
            "System",
            "lsass.exe",
            "csrss.exe",
            "dwm.exe",
            "explorer.exe",
        ] {
            assert!(
                immutable_reason(&facts(name)).is_some(),
                "{name} должен быть защищён"
            );
        }
    }

    #[test]
    fn case_does_not_matter() {
        assert!(immutable_reason(&facts("LSASS.EXE")).is_some());
        assert!(immutable_reason(&facts("Explorer.Exe")).is_some());
    }

    #[test]
    fn security_software_is_untouchable() {
        assert!(immutable_reason(&facts("MsMpEng.exe")).is_some());

        let vendor = ProcessFacts {
            image_name: "неизвестный-антивирус.exe",
            security_vendor: true,
            ..Default::default()
        };
        assert!(immutable_reason(&vendor).is_some());
    }

    #[test]
    fn an_unknown_system_process_in_session_zero_is_protected() {
        // Имя нам незнакомо, но сессия 0 плюс уровень System означают
        // часть системной инфраструктуры.
        let unknown = ProcessFacts {
            image_name: "какой-то-хост.exe",
            session_id: 0,
            system_integrity: true,
            ..Default::default()
        };
        assert!(immutable_reason(&unknown).is_some());
    }

    #[test]
    fn a_normal_user_application_is_not_protected() {
        let app = ProcessFacts {
            image_name: "slack.exe",
            session_id: 1,
            ..Default::default()
        };
        assert!(immutable_reason(&app).is_none());
    }

    #[test]
    fn the_refusal_explains_itself() {
        // Интерфейс должен уметь объяснить, почему кандидата нет в списке.
        let reason = immutable_reason(&facts("lsass.exe")).unwrap();
        assert!(!reason.is_empty());
    }

    #[test]
    fn twice_rejected_means_silence_forever() {
        let mut whitelist = UserWhitelist::new();

        assert!(!whitelist.reject("slack"), "первый отказ ещё не приговор");
        assert!(!whitelist.contains("slack", None));

        assert!(
            whitelist.reject("slack"),
            "второй отказ должен закрыть тему"
        );
        assert!(whitelist.contains("slack", None));
    }

    #[test]
    fn rejections_are_counted_per_application() {
        let mut whitelist = UserWhitelist::new();
        whitelist.reject("slack");
        whitelist.reject("teams");

        assert_eq!(whitelist.rejection_count("slack"), 1);
        assert!(!whitelist.contains("slack", None));
        assert!(!whitelist.contains("teams", None));
    }

    #[test]
    fn a_whole_class_can_be_excluded() {
        let mut whitelist = UserWhitelist::new();
        whitelist.exclude_class(AppClass::Games);

        assert!(whitelist.contains("любая-игра", Some(AppClass::Games)));
        assert!(!whitelist.contains("любая-игра", Some(AppClass::DevTools)));
        assert!(!whitelist.contains("любая-игра", None));
    }

    #[test]
    fn removing_an_entry_also_clears_its_rejections() {
        let mut whitelist = UserWhitelist::new();
        whitelist.reject("slack");
        whitelist.reject("slack");
        assert!(whitelist.contains("slack", None));

        whitelist.remove("slack");
        assert!(!whitelist.contains("slack", None));
        assert_eq!(whitelist.rejection_count("slack"), 0);
    }
}

#[cfg(test)]
mod translation_tests {
    use super::*;
    use bamboo_core::{set_language, Language};
    use std::sync::Mutex;

    static LANGUAGE_LOCK: Mutex<()> = Mutex::new(());

    /// Набор фактов, дающий каждую из причин отказа.
    ///
    /// Список перечислен целиком нарочно: добавите новую причину — тест
    /// не упадёт сам, но соседний, считающий покрытие, упадёт и напомнит.
    fn every_reason() -> Vec<ProcessFacts<'static>> {
        vec![
            ProcessFacts {
                image_name: "csrss.exe",
                ..Default::default()
            },
            ProcessFacts {
                image_name: "msmpeng.exe",
                ..Default::default()
            },
            ProcessFacts {
                image_name: "audiodg.exe",
                ..Default::default()
            },
            ProcessFacts {
                image_name: "чужой.exe",
                elam_driver: true,
                ..Default::default()
            },
            ProcessFacts {
                image_name: "чужой.exe",
                security_vendor: true,
                ..Default::default()
            },
            ProcessFacts {
                image_name: "чужой.exe",
                protected_process: true,
                ..Default::default()
            },
            ProcessFacts {
                image_name: "чужой.exe",
                session_id: 0,
                system_integrity: true,
                ..Default::default()
            },
        ]
    }

    #[test]
    fn every_refusal_reason_speaks_both_languages() {
        // Отказ — то, что человек видит чаще всего: он нажал, и ему
        // объяснили, почему нельзя. Оставить это на одном языке значило бы
        // перевести интерфейс и не перевести разговор.
        let _guard = LANGUAGE_LOCK.lock().unwrap();

        for facts in every_reason() {
            set_language(Language::Russian);
            let russian = immutable_reason(&facts).expect("причина обязана быть");
            set_language(Language::English);
            let english = immutable_reason(&facts).expect("причина обязана быть");

            assert_ne!(
                russian, english,
                "причина не переведена: {}",
                facts.image_name
            );
            assert!(
                !english
                    .chars()
                    .any(|c| ('\u{0410}'..='\u{044f}').contains(&c)),
                "в английской причине осталась кириллица: {english}"
            );
        }
        set_language(Language::Russian);
    }

    #[test]
    fn the_list_of_reasons_covers_them_all() {
        // Сторож для соседнего теста: если появится новая причина,
        // а набор фактов её не даст, тот проверит на одну меньше
        // и останется зелёным.
        //
        // Считаем ветки в боевой части файла, до модуля тестов.
        // Первые две редакции этого счёта ловили сами себя: строка,
        // которая ищет текст, содержит этот текст.
        let source = include_str!("whitelist.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("боевая часть файла");
        let branches = production.matches("return Some(").count();
        assert_eq!(
            branches,
            every_reason().len(),
            "причин отказа {branches}, а проверяется {}",
            every_reason().len()
        );
    }
}
