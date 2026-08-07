//! Профили и правила автопереключения (ТЗ, раздел 11.4).

use crate::action::{Action, AutonomyMode};
use crate::whitelist::AppClass;

/// Именованный набор политик.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Profile {
    /// Уровни 1–2 автономно, наблюдение полное.
    #[default]
    Normal,
    /// Все действия приостановлены, виджет скрыт, расследования выключены.
    Game,
    /// Агрессивный EcoQoS для фоновых приложений.
    Battery,
    /// Полное молчание.
    Presentation,
    /// Исключены средства разработки и СУБД.
    Work,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::Normal => "Обычный",
            Profile::Game => "Игра",
            Profile::Battery => "Батарея",
            Profile::Presentation => "Презентация",
            Profile::Work => "Работа",
        }
    }

    /// Разрешены ли действия в этом профиле.
    pub fn actions_allowed(self) -> bool {
        !matches!(self, Profile::Game | Profile::Presentation)
    }

    /// Можно ли показывать уведомления.
    pub fn may_notify(self) -> bool {
        !matches!(self, Profile::Game | Profile::Presentation)
    }

    /// Разрешены ли тяжёлые ETW-расследования.
    pub fn investigations_allowed(self) -> bool {
        matches!(self, Profile::Normal | Profile::Work)
    }

    /// Классы приложений, которые профиль исключает сам.
    pub fn excluded_classes(self) -> &'static [AppClass] {
        match self {
            Profile::Work => &[AppClass::DevTools, AppClass::Databases],
            Profile::Game => &[AppClass::Games],
            _ => &[],
        }
    }

    /// Режим автономности, который навязывает профиль.
    pub fn autonomy(self, user_choice: AutonomyMode) -> AutonomyMode {
        if self.actions_allowed() {
            user_choice
        } else {
            AutonomyMode::Observe
        }
    }

    /// Пропускает ли профиль конкретное действие.
    pub fn allows(self, action: Action) -> bool {
        if !self.actions_allowed() {
            return false;
        }
        // В профиле «Батарея» экономия важнее отзывчивости, но ломать
        // систему всё равно нельзя: уровни риска работают как обычно.
        let _ = action;
        true
    }
}

/// Условия для автопереключения.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Situation {
    pub fullscreen_app: bool,
    pub presentation_mode: bool,
    pub on_battery: bool,
}

/// Профиль, который следует включить по текущей обстановке.
///
/// Возвращает `None`, если ничего переключать не надо и остаётся выбор
/// пользователя.
pub fn auto_profile(situation: &Situation) -> Option<Profile> {
    // Порядок важен: презентация перевешивает игру. Полноэкранный
    // показ слайдов выглядит для системы как полноэкранное приложение,
    // но молчать в нём надо строже.
    if situation.presentation_mode {
        return Some(Profile::Presentation);
    }
    if situation.fullscreen_app {
        return Some(Profile::Game);
    }
    if situation.on_battery {
        return Some(Profile::Battery);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn games_and_presentations_suspend_everything() {
        for profile in [Profile::Game, Profile::Presentation] {
            assert!(!profile.actions_allowed(), "{}", profile.name());
            assert!(!profile.may_notify(), "{}", profile.name());
            assert!(!profile.investigations_allowed(), "{}", profile.name());
        }
    }

    #[test]
    fn a_suspended_profile_overrides_the_user_choice() {
        assert_eq!(
            Profile::Game.autonomy(AutonomyMode::Assist),
            AutonomyMode::Observe
        );
        assert_eq!(
            Profile::Normal.autonomy(AutonomyMode::Assist),
            AutonomyMode::Assist
        );
    }

    #[test]
    fn no_action_passes_in_the_game_profile() {
        assert!(!Profile::Game.allows(Action::EnableEcoQos));
        assert!(Profile::Normal.allows(Action::EnableEcoQos));
    }

    #[test]
    fn work_profile_leaves_dev_tools_and_databases_alone() {
        let excluded = Profile::Work.excluded_classes();
        assert!(excluded.contains(&AppClass::DevTools));
        assert!(excluded.contains(&AppClass::Databases));
    }

    #[test]
    fn battery_keeps_working_but_quietly() {
        // Экономия — не повод перестать наблюдать.
        assert!(Profile::Battery.actions_allowed());
        assert!(!Profile::Battery.investigations_allowed());
    }

    #[test]
    fn presentation_beats_fullscreen() {
        // Показ слайдов на весь экран выглядит как полноэкранное
        // приложение, но молчать в нём надо строже.
        let situation = Situation {
            fullscreen_app: true,
            presentation_mode: true,
            on_battery: true,
        };
        assert_eq!(auto_profile(&situation), Some(Profile::Presentation));
    }

    #[test]
    fn fullscreen_beats_battery() {
        let situation = Situation {
            fullscreen_app: true,
            on_battery: true,
            ..Default::default()
        };
        assert_eq!(auto_profile(&situation), Some(Profile::Game));
    }

    #[test]
    fn an_ordinary_situation_does_not_override_the_user() {
        assert_eq!(auto_profile(&Situation::default()), None);
    }
}
