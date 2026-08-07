//! Состояние «до» и обратные рецепты.
//!
//! Без снятого состояния «до» действие не выполняется. Это не перестраховка:
//! `prior_state` должен позволить восстановить систему даже если штатный
//! откат не сработает — например, если процесс уже перезапустился и
//! обратное действие применять некуда.

use core::fmt;

/// Снимок состояния цели до изменения.
///
/// Сериализуется в журнал текстом. Формат простой и разбираемый глазами:
/// человеку, который будет чинить систему руками, важнее читаемость,
/// чем компактность.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PriorState {
    fields: Vec<(String, String)>,
}

impl PriorState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, key: &str, value: impl fmt::Display) -> Self {
        self.fields.push((key.to_string(), value.to_string()));
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            "да" => Some(true),
            "нет" => Some(false),
            _ => None,
        }
    }

    pub fn get_u32(&self, key: &str) -> Option<u32> {
        self.get(key)?.parse().ok()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Разбирает состояние обратно.
    pub fn parse(text: &str) -> PriorState {
        let fields = text
            .split(';')
            .filter_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                let key = key.trim();
                if key.is_empty() {
                    return None;
                }
                Some((key.to_string(), value.trim().to_string()))
            })
            .collect();
        PriorState { fields }
    }
}

impl fmt::Display for PriorState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text: Vec<String> = self
            .fields
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        f.write_str(&text.join(";"))
    }
}

/// Помощник для булевых значений: в журнале они пишутся по-русски,
/// чтобы запись читалась человеком без расшифровки.
pub fn yes_no(value: bool) -> &'static str {
    if value {
        "да"
    } else {
        "нет"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_survives_a_round_trip() {
        let state = PriorState::new()
            .with("eco_qos", yes_no(false))
            .with("memory_priority", 5);

        let text = state.to_string();
        assert_eq!(text, "eco_qos=нет;memory_priority=5");

        let parsed = PriorState::parse(&text);
        assert_eq!(parsed.get_bool("eco_qos"), Some(false));
        assert_eq!(parsed.get_u32("memory_priority"), Some(5));
        assert_eq!(parsed, state);
    }

    #[test]
    fn a_missing_field_is_not_guessed() {
        let state = PriorState::new().with("eco_qos", yes_no(true));
        assert_eq!(state.get("нет-такого"), None);
        assert_eq!(state.get_u32("eco_qos"), None);
    }

    #[test]
    fn garbage_does_not_panic() {
        let parsed = PriorState::parse("мусор без равно;;=пусто;a=1");
        assert_eq!(parsed.get_u32("a"), Some(1));
    }

    #[test]
    fn an_empty_state_is_recognisable() {
        assert!(PriorState::new().is_empty());
        assert!(PriorState::parse("").is_empty());
    }
}
