//! Самостоятельная оптимизация (ТЗ, разделы 10.3 и 11.2).
//!
//! До сих пор Bamboo только предлагал, а решал человек. Это правильно для
//! всего, что необратимо, но у части действий необратимости нет вовсе:
//! экономичный режим, приоритет памяти и придержанный диск снимаются
//! в один вызов и не оставляют следов. Такое можно делать самому — при
//! одном условии: вернуть как было, когда человек вернётся.
//!
//! Отсюда и весь замысел. Пока человека нет, фоновая работа придерживается,
//! и это честная выгода: диск и энергоэффективные ядра достаются тому, кто
//! действительно работает. Как только человек тронул мышь, всё снимается
//! до последнего процесса — раньше, чем он успеет это заметить.
//!
//! Чего здесь нет и не будет: «оптимизации» переднего плана, чистки памяти
//! и всего прочего из раздела 11.5. Автоматика имеет право только на то,
//! что умеет отменить.

use bamboo_analyze::suggest::{Remedy, Suggestion};

/// Сколько человек должен отсутствовать, прежде чем автоматика вмешается.
///
/// Заметно больше, чем нужно для простого предложения: предложение можно
/// не заметить, а вмешательство человек почувствует. Десять минут — это
/// уже точно «отошёл», а не «задумался».
pub const IDLE_BEFORE_ACTING_MS: u64 = 10 * 60 * 1000;

/// Что автоматика применила к процессу и что придётся вернуть.
#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    pub pid: u32,
    pub name: String,
    pub remedy: Remedy,
}

/// Можно ли применить это средство самостоятельно.
///
/// Обратимость — единственный критерий. Всё, что оставляет след после
/// снятия, остаётся за человеком.
pub fn is_reversible(remedy: Remedy) -> bool {
    match remedy {
        // Оба снимаются одним вызовом и ничего после себя не оставляют.
        Remedy::EcoQos | Remedy::LowerMemory => true,
        // Придержание диска автопилоту не отдаём, и это исправление
        // вранья, а не осторожность: прежде оно значилось обратимым,
        // но исполнитель автопилота его не умел — планировал и молча
        // пропускал. Ограничение живёт, пока жив дескриптор job-объекта,
        // а тем реестром владеет человек через окно.
        Remedy::ThrottleDisk => false,
        // Сообщение — не действие, применять нечего.
        Remedy::JustSaying => false,
    }
}

/// Решение автоматики на текущий тик.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// Что применить прямо сейчас.
    pub apply: Vec<Held>,
    /// Что снять прямо сейчас.
    pub release: Vec<Held>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.apply.is_empty() && self.release.is_empty()
    }
}

/// Что автоматика удерживает сейчас.
#[derive(Default)]
pub struct Autopilot {
    /// Включена ли она вообще. По умолчанию — нет: вмешательство без
    /// спроса человек должен разрешить сам.
    enabled: bool,
    held: Vec<Held>,
}

/// Больше стольких процессов за раз автоматика не трогает.
///
/// Не ради экономии, а ради понятности: список из сорока строк в журнале
/// человек не прочтёт, а значит не проверит. Пять — прочтёт.
const AT_MOST: usize = 5;

