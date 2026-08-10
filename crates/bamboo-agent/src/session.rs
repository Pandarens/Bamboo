//! Запись наблюдения за программой.
//!
//! Собирает замеры из тех же снимков, что питают всё остальное окно:
//! отдельного опроса системы здесь нет и не нужно — данные уже есть,
//! их надо только сохранить и сложить по программе.
//!
//! Складываем именно по программе, а не по процессу. Игра запускает
//! вспомогательные процессы — лаунчер, античит, — и они тратят те же
//! ресурсы. Человек спрашивает «сколько занимает игра», а не «сколько
//! занимает вот этот её процесс».

#![forbid(unsafe_code)]

use bamboo_analyze::record::Sample;

use crate::collector::Snapshot;

/// Идущая запись.
pub struct Session {
    /// За какой программой наблюдаем.
    app: String,
    samples: Vec<Sample>,
    /// Время первого замера по монотонным часам.
    started_at: Option<u64>,
}

/// Сколько замеров храним.
///
/// Час наблюдения при замере раз в секунду. Дольше держать незачем: запись
/// делают под конкретный сеанс игры, а не круглосуточно, и неограниченный
/// список рано или поздно съел бы память — ровно то, за чем Bamboo следит
/// у других.
const KEEP: usize = 3600;

impl Session {
    pub fn start(app: &str) -> Session {
        Session {
            app: app.to_string(),
            samples: Vec::new(),
            started_at: None,
        }
    }

