//! Сессия расследования по триггеру (ТЗ, раздел 7.3).
//!
//! Отличается от постоянной сессии на `Kernel-Process` не устройством,
//! а замыслом. Постоянная сессия дешёвая: десятки событий в минуту.
//! Эта — дорогая: файловые и сетевые события идут тысячами в секунду.
//! Держать её включённой круглосуточно значило бы стать той самой
//! нагрузкой, ради борьбы с которой Bamboo и написан.
//!
//! Поэтому она живёт короткими отрезками и только когда есть повод:
//! диск встал в очередь, всплеск сети, непонятная запись. Отрезок задаётся
//! заранее — сессия, которую забыли остановить, хуже, чем её отсутствие.
//!
//! Разбор событий здесь не делается: он зависит от провайдера и от версии
//! шаблона, и его место — в разборщиках. Задача этого модуля — включить
//! ровно то, что нужно, и выключить точно в срок.

#![forbid(unsafe_code)]

use bamboo_core::Result;
use bamboo_sys::etw::{self, Session};

/// Имя сессии расследования. Отдельное от постоянной: их не должно быть
/// возможно перепутать, а оставшуюся от прошлого запуска — подчистить.
pub const SESSION_NAME: &str = "Bamboo-Investigate";

/// Что расследуем.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    /// Кто нагружает накопитель.
    Disk,
    /// Кто пишет, создаёт и удаляет файлы. Чтения намеренно не включаются:
    /// их десятки тысяч в секунду, и расследованию они не нужны.
    Files,
    /// Кто ходит в сеть.
    Network,
}

impl Subject {
    fn provider(self) -> &'static windows_sys::core::GUID {
        match self {
            Subject::Disk => &etw::KERNEL_DISK_GUID,
            Subject::Files => &etw::KERNEL_FILE_GUID,
            Subject::Network => &etw::KERNEL_NETWORK_GUID,
        }
    }

    fn keywords(self) -> u64 {
        match self {
            Subject::Disk => etw::KEYWORD_DISK_ALL,
            Subject::Files => {
                etw::KEYWORD_FILE_NAME
                    | etw::KEYWORD_FILE_CREATE
                    | etw::KEYWORD_FILE_WRITE
                    | etw::KEYWORD_FILE_DELETE
                    | etw::KEYWORD_FILE_RENAME
            }
            Subject::Network => etw::KEYWORD_NET_IPV4 | etw::KEYWORD_NET_IPV6,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Subject::Disk => "накопитель",
            Subject::Files => "файлы",
            Subject::Network => "сеть",
        }
    }
}

/// Дольше этого сессия расследования не живёт.
///
/// Две минуты. Не «сколько понадобится»: поток событий здесь такой, что
/// забытая включённой сессия сама становится проблемой. Если за две минуты
/// причина не нашлась, честнее начать заново, чем греть машину.
pub const LONGEST_MS: u64 = 2 * 60 * 1000;

/// Идущее расследование.
///
/// Останавливается само при удалении: сессия ETW, пережившая программу,
/// продолжает писать в никуда и снимается только вручную.
pub struct Investigation {
    session: Session,
    subjects: Vec<Subject>,
}

impl Drop for Investigation {
    fn drop(&mut self) {
        let _ = self.session.stop();
    }
}

impl Investigation {
    /// Начинает расследование по указанным предметам.
    ///
    /// Пустой список — ошибка вызывающего, а не повод включить всё:
    /// «всё» здесь означает поток в тысячи событий в секунду.
    pub fn start(subjects: &[Subject]) -> Result<Investigation> {
        if subjects.is_empty() {
            return Err(bamboo_core::Error::Unsupported(
                "расследование без предмета: включать все провайдеры разом нельзя",
            ));
        }

        let session = Session::start(SESSION_NAME)?;
        for subject in subjects {
            session.enable_provider(subject.provider(), subject.keywords())?;
        }

        Ok(Investigation {
            session,
            subjects: subjects.to_vec(),
        })
    }

    /// Что расследуется.
    pub fn subjects(&self) -> &[Subject] {
        &self.subjects
    }

    /// Останавливает досрочно.
    pub fn stop(&mut self) -> Result<()> {
        self.session.stop()
    }
}

/// Проверяет, что отрезок расследования не выходит за предел.
///
/// Чистая функция ради одного: предел должен проверяться до запуска
/// сессии, а не после — забытая сессия и есть та беда, от которой
/// предел защищает.
pub fn clamp_duration(requested_ms: u64) -> u64 {
    requested_ms.clamp(1000, LONGEST_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_subject_has_its_own_provider() {
        // Перепутать провайдеры значило бы расследовать сеть, думая,
        // что смотришь на диск.
        let disk = etw::guid_to_u128(Subject::Disk.provider());
        let files = etw::guid_to_u128(Subject::Files.provider());
        let network = etw::guid_to_u128(Subject::Network.provider());

        assert_ne!(disk, files);
        assert_ne!(files, network);
        assert_ne!(disk, network);
    }

    #[test]
    fn file_investigation_does_not_ask_for_reads() {
        // Чтений десятки тысяч в секунду. Включить их значило бы утопить
        // расследование в шуме и нагрузить машину сильнее, чем виновник.
        const KEYWORD_FILE_READ: u64 = 0x100;
        assert_eq!(Subject::Files.keywords() & KEYWORD_FILE_READ, 0);
    }

    #[test]
    fn file_investigation_asks_for_names() {
        // Без имён события файлов бесполезны: «кто-то что-то записал».
        assert_ne!(Subject::Files.keywords() & etw::KEYWORD_FILE_NAME, 0);
    }

    #[test]
    fn network_investigation_covers_both_protocols() {
        let keywords = Subject::Network.keywords();
        assert_ne!(keywords & etw::KEYWORD_NET_IPV4, 0);
        assert_ne!(keywords & etw::KEYWORD_NET_IPV6, 0);
    }

    #[test]
    fn an_investigation_without_a_subject_is_refused() {
        // «Всё сразу» здесь означает поток в тысячи событий в секунду,
        // и молча его включать нельзя.
        let Err(error) = Investigation::start(&[]) else {
            panic!("пустой список обязан быть ошибкой");
        };
        assert!(error.to_string().contains("без предмета"), "{error}");
    }

    #[test]
    fn a_long_investigation_is_cut_to_the_limit() {
        // Сессия, которую забыли остановить, хуже, чем её отсутствие.
        assert_eq!(clamp_duration(u64::MAX), LONGEST_MS);
        assert_eq!(clamp_duration(30_000), 30_000);
    }

    #[test]
    fn a_zero_length_investigation_still_runs_a_moment() {
        // Ноль означал бы сессию, которая включилась и выключилась,
        // не увидев ни одного события, — то есть впустую нагрузила машину.
        assert_eq!(clamp_duration(0), 1000);
    }

    #[test]
    fn the_session_name_is_not_the_permanent_one() {
        // Перепутать их значило бы остановить постоянное наблюдение,
        // остановив расследование.
        assert_ne!(SESSION_NAME, crate::session::SESSION_NAME);
    }

    #[test]
    fn every_subject_has_a_name_for_people() {
        for subject in [Subject::Disk, Subject::Files, Subject::Network] {
            assert!(!subject.name().is_empty());
        }
    }
}
