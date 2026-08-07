//! Учёт короткоживущих процессов (ТЗ, раздел 7.1).
//!
//! Решает фундаментальную проблему опроса: снимок раз в пять секунд
//! не видит процесс, живущий две. А именно короткоживущие процессы вызывают
//! внезапные фризы — пользователь открывает диспетчер задач, а там уже пусто.

use std::collections::HashMap;

use crate::event::{CompletedProcess, ProcessEvent};

/// Сколько незакрытых запусков держим, прежде чем забыть самые старые.
///
/// Процесс мог стартовать до включения трассировки или пережить её —
/// без ограничения такие записи копились бы бесконечно.
const MAX_PENDING: usize = 4096;

#[derive(Clone, Debug)]
struct Pending {
    parent_pid: u32,
    image_name: String,
    started_at_unix_ms: i64,
}

/// Сопоставляет события запуска и завершения.
#[derive(Debug, Default)]
pub struct ProcessTracker {
    pending: HashMap<u32, Pending>,
    /// Порядок появления, чтобы вытеснять самые старые записи.
    order: Vec<u32>,
    completed: Vec<CompletedProcess>,
}

impl ProcessTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Скармливает событие. Возвращает завершившийся процесс, если пара сошлась.
    pub fn observe(&mut self, event: &ProcessEvent) -> Option<CompletedProcess> {
        match event {
            ProcessEvent::Started {
                at_unix_ms,
                pid,
                parent_pid,
                image_name,
                ..
            } => {
                if self.pending.len() >= MAX_PENDING {
                    self.forget_oldest();
                }
                // Тот же PID мог остаться от процесса, чьё завершение мы
                // пропустили. Новый запуск затирает старую запись: PID
                // переиспользуются, и держать обе бессмысленно.
                if self
                    .pending
                    .insert(
                        *pid,
                        Pending {
                            parent_pid: *parent_pid,
                            image_name: image_name.clone(),
                            started_at_unix_ms: *at_unix_ms,
                        },
                    )
                    .is_none()
                {
                    self.order.push(*pid);
                }
                None
            }

            ProcessEvent::Stopped {
                at_unix_ms,
                pid,
                exit_code,
            } => {
                // Завершение без запуска — процесс жил до начала трассировки.
                // Времени жизни у него не вычислить, и придумывать нельзя.
                let started = self.pending.remove(pid)?;
                self.order.retain(|value| value != pid);

                let completed = CompletedProcess {
                    pid: *pid,
                    parent_pid: started.parent_pid,
                    image_name: started.image_name,
                    started_at_unix_ms: started.started_at_unix_ms,
                    lifetime_ms: (at_unix_ms - started.started_at_unix_ms).max(0),
                    exit_code: *exit_code,
                };
                self.completed.push(completed.clone());
                Some(completed)
            }
        }
    }

    fn forget_oldest(&mut self) {
        if self.order.is_empty() {
            return;
        }
        let oldest = self.order.remove(0);
        self.pending.remove(&oldest);
    }

    /// Сколько запусков ещё не получили пары.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn completed(&self) -> &[CompletedProcess] {
        &self.completed
    }

    /// Сводка по образам: сколько раз запускался и сколько суммарно прожил.
    ///
    /// Отсортирована по числу запусков. Именно это число объясняет фризы:
    /// один процесс на две секунды незаметен, двести таких за ночь — нет.
    pub fn summary(&self) -> Vec<ImageSummary> {
        let mut by_image: HashMap<&str, ImageSummary> = HashMap::new();

        for process in &self.completed {
            let entry = by_image
                .entry(process.image_name.as_str())
                .or_insert_with(|| ImageSummary {
                    image_name: process.image_name.clone(),
                    launches: 0,
                    total_lifetime_ms: 0,
                    shortest_ms: i64::MAX,
                    longest_ms: 0,
                });
            entry.launches += 1;
            entry.total_lifetime_ms += process.lifetime_ms;
            entry.shortest_ms = entry.shortest_ms.min(process.lifetime_ms);
            entry.longest_ms = entry.longest_ms.max(process.lifetime_ms);
        }

        let mut result: Vec<ImageSummary> = by_image.into_values().collect();
        result.sort_by(|a, b| {
            b.launches
                .cmp(&a.launches)
                .then(b.total_lifetime_ms.cmp(&a.total_lifetime_ms))
        });
        result
    }

    /// Процессы, которых не увидел бы опрос с заданным интервалом.
    pub fn invisible_to_polling(&self, poll_interval_ms: i64) -> Vec<&CompletedProcess> {
        self.completed
            .iter()
            .filter(|process| process.invisible_to_polling(poll_interval_ms))
            .collect()
    }
}

