//! Наполнение главного окна данными (ТЗ, раздел 14.3).
//!
//! Разделы «Обзор» и «Процессы» берут данные из общего снимка коллектора.
//! «Диск», «Питание» и «Журнал» загружаются по требованию при открытии
//! раздела: это разовые запросы, а не поток, и держать их в фоне незачем.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use bamboo_analyze::WearInput;

use crate::collector::Snapshot;

/// Где лежит журнал действий агента.
fn journal_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("Bamboo").join("journal.db")
}

/// Строка процесса для таблицы главного окна.
pub struct ProcessRow {
    pub name: String,
    pub pid: String,
    pub cpu: String,
    pub memory: String,
    pub threads: String,
    pub badge: String,
    /// Рост памяти: «+40 МБ/ч» или пусто, если память не растёт.
    pub growth: String,
    /// Достаточно ли наблюдений, чтобы называть рост подозрением на утечку.
    /// От этого зависит только цвет строки — текст честен в обоих случаях.
    pub leak: bool,
    /// «Не отвечает», если окно процесса перестало разбирать сообщения.
    /// У процессов без окон пусто: зависать там нечему.
    pub state: String,
    pub hung: bool,
    /// Нагрузка на диск: «12.4 МБ/с». Пусто, если процесс диск не трогает.
    pub disk: String,
    /// Строка — группа из нескольких процессов.
    pub is_group: bool,
    /// Группа развёрнута. Стрелку рисует интерфейс: данные не должны
    /// нести в себе оформление.
    pub expanded: bool,
    /// Строка — процесс внутри развёрнутой группы.
    pub is_member: bool,
    /// Номера всех процессов группы через запятую: по ним завершается
    /// вся группа разом.
    pub member_pids: String,
}

/// По какому столбцу сортировать таблицу процессов.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Pid,
    Cpu,
    Memory,
    Threads,
    Growth,
    /// Зависшие окна — наверх: если что-то не отвечает, это первое,
    /// что человек хочет увидеть.
    State,
    /// Нагрузка на диск. Тот самый случай «кто забил диск на сто процентов»,
    /// ради которого в список и добавлен этот столбец.
    Disk,
}

impl SortColumn {
    /// Столбец по индексу из интерфейса. Неизвестный индекс — процессор:
    /// это разумное поведение по умолчанию, а не повод падать.
    pub fn from_index(index: i32) -> SortColumn {
        match index {
            0 => SortColumn::Name,
            1 => SortColumn::Pid,
            3 => SortColumn::Memory,
            4 => SortColumn::Threads,
            5 => SortColumn::Growth,
            6 => SortColumn::State,
            7 => SortColumn::Disk,
            _ => SortColumn::Cpu,
        }
    }
}

/// Сводка по программе: все её процессы вместе.
///
/// Chrome, Edge и почти всё современное — это десяток процессов одной
/// программы. По отдельности каждый выглядит скромно, а вместе они и
/// съедают память. Складываем их в одну строку, чтобы человек видел цену
/// программы, а не её кусочков.
pub struct AppGroup {
    pub name: String,
    /// Сколько процессов в группе.
    pub count: usize,
    pub cpu_percent: f32,
    pub memory: bamboo_core::Bytes,
    pub disk_per_second: u64,
    /// Самый заметный процесс группы: к нему применяются действия.
    pub lead_pid: u32,
    /// Хоть у одного окна нет отклика.
    pub hung: bool,
    /// Хоть у одного подозрение на утечку.
    pub leak: bool,
    /// Память ведущего процесса — по ней он и выбирается.
    lead_memory: u64,
    /// Все процессы группы: нужны, чтобы группу можно было развернуть
    /// и разобраться с ней по одному.
    pub members: Vec<GroupMember>,
}

/// Один процесс внутри группы.
#[derive(Clone, Debug)]
pub struct GroupMember {
    pub pid: u32,
    pub cpu_percent: f32,
    pub memory: bamboo_core::Bytes,
    pub disk_per_second: u64,
    pub hung: bool,
}

