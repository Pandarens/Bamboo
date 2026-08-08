//! Системный дифф (ТЗ, раздел 9.9).
//!
//! Еженедельный снимок служб, задач планировщика, элементов автозагрузки,
//! драйверов и установленных приложений, и дифф с предыдущим снимком.
//!
//! Софт часто устанавливает фоновые обновляторы молча. Сам факт «за неделю
//! добавилось три задачи планировщика» уже полезен без каких-либо действий:
//! пользователь узнаёт, что на его машине завелось, пока он не смотрел.
//!
//! Логика здесь чистая: два снимка на вход, список изменений на выход.

use std::collections::BTreeSet;

use crate::observation::{Observation, ObservationKind, Severity};

/// Категория отслеживаемых сущностей.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Service,
    ScheduledTask,
    StartupItem,
    Driver,
    Application,
}

impl Category {
    pub fn singular(self) -> &'static str {
        match self {
            Category::Service => "служба",
            Category::ScheduledTask => "задача планировщика",
            Category::StartupItem => "элемент автозагрузки",
            Category::Driver => "драйвер",
            Category::Application => "приложение",
        }
    }

    /// Форма для числа: 1 служба, 2 службы, 5 служб.
    pub fn plural(self, count: usize) -> String {
        let (one, few, many) = match self {
            Category::Service => ("служба", "службы", "служб"),
            Category::ScheduledTask => ("задача", "задачи", "задач"),
            Category::StartupItem => (
                "элемент автозагрузки",
                "элемента автозагрузки",
                "элементов автозагрузки",
            ),
            Category::Driver => ("драйвер", "драйвера", "драйверов"),
            Category::Application => ("приложение", "приложения", "приложений"),
        };
        let last_two = count % 100;
        let last = count % 10;
        let word = if (11..=14).contains(&last_two) {
            many
        } else {
            match last {
                1 => one,
                2..=4 => few,
                _ => many,
            }
        };
        format!("{count} {word}")
    }
}

/// Снимок системы: множества имён по категориям.
///
/// Имена, а не полные объекты: для диффа важен только факт появления
/// или исчезновения, а имя — устойчивый идентификатор.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemSnapshot {
    pub services: BTreeSet<String>,
    pub scheduled_tasks: BTreeSet<String>,
    pub startup_items: BTreeSet<String>,
    pub drivers: BTreeSet<String>,
    pub applications: BTreeSet<String>,
}

impl SystemSnapshot {
    pub fn new() -> Self {
        Self::default()
    }

    fn set(&self, category: Category) -> &BTreeSet<String> {
        match category {
            Category::Service => &self.services,
            Category::ScheduledTask => &self.scheduled_tasks,
            Category::StartupItem => &self.startup_items,
            Category::Driver => &self.drivers,
            Category::Application => &self.applications,
        }
    }
}

/// Изменения в одной категории.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CategoryDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl CategoryDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// Полный дифф двух снимков.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemDiff {
    pub services: CategoryDiff,
    pub scheduled_tasks: CategoryDiff,
    pub startup_items: CategoryDiff,
    pub drivers: CategoryDiff,
    pub applications: CategoryDiff,
}

impl SystemDiff {
    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
            && self.scheduled_tasks.is_empty()
            && self.startup_items.is_empty()
            && self.drivers.is_empty()
            && self.applications.is_empty()
    }

    fn by_category(&self, category: Category) -> &CategoryDiff {
        match category {
            Category::Service => &self.services,
            Category::ScheduledTask => &self.scheduled_tasks,
            Category::StartupItem => &self.startup_items,
            Category::Driver => &self.drivers,
            Category::Application => &self.applications,
        }
    }
}

const CATEGORIES: [Category; 5] = [
    Category::Service,
    Category::ScheduledTask,
    Category::StartupItem,
    Category::Driver,
    Category::Application,
];

/// Считает дифф: что появилось и что исчезло между снимками.
pub fn diff(before: &SystemSnapshot, after: &SystemSnapshot) -> SystemDiff {
    let one = |category: Category| {
        let old = before.set(category);
        let new = after.set(category);
        CategoryDiff {
            added: new.difference(old).cloned().collect(),
            removed: old.difference(new).cloned().collect(),
        }
    };

    SystemDiff {
        services: one(Category::Service),
        scheduled_tasks: one(Category::ScheduledTask),
        startup_items: one(Category::StartupItem),
        drivers: one(Category::Driver),
        applications: one(Category::Application),
    }
}

