//! Именованный канал и правила доступа к нему (ТЗ, раздел 3.3).
//!
//! Здесь только имена и правила — сама работа с каналом требует `unsafe`
//! и живёт в `bamboo-sys`. Разделение осознанное: правила доступа должны
//! проверяться тестами, а не глазами при чтении кода с `unsafe`.

/// Имя канала для сессии.
///
/// Канал на сессию, а не один на всю машину: у каждого вошедшего
/// пользователя свой агент, и смешивать их потоки нельзя.
pub fn pipe_name(session_id: u32) -> String {
    format!("\\\\.\\pipe\\bamboo-{session_id}")
}

/// Кому разрешён доступ к каналу.
///
/// Список закрытый и проверяется тестом: добавление сюда `Everyone`
/// превращает Bamboo в способ повысить привилегии до SYSTEM.
pub const ALLOWED: &[&str] = &[
    "BUILTIN\\Administrators",
    "интерактивные пользователи сессии",
];

/// Кому доступ запрещён явно.
pub const DENIED: &[&str] = &["Everyone", "ANONYMOUS LOGON", "NETWORK"];

/// Обязательные флаги канала.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PipeGuard {
    /// Удалённые клиенты отвергаются на уровне системы.
    pub reject_remote_clients: bool,
    /// Режим сообщений, а не потока байт.
    pub message_mode: bool,
    /// Явный дескриптор безопасности, а не унаследованный.
    pub explicit_security_descriptor: bool,
    /// Проверка образа подключившегося клиента.
    pub verify_client_image: bool,
    /// Проверка SID клиента.
    pub verify_client_sid: bool,
}

impl PipeGuard {
    /// Набор, который обязан быть выставлен перед созданием канала.
    pub const REQUIRED: PipeGuard = PipeGuard {
        reject_remote_clients: true,
        message_mode: true,
        explicit_security_descriptor: true,
        verify_client_image: true,
        verify_client_sid: true,
    };

    /// Всё ли на месте. Возвращает список недостающего.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.reject_remote_clients {
            missing.push("не выставлен PIPE_REJECT_REMOTE_CLIENTS");
        }
        if !self.message_mode {
            missing.push("канал не в режиме сообщений");
        }
        if !self.explicit_security_descriptor {
            missing.push("нет явного дескриптора безопасности");
        }
        if !self.verify_client_image {
            missing.push("не проверяется образ клиента");
        }
        if !self.verify_client_sid {
            missing.push("не проверяется SID клиента");
        }
        missing
    }

    pub fn is_complete(&self) -> bool {
        self.missing().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_session_gets_its_own_pipe() {
        assert_ne!(pipe_name(1), pipe_name(2));
        assert!(pipe_name(1).starts_with("\\\\.\\pipe\\bamboo-"));
    }

    #[test]
    fn everyone_is_never_allowed() {
        // Разрешить Everyone — значит дать любому процессу право
        // просить SYSTEM-службу выполнить действие.
        for denied in DENIED {
            assert!(
                !ALLOWED.contains(denied),
                "{denied} не должен быть в списке разрешённых"
            );
        }
        assert!(DENIED.contains(&"Everyone"));
        assert!(DENIED.contains(&"ANONYMOUS LOGON"));
        assert!(DENIED.contains(&"NETWORK"));
    }

    #[test]
    fn the_required_set_is_complete() {
        assert!(PipeGuard::REQUIRED.is_complete());
        assert!(PipeGuard::REQUIRED.missing().is_empty());
    }

    #[test]
    fn every_missing_flag_is_named() {
        let nothing = PipeGuard {
            reject_remote_clients: false,
            message_mode: false,
            explicit_security_descriptor: false,
            verify_client_image: false,
            verify_client_sid: false,
        };
        assert_eq!(nothing.missing().len(), 5);
        assert!(!nothing.is_complete());
    }

    #[test]
    fn a_single_missing_flag_fails_the_check() {
        // Каждый флаг обязателен по отдельности: пропуск любого из них —
        // готовая уязвимость повышения привилегий.
        let variants: [fn(&mut PipeGuard); 5] = [
            |guard| guard.reject_remote_clients = false,
            |guard| guard.message_mode = false,
            |guard| guard.explicit_security_descriptor = false,
            |guard| guard.verify_client_image = false,
            |guard| guard.verify_client_sid = false,
        ];

        for disable in variants {
            let mut guard = PipeGuard::REQUIRED;
            disable(&mut guard);
            assert!(!guard.is_complete());
            assert_eq!(guard.missing().len(), 1);
        }
    }
}