/// Группирует процессы по имени образа.
///
/// Именно по имени, а не по пути: у одной программы процессы лежат в одной
/// папке, а разные программы с одинаковым именем — редкость по сравнению
/// с пользой от того, что двадцать вкладок Chrome схлопнутся в строку.
pub fn group_by_app(snapshot: &Snapshot) -> Vec<AppGroup> {
    use std::collections::HashMap;

    let mut groups: HashMap<String, AppGroup> = HashMap::new();

    for line in &snapshot.top {
        let key = line.name.to_lowercase();
        let disk = line.read_per_second.saturating_add(line.write_per_second);

        match groups.get_mut(&key) {
            Some(group) => {
                group.count += 1;
                group.members.push(GroupMember {
                    pid: line.pid,
                    cpu_percent: line.cpu_percent,
                    memory: line.memory,
                    disk_per_second: disk,
                    hung: line.hung,
                });
                group.cpu_percent += line.cpu_percent;
                group.memory = bamboo_core::Bytes(group.memory.as_u64() + line.memory.as_u64());
                group.disk_per_second = group.disk_per_second.saturating_add(disk);
                group.hung |= line.hung;
                group.leak |= line.memory_growth.is_some_and(|trend| trend.suspected_leak);

                // Ведущим считаем самый прожорливый по памяти: действие
                // разумнее применить к нему, а не к случайному процессу.
                if line.memory.as_u64() > group.lead_memory {
                    group.lead_pid = line.pid;
                    group.lead_memory = line.memory.as_u64();
                }
            }
            None => {
                groups.insert(
                    key,
                    AppGroup {
                        name: line.name.clone(),
                        count: 1,
                        cpu_percent: line.cpu_percent,
                        memory: line.memory,
                        disk_per_second: disk,
                        lead_pid: line.pid,
                        lead_memory: line.memory.as_u64(),
                        members: vec![GroupMember {
                            pid: line.pid,
                            cpu_percent: line.cpu_percent,
                            memory: line.memory,
                            disk_per_second: disk,
                            hung: line.hung,
                        }],
                        hung: line.hung,
                        leak: line.memory_growth.is_some_and(|trend| trend.suspected_leak),
                    },
                );
            }
        }
    }

    groups.into_values().collect()
}

/// Сколько строк показываем в таблице.
///
/// Сортируем весь список, а показываем верхушку. Триста строк, которые
/// перерисовываются каждую секунду, стоят заметного процессора и памяти,
/// а глазами дальше первых десятков никто не смотрит. Важно, что обрезка
/// идёт **после** сортировки: при сортировке по памяти видны самые
/// прожорливые по памяти, а не случайные.
pub const VISIBLE_ROWS: usize = 80;