    pub fn app(&self) -> &str {
        &self.app
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    /// Добавляет замер из очередного снимка.
    ///
    /// `now_ms` — монотонные часы: время записи считается от первого замера,
    /// а не от стенных часов, которые могут перевестись посреди наблюдения.
    pub fn observe(&mut self, snapshot: &Snapshot, now_ms: u64) {
        let started = *self.started_at.get_or_insert(now_ms);

        let mut cpu = 0.0_f32;
        let mut memory = 0_u64;
        let mut disk = 0_u64;
        let mut gpu = 0.0_f32;
        let mut gpu_known = false;
        let mut alive = false;

        for line in &snapshot.top {
            if !line.name.eq_ignore_ascii_case(&self.app) {
                continue;
            }
            alive = true;
            cpu += line.cpu_percent;
            memory += line.memory.as_u64();
            disk += line.read_per_second.saturating_add(line.write_per_second);

            if let Some((_, percent)) = snapshot.gpu.iter().find(|(pid, _)| *pid == line.pid) {
                gpu += percent;
                gpu_known = true;
            }
        }

        // Программа закрылась — записывать нечего. Ноль на графике означал бы
        // «простаивала», а это неправда: её просто не стало.
        if !alive {
            return;
        }

        // Счётчиков видеокарты может не быть вовсе. Отличать «не измеряли»
        // от «ноль» обязательно: иначе разбор решит, что видеокарта
        // простаивала, и назовёт узким местом что-нибудь другое.
        let gpu_percent = (gpu_known || !snapshot.gpu.is_empty()).then_some(gpu);

        self.samples.push(Sample {
            at_ms: now_ms.saturating_sub(started),
            cpu_percent: cpu,
            memory: bamboo_core::Bytes(memory),
            disk_per_second: disk,
            gpu_percent,
            memory_pressure: snapshot.memory_pressure(),
        });

        if self.samples.len() > KEEP {
            self.samples.remove(0);
        }
    }

    /// Сколько идёт запись, словами.
    pub fn spell_length(&self) -> String {
        let seconds = self.samples.last().map_or(0, |sample| sample.at_ms) / 1000;
        if seconds < 60 {
            format!("{seconds} с")
        } else {
            format!("{} мин {} с", seconds / 60, seconds % 60)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::ProcessLine;

    fn line(name: &str, pid: u32, cpu: f32, memory_mb: u64) -> ProcessLine {
        ProcessLine {
            name: name.to_string(),
            pid,
            cpu_percent: cpu,
            memory: bamboo_core::Bytes(memory_mb << 20),
            ..Default::default()
        }
    }

    fn snapshot(lines: Vec<ProcessLine>, gpu: Vec<(u32, f32)>) -> Snapshot {
        Snapshot {
            top: lines,
            gpu,
            ..Default::default()
        }
    }

    #[test]
    fn all_processes_of_one_program_are_added_up() {
        // Игра запускает лаунчер и античит, и они тратят те же ресурсы.
        // Человек спрашивает «сколько занимает игра».
        let mut session = Session::start("game.exe");
        session.observe(
            &snapshot(
                vec![
                    line("game.exe", 100, 40.0, 2000),
                    line("game.exe", 101, 10.0, 500),
                    line("chrome.exe", 200, 90.0, 4000),
                ],
                vec![(100, 85.0), (200, 3.0)],
            ),
            0,
        );

        let sample = session.samples()[0];
        assert!((sample.cpu_percent - 50.0).abs() < 0.01, "{sample:?}");
        assert_eq!(sample.memory.as_u64(), 2500 << 20);
        // Чужая нагрузка на видеокарту в запись не попадает.
        assert_eq!(sample.gpu_percent, Some(85.0));
    }

    #[test]
    fn time_is_counted_from_the_first_sample() {
        let mut session = Session::start("game.exe");
        let frame = snapshot(vec![line("game.exe", 100, 10.0, 100)], vec![]);

        session.observe(&frame, 500_000);
        session.observe(&frame, 501_000);

        assert_eq!(session.samples()[0].at_ms, 0);
        assert_eq!(session.samples()[1].at_ms, 1000);
    }

    #[test]
    fn a_closed_program_is_not_recorded_as_idle() {
        // Ноль на графике означал бы «простаивала». Её просто не стало —
        // это разные вещи, и вторая не должна выглядеть как первая.
        let mut session = Session::start("game.exe");
        session.observe(&snapshot(vec![line("game.exe", 100, 10.0, 100)], vec![]), 0);
        session.observe(
            &snapshot(vec![line("other.exe", 200, 10.0, 100)], vec![]),
            1000,
        );

        assert_eq!(session.len(), 1);
    }

    #[test]
    fn a_missing_gpu_counter_is_told_apart_from_zero_load() {
        // Если счётчиков нет, разбор не должен решить, что видеокарта
        // простаивала, и назвать узким местом что-нибудь другое.
        let mut session = Session::start("game.exe");
        session.observe(&snapshot(vec![line("game.exe", 100, 10.0, 100)], vec![]), 0);
        assert_eq!(session.samples()[0].gpu_percent, None);

        // А вот когда счётчики есть, но эта программа видеокарту не трогает,
        // ноль — настоящий ноль.
        let mut other = Session::start("game.exe");
        other.observe(
            &snapshot(vec![line("game.exe", 100, 10.0, 100)], vec![(999, 50.0)]),
            0,
        );
        assert_eq!(other.samples()[0].gpu_percent, Some(0.0));
    }

    #[test]
    fn the_name_is_matched_regardless_of_case() {
        let mut session = Session::start("Game.exe");
        session.observe(&snapshot(vec![line("GAME.EXE", 100, 10.0, 100)], vec![]), 0);
        assert_eq!(session.len(), 1);
    }

    #[test]
    fn a_long_recording_does_not_grow_without_bound() {
        // Bamboo следит за ростом памяти у других и не имеет права расти сам.
        let mut session = Session::start("game.exe");
        let frame = snapshot(vec![line("game.exe", 100, 10.0, 100)], vec![]);

        for tick in 0..(KEEP as u64 + 100) {
            session.observe(&frame, tick * 1000);
        }
        assert_eq!(session.len(), KEEP);
    }

    #[test]
    fn the_length_is_spelled_for_people() {
        let mut session = Session::start("game.exe");
        let frame = snapshot(vec![line("game.exe", 100, 10.0, 100)], vec![]);

        session.observe(&frame, 0);
        session.observe(&frame, 45_000);
        assert_eq!(session.spell_length(), "45 с");

        session.observe(&frame, 125_000);
        assert_eq!(session.spell_length(), "2 мин 5 с");
    }

    #[test]
    fn an_empty_session_has_a_length_of_zero() {
        assert_eq!(Session::start("game.exe").spell_length(), "0 с");
    }
}
