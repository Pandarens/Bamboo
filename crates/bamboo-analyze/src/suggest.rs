//! Предложения по оптимизации (ТЗ, разделы 10 и 11.1).
//!
//! Здесь Bamboo переходит от «показываю цифры» к «вижу, что можно
//! улучшить». Правила простые и все до одного объяснимые: каждое
//! предложение несёт причину, по которой оно возникло, и человек решает
//! сам.
//!
//! Чего здесь принципиально нет: предложений «на всякий случай». Пустой
//! список — нормальный и самый частый исход, и это не признак того, что
//! Bamboo плохо старался. Утилита, которая всегда находит десять проблем,
//! просто набивает себе цену — ровно то, чем занимаются «оптимизаторы»
//! из раздела 11.5 ТЗ.
//!
//! Логика чистая: на вход факты, на выход список. Никаких обращений
//! к системе, поэтому правила проверяются тестами целиком.

use bamboo_core::Bytes;

/// Сколько человек должен отсутствовать, чтобы фоновую работу можно было
/// назвать фоновой.
///
/// Пять минут: меньше — это пауза в работе, человек отошёл за чаем и
/// вернётся. Придерживать то, к чему он сейчас вернётся, — вредительство.
pub const IDLE_BEFORE_SUGGESTING_MS: u64 = 5 * 60 * 1000;

/// Порог заметной фоновой нагрузки на процессор, доля ядра.
const CPU_WHILE_IDLE: f32 = 5.0;

/// Порог заметной фоновой работы с диском, байт в секунду.
const DISK_WHILE_IDLE: u64 = 5 * 1024 * 1024;

/// Начиная с какого размера память процесса стоит обсуждать.
const BIG_MEMORY: u64 = 500 * 1024 * 1024;

/// Что предлагается сделать.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remedy {
    /// Экономичный режим: увести на энергоэффективные ядра.
    EcoQos,
    /// Понизить приоритет памяти.
    LowerMemory,
    /// Придержать скорость диска.
    ThrottleDisk,
    /// Ничего не делать: только сообщить наблюдение.
    JustSaying,
}

impl Remedy {
    pub fn name(self) -> &'static str {
        match self {
            Remedy::EcoQos => "включить экономичный режим",
            Remedy::LowerMemory => "понизить приоритет памяти",
            Remedy::ThrottleDisk => "придержать диск",
            Remedy::JustSaying => "просто знать",
        }
    }
}

/// Предложение по одному процессу.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub pid: u32,
    pub process_name: String,
    pub remedy: Remedy,
    /// Почему предложено — одной фразой, языком человека.
    pub reason: String,
    /// Что изменится, если согласиться. Обещать выгоду в мегабайтах
    /// и процентах мы не станем: снаружи её не измерить.
    pub effect: String,
}

/// Факты о процессе, по которым принимается решение.
///
/// Всё это Bamboo действительно наблюдает. Полей вроде «программа
/// не нужна пользователю» здесь нет и быть не может: такого мы не знаем.
#[derive(Clone, Debug)]
pub struct ProcessFacts<'a> {
    pub pid: u32,
    pub name: &'a str,
    /// Доля процессора, проценты.
    pub cpu_percent: f32,
    pub memory: Bytes,
    /// Чтение и запись вместе, байт в секунду.
    pub disk_per_second: u64,
    /// Сессия процесса. Ноль — системная, туда мы не лезем.
    pub session_id: u32,
    /// Окно процесса перестало отвечать.
    pub hung: bool,
    /// Похоже на утечку памяти.
    pub leaking: bool,
    /// К процессу уже что-то применено — второй раз не предлагаем.
    pub already_handled: bool,
    /// Процесс защищён неизменяемым списком.
    pub protected: bool,
}

/// Состояние системы на момент разбора.
#[derive(Clone, Copy, Debug, Default)]
pub struct Situation {
    /// Сколько человек не трогал мышь и клавиатуру.
    pub user_idle_ms: u64,
    /// Диск упирается в предел.
    pub disk_saturated: bool,
}