/// Готовит строки по программам: процессы одной программы схлопнуты.
///
/// Возвращает те же `ProcessRow`, что и обычный список, чтобы таблица
/// не знала о двух разных режимах: в поле `pid` уезжает ведущий процесс,
/// к нему и применяются действия.
pub fn grouped_rows(
    snapshot: &Snapshot,
    sort: SortColumn,
    descending: bool,
    expanded: &dyn Fn(&str) -> bool,
) -> Vec<ProcessRow> {
    let mut groups = group_by_app(snapshot);

    match sort {
        SortColumn::Name => groups.sort_by_key(|group| group.name.to_lowercase()),
        SortColumn::Memory => groups.sort_by_key(|group| group.memory.as_u64()),
        SortColumn::Disk => groups.sort_by_key(|group| group.disk_per_second),
        SortColumn::State => groups.sort_by_key(|group| group.hung),
        SortColumn::Growth => groups.sort_by_key(|group| group.leak),
        // По числу процессов сортировать полезнее, чем по PID ведущего:
        // у группы своего PID нет, а «сколько их» — осмысленный вопрос.
        SortColumn::Pid | SortColumn::Threads => groups.sort_by_key(|group| group.count),
        SortColumn::Cpu => groups.sort_by(|a, b| a.cpu_percent.total_cmp(&b.cpu_percent)),
    }
    if descending {
        groups.reverse();
    }

    let mut rows = Vec::new();
    for group in groups.into_iter().take(VISIBLE_ROWS) {
        let open = group.count > 1 && expanded(&group.name);

        rows.push(ProcessRow {
            name: group.name.clone(),
            pid: group.lead_pid.to_string(),
            cpu: format!("{:.1}%", group.cpu_percent),
            memory: group.memory.to_string(),
            // В режиме групп в колонке потоков полезнее число процессов:
            // сумма потоков у двадцати вкладок ничего не объясняет.
            threads: format!("{} проц.", group.count),
            badge: String::new(),
            growth: if group.leak {
                "утечка?".to_string()
            } else {
                String::new()
            },
            leak: group.leak,
            state: if group.hung {
                "не отвечает".to_string()
            } else {
                String::new()
            },
            hung: group.hung,
            disk: if group.disk_per_second >= 1024 {
                format!("{}/с", bamboo_core::Bytes(group.disk_per_second))
            } else {
                String::new()
            },
            is_group: group.count > 1,
            expanded: open,
            is_member: false,
            member_pids: group
                .members
                .iter()
                .map(|member| member.pid.to_string())
                .collect::<Vec<_>>()
                .join(","),
        });

        if !open {
            continue;
        }

        // Разворачиваем: внутри группы порядок по памяти, от крупного —
        // именно крупный процесс обычно и ищут.
        let mut members = group.members;
        members.sort_by_key(|member| core::cmp::Reverse(member.memory.as_u64()));

        for member in members {
            rows.push(ProcessRow {
                name: group.name.clone(),
                pid: member.pid.to_string(),
                cpu: format!("{:.1}%", member.cpu_percent),
                memory: member.memory.to_string(),
                threads: String::new(),
                badge: String::new(),
                growth: String::new(),
                leak: false,
                state: if member.hung {
                    "не отвечает".to_string()
                } else {
                    String::new()
                },
                hung: member.hung,
                disk: if member.disk_per_second >= 1024 {
                    format!("{}/с", bamboo_core::Bytes(member.disk_per_second))
                } else {
                    String::new()
                },
                is_group: false,
                expanded: false,
                is_member: true,
                member_pids: String::new(),
            });
        }
    }

    rows
}

/// Готовит строки процессов из снимка, отсортированные по столбцу.
///
/// Сортируем по сырым числам из снимка, а не по показанному тексту: иначе
/// «1.2 ГБ» оказалось бы меньше «900 МБ», потому что единица меньше девятки.
pub fn process_rows(snapshot: &Snapshot, sort: SortColumn, descending: bool) -> Vec<ProcessRow> {
    let mut lines: Vec<&crate::collector::ProcessLine> = snapshot.top.iter().collect();

    match sort {
        // Имя сравниваем без учёта регистра: иначе Windows-процессы
        // с большой буквы собрались бы отдельной кучей от остальных.
        SortColumn::Name => lines.sort_by_key(|line| line.name.to_lowercase()),
        SortColumn::Pid => lines.sort_by_key(|line| line.pid),
        SortColumn::Cpu => lines.sort_by(|a, b| a.cpu_percent.total_cmp(&b.cpu_percent)),
        SortColumn::Memory => lines.sort_by_key(|line| line.memory.as_u64()),
        SortColumn::Threads => lines.sort_by_key(|line| line.threads),
        // Процессы без роста идут как нули и остаются в конце при убывании.
        SortColumn::Growth => lines.sort_by(|a, b| growth_rate(a).total_cmp(&growth_rate(b))),
        SortColumn::State => lines.sort_by_key(|line| line.hung),
        SortColumn::Disk => lines.sort_by_key(disk_bytes),
    }
    if descending {
        lines.reverse();
    }

    lines
        .into_iter()
        .take(VISIBLE_ROWS)
        .map(|line| ProcessRow {
            name: line.name.clone(),
            pid: line.pid.to_string(),
            cpu: format!("{:.1}%", line.cpu_percent),
            memory: line.memory.to_string(),
            threads: line.threads.to_string(),
            badge: line.badge.clone(),
            growth: describe_growth(line),
            leak: line.memory_growth.is_some_and(|trend| trend.suspected_leak),
            // Про отвечающий процесс не пишем ничего: строка «отвечает»
            // у восьмидесяти процессов — это шум, а не сведения.
            state: if line.hung {
                "не отвечает".to_string()
            } else {
                String::new()
            },
            hung: line.hung,
            disk: describe_disk(line),
            is_group: false,
            expanded: false,
            is_member: false,
            member_pids: String::new(),
        })
        .collect()
}

