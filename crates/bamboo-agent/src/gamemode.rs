//! Игровой режим (ТЗ, раздел 11.4).
//!
//! Одна кнопка: игре и голосовому чату — всё внимание, фоновой мелочи —
//! обратно в фон. Ровно то, чего ждут от «игрового режима», и ничего
//! сверх того.
//!
//! Про честность обещаний. Управлять мы можем тремя вещами: процессорным
//! временем (класс приоритета и экономичный режим), приоритетом памяти
//! и скоростью диска. **Сетью — не можем**: раздать полосу отдельным
//! процессам Windows без своего драйвера не позволяет, и обещать
//! «приоритет сети для Discord» значило бы врать. Поэтому не обещаем.
//!
//! Всё, что режим делает, он умеет отменить: выключение возвращает
//! процессам ровно те значения, что были до включения.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use bamboo_sys::control::{self, MemoryPriority, PriorityClass};

/// Кого считаем спутником игры: их не трогаем и даже поднимаем.
///
/// Список намеренно короткий и очевидный. Голосовой чат во время игры
/// нужен не меньше самой игры: придушить Discord и получить заикающийся
/// звук — это не оптимизация, а поломка.
const COMPANIONS: &[&str] = &[
    "discord",
    "discordptb",
    "discordcanary",
    "teamspeak3",
    "ts3client_win64",
    "mumble",
    "steam",
    "steamwebhelper",
    "obs64",
    "obs32",
];

/// Что было с процессом до включения режима.
///
/// Храним ровно то, что меняли: вернуть надо в точности прежнее
/// состояние, а не «обычное по умолчанию».
#[derive(Clone, Copy, Debug)]
struct Before {
    priority: PriorityClass,
    memory: MemoryPriority,
    eco: bool,
}

/// Игровой режим: включён или нет, и что менял.
#[derive(Default)]
pub struct GameMode {
    /// Пусто — режим выключен.
    touched: HashMap<u32, Before>,
    /// Номер процесса игры, ради которой всё затевалось.
    game_pid: Option<u32>,
}

/// Что известно о процессе на момент включения режима.
pub struct Candidate<'a> {
    pub pid: u32,
    pub name: &'a str,
    pub cpu_percent: f32,
    /// Процесс сейчас на переднем плане либо занимает весь экран.
    pub is_foreground: bool,
    /// Процесс защищён неизменяемым списком.
    pub protected: bool,
}

/// Роль процесса в игровом режиме.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Сама игра: ей всё внимание.
    Game,
    /// Спутник игры: не трогаем и слегка поднимаем.
    Companion,
    /// Фоновая мелочь: уводим в фон.
    Background,
    /// Не трогаем вовсе.
    Untouched,
}

/// Определяет роль процесса.
///
/// Игрой считаем то, что на переднем плане и при этом заметно работает.
/// Чутьё простое, зато не выдумывает: программа, занимающая экран и
/// процессор, и есть то, чем человек сейчас занят.
pub fn role_of(candidate: &Candidate<'_>, game_pid: Option<u32>) -> Role {
    if candidate.protected {
        return Role::Untouched;
    }
    if Some(candidate.pid) == game_pid {
        return Role::Game;
    }

    let name = candidate.name.to_lowercase();
    let name = name.trim_end_matches(".exe");
    if COMPANIONS.contains(&name) {
        return Role::Companion;
    }

    // Совсем спящие процессы трогать незачем: с них нечего снимать,
    // а запись в журнале появится.
    if candidate.cpu_percent < 0.5 {
        return Role::Untouched;
    }

    Role::Background
}

impl GameMode {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_on(&self) -> bool {
        !self.touched.is_empty()
    }