impl Autopilot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Включает или выключает автоматику.
    ///
    /// При выключении всё удерживаемое обязано вернуться: выключенная
    /// автоматика, оставившая после себя придержанные процессы, — худшее
    /// из возможных поведений.
    pub fn set_enabled(&mut self, on: bool) -> Vec<Held> {
        self.enabled = on;
        if on {
            Vec::new()
        } else {
            core::mem::take(&mut self.held)
        }
    }

    /// Сколько процессов придержано.
    pub fn holding(&self) -> usize {
        self.held.len()
    }

    /// Что делать на этом тике.
    ///
    /// `user_idle_ms` — сколько человек не трогал мышь и клавиатуру,
    /// `suggestions` — то, что разбор уже нашёл.
    pub fn decide(&mut self, user_idle_ms: u64, suggestions: &[Suggestion]) -> Plan {
        if !self.enabled {
            // Выключена — но если что-то осталось удерживаться, вернуть
            // это всё равно обязаны.
            return Plan {
                release: core::mem::take(&mut self.held),
                ..Default::default()
            };
        }

        // Человек вернулся. Снимаем всё и ничего не применяем: пока он
        // здесь, компьютер принадлежит ему целиком.
        if user_idle_ms < IDLE_BEFORE_ACTING_MS {
            return Plan {
                release: core::mem::take(&mut self.held),
                ..Default::default()
            };
        }

        // Человека нет: применяем то, что обратимо и ещё не применено.
        let mut apply = Vec::new();
        for suggestion in suggestions {
            if apply.len() + self.held.len() >= AT_MOST {
                break;
            }
            if !is_reversible(suggestion.remedy) {
                continue;
            }
            let held = Held {
                pid: suggestion.pid,
                name: suggestion.process_name.clone(),
                remedy: suggestion.remedy,
            };
            if self.held.contains(&held) || apply.contains(&held) {
                continue;
            }
            apply.push(held);
        }

        self.held.extend(apply.iter().cloned());
        Plan {
            apply,
            release: Vec::new(),
        }
    }

    /// Забывает про процесс, которого больше нет.
    ///
    /// Снимать с завершённого процесса нечего, а держать его в списке —
    /// значит показывать человеку неправду о числе придержанных.
    pub fn forget(&mut self, pid: u32) {
        self.held.retain(|held| held.pid != pid);
    }

    /// Что сказать человеку про работу автоматики.
    pub fn status(&self, user_idle_ms: u64) -> String {
        if !self.enabled {
            return "Автоматика выключена: Bamboo только предлагает, решаете вы.".to_string();
        }
        if !self.held.is_empty() {
            let names: Vec<&str> = self
                .held
                .iter()
                .map(|held| held.name.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            return format!(
                "Придержано процессов: {} ({}). Всё вернётся на место, как только вы                  тронете мышь.",
                self.holding(),
                names.join(", "),
            );
        }
        if user_idle_ms < IDLE_BEFORE_ACTING_MS {
            let left = (IDLE_BEFORE_ACTING_MS - user_idle_ms) / 60_000 + 1;
            return format!(
                "Автоматика включена и ждёт. Пока вы за компьютером, она ничего                  не трогает — вмешается не раньше чем через {left} мин без вас."
            );
        }
        "Автоматика включена, но придерживать нечего: фоновой работы, которая          мешала бы, сейчас нет."
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggestion(pid: u32, name: &str, remedy: Remedy) -> Suggestion {
        Suggestion {
            pid,
            process_name: name.to_string(),
            remedy,
            reason: "причина".into(),
            effect: "следствие".into(),
        }
    }

    #[test]
    fn a_disabled_autopilot_does_nothing() {
        let mut pilot = Autopilot::new();
        let plan = pilot.decide(u64::MAX, &[suggestion(100, "updater.exe", Remedy::EcoQos)]);
        assert!(plan.is_empty());
    }

    #[test]
    fn nothing_happens_while_the_user_is_present() {
        // Главное правило: пока человек за компьютером, компьютер его.
        let mut pilot = Autopilot::new();
        pilot.set_enabled(true);

        let plan = pilot.decide(1000, &[suggestion(100, "updater.exe", Remedy::EcoQos)]);
        assert!(plan.apply.is_empty(), "{plan:?}");
        assert_eq!(pilot.holding(), 0);
    }

    #[test]
    fn background_work_is_held_back_once_the_user_is_away() {
        let mut pilot = Autopilot::new();
        pilot.set_enabled(true);

        let plan = pilot.decide(
            IDLE_BEFORE_ACTING_MS + 1,
            &[suggestion(100, "updater.exe", Remedy::EcoQos)],
        );
        assert_eq!(plan.apply.len(), 1);
        assert_eq!(plan.apply[0].pid, 100);
        assert_eq!(pilot.holding(), 1);
    }

    #[test]
    fn everything_is_released_the_moment_the_user_returns() {
        // Ради этого всё и затевалось. Человек, севший за придержанный
        // компьютер, — провал автоматики целиком.
        let mut pilot = Autopilot::new();
        pilot.set_enabled(true);
        pilot.decide(
            IDLE_BEFORE_ACTING_MS + 1,
            &[suggestion(100, "updater.exe", Remedy::EcoQos)],
        );

        let plan = pilot.decide(0, &[]);
        assert_eq!(plan.release.len(), 1);
        assert_eq!(pilot.holding(), 0, "после возврата держать нечего");
    }

    #[test]
    fn the_same_process_is_not_held_twice() {
        let mut pilot = Autopilot::new();
        pilot.set_enabled(true);
        let items = [suggestion(100, "updater.exe", Remedy::EcoQos)];

        pilot.decide(IDLE_BEFORE_ACTING_MS + 1, &items);
        let again = pilot.decide(IDLE_BEFORE_ACTING_MS + 2, &items);
        assert!(again.apply.is_empty(), "{again:?}");
        assert_eq!(pilot.holding(), 1);
    }

    #[test]
    fn irreversible_remedies_stay_with_the_human() {
        // Автоматика имеет право только на то, что умеет отменить.
        let mut pilot = Autopilot::new();
        pilot.set_enabled(true);

        let plan = pilot.decide(
            IDLE_BEFORE_ACTING_MS + 1,
            &[suggestion(100, "hung.exe", Remedy::JustSaying)],
        );
        assert!(plan.apply.is_empty(), "{plan:?}");
    }

    #[test]
    fn turning_the_autopilot_off_returns_everything() {
        let mut pilot = Autopilot::new();
        pilot.set_enabled(true);
        pilot.decide(
            IDLE_BEFORE_ACTING_MS + 1,
            &[suggestion(100, "updater.exe", Remedy::EcoQos)],
        );

        let returned = pilot.set_enabled(false);
        assert_eq!(returned.len(), 1, "выключение обязано вернуть придержанное");
        assert_eq!(pilot.holding(), 0);
    }

    #[test]
    fn no_more_than_a_handful_at_a_time() {
        let mut pilot = Autopilot::new();
        pilot.set_enabled(true);

        let many: Vec<Suggestion> = (0..20)
            .map(|n| suggestion(n, &format!("процесс{n}.exe"), Remedy::EcoQos))
            .collect();
        let plan = pilot.decide(IDLE_BEFORE_ACTING_MS + 1, &many);
        assert_eq!(plan.apply.len(), AT_MOST);
    }

    #[test]
    fn a_departed_process_is_forgotten() {
        let mut pilot = Autopilot::new();
        pilot.set_enabled(true);
        pilot.decide(
            IDLE_BEFORE_ACTING_MS + 1,
            &[suggestion(100, "updater.exe", Remedy::EcoQos)],
        );

        pilot.forget(100);
        assert_eq!(pilot.holding(), 0);
    }

    #[test]
    fn the_status_says_what_is_happening_in_every_state() {
        let mut pilot = Autopilot::new();
        assert!(pilot.status(0).contains("выключена"));

        pilot.set_enabled(true);
        assert!(pilot.status(0).contains("ждёт"), "{}", pilot.status(0));
        assert!(
            pilot.status(IDLE_BEFORE_ACTING_MS + 1).contains("нечего"),
            "{}",
            pilot.status(IDLE_BEFORE_ACTING_MS + 1)
        );

        pilot.decide(
            IDLE_BEFORE_ACTING_MS + 1,
            &[suggestion(100, "updater.exe", Remedy::EcoQos)],
        );
        let text = pilot.status(IDLE_BEFORE_ACTING_MS + 1);
        assert!(text.contains("updater.exe"), "{text}");
        assert!(text.contains("вернётся"), "{text}");
    }

    #[test]
    fn a_disabled_autopilot_still_returns_what_it_held() {
        // Выключили не через set_enabled, а как-то иначе — всё равно
        // ничего не должно остаться придержанным.
        let mut pilot = Autopilot::new();
        pilot.set_enabled(true);
        pilot.decide(
            IDLE_BEFORE_ACTING_MS + 1,
            &[suggestion(100, "updater.exe", Remedy::EcoQos)],
        );
        pilot.enabled = false;

        let plan = pilot.decide(IDLE_BEFORE_ACTING_MS + 1, &[]);
        assert_eq!(plan.release.len(), 1);
    }
}