/// Суммарная нагрузка процесса на диск, байт в секунду.
fn disk_bytes(line: &&crate::collector::ProcessLine) -> u64 {
    line.read_per_second.saturating_add(line.write_per_second)
}

/// Текст про нагрузку на диск.
///
/// Ниже килобайта в секунду не пишем ничего: у большинства процессов
/// постоянно капает по несколько байт, и восемьдесят строк с «0.1 КБ/с»
/// только мешали бы увидеть того, кто действительно грузит диск.
fn describe_disk(line: &crate::collector::ProcessLine) -> String {
    const FLOOR: u64 = 1024;

    let total = line.read_per_second.saturating_add(line.write_per_second);
    if total < FLOOR {
        return String::new();
    }
    format!("{}/с", bamboo_core::Bytes(total))
}

fn growth_rate(line: &crate::collector::ProcessLine) -> f64 {
    line.memory_growth.map_or(0.0, |trend| trend.mb_per_hour)
}

/// Текст про рост памяти.
///
/// Это то, чего не показывает диспетчер задач: он знает, сколько памяти
/// занято сейчас, но не знает, растёт ли она. Слово «утечка» позволяем себе
/// только когда за него ручается анализатор — в остальных случаях просто
/// называем измеренную скорость.
fn describe_growth(line: &crate::collector::ProcessLine) -> String {
    match line.memory_growth {
        None => String::new(),
        Some(trend) if trend.suspected_leak => {
            format!("утечка? +{:.0} МБ/ч", trend.mb_per_hour)
        }
        Some(trend) => format!("+{:.0} МБ/ч", trend.mb_per_hour),
    }
}

/// Строка накопителя.
pub struct DriveRow {
    pub title: String,
    pub facts: String,
    pub verdict: String,
}

/// Загружает накопители и их здоровье. Разовый запрос при открытии раздела.
pub fn drive_rows() -> (Vec<DriveRow>, String) {
    let mut rows = Vec::new();
    let drives = bamboo_sys::enumerate_drives();

    for info in &drives {
        let health = bamboo_sys::read_smart(info);

        let (facts, verdict) = match health {
            Ok(health) => {
                let mut facts = Vec::new();
                if let Some(t) = health.temperature_c {
                    facts.push(format!("{t} °C"));
                }
                if let Some(h) = health.power_on_hours {
                    facts.push(format!("наработка {h} ч"));
                }
                if let Some(w) = health.data_written {
                    facts.push(format!("записано {w}"));
                }

                let verdict = bamboo_analyze::wear::analyze(&WearInput {
                    drive_name: &info.display_name(),
                    capacity: info.capacity,
                    health: &health,
                    daily_write: None,
                    baseline_daily_write: None,
                    media_errors_grew: false,
                    top_writers: &[],
                });
                (facts.join(", "), verdict.observation.summary)
            }
            Err(error) => (
                String::new(),
                format!("здоровье прочитать не удалось: {error}"),
            ),
        };

        rows.push(DriveRow {
            title: format!("{} — {}, {}", info.display_name(), info.bus, info.capacity),
            facts,
            verdict,
        });
    }

    let note = if drives.iter().any(|d| d.bus.name() == "SATA") {
        "SMART у SATA доступен только с правами администратора — под обычным \
         пользователем здесь будет отказ, а не оценка."
            .to_string()
    } else {
        String::new()
    };

    (rows, note)
}

/// Строка пробуждения.
pub struct WakeRow {
    pub when: String,
    pub source: String,
}

/// Загружает историю пробуждений.
pub fn wake_rows() -> (Vec<WakeRow>, String) {
    match bamboo_sys::wake_history(20) {
        Ok(events) if !events.is_empty() => {
            let rows = events
                .iter()
                .map(|event| WakeRow {
                    when: date(event.at_unix_ms),
                    source: event.source.describe(),
                })
                .collect();
            (rows, String::new())
        }
        Ok(_) => (
            Vec::new(),
            "Пробуждений из сна в журнале нет — так бывает на машинах без \
             спящего режима."
                .to_string(),
        ),
        Err(error) => (Vec::new(), format!("не удалось прочитать: {error}")),
    }
}

