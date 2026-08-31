//! Анализаторы Bamboo.
//!
//! Крейт не знает о Windows и не имеет права о ней узнать. На вход —
//! временные ряды и метаданные, на выход — наблюдения. Это позволяет
//! прогонять всю логику детекта на записанных трассах без живой системы:
//! проверить «правильно ли детектируется утечка», дёргая настоящий Windows,
//! невозможно.

#![forbid(unsafe_code)]

pub mod baseline;
pub mod boot;
pub mod driver;
pub mod freeze;
pub mod growth;
pub mod idle;
pub mod observation;
pub mod origin;
pub mod record;
pub mod regression;
pub mod report;
pub mod slowstart;
pub mod spike;
pub mod suggest;
pub mod sysdiff;
pub mod systemio;
pub mod tbw;
pub mod wear;

pub use baseline::{Baseline, Learning};
pub use boot::{BootPoint, BootVerdict};
pub use driver::DriverInput;
pub use freeze::{used_share, FreezeCause, FreezeLog, Moment};
pub use growth::{memory_trend, GrowthInput, MemoryTrend, Point};
pub use idle::IdleInput;
pub use observation::{Observation, ObservationKind, Severity};
pub use origin::{attribute, Origin, ProcessDescriptor};
pub use record::{analyse as analyse_recording, Bottleneck, Sample, Verdict};
pub use regression::{fit, Trend};
pub use report::{weekly_html, weekly_json, weekly_markdown, ActionEffect, WeeklyData};
pub use slowstart::{slow_starters, BootCost, SlowStarter, StartupEntry};
pub use spike::SpikeInput;
pub use suggest::{suggest, Remedy, Situation, Suggestion};
pub use sysdiff::{diff as system_diff, SystemDiff, SystemSnapshot};
pub use systemio::{explain as explain_system_io, Bystanders, SystemIoCause, SystemIoVerdict};
pub use tbw::{rating_for, TbwRating};
pub use wear::{WearInput, WearVerdict};

/// Замок языка для тестов.
///
/// Язык — одно значение на процесс, а тестовый бинарник крейта гоняет
/// модули параллельно. Замок в каждом модуле по отдельности не защищает
/// от соседнего, поэтому он один на крейт.
#[cfg(test)]
pub(crate) static LANGUAGE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod translation_tests {
    use bamboo_core::{set_language, Language};

    use crate::LANGUAGE_LOCK;

    /// Прогоняет каждое объяснение на обоих языках.
    ///
    /// Сторож от полупереведённого вывода. Забыть перевести новую ветку
    /// легко: код соберётся, тесты пройдут, и заметит недосмотр только
    /// человек, выбравший английский, — то есть тот, кому уже поздно.
    fn on_both<T: Copy>(values: &[T], text: impl Fn(T) -> &'static str) {
        let _guard = LANGUAGE_LOCK.lock().unwrap();

        for value in values {
            set_language(Language::Russian);
            let russian = text(*value);
            set_language(Language::English);
            let english = text(*value);

            assert!(!russian.trim().is_empty());
            assert!(!english.trim().is_empty());
            assert_ne!(
                russian, english,
                "строка не переведена: осталась одинаковой на обоих языках"
            );
            assert!(
                !english
                    .chars()
                    .any(|c| ('\u{0410}'..='\u{044f}').contains(&c)),
                "в английском тексте осталась кириллица: {english}"
            );
        }
        set_language(Language::Russian);
    }

    #[test]
    fn every_bottleneck_speaks_both_languages() {
        use crate::record::Bottleneck;
        const ALL: [Bottleneck; 5] = [
            Bottleneck::Gpu,
            Bottleneck::Cpu,
            Bottleneck::Memory,
            Bottleneck::Disk,
            Bottleneck::Nothing,
        ];
        on_both(&ALL, |value| value.name());
        on_both(&ALL, |value| value.advice());
    }

    #[test]
    fn every_freeze_cause_speaks_both_languages() {
        use crate::freeze::FreezeCause;
        const ALL: [FreezeCause; 3] = [
            FreezeCause::DiskQueue,
            FreezeCause::DriverTime,
            FreezeCause::MemoryPressure,
        ];
        on_both(&ALL, |value| value.name());
        on_both(&ALL, |value| value.advice());
    }

    #[test]
    fn the_english_advice_stays_workable() {
        // То же требование, что и к русскому: совет обязан говорить,
        // что делать, а не «перезагрузите компьютер».
        let _guard = LANGUAGE_LOCK.lock().unwrap();
        set_language(Language::English);

        for advice in [
            crate::record::Bottleneck::Gpu.advice(),
            crate::record::Bottleneck::Cpu.advice(),
            crate::freeze::FreezeCause::DiskQueue.advice(),
        ] {
            assert!(advice.len() > 80, "совет слишком общий: {advice}");
            assert!(
                !advice.to_lowercase().contains("reboot"),
                "бесполезный совет: {advice}"
            );
        }
        set_language(Language::Russian);
    }
}