/// Превращает дифф в наблюдение для отчёта.
///
/// Пустой дифф даёт успокаивающий вывод: за неделю система не изменилась,
/// и это нормально. Появление нового — уведомление, а не тревога: сам факт
/// полезен, но никаких действий не требует.
pub fn observe(diff: &SystemDiff) -> Observation {
    if diff.is_empty() {
        return Observation::calm(
            ObservationKind::SystemDiff,
            "За неделю в автозагрузке, службах и задачах ничего не изменилось",
        );
    }

    let mut added_parts = Vec::new();
    let mut removed_parts = Vec::new();

    for category in CATEGORIES {
        let cat = diff.by_category(category);
        if !cat.added.is_empty() {
            added_parts.push(category.plural(cat.added.len()));
        }
        if !cat.removed.is_empty() {
            removed_parts.push(category.plural(cat.removed.len()));
        }
    }

    let mut summary = String::from("За неделю ");
    if !added_parts.is_empty() {
        summary.push_str(&format!("добавилось: {}", added_parts.join(", ")));
    }
    if !removed_parts.is_empty() {
        if !added_parts.is_empty() {
            summary.push_str("; ");
        }
        summary.push_str(&format!("исчезло: {}", removed_parts.join(", ")));
    }

    let detail = detail_lines(diff);
    Observation::new(ObservationKind::SystemDiff, Severity::Notice, 1.0, summary)
        .with_detail(detail)
}

fn detail_lines(diff: &SystemDiff) -> String {
    let mut lines = Vec::new();
    for category in CATEGORIES {
        let cat = diff.by_category(category);
        for name in &cat.added {
            lines.push(format!("+ {} {name}", category.singular()));
        }
        for name in &cat.removed {
            lines.push(format!("− {} {name}", category.singular()));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(services: &[&str], tasks: &[&str]) -> SystemSnapshot {
        SystemSnapshot {
            services: services.iter().map(|s| s.to_string()).collect(),
            scheduled_tasks: tasks.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn an_identical_pair_produces_an_empty_diff() {
        let snap = snapshot(&["SysMain", "Spooler"], &["Task1"]);
        let diff = diff(&snap, &snap);
        assert!(diff.is_empty());
        assert_eq!(observe(&diff).severity, Severity::Calm);
    }

    #[test]
    fn added_items_are_detected() {
        let before = snapshot(&["SysMain"], &[]);
        let after = snapshot(&["SysMain", "AcmeUpdater"], &["AcmeDailyCheck"]);

        let diff = diff(&before, &after);
        assert_eq!(diff.services.added, vec!["AcmeUpdater"]);
        assert_eq!(diff.scheduled_tasks.added, vec!["AcmeDailyCheck"]);
        assert!(diff.services.removed.is_empty());
    }

    #[test]
    fn removed_items_are_detected() {
        let before = snapshot(&["SysMain", "OldService"], &[]);
        let after = snapshot(&["SysMain"], &[]);

        let diff = diff(&before, &after);
        assert_eq!(diff.services.removed, vec!["OldService"]);
        assert!(diff.services.added.is_empty());
    }

    #[test]
    fn the_headline_names_the_categories_and_counts() {
        // Ровно требование ТЗ: «за неделю добавилось три задачи планировщика».
        let before = SystemSnapshot::new();
        let after = snapshot(&[], &["A", "B", "C"]);

        let observation = observe(&diff(&before, &after));
        assert!(observation.summary.contains("3 задачи"));
        assert_eq!(observation.severity, Severity::Notice);
    }

    #[test]
    fn russian_plurals_are_correct() {
        assert_eq!(Category::ScheduledTask.plural(1), "1 задача");
        assert_eq!(Category::ScheduledTask.plural(3), "3 задачи");
        assert_eq!(Category::ScheduledTask.plural(5), "5 задач");
        assert_eq!(Category::ScheduledTask.plural(11), "11 задач");
        assert_eq!(Category::Service.plural(2), "2 службы");
        assert_eq!(Category::Driver.plural(21), "21 драйвер");
    }

    #[test]
    fn the_detail_lists_each_change_with_a_sign() {
        let before = snapshot(&["Keep"], &[]);
        let after = snapshot(&["Keep", "New"], &[]);

        let observation = observe(&diff(&before, &after));
        let detail = observation.detail.unwrap();
        assert!(detail.contains("+ служба New"));
        assert!(
            !detail.contains("Keep"),
            "неизменившееся не должно попадать в дифф"
        );
    }

    #[test]
    fn additions_and_removals_appear_together() {
        let before = snapshot(&["Old"], &[]);
        let after = snapshot(&["New"], &[]);

        let summary = observe(&diff(&before, &after)).summary;
        assert!(summary.contains("добавилось"));
        assert!(summary.contains("исчезло"));
    }

    #[test]
    fn empty_snapshots_are_calm() {
        assert_eq!(
            observe(&diff(&SystemSnapshot::new(), &SystemSnapshot::new())).severity,
            Severity::Calm
        );
    }
}