/// Строка журнала.
pub struct JournalRow {
    pub when: String,
    pub action: String,
    pub target: String,
    pub status: String,
}

/// Загружает журнал действий.
pub fn journal_rows() -> (Vec<JournalRow>, String) {
    let Ok(journal) = bamboo_journal::Journal::open(journal_path()) else {
        return (Vec::new(), "журнал действий недоступен".to_string());
    };

    match journal.since(0) {
        Ok(entries) if !entries.is_empty() => {
            let rows = entries
                .iter()
                .map(|entry| JournalRow {
                    when: date(entry.at_unix_ms),
                    action: entry.action.name().to_string(),
                    target: entry.target.describe(),
                    status: entry.status.as_str().to_string(),
                })
                .collect();
            (rows, String::new())
        }
        Ok(_) => (
            Vec::new(),
            "Записей нет — Bamboo пока ничего не менял в системе.".to_string(),
        ),
        Err(_) => (Vec::new(), "журнал не читается".to_string()),
    }
}

/// Краткая сводка для карточек «Обзор».
pub struct Overview {
    pub cpu: String,
    pub memory: String,
    pub processes: String,
}

pub fn overview(snapshot: &Snapshot) -> Overview {
    Overview {
        cpu: format!("{:.0}%", snapshot.cpu_busy * 100.0),
        memory: format!("{} из {}", snapshot.memory_used, snapshot.memory_total),
        processes: snapshot.process_count.to_string(),
    }
}

/// Дата и время из миллисекунд эпохи Unix, в UTC.
fn date(unix_ms: i64) -> String {
    let total_seconds = unix_ms.div_euclid(1000);
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
    )
}