/// Собирает предложения по списку процессов.
///
/// Порядок результата — от самого весомого к менее весомому: наверху то,
/// что мешает прямо сейчас.
pub fn suggest(processes: &[ProcessFacts<'_>], situation: Situation) -> Vec<Suggestion> {
    let mut out = Vec::new();

    for process in processes {
        // Системные и защищённые процессы не обсуждаются вовсе. Трогать
        // их нельзя, и предлагать человеку то, что всё равно будет
        // отклонено, — обман.
        if process.protected || process.session_id == 0 {
            continue;
        }
        if process.already_handled {
            continue;
        }

        if let Some(suggestion) = for_process(process, situation) {
            out.push(suggestion);
        }
    }

    // Сначала то, что грузит систему, потом наблюдения без действий.
    out.sort_by_key(|suggestion| match suggestion.remedy {
        Remedy::ThrottleDisk => 0,
        Remedy::EcoQos => 1,
        Remedy::LowerMemory => 2,
        Remedy::JustSaying => 3,
    });
    out
}

fn for_process(process: &ProcessFacts<'_>, situation: Situation) -> Option<Suggestion> {
    let idle = situation.user_idle_ms >= IDLE_BEFORE_SUGGESTING_MS;

    // 1. Работа с диском, пока человека нет. Самое заметное: именно
    // из-за этого «компьютер шуршит сам по себе».
    if idle && process.disk_per_second >= DISK_WHILE_IDLE {
        let minutes = situation.user_idle_ms / 60_000;
        return Some(Suggestion {
            pid: process.pid,
            process_name: process.name.to_string(),
            remedy: Remedy::ThrottleDisk,
            reason: format!(
                "работает с диском на {}/с, пока вас нет за компьютером уже {minutes} мин",
                Bytes(process.disk_per_second)
            ),
            effect: "Скорость диска для этой программы будет ограничена. \
                     Работать она не перестанет — перестанет мешать."
                .to_string(),
        });
    }

    // 2. Процессор в фоне.
    if idle && process.cpu_percent >= CPU_WHILE_IDLE {
        let minutes = situation.user_idle_ms / 60_000;
        return Some(Suggestion {
            pid: process.pid,
            process_name: process.name.to_string(),
            remedy: Remedy::EcoQos,
            reason: format!(
                "занимает {:.0}% процессора, пока вас нет за компьютером уже {minutes} мин",
                process.cpu_percent
            ),
            effect: "Планировщик уведёт программу на энергоэффективные ядра. \
                     На ноутбуке это заметно по времени работы от батареи."
                .to_string(),
        });
    }

    // 3. Много памяти у фоновой программы.
    if idle && process.memory.as_u64() >= BIG_MEMORY {
        return Some(Suggestion {
            pid: process.pid,
            process_name: process.name.to_string(),
            remedy: Remedy::LowerMemory,
            reason: format!(
                "держит {} памяти и не используется прямо сейчас",
                process.memory
            ),
            effect: "Понизим приоритет памяти: при нехватке Windows заберёт \
                     страницы у этой программы раньше, чем у той, с которой \
                     вы работаете. Саму память это не освобождает."
                .to_string(),
        });
    }

    // 4. Наблюдения без действий. Предлагать тут нечего, но знать полезно.
    if process.leaking {
        return Some(Suggestion {
            pid: process.pid,
            process_name: process.name.to_string(),
            remedy: Remedy::JustSaying,
            reason: "память растёт ровно и не возвращается — похоже на утечку".to_string(),
            effect: "Действия не предлагаю: утечку лечит только обновление \
                     самой программы. Помогает перезапуск, но это решение ваше."
                .to_string(),
        });
    }

    if process.hung {
        return Some(Suggestion {
            pid: process.pid,
            process_name: process.name.to_string(),
            remedy: Remedy::JustSaying,
            reason: "окно перестало отвечать".to_string(),
            effect: "Программа может считать что-то долгое и вернуться сама. \
                     Завершать её — ваше решение: несохранённое пропадёт."
                .to_string(),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(name: &str, pid: u32) -> ProcessFacts<'_> {
        ProcessFacts {
            pid,
            name,
            cpu_percent: 0.0,
            memory: Bytes::from_mib(50),
            disk_per_second: 0,
            session_id: 1,
            hung: false,
            leaking: false,
            already_handled: false,
            protected: false,
        }
    }

    fn away() -> Situation {
        Situation {
            user_idle_ms: 10 * 60 * 1000,
            disk_saturated: false,
        }
    }

    fn present() -> Situation {
        Situation {
            user_idle_ms: 0,
            disk_saturated: false,
        }
    }

    #[test]
    fn a_calm_system_yields_no_suggestions() {
        // Самый частый исход, и он не означает, что Bamboo плохо смотрел.
        let processes = [process("notepad.exe", 100)];
        assert!(suggest(&processes, away()).is_empty());
        assert!(suggest(&processes, present()).is_empty());
    }

    #[test]
    fn disk_work_while_the_user_is_away_is_suggested() {
        let mut updater = process("updater.exe", 100);
        updater.disk_per_second = 20 * 1024 * 1024;

        let out = suggest(&[updater], away());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].remedy, Remedy::ThrottleDisk);
        assert!(out[0].reason.contains("пока вас нет"), "{}", out[0].reason);
        // Обещания должны быть скромными: «перестанет мешать», а не
        // «ускорит компьютер на 40%».
        assert!(out[0].effect.contains("перестанет мешать"));
    }

    #[test]
    fn the_same_work_is_not_suggested_while_the_user_is_present() {
        // Человек за компьютером — значит программа, возможно, нужна ему
        // прямо сейчас. Придерживать её было бы вредительством.
        let mut updater = process("updater.exe", 100);
        updater.disk_per_second = 20 * 1024 * 1024;
        assert!(suggest(&[updater], present()).is_empty());
    }

    #[test]
    fn background_cpu_is_suggested_for_eco_qos() {
        let mut worker = process("indexer.exe", 100);
        worker.cpu_percent = 30.0;

        let out = suggest(&[worker], away());
        assert_eq!(out[0].remedy, Remedy::EcoQos);
        assert!(out[0].reason.contains("30%"), "{}", out[0].reason);
    }

    #[test]
    fn a_big_idle_process_gets_a_memory_suggestion_with_an_honest_effect() {
        let mut heavy = process("editor.exe", 100);
        heavy.memory = Bytes::from_mib(900);

        let out = suggest(&[heavy], away());
        assert_eq!(out[0].remedy, Remedy::LowerMemory);
        // Здесь особенно важно не соврать: приоритет памяти её не
        // освобождает, и в тексте это сказано прямо.
        assert!(
            out[0].effect.contains("не освобождает"),
            "{}",
            out[0].effect
        );
    }

    #[test]
    fn protected_and_system_processes_are_never_suggested() {
        let mut guarded = process("lsass.exe", 100);
        guarded.protected = true;
        guarded.disk_per_second = 50 * 1024 * 1024;

        let mut system = process("svchost.exe", 101);
        system.session_id = 0;
        system.disk_per_second = 50 * 1024 * 1024;

        assert!(suggest(&[guarded, system], away()).is_empty());
    }

    #[test]
    fn an_already_handled_process_is_left_alone() {
        // Второй раз предлагать то же самое — назойливость.
        let mut done = process("updater.exe", 100);
        done.disk_per_second = 20 * 1024 * 1024;
        done.already_handled = true;
        assert!(suggest(&[done], away()).is_empty());
    }

    #[test]
    fn a_leak_is_reported_without_offering_a_cure() {
        let mut leaky = process("teams.exe", 100);
        leaky.leaking = true;

        let out = suggest(&[leaky], present());
        assert_eq!(out[0].remedy, Remedy::JustSaying);
        // Утечку снаружи не лечится, и обещать лечение нельзя.
        assert!(out[0].effect.contains("обновление"), "{}", out[0].effect);
    }

    #[test]
    fn a_hung_window_is_reported_but_not_killed_behind_the_users_back() {
        let mut stuck = process("app.exe", 100);
        stuck.hung = true;

        let out = suggest(&[stuck], present());
        assert_eq!(out[0].remedy, Remedy::JustSaying);
        assert!(out[0].effect.contains("ваше решение"), "{}", out[0].effect);
    }

    #[test]
    fn the_heaviest_trouble_comes_first() {
        let mut leaky = process("teams.exe", 100);
        leaky.leaking = true;
        let mut hungry = process("updater.exe", 101);
        hungry.disk_per_second = 20 * 1024 * 1024;
        let mut busy = process("indexer.exe", 102);
        busy.cpu_percent = 30.0;

        let out = suggest(&[leaky, hungry, busy], away());
        assert_eq!(out[0].remedy, Remedy::ThrottleDisk);
        assert_eq!(out[1].remedy, Remedy::EcoQos);
        assert_eq!(out[2].remedy, Remedy::JustSaying);
    }

    #[test]
    fn a_brief_pause_is_not_absence() {
        // Три минуты — человек отошёл за чаем и сейчас вернётся.
        let mut updater = process("updater.exe", 100);
        updater.disk_per_second = 20 * 1024 * 1024;

        let brief = Situation {
            user_idle_ms: 3 * 60 * 1000,
            disk_saturated: false,
        };
        assert!(suggest(&[updater], brief).is_empty());
    }
}