/// Сводка по одному образу.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageSummary {
    pub image_name: String,
    pub launches: u32,
    pub total_lifetime_ms: i64,
    pub shortest_ms: i64,
    pub longest_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(at: i64, pid: u32, name: &str) -> ProcessEvent {
        ProcessEvent::Started {
            at_unix_ms: at,
            pid,
            parent_pid: 4,
            image_name: name.to_string(),
            session_id: 1,
        }
    }

    fn stopped(at: i64, pid: u32) -> ProcessEvent {
        ProcessEvent::Stopped {
            at_unix_ms: at,
            pid,
            exit_code: 0,
        }
    }

    #[test]
    fn a_matched_pair_gives_a_lifetime() {
        let mut tracker = ProcessTracker::new();
        assert!(tracker
            .observe(&started(1000, 100, "TiWorker.exe"))
            .is_none());

        let completed = tracker.observe(&stopped(3500, 100)).unwrap();
        assert_eq!(completed.lifetime_ms, 2500);
        assert_eq!(completed.image_name, "TiWorker.exe");
    }

    #[test]
    fn a_stop_without_a_start_is_not_invented() {
        // Процесс жил до начала трассировки: времени жизни не вычислить.
        let mut tracker = ProcessTracker::new();
        assert!(tracker.observe(&stopped(1000, 100)).is_none());
        assert!(tracker.completed().is_empty());
    }

    #[test]
    fn a_reused_pid_does_not_produce_a_negative_lifetime() {
        let mut tracker = ProcessTracker::new();
        tracker.observe(&started(1000, 100, "первый.exe"));
        // Завершение первого пропустили, PID переиспользован.
        tracker.observe(&started(5000, 100, "второй.exe"));

        let completed = tracker.observe(&stopped(6000, 100)).unwrap();
        assert_eq!(completed.image_name, "второй.exe");
        assert_eq!(completed.lifetime_ms, 1000);
    }

    #[test]
    fn short_lived_processes_are_the_ones_polling_misses() {
        let mut tracker = ProcessTracker::new();
        tracker.observe(&started(0, 100, "быстрый.exe"));
        tracker.observe(&stopped(2000, 100));
        tracker.observe(&started(0, 200, "долгий.exe"));
        tracker.observe(&stopped(60_000, 200));

        let invisible = tracker.invisible_to_polling(5000);
        assert_eq!(invisible.len(), 1);
        assert_eq!(invisible[0].image_name, "быстрый.exe");
    }

    #[test]
    fn summary_ranks_by_launch_count() {
        let mut tracker = ProcessTracker::new();

        // Один долгий запуск и двести коротких: фризы объясняют вторые.
        tracker.observe(&started(0, 1, "долгий.exe"));
        tracker.observe(&stopped(600_000, 1));
        for index in 0..200u32 {
            let pid = 1000 + index;
            tracker.observe(&started(index as i64 * 1000, pid, "CompatTelRunner.exe"));
            tracker.observe(&stopped(index as i64 * 1000 + 800, pid));
        }

        let summary = tracker.summary();
        assert_eq!(summary[0].image_name, "CompatTelRunner.exe");
        assert_eq!(summary[0].launches, 200);
        assert_eq!(summary[0].shortest_ms, 800);
        assert_eq!(summary[1].image_name, "долгий.exe");
    }

    #[test]
    fn pending_starts_do_not_grow_without_bound() {
        let mut tracker = ProcessTracker::new();
        for pid in 0..(MAX_PENDING as u32 + 500) {
            tracker.observe(&started(pid as i64, pid, "залипший.exe"));
        }
        assert!(tracker.pending_count() <= MAX_PENDING);
    }

    #[test]
    fn a_clock_jump_backwards_does_not_make_lifetime_negative() {
        let mut tracker = ProcessTracker::new();
        tracker.observe(&started(10_000, 100, "app.exe"));
        let completed = tracker.observe(&stopped(5_000, 100)).unwrap();
        assert_eq!(completed.lifetime_ms, 0);
    }
}