/// Календарная дата из числа дней от эпохи (алгоритм Хиннанта).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (mp + if mp < 10 { 3 } else { -9 }) as u32;
    (year + if month <= 2 { 1 } else { 0 }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::ProcessLine;
    use bamboo_core::Bytes;

    fn line(name: &str, pid: u32, cpu: f32, mib: u64, threads: u32) -> ProcessLine {
        ProcessLine {
            name: name.to_string(),
            pid,
            threads,
            cpu_percent: cpu,
            memory: Bytes::from_mib(mib),
            badge: String::new(),
            memory_growth: None,
            hung: false,
            read_per_second: 0,
            write_per_second: 0,
        }
    }

    /// Три процесса, у которых порядок по каждому столбцу свой.
    fn snapshot() -> Snapshot {
        Snapshot {
            top: vec![
                line("chrome.exe", 300, 1.0, 900, 40),
                line("Alpha.exe", 100, 5.0, 100, 10),
                line("beta.exe", 200, 3.0, 500, 90),
            ],
            ..Default::default()
        }
    }

    fn names(rows: &[ProcessRow]) -> Vec<&str> {
        rows.iter().map(|row| row.name.as_str()).collect()
    }

    #[test]
    fn sorting_by_memory_uses_bytes_not_the_printed_text() {
        // Ровно та ошибка, ради которой сортируем по сырым числам:
        // «900 МБ» как текст больше «1.2 ГБ», а как размер — меньше.
        let rows = process_rows(&snapshot(), SortColumn::Memory, true);
        assert_eq!(names(&rows), vec!["chrome.exe", "beta.exe", "Alpha.exe"]);
    }

    #[test]
    fn sorting_by_cpu_puts_the_hungriest_first() {
        let rows = process_rows(&snapshot(), SortColumn::Cpu, true);
        assert_eq!(names(&rows), vec!["Alpha.exe", "beta.exe", "chrome.exe"]);
    }

    #[test]
    fn sorting_by_threads_and_pid_works() {
        let by_threads = process_rows(&snapshot(), SortColumn::Threads, true);
        assert_eq!(
            names(&by_threads),
            vec!["beta.exe", "chrome.exe", "Alpha.exe"]
        );

        let by_pid = process_rows(&snapshot(), SortColumn::Pid, false);
        assert_eq!(names(&by_pid), vec!["Alpha.exe", "beta.exe", "chrome.exe"]);
    }

    #[test]
    fn names_sort_case_insensitively() {
        // Иначе «Alpha» и «beta» разъехались бы по регистру: все процессы
        // с большой буквы собрались бы отдельной кучей.
        let rows = process_rows(&snapshot(), SortColumn::Name, false);
        assert_eq!(names(&rows), vec!["Alpha.exe", "beta.exe", "chrome.exe"]);
    }

    #[test]
    fn the_direction_flips_the_order() {
        let down = process_rows(&snapshot(), SortColumn::Memory, true);
        let up = process_rows(&snapshot(), SortColumn::Memory, false);
        assert_eq!(
            names(&down),
            names(&up).into_iter().rev().collect::<Vec<_>>()
        );
    }

    #[test]
    fn only_the_top_rows_are_shown_but_sorting_happens_first() {
        // Обрезка после сортировки: иначе при сортировке по памяти
        // в списке оказались бы случайные процессы, а не прожорливые.
        let mut snapshot = Snapshot::default();
        for index in 0..(VISIBLE_ROWS as u64 + 40) {
            snapshot
                .top
                .push(line(&format!("p{index}.exe"), index as u32, 0.0, index, 1));
        }

        let rows = process_rows(&snapshot, SortColumn::Memory, true);
        assert_eq!(rows.len(), VISIBLE_ROWS, "список не обрезан");
        // Первым обязан быть самый прожорливый из всех, а не из первых.
        assert_eq!(rows[0].name, format!("p{}.exe", VISIBLE_ROWS + 39));
    }

    #[test]
    fn hung_processes_can_be_brought_to_the_top() {
        let mut snapshot = snapshot();
        snapshot.top[1].hung = true; // Alpha.exe не отвечает

        let rows = process_rows(&snapshot, SortColumn::State, true);
        assert_eq!(
            rows[0].name, "Alpha.exe",
            "зависший процесс должен быть первым"
        );
        assert_eq!(rows[0].state, "не отвечает");
        assert!(rows[0].hung);
        // Про отвечающие процессы ничего не пишем — это был бы шум.
        assert!(rows[1].state.is_empty());
    }

    #[test]
    fn growth_is_shown_only_when_memory_actually_grows() {
        let mut snapshot = snapshot();
        snapshot.top[0].memory_growth = Some(bamboo_analyze::MemoryTrend {
            mb_per_hour: 42.0,
            r_squared: 0.99,
            window_ms: 7 * 3_600_000,
            suspected_leak: true,
        });
        snapshot.top[1].memory_growth = Some(bamboo_analyze::MemoryTrend {
            mb_per_hour: 12.0,
            r_squared: 0.7,
            window_ms: 30 * 60_000,
            suspected_leak: false,
        });

        let rows = process_rows(&snapshot, SortColumn::Growth, true);
        // Самый быстрый рост — первым, процесс без роста — последним.
        assert_eq!(names(&rows), vec!["chrome.exe", "Alpha.exe", "beta.exe"]);

        assert_eq!(rows[0].growth, "утечка? +42 МБ/ч");
        assert!(rows[0].leak);
        // Полчаса наблюдений — скорость называем, слово «утечка» нет.
        assert_eq!(rows[1].growth, "+12 МБ/ч");
        assert!(!rows[1].leak);
        // Не растёт — не пишем ничего, а не «0 МБ/ч».
        assert!(rows[2].growth.is_empty());
    }
}

#[cfg(test)]
mod grouping_tests {
    use super::*;
    use crate::collector::ProcessLine;
    use bamboo_core::Bytes;

    fn line(name: &str, pid: u32, cpu: f32, mib: u64, disk: u64) -> ProcessLine {
        ProcessLine {
            name: name.to_string(),
            pid,
            threads: 10,
            cpu_percent: cpu,
            memory: Bytes::from_mib(mib),
            badge: String::new(),
            memory_growth: None,
            hung: false,
            read_per_second: disk,
            write_per_second: 0,
        }
    }

