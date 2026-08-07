//! События жизненного цикла процессов.

/// Что произошло с процессом.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessEvent {
    Started {
        at_unix_ms: i64,
        pid: u32,
        parent_pid: u32,
        image_name: String,
        session_id: u32,
    },
    Stopped {
        at_unix_ms: i64,
        pid: u32,
        exit_code: u32,
    },
}

impl ProcessEvent {
    pub fn at_unix_ms(&self) -> i64 {
        match self {
            ProcessEvent::Started { at_unix_ms, .. } => *at_unix_ms,
            ProcessEvent::Stopped { at_unix_ms, .. } => *at_unix_ms,
        }
    }

    pub fn pid(&self) -> u32 {
        match self {
            ProcessEvent::Started { pid, .. } => *pid,
            ProcessEvent::Stopped { pid, .. } => *pid,
        }
    }

    /// Приблизительный вес события в памяти. Нужен, чтобы держать
    /// кольцевой буфер в обещанных 4–8 МБ.
    pub fn size_bytes(&self) -> usize {
        let base = core::mem::size_of::<ProcessEvent>();
        match self {
            ProcessEvent::Started { image_name, .. } => base + image_name.len(),
            ProcessEvent::Stopped { .. } => base,
        }
    }
}

/// Завершившийся процесс с известным временем жизни.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletedProcess {
    pub pid: u32,
    pub parent_pid: u32,
    pub image_name: String,
    pub started_at_unix_ms: i64,
    pub lifetime_ms: i64,
    pub exit_code: u32,
}

impl CompletedProcess {
    /// Прожил ли процесс меньше, чем интервал опроса.
    ///
    /// Ровно эти процессы обычный мониторинг не видит: к моменту следующего
    /// снимка их уже нет.
    pub fn invisible_to_polling(&self, poll_interval_ms: i64) -> bool {
        self.lifetime_ms < poll_interval_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_two_second_process_is_invisible_to_a_five_second_poll() {
        let process = CompletedProcess {
            pid: 100,
            parent_pid: 4,
            image_name: "CompatTelRunner.exe".into(),
            started_at_unix_ms: 0,
            lifetime_ms: 2_000,
            exit_code: 0,
        };
        assert!(process.invisible_to_polling(5_000));
        assert!(!process.invisible_to_polling(1_000));
    }

    #[test]
    fn event_size_counts_the_name() {
        let with_name = ProcessEvent::Started {
            at_unix_ms: 0,
            pid: 1,
            parent_pid: 2,
            image_name: "очень-длинное-имя.exe".into(),
            session_id: 1,
        };
        let without = ProcessEvent::Stopped {
            at_unix_ms: 0,
            pid: 1,
            exit_code: 0,
        };
        assert!(with_name.size_bytes() > without.size_bytes());
    }
}