    /// Включает режим. Возвращает строку для показа человеку.
    ///
    /// Игру ищем среди кандидатов: тот, кто на переднем плане. Не нашли —
    /// не включаем и честно говорим об этом: «игровой режим» без игры
    /// означал бы, что мы придушили фон просто так.
    pub fn turn_on(&mut self, candidates: &[Candidate<'_>]) -> String {
        if self.is_on() {
            return "Игровой режим уже включён.".to_string();
        }

        let Some(game) = candidates.iter().find(|c| c.is_foreground && !c.protected) else {
            return "Игру найти не удалось: на переднем плане ничего нет. \
                    Запустите игру и включите режим из неё — Bamboo поймёт, \
                    кому отдавать ресурсы."
                .to_string();
        };
        self.game_pid = Some(game.pid);

        let mut moved_back = 0usize;
        for candidate in candidates {
            let role = role_of(candidate, self.game_pid);
            if role == Role::Untouched {
                continue;
            }

            // Запоминаем прежнее состояние до того, как что-то менять:
            // не прочитали — не тронули, иначе вернуть будет некуда.
            let Ok(priority) = control::priority_class(candidate.pid) else {
                continue;
            };
            let Ok(memory) = control::memory_priority(candidate.pid) else {
                continue;
            };
            let eco = control::eco_qos(candidate.pid).unwrap_or(false);

            let applied = match role {
                Role::Game => {
                    // Игре — выше обычного и обычный приоритет памяти.
                    // Выше не поднимаем: класс «высокий» вытесняет системные
                    // потоки, включая обработку ввода мыши.
                    let a = control::set_priority_class(candidate.pid, PriorityClass::ABOVE_NORMAL);
                    let b = control::set_memory_priority(candidate.pid, MemoryPriority::NORMAL);
                    let c = control::clear_eco_qos(candidate.pid);
                    a.is_ok() || b.is_ok() || c.is_ok()
                }
                Role::Companion => {
                    // Спутнику — обычный приоритет и никакого экономичного
                    // режима: заикающийся голос в чате хуже лишних процентов.
                    let a = control::set_priority_class(candidate.pid, PriorityClass::NORMAL);
                    let b = control::clear_eco_qos(candidate.pid);
                    a.is_ok() || b.is_ok()
                }
                Role::Background => {
                    let a = control::set_priority_class(candidate.pid, PriorityClass::BELOW_NORMAL);
                    let b = control::set_eco_qos(candidate.pid, true);
                    let c = control::set_memory_priority(candidate.pid, MemoryPriority::MEDIUM);
                    a.is_ok() || b.is_ok() || c.is_ok()
                }
                Role::Untouched => false,
            };

            if applied {
                self.touched.insert(
                    candidate.pid,
                    Before {
                        priority,
                        memory,
                        eco,
                    },
                );
                if role == Role::Background {
                    moved_back += 1;
                }
            }
        }

        if self.touched.is_empty() {
            self.game_pid = None;
            return "Ничего менять не пришлось: фоновых программ, которые \
                    мешали бы игре, не нашлось."
                .to_string();
        }

        format!(
            "Игровой режим включён. {} — всё внимание, {moved_back} фоновых программ \
             уведены в фон. Голосовые чаты не трогаю: заикающийся звук хуже \
             лишних процентов. Сеть не регулирую — Windows не даёт делить \
             полосу между программами, и обещать этого я не буду.",
            game.name
        )
    }

    /// Выключает режим и возвращает всё как было.
    pub fn turn_off(&mut self) -> String {
        if !self.is_on() {
            return "Игровой режим и так выключен.".to_string();
        }

        let mut restored = 0usize;
        let mut failed = 0usize;

        for (pid, before) in self.touched.drain() {
            let a = control::set_priority_class(pid, before.priority);
            let b = control::set_memory_priority(pid, before.memory);
            // Экономичный режим возвращаем в точности: было включено —
            // включаем, была системная воля — возвращаем системе.
            let c = if before.eco {
                control::set_eco_qos(pid, true)
            } else {
                control::clear_eco_qos(pid)
            };

            if a.is_ok() && b.is_ok() && c.is_ok() {
                restored += 1;
            } else {
                // Процесс мог завершиться, пока режим был включён —
                // это самый обычный случай, а не сбой.
                failed += 1;
            }
        }
        self.game_pid = None;

        if failed == 0 {
            format!("Игровой режим выключен, {restored} программ вернулись к прежним настройкам.")
        } else {
            format!(
                "Игровой режим выключен. Вернул {restored}, у {failed} не вышло — \
                 скорее всего, они успели закрыться."
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate<'a>(name: &'a str, pid: u32, cpu: f32) -> Candidate<'a> {
        Candidate {
            pid,
            name,
            cpu_percent: cpu,
            is_foreground: false,
            protected: false,
        }
    }

    #[test]
    fn the_foreground_process_becomes_the_game() {
        let mut game = candidate("thegame.exe", 100, 60.0);
        game.is_foreground = true;
        assert_eq!(role_of(&game, Some(100)), Role::Game);
    }

    #[test]
    fn voice_chats_are_companions_not_victims() {
        // Придушить Discord во время игры — значит получить заикающийся
        // звук. Это поломка, а не оптимизация.
        for name in ["Discord.exe", "TeamSpeak3.exe", "mumble.exe"] {
            let voice = candidate(name, 200, 5.0);
            assert_eq!(role_of(&voice, Some(100)), Role::Companion, "{name}");
        }
    }

    #[test]
    fn a_busy_background_process_is_moved_back() {
        let worker = candidate("updater.exe", 300, 15.0);
        assert_eq!(role_of(&worker, Some(100)), Role::Background);
    }

    #[test]
    fn a_sleeping_process_is_left_alone() {
        // С неработающего процесса нечего снимать, а запись в журнале
        // появилась бы.
        let idle = candidate("notepad.exe", 400, 0.1);
        assert_eq!(role_of(&idle, Some(100)), Role::Untouched);
    }

    #[test]
    fn protected_processes_are_never_touched() {
        let mut system = candidate("lsass.exe", 500, 50.0);
        system.protected = true;
        assert_eq!(role_of(&system, Some(100)), Role::Untouched);
    }

    #[test]
    fn without_a_foreground_process_the_mode_does_not_turn_on() {
        // «Игровой режим» без игры означал бы, что мы придушили фон
        // просто так.
        let mut mode = GameMode::new();
        let note = mode.turn_on(&[candidate("updater.exe", 300, 15.0)]);

        assert!(note.contains("Игру найти не удалось"), "{note}");
        assert!(!mode.is_on());
    }

    #[test]
    fn turning_off_a_mode_that_is_off_is_harmless() {
        let mut mode = GameMode::new();
        assert!(mode.turn_off().contains("и так выключен"));
    }

    #[test]
    fn the_mode_touches_itself_and_restores_on_a_live_process() {
        // Проверяем на своём же процессе: он точно жив и его не жалко.
        let me = std::process::id();
        let before = control::priority_class(me).expect("свой приоритет читается");

        let mut mode = GameMode::new();
        let mut game = candidate("bamboo-agent.exe", me, 10.0);
        game.is_foreground = true;

        let note = mode.turn_on(&[game]);
        assert!(note.contains("включён"), "{note}");
        assert!(mode.is_on());

        mode.turn_off();
        assert!(!mode.is_on());
        assert_eq!(
            control::priority_class(me).unwrap(),
            before,
            "после выключения приоритет обязан вернуться"
        );
    }
}