    /// Chrome из трёх процессов и одинокий блокнот.
    fn snapshot() -> Snapshot {
        Snapshot {
            top: vec![
                line("chrome.exe", 100, 2.0, 400, 1 << 20),
                line("chrome.exe", 101, 1.0, 300, 2 << 20),
                line("chrome.exe", 102, 0.5, 500, 0),
                line("notepad.exe", 200, 0.1, 50, 0),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn processes_of_one_app_collapse_into_a_single_row() {
        let groups = group_by_app(&snapshot());
        assert_eq!(groups.len(), 2, "должно остаться две программы");

        let chrome = groups.iter().find(|g| g.name == "chrome.exe").unwrap();
        assert_eq!(chrome.count, 3);
        // Цена программы — это сумма её процессов, а не самый крупный.
        assert_eq!(chrome.memory.as_u64(), Bytes::from_mib(1200).as_u64());
        assert!((chrome.cpu_percent - 3.5).abs() < 1e-5);
        assert_eq!(chrome.disk_per_second, 3 << 20);
    }

    #[test]
    fn the_lead_process_is_the_hungriest_one() {
        // Действие применяется к ведущему процессу: разумно взять того,
        // кто занимает больше всех, а не случайного.
        let groups = group_by_app(&snapshot());
        let chrome = groups.iter().find(|g| g.name == "chrome.exe").unwrap();
        assert_eq!(chrome.lead_pid, 102, "ведущим должен быть самый крупный");
    }

    #[test]
    fn a_single_hung_process_marks_the_whole_app() {
        let mut snapshot = snapshot();
        snapshot.top[1].hung = true;

        let groups = group_by_app(&snapshot);
        let chrome = groups.iter().find(|g| g.name == "chrome.exe").unwrap();
        assert!(
            chrome.hung,
            "если одна вкладка висит, программа не отвечает"
        );
    }

    #[test]
    fn grouped_rows_sort_by_total_memory() {
        let rows = grouped_rows(&snapshot(), SortColumn::Memory, true, &|_| false);
        assert_eq!(rows[0].name, "chrome.exe");
        assert_eq!(rows[0].threads, "3 проц.");
        assert_eq!(rows[1].name, "notepad.exe");
        assert_eq!(rows[1].threads, "1 проц.");
    }

    #[test]
    fn grouped_rows_sort_by_disk() {
        let rows = grouped_rows(&snapshot(), SortColumn::Disk, true, &|_| false);
        // У Chrome суммарно 3 МБ/с, у блокнота ничего.
        assert_eq!(rows[0].name, "chrome.exe");
        assert!(rows[0].disk.contains("МБ/с"), "{}", rows[0].disk);
        assert!(rows[1].disk.is_empty());
    }

    #[test]
    fn an_empty_snapshot_yields_no_groups() {
        assert!(group_by_app(&Snapshot::default()).is_empty());
    }
}

/// Строка предложения для раздела «Оптимизация».
pub struct SuggestionRow {
    pub pid: String,
    pub title: String,
    pub reason: String,
    pub effect: String,
    /// Подпись кнопки. Пусто — предложение без действия.
    pub button: String,
    /// Код действия для `apply-action`; -1 у наблюдений без действия.
    pub action: i32,
}

/// Собирает предложения по снимку.
///
/// Пустой список — обычный исход: значит, прямо сейчас улучшать нечего.
/// Показывать в этом случае что-нибудь ради заполнения экрана мы не будем.
pub fn suggestion_rows(
    snapshot: &Snapshot,
    handled: &dyn Fn(u32) -> bool,
) -> (Vec<SuggestionRow>, String) {
    use bamboo_analyze::suggest::{ProcessFacts, Remedy, Situation};

    let facts: Vec<ProcessFacts<'_>> = snapshot
        .top
        .iter()
        .map(|line| {
            let protected = bamboo_policy::immutable_reason(&bamboo_policy::ProcessFacts {
                image_name: &line.name,
                session_id: 1,
                ..Default::default()
            })
            .is_some();

            ProcessFacts {
                pid: line.pid,
                name: &line.name,
                cpu_percent: line.cpu_percent,
                memory: line.memory,
                disk_per_second: line.read_per_second.saturating_add(line.write_per_second),
                // Сессию процесса в снимке не держим, а системные всё равно
                // отсекает неизменяемый список. Ставим пользовательскую.
                session_id: 1,
                hung: line.hung,
                leaking: line.memory_growth.is_some_and(|trend| trend.suspected_leak),
                already_handled: handled(line.pid),
                protected,
            }
        })
        .collect();

    let situation = Situation {
        user_idle_ms: snapshot.user_idle_ms,
        disk_saturated: snapshot.disks.iter().any(|disk| disk.saturated),
    };

    let suggestions = bamboo_analyze::suggest(&facts, situation);

    let note = if suggestions.is_empty() {
        if snapshot.user_idle_ms < bamboo_analyze::suggest::IDLE_BEFORE_SUGGESTING_MS {
            "Пока вы за компьютером, Bamboo не предлагает придерживать программы: \
             любая из них может понадобиться вам сию секунду. Предложения появятся, \
             если система будет чем-то занята в ваше отсутствие."
                .to_string()
        } else {
            "Улучшать нечего: никто не шумит в фоне. Это нормальный исход, \
             а не признак того, что Bamboo плохо посмотрел."
                .to_string()
        }
    } else {
        String::new()
    };

    let rows = suggestions
        .into_iter()
        .map(|suggestion| {
            let (button, action) = match suggestion.remedy {
                Remedy::EcoQos => ("Включить", 0),
                Remedy::LowerMemory => ("Понизить", 1),
                Remedy::ThrottleDisk => ("Придержать", 2),
                Remedy::JustSaying => ("", -1),
            };
            SuggestionRow {
                pid: suggestion.pid.to_string(),
                title: format!("{} — {}", suggestion.process_name, suggestion.remedy.name()),
                reason: suggestion.reason,
                effect: suggestion.effect,
                button: button.to_string(),
                action,
            }
        })
        .collect();

    (rows, note)
}

#[cfg(test)]
mod expansion_tests {
    use super::*;
    use crate::collector::ProcessLine;
    use bamboo_core::Bytes;

    fn line(name: &str, pid: u32, mib: u64) -> ProcessLine {
        ProcessLine {
            name: name.to_string(),
            pid,
            threads: 4,
            cpu_percent: 1.0,
            memory: Bytes::from_mib(mib),
            badge: String::new(),
            memory_growth: None,
            hung: false,
            read_per_second: 0,
            write_per_second: 0,
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            top: vec![
                line("chrome.exe", 100, 100),
                line("chrome.exe", 101, 300),
                line("chrome.exe", 102, 200),
                line("notepad.exe", 200, 50),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn a_collapsed_group_is_one_row() {
        let rows = grouped_rows(&snapshot(), SortColumn::Memory, true, &|_| false);
        assert_eq!(rows.len(), 2, "свёрнутыми должны быть две строки");
        assert!(rows[0].is_group, "chrome — группа");
        assert!(!rows[1].is_group, "блокнот — одиночка");
    }

    #[test]
    fn an_expanded_group_lists_its_processes() {
        let rows = grouped_rows(&snapshot(), SortColumn::Memory, true, &|name| {
            name == "chrome.exe"
        });
        // Строка группы плюс три её процесса плюс блокнот.
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].name, "chrome.exe");
        assert!(rows[0].expanded, "группа должна быть помечена развёрнутой");
        assert!(rows[1].is_member, "следом должны идти процессы группы");

        // Внутри группы порядок по памяти: крупный процесс ищут первым.
        assert_eq!(rows[1].pid, "101");
        assert_eq!(rows[2].pid, "102");
        assert_eq!(rows[3].pid, "100");
    }

    #[test]
    fn a_group_carries_all_its_pids_for_bulk_termination() {
        let rows = grouped_rows(&snapshot(), SortColumn::Memory, true, &|_| false);
        let chrome = rows.iter().find(|row| row.is_group).unwrap();

        let pids: Vec<&str> = chrome.member_pids.split(',').collect();
        assert_eq!(
            pids.len(),
            3,
            "в группе три процесса: {}",
            chrome.member_pids
        );
        for pid in ["100", "101", "102"] {
            assert!(pids.contains(&pid), "потерян {pid}");
        }
    }

    #[test]
    fn a_lone_process_is_never_marked_as_a_group() {
        // Разворачивать одиночку нечего, и стрелку рисовать незачем.
        let rows = grouped_rows(&snapshot(), SortColumn::Memory, true, &|_| true);
        let notepad = rows
            .iter()
            .find(|row| row.name.contains("notepad"))
            .unwrap();
        assert!(!notepad.is_group);
        assert!(notepad.member_pids.contains("200"));
    }
}
