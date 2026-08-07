//! Наблюдение — результат работы анализатора.

use core::fmt;

/// Насколько всё серьёзно.
///
/// `Calm` — не «нечего сказать», а активное сообщение «проблем нет».
/// Индустрия оптимизаторов построена на генерации тревоги; Bamboo при
/// отсутствии проблем обязан прямо говорить, что их нет.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Calm,
    Notice,
    Warning,
}

impl Severity {
    /// Стоит ли по этому поводу вообще беспокоить пользователя.
    pub fn deserves_attention(self) -> bool {
        self != Severity::Calm
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObservationKind {
    /// Ресурс накопителя.
    SsdWear,
    /// Монотонный рост приватной памяти процесса.
    MemoryGrowth,
    /// Рост числа дескрипторов.
    HandleGrowth,
    /// Рост числа объектов GDI или User.
    GdiGrowth,
    /// Загрузка системы стала дольше.
    BootRegression,
    /// Система просыпается сама.
    Wakeups,
    /// Всплеск процессора в фоне.
    BackgroundCpu,
    /// Нагрузка на уровне драйверов.
    DriverLoad,
    /// Приложение простаивает, но тратит ресурсы.
    IdleApp,
}

impl ObservationKind {
    pub fn title(self) -> &'static str {
        match self {
            ObservationKind::SsdWear => "ресурс накопителя",
            ObservationKind::MemoryGrowth => "рост памяти",
            ObservationKind::HandleGrowth => "рост числа дескрипторов",
            ObservationKind::GdiGrowth => "рост числа объектов GDI",
            ObservationKind::BootRegression => "время загрузки",
            ObservationKind::Wakeups => "пробуждения",
            ObservationKind::BackgroundCpu => "фоновая нагрузка",
            ObservationKind::DriverLoad => "нагрузка драйверов",
            ObservationKind::IdleApp => "простаивающее приложение",
        }
    }
}

/// Зафиксированный анализатором факт.
#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
    pub kind: ObservationKind,
    pub severity: Severity,
    /// Уверенность 0..1. Не вероятность в строгом смысле, а оценка того,
    /// насколько данные соответствуют признаку.
    pub confidence: f32,
    /// Одна фраза для пользователя. Конкретная и измеримая.
    pub summary: String,
    /// Подробности: числа, атрибуция, что с этим делать.
    pub detail: Option<String>,
}

impl Observation {
    pub fn calm(kind: ObservationKind, summary: impl Into<String>) -> Self {
        Observation {
            kind,
            severity: Severity::Calm,
            confidence: 1.0,
            summary: summary.into(),
            detail: None,
        }
    }

    pub fn new(
        kind: ObservationKind,
        severity: Severity,
        confidence: f32,
        summary: impl Into<String>,
    ) -> Self {
        Observation {
            kind,
            severity,
            confidence: confidence.clamp(0.0, 1.0),
            summary: summary.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl fmt::Display for Observation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.summary)?;
        if let Some(detail) = &self.detail {
            write!(f, "\n{detail}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calm_observations_do_not_ask_for_attention() {
        let observation = Observation::calm(ObservationKind::SsdWear, "всё в порядке");
        assert!(!observation.severity.deserves_attention());
    }

    #[test]
    fn confidence_stays_in_range() {
        let over = Observation::new(ObservationKind::MemoryGrowth, Severity::Warning, 5.0, "x");
        assert_eq!(over.confidence, 1.0);

        let under = Observation::new(ObservationKind::MemoryGrowth, Severity::Warning, -1.0, "x");
        assert_eq!(under.confidence, 0.0);
    }
}
