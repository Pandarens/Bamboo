//! Политики Bamboo: что можно делать, с чем и когда.
//!
//! Крейт про Windows не знает: на вход факты о процессе, на выход решение.
//! Это позволяет проверить всю логику предохранителей тестами, а не
//! экспериментами на живой системе — что для кода, способного остановить
//! службу или заморозить процесс, единственный приемлемый способ.

#![forbid(unsafe_code)]

pub mod action;
pub mod freeze;
pub mod learn;
pub mod notify;
pub mod profile;
pub mod whitelist;

pub use action::{may_apply_autonomously, Action, Autonomy, AutonomyMode};
pub use freeze::{may_freeze, FreezeFacts, FreezeVerdict};
pub use learn::{Learned, Rejections, REJECTIONS_TO_SILENCE};
pub use notify::{may_notify, NotifyContext, Silence};
pub use profile::{auto_profile, Profile, Situation};
pub use whitelist::{immutable_reason, AppClass, ProcessFacts, UserWhitelist};

/// Итог проверки предложения перед показом пользователю.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Можно применить без спроса.
    Apply,
    /// Можно предложить, но нужно подтверждение.
    Offer,
    /// Нельзя. Причина — для объяснения пользователю.
    Refuse(String),
}

/// Обстоятельства, в которых принимается решение.
pub struct Context<'a> {
    pub action: Action,
    pub process: ProcessFacts<'a>,
    pub app_key: &'a str,
    pub app_class: Option<AppClass>,
    pub profile: Profile,
    pub mode: AutonomyMode,
    /// Идёт ли период обучения (первые 14 суток).
    pub learning: bool,
    pub whitelist: &'a UserWhitelist,
}

/// Единая точка принятия решения.
///
/// Собирает все предохранители в одном месте, чтобы их нельзя было обойти,
/// вызвав действие мимо проверок.
pub fn decide(context: &Context<'_>) -> Decision {
    if let Some(reason) = immutable_reason(&context.process) {
        return Decision::Refuse(reason.to_string());
    }

    if context
        .whitelist
        .contains(context.app_key, context.app_class)
    {
        return Decision::Refuse("приложение в вашем белом списке".to_string());
    }

    if context
        .profile
        .excluded_classes()
        .iter()
        .any(|class| Some(*class) == context.app_class)
    {
        return Decision::Refuse(format!(
            "профиль «{}» не трогает эту категорию",
            context.profile.name()
        ));
    }

    if !context.profile.allows(context.action) {
        return Decision::Refuse(format!(
            "в профиле «{}» действия приостановлены",
            context.profile.name()
        ));
    }

    if may_apply_autonomously(context.action, context.mode, context.learning) {
        Decision::Apply
    } else {
        Decision::Offer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context<'a>(whitelist: &'a UserWhitelist, name: &'a str) -> Context<'a> {
        Context {
            action: Action::EnableEcoQos,
            process: ProcessFacts {
                image_name: name,
                session_id: 1,
                ..Default::default()
            },
            app_key: name,
            app_class: None,
            profile: Profile::Normal,
            mode: AutonomyMode::Assist,
            learning: false,
            whitelist,
        }
    }

    #[test]
    fn a_safe_action_on_an_ordinary_app_is_applied() {
        let whitelist = UserWhitelist::new();
        assert_eq!(decide(&context(&whitelist, "slack.exe")), Decision::Apply);
    }

    #[test]
    fn the_immutable_list_wins_over_everything() {
        let whitelist = UserWhitelist::new();
        let decision = decide(&context(&whitelist, "lsass.exe"));
        assert!(matches!(decision, Decision::Refuse(_)));
    }

    #[test]
    fn a_twice_rejected_app_is_never_offered_again() {
        let mut whitelist = UserWhitelist::new();
        whitelist.reject("slack.exe");
        whitelist.reject("slack.exe");

        let decision = decide(&context(&whitelist, "slack.exe"));
        assert!(matches!(decision, Decision::Refuse(_)));
    }

    #[test]
    fn during_learning_everything_is_only_offered() {
        let whitelist = UserWhitelist::new();
        let mut context = context(&whitelist, "slack.exe");
        context.learning = true;
        assert_eq!(decide(&context), Decision::Offer);
    }

    #[test]
    fn a_risky_action_is_offered_but_never_applied() {
        let whitelist = UserWhitelist::new();
        let mut context = context(&whitelist, "slack.exe");
        context.action = Action::DisableService;
        assert_eq!(decide(&context), Decision::Offer);
    }

    #[test]
    fn the_game_profile_refuses_everything() {
        let whitelist = UserWhitelist::new();
        let mut context = context(&whitelist, "slack.exe");
        context.profile = Profile::Game;
        assert!(matches!(decide(&context), Decision::Refuse(_)));
    }

    #[test]
    fn the_work_profile_protects_dev_tools() {
        let whitelist = UserWhitelist::new();
        let mut context = context(&whitelist, "postgres.exe");
        context.profile = Profile::Work;
        context.app_class = Some(AppClass::Databases);

        match decide(&context) {
            Decision::Refuse(reason) => assert!(reason.contains("Работа")),
            other => panic!("ожидался отказ, получили {other:?}"),
        }
    }

    #[test]
    fn refusals_always_explain_themselves() {
        let mut whitelist = UserWhitelist::new();
        whitelist.reject("x");
        whitelist.reject("x");

        for name in ["lsass.exe", "x"] {
            if let Decision::Refuse(reason) = decide(&context(&whitelist, name)) {
                assert!(!reason.is_empty(), "пустая причина отказа для {name}");
            }
        }
    }
}
