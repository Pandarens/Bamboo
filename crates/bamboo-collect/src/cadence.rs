//! Адаптивная частота опроса (ТЗ, раздел 6.2).
//!
//! Резидентная утилита не имеет права опрашивать систему раз в секунду
//! круглосуточно. Частота зависит от того, смотрит ли пользователь на данные,
//! от чего питается машина и занят ли экран игрой.

use core::time::Duration;

/// Режим опроса.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cadence {
    /// Виджет открыт — пользователь смотрит на цифры прямо сейчас.
    WidgetOpen,
    /// Обычный фон: питание от сети, пользователь активен.
    Active,
    /// Пользователь ушёл.
    UserIdle,
    /// Питание от батареи.
    Battery,
    /// Полноэкранное приложение. Тяжёлые анализаторы при этом выключены.
    FullScreen,
    /// Батарея на исходе.
    BatteryLow,
}

impl Cadence {
    pub fn interval(self) -> Duration {
        match self {
            Cadence::WidgetOpen => Duration::from_secs(1),
            Cadence::Active => Duration::from_secs(5),
            Cadence::UserIdle | Cadence::Battery => Duration::from_secs(15),
            Cadence::FullScreen => Duration::from_secs(30),
            Cadence::BatteryLow => Duration::from_secs(60),
        }
    }

    /// Разрешены ли в этом режиме тяжёлые анализаторы.
    pub fn heavy_analysis_allowed(self) -> bool {
        !matches!(self, Cadence::FullScreen | Cadence::BatteryLow)
    }
}

/// Условия, по которым выбирается частота.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Conditions {
    pub widget_open: bool,
    pub fullscreen: bool,
    pub on_battery: bool,
    pub battery_low: bool,
    pub idle_ms: u64,
}

/// Порог, после которого пользователь считается ушедшим.
const IDLE_THRESHOLD_MS: u64 = 5 * 60 * 1000;

/// Сколько раз подряд условие должно подтвердиться перед замедлением.
const CONFIRMATIONS_TO_SLOW_DOWN: u8 = 3;

impl Conditions {
    fn desired_cadence(&self) -> Cadence {
        // Порядок важен: виджет открыт — значит пользователь смотрит,
        // и это перевешивает всё остальное.
        if self.widget_open {
            Cadence::WidgetOpen
        } else if self.fullscreen {
            Cadence::FullScreen
        } else if self.battery_low {
            Cadence::BatteryLow
        } else if self.on_battery {
            Cadence::Battery
        } else if self.idle_ms >= IDLE_THRESHOLD_MS {
            Cadence::UserIdle
        } else {
            Cadence::Active
        }
    }
}

/// Переключатель частоты с гистерезисом.
///
/// Гистерезис несимметричный, и это осознанно. Ускоряемся сразу: человек
/// открыл виджет и должен увидеть живые цифры, а не подождать три тика.
/// Замедляемся только после нескольких подтверждений подряд, иначе на границе
/// «5 минут простоя» частота будет осциллировать от каждого дрожания мыши.
#[derive(Clone, Debug)]
pub struct CadenceController {
    current: Cadence,
    pending: Option<(Cadence, u8)>,
}

impl Default for CadenceController {
    fn default() -> Self {
        Self::new()
    }
}

impl CadenceController {
    pub fn new() -> Self {
        CadenceController {
            current: Cadence::Active,
            pending: None,
        }
    }

    pub fn current(&self) -> Cadence {
        self.current
    }

    pub fn interval(&self) -> Duration {
        self.current.interval()
    }

    /// Пересчитывает режим по текущим условиям и возвращает актуальный.
    pub fn update(&mut self, conditions: Conditions) -> Cadence {
        let desired = conditions.desired_cadence();

        if desired == self.current {
            self.pending = None;
            return self.current;
        }

        // Ускорение применяется немедленно.
        if desired.interval() < self.current.interval() {
            self.current = desired;
            self.pending = None;
            return self.current;
        }

        let confirmations = match self.pending {
            Some((cadence, count)) if cadence == desired => count + 1,
            _ => 1,
        };

        if confirmations >= CONFIRMATIONS_TO_SLOW_DOWN {
            self.current = desired;
            self.pending = None;
        } else {
            self.pending = Some((desired, confirmations));
        }

        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle() -> Conditions {
        Conditions {
            idle_ms: IDLE_THRESHOLD_MS + 1,
            ..Default::default()
        }
    }

    #[test]
    fn widget_beats_everything_else() {
        let conditions = Conditions {
            widget_open: true,
            fullscreen: true,
            on_battery: true,
            battery_low: true,
            idle_ms: 3_600_000,
        };
        assert_eq!(conditions.desired_cadence(), Cadence::WidgetOpen);
    }

    #[test]
    fn intervals_match_the_spec() {
        assert_eq!(Cadence::WidgetOpen.interval(), Duration::from_secs(1));
        assert_eq!(Cadence::Active.interval(), Duration::from_secs(5));
        assert_eq!(Cadence::UserIdle.interval(), Duration::from_secs(15));
        assert_eq!(Cadence::Battery.interval(), Duration::from_secs(15));
        assert_eq!(Cadence::FullScreen.interval(), Duration::from_secs(30));
        assert_eq!(Cadence::BatteryLow.interval(), Duration::from_secs(60));
    }

    #[test]
    fn heavy_analysis_is_off_in_games() {
        assert!(!Cadence::FullScreen.heavy_analysis_allowed());
        assert!(!Cadence::BatteryLow.heavy_analysis_allowed());
        assert!(Cadence::Active.heavy_analysis_allowed());
    }

    #[test]
    fn speeding_up_is_immediate() {
        let mut controller = CadenceController::new();
        controller.update(idle());
        controller.update(idle());
        controller.update(idle());
        assert_eq!(controller.current(), Cadence::UserIdle);

        let opened = Conditions {
            widget_open: true,
            ..Default::default()
        };
        assert_eq!(controller.update(opened), Cadence::WidgetOpen);
    }

    #[test]
    fn slowing_down_waits_for_confirmations() {
        let mut controller = CadenceController::new();
        assert_eq!(controller.update(idle()), Cadence::Active);
        assert_eq!(controller.update(idle()), Cadence::Active);
        assert_eq!(controller.update(idle()), Cadence::UserIdle);
    }

    #[test]
    fn a_single_mouse_twitch_does_not_reset_the_mode() {
        let mut controller = CadenceController::new();
        for _ in 0..5 {
            controller.update(idle());
        }
        assert_eq!(controller.current(), Cadence::UserIdle);

        // Пользователь дёрнул мышью и снова затих: частота не должна
        // прыгать туда-обратно.
        let active = Conditions::default();
        assert_eq!(controller.update(active), Cadence::Active);
        assert_eq!(controller.update(idle()), Cadence::Active);
        assert_eq!(controller.update(idle()), Cadence::Active);
        assert_eq!(controller.update(idle()), Cadence::UserIdle);
    }

    #[test]
    fn alternating_conditions_never_settle_on_the_slow_mode() {
        let mut controller = CadenceController::new();
        for _ in 0..20 {
            controller.update(idle());
            controller.update(Conditions::default());
        }
        assert_eq!(controller.current(), Cadence::Active);
    }
}
