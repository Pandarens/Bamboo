//! Экспорт и импорт трасс (ТЗ, раздел 8.5).
//!
//! Коллектор умеет писать сырой поток сэмплов в файл, чтобы потом прогнать
//! анализаторы на реальных данных без живой системы. Назначение двойное:
//! тесты анализаторов в CI и возможность пользователю приложить трассу
//! к issue — тогда разработчик воспроизведёт проблему, не имея доступа
//! к машине.
//!
//! Формат: длина-префикс, затем bincode, сжатый zstd. Так трасса за часы
//! наблюдения укладывается в разумный размер.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// Версия формата трассы. Растёт при несовместимых изменениях.
pub const TRACE_VERSION: u16 = 1;

/// Магическая подпись в начале файла: «BMBT» плюс версия.
/// Ловит попытку скормить не-трассу до распаковки.
const MAGIC: &[u8; 4] = b"BMBT";

/// Один процесс в кадре трассы.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceProcess {
    pub app_key: String,
    pub image_name: String,
    /// Процессорное время за интервал, миллисекунды.
    pub cpu_ms: u32,
    pub private_kib: u32,
    pub working_set_kib: u32,
    pub read_kib: u32,
    pub write_kib: u32,
    pub handles: u32,
    pub threads: u32,
}

/// Кадр трассы: снимок всех процессов на один тик.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceFrame {
    pub at_unix_ms: i64,
    pub processes: Vec<TraceProcess>,
}

/// Записанная трасса.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trace {
    /// Номинальный интервал между кадрами, миллисекунды.
    pub interval_ms: u32,
    pub frames: Vec<TraceFrame>,
}

impl Trace {
    pub fn new(interval_ms: u32) -> Self {
        Trace {
            interval_ms,
            frames: Vec::new(),
        }
    }

    pub fn push(&mut self, frame: TraceFrame) {
        self.frames.push(frame);
    }

    /// Собирает временной ряд приватной памяти по приложению.
    ///
    /// Ровно то, что нужно анализатору роста: пары «время, байты».
    /// Для приложения, встречающегося не в каждом кадре, пропуски
    /// не заполняются — анализатор работает по тем точкам, что есть.
    pub fn private_series(&self, app_key: &str) -> Vec<(u64, f64)> {
        self.frames
            .iter()
            .filter_map(|frame| {
                frame
                    .processes
                    .iter()
                    .find(|process| process.app_key == app_key)
                    .map(|process| {
                        (
                            frame.at_unix_ms as u64,
                            (process.private_kib as f64) * 1024.0,
                        )
                    })
            })
            .collect()
    }

    /// Все ключи приложений, встречающиеся в трассе.
    pub fn app_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .frames
            .iter()
            .flat_map(|frame| frame.processes.iter().map(|p| p.app_key.clone()))
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// Сериализует трассу в сжатый поток с заголовком.
    pub fn write_to(&self, mut sink: impl Write) -> std::io::Result<()> {
        let body = bincode::serialize(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let compressed = zstd::encode_all(body.as_slice(), 3)?;

        sink.write_all(MAGIC)?;
        sink.write_all(&TRACE_VERSION.to_le_bytes())?;
        // Длина-префикс сжатого тела.
        sink.write_all(&(compressed.len() as u32).to_le_bytes())?;
        sink.write_all(&compressed)?;
        Ok(())
    }

    /// Читает трассу из потока.
    pub fn read_from(mut source: impl Read) -> std::io::Result<Trace> {
        let invalid =
            |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string());

        let mut magic = [0u8; 4];
        source.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("это не файл трассы Bamboo"));
        }

        let mut version = [0u8; 2];
        source.read_exact(&mut version)?;
        let version = u16::from_le_bytes(version);
        if version != TRACE_VERSION {
            return Err(invalid("несовместимая версия формата трассы"));
        }

        let mut length = [0u8; 4];
        source.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;

        // Ограничение защищает от битого заголовка, объявившего гигабайты.
        const MAX_COMPRESSED: usize = 512 * 1024 * 1024;
        if length > MAX_COMPRESSED {
            return Err(invalid("объявленный размер трассы неправдоподобно велик"));
        }

        let mut compressed = vec![0u8; length];
        source.read_exact(&mut compressed)?;

        let body = zstd::decode_all(compressed.as_slice())?;
        bincode::deserialize(&body).map_err(|e| invalid(&e.to_string()))
    }

    /// Приблизительный размер трассы в памяти. Для контроля объёма.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(at_ms: i64, app: &str, private_kib: u32) -> TraceFrame {
        TraceFrame {
            at_unix_ms: at_ms,
            processes: vec![TraceProcess {
                app_key: app.to_string(),
                image_name: "app.exe".to_string(),
                private_kib,
                cpu_ms: 100,
                ..Default::default()
            }],
        }
    }

    #[test]
    fn a_trace_survives_a_write_read_round_trip() {
        let mut trace = Trace::new(5000);
        for minute in 0..10 {
            trace.push(frame(
                minute * 60_000,
                "app",
                100_000 + minute as u32 * 1000,
            ));
        }

        let mut buffer = Vec::new();
        trace.write_to(&mut buffer).unwrap();
        let back = Trace::read_from(buffer.as_slice()).unwrap();

        assert_eq!(back, trace);
        assert_eq!(back.interval_ms, 5000);
        assert_eq!(back.frame_count(), 10);
    }

    #[test]
    fn compression_actually_shrinks_a_repetitive_trace() {
        // Трасса за часы наблюдения сильно повторяется — сжатие обязано
        // давать выигрыш, иначе файлы будут неподъёмными.
        let mut trace = Trace::new(5000);
        for i in 0..2000 {
            trace.push(frame(i * 5000, "app", 100_000));
        }
        let raw = bincode::serialize(&trace).unwrap();

        let mut compressed = Vec::new();
        trace.write_to(&mut compressed).unwrap();

        assert!(
            compressed.len() < raw.len() / 5,
            "сжатие дало {} из {} — слишком слабо",
            compressed.len(),
            raw.len()
        );
    }

    #[test]
    fn a_private_series_is_extracted_per_app() {
        let mut trace = Trace::new(5000);
        trace.push(frame(0, "leaky", 100_000));
        trace.push(frame(60_000, "leaky", 200_000));
        // Другое приложение в том же кадре не должно попасть в ряд.
        trace.frames[1].processes.push(TraceProcess {
            app_key: "other".into(),
            private_kib: 999_999,
            ..Default::default()
        });

        let series = trace.private_series("leaky");
        assert_eq!(series.len(), 2);
        assert_eq!(series[0], (0, 100_000.0 * 1024.0));
        assert_eq!(series[1], (60_000, 200_000.0 * 1024.0));
    }

    #[test]
    fn app_keys_are_unique_and_sorted() {
        let mut trace = Trace::new(5000);
        trace.push(frame(0, "b", 1));
        trace.push(frame(1, "a", 1));
        trace.push(frame(2, "b", 1));
        assert_eq!(trace.app_keys(), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn garbage_is_rejected_not_decompressed() {
        assert!(Trace::read_from([0u8; 10].as_slice()).is_err());
        assert!(Trace::read_from(b"BMBTxxxxxxxx".as_slice()).is_err());
    }

    #[test]
    fn a_truncated_file_is_an_error_not_a_panic() {
        let mut trace = Trace::new(5000);
        trace.push(frame(0, "app", 100));
        let mut buffer = Vec::new();
        trace.write_to(&mut buffer).unwrap();

        // Обрезанный хвост.
        assert!(Trace::read_from(&buffer[..buffer.len() - 5]).is_err());
    }
}
