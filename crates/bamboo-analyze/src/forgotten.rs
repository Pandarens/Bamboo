//! Забытые серверы разработки.
//!
//! С живой машины: vite проекта stanica_club работал вторые сутки и держал
//! 813 МБ, а рядом — сервер Laravel, планировщик и очередь того же проекта.
//! Запущены они были из сессии, которую давно закрыли глазами, но не
//! процессами. Памяти машине не хватало, и эти мегабайты уходили впустую.
//!
//! Признаки подобраны так, чтобы не задеть рабочее:
//!
//! - исполнитель сценариев с подписью — node, python, php и родня;
//! - слушает сетевой порт. Это и отличает сервер разработки от серверов
//!   инструментов, которые общаются через стандартный ввод: те живут, пока
//!   жива их сессия, и закрывать их надо вместе с ней, а не по одному;
//! - работает давно — дольше рабочего дня;
//! - последний час не делал ничего: ни запроса из браузера, ни пересборки
//!   после правки. Сервер, с которым работают, тратит процессор.

use bamboo_core::Bytes;

/// Сколько сервер должен проработать, чтобы о нём стоило спросить.
/// Восемь часов — рабочий день: утренний сервер вечером ещё может быть нужен.
pub const OLD_ENOUGH_MS: u64 = 8 * 60 * 60 * 1000;

/// Сколько он должен простоять без дела. Час: пауза на обед или созвон
/// короче, а сервер, к которому за час не обратились ни разу, забыт.
pub const QUIET_ENOUGH_MS: u64 = 60 * 60 * 1000;

/// С какого размера о сервере стоит говорить.
pub const WORTH: Bytes = Bytes(100 * 1024 * 1024);

/// Сколько сервер должен без перерыва жечь процессор, чтобы это был
/// холостой ход, а не работа. Три часа: самая долгая пересборка — минуты,
/// а сервер, который три часа подряд держит полядра, ничего не собирает.
pub const SPINNING_MS: u64 = 3 * 60 * 60 * 1000;

/// Оболочки и обёртки: они запускают, но сами ничего не значат. Ища, кто
/// запустил сервер, их проходим насквозь.
const PASS_THROUGH: &[&str] = &[
    "cmd.exe",
    "conhost.exe",
    "bash.exe",
    "sh.exe",
    "powershell.exe",
    "pwsh.exe",
    "node.exe",
    "python.exe",
    "php.exe",
];

/// Что известно о процессе.
#[derive(Clone, Copy, Debug)]
pub struct ServerFacts<'a> {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: &'a str,
    /// Что выполняет исполнитель сценариев: «vite (stanica_club)».
    pub label: &'a str,
    pub memory: Bytes,
    /// Какие порты слушает.
    pub ports: &'a [u16],
    /// Сколько процесс уже работает.
    pub age_ms: u64,
    /// Сколько последних минут подряд он ничего не делал, в миллисекундах.
    pub quiet_ms: u64,
    /// Сколько последних минут он без перерыва занят, в миллисекундах.
    pub busy_ms: u64,
    /// Сколько процессора занимает сейчас, в процентах одного ядра.
    pub cpu_percent: f32,
    /// Сколько дескрипторов держит.
    pub handles: u32,
}

/// Почему сервер стоит закрыть.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Why {
    /// Забыт и простаивает: держит память, ничего не делая.
    Idle,
    /// Без перерыва жжёт процессор — холостой цикл. `cores` — сколько
    /// ядер занимает сейчас.
    Spinning { cores: f32 },
}

/// Забытый сервер.
#[derive(Clone, Debug, PartialEq)]
pub struct Forgotten {
    pub why: Why,
    /// Сколько он уже без перерыва занят — для холостого хода.
    pub busy_ms: u64,
    pub handles: u32,
    pub pid: u32,
    pub label: String,
    pub ports: Vec<u16>,
    pub age_ms: u64,
    pub quiet_ms: u64,
    /// Сколько памяти держит вместе со всем, что запустил.
    pub bytes: Bytes,
    /// Кого завершать: сам сервер и всё, что он запустил.
    pub close: Vec<u32>,
    /// Кто его запустил, если это видно: «claude.exe», «Code.exe».
    pub launcher: Option<String>,
}

/// Находит забытые серверы, крупные первыми.
pub fn forgotten(processes: &[ServerFacts<'_>]) -> Vec<Forgotten> {
    use std::collections::{HashMap, HashSet};

    let by_pid: HashMap<u32, &ServerFacts<'_>> =
        processes.iter().map(|facts| (facts.pid, facts)).collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for facts in processes {
        if facts.parent_pid != facts.pid {
            children
                .entry(facts.parent_pid)
                .or_default()
                .push(facts.pid);
        }
    }
    let is_server = |facts: &ServerFacts<'_>| !facts.label.is_empty() && !facts.ports.is_empty();

    // Предки, ближние первыми. Глубина ограничена: номера процессов
    // переиспользуются, и цепочка может замкнуться.
    let ancestors = |facts: &ServerFacts<'_>| {
        let mut chain: Vec<&ServerFacts<'_>> = Vec::new();
        let mut pid = facts.parent_pid;
        while chain.len() < 8 {
            match by_pid.get(&pid) {
                Some(parent) if parent.pid != facts.pid && !chain.iter().any(|p| p.pid == pid) => {
                    chain.push(parent);
                    pid = parent.parent_pid;
                }
                _ => break,
            }
        }
        chain
    };

    let mut found = Vec::new();
    for server in processes.iter().filter(|facts| is_server(facts)) {
        let chain = ancestors(server);
        // Слушает и родитель — значит, это часть его сервера (next dev
        // держит порт и в родителе, и в рабочем процессе).
        if chain.iter().any(|parent| is_server(parent)) {
            continue;
        }

        // Откуда закрывать: с верхней обёртки с подписью — «npm run dev»,
        // «artisan serve». Закрой один vite, и обёртка осталась бы висеть
        // со своими мегабайтами. Выше обёрток не идём: там оболочка
        // терминала, и её закрытие закрыло бы человеку вкладку.
        let root = chain
            .iter()
            .take_while(|parent| {
                PASS_THROUGH
                    .iter()
                    .any(|name| parent.name.eq_ignore_ascii_case(name))
            })
            .filter(|parent| !parent.label.is_empty())
            .last()
            .copied()
            .unwrap_or(server);

        // Обёртка, сервер и всё, что он запустил.
        let mut tree: Vec<&ServerFacts<'_>> = vec![root];
        let mut seen: HashSet<u32> = HashSet::from([root.pid]);
        let mut at = 0;
        while at < tree.len() && tree.len() < 64 {
            if let Some(kids) = children.get(&tree[at].pid) {
                for kid in kids {
                    if seen.insert(*kid) {
                        if let Some(facts) = by_pid.get(kid) {
                            tree.push(facts);
                        }
                    }
                }
            }
            at += 1;
        }

        // Тишина — у всех исполнителей дерева: рабочий процесс сборщика
        // может трудиться, пока главный ждёт. Оболочки вроде cmd.exe между
        // ними не в счёт — простой у них не меряется, а сами они не делают
        // ничего. Без этого цепочка npm → cmd → vite не находилась никогда.
        let quiet_ms = tree
            .iter()
            .filter(|facts| !facts.label.is_empty())
            .map(|facts| facts.quiet_ms)
            .min()
            .unwrap_or(0);
        let bytes = Bytes(tree.iter().map(|facts| facts.memory.as_u64()).sum());
        // Холостой ход — по самому серверу: у Jupyter долгий расчёт идёт
        // в дочернем ядре, и это работа, а не зависание.
        let why = if server.busy_ms >= SPINNING_MS {
            Why::Spinning {
                cores: server.cpu_percent / 100.0,
            }
        } else if server.age_ms >= OLD_ENOUGH_MS && quiet_ms >= QUIET_ENOUGH_MS && bytes >= WORTH {
            Why::Idle
        } else {
            continue;
        };

        let launcher = chain
            .iter()
            .find(|parent| {
                !PASS_THROUGH
                    .iter()
                    .any(|name| parent.name.eq_ignore_ascii_case(name))
            })
            .map(|parent| parent.name.to_string());

        found.push(Forgotten {
            why,
            busy_ms: server.busy_ms,
            handles: server.handles,
            pid: server.pid,
            label: server.label.to_string(),
            ports: server.ports.to_vec(),
            age_ms: server.age_ms,
            quiet_ms,
            bytes,
            close: tree.iter().map(|facts| facts.pid).collect(),
            launcher,
        });
    }
    found.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.pid.cmp(&b.pid)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 60 * 60 * 1000;
    const MB: u64 = 1024 * 1024;

    fn facts<'a>(
        pid: u32,
        parent_pid: u32,
        name: &'a str,
        label: &'a str,
        mb: u64,
        ports: &'a [u16],
    ) -> ServerFacts<'a> {
        ServerFacts {
            pid,
            parent_pid,
            name,
            label,
            memory: Bytes(mb * MB),
            ports,
            age_ms: 30 * HOUR,
            quiet_ms: 3 * HOUR,
            busy_ms: 0,
            cpu_percent: 0.0,
            handles: 200,
        }
    }

    /// Живая машина: vite запущен через npm run dev из сессии Claude,
    /// рядом сервер PDF той же сессии — через стандартный ввод, без порта.
    fn the_live_machine() -> Vec<ServerFacts<'static>> {
        // У оболочек простой не меряется — ноль, как и в живом снимке.
        let shell = |pid, parent| ServerFacts {
            quiet_ms: 0,
            ..facts(pid, parent, "cmd.exe", "", 4, &[])
        };
        vec![
            facts(1, 0, "claude.exe", "", 500, &[]),
            shell(10, 1),
            facts(11, 10, "node.exe", "npm run dev", 59, &[]),
            shell(12, 11),
            facts(13, 12, "node.exe", "vite (stanica_club)", 813, &[5173]),
            facts(20, 1, "cmd.exe", "", 4, &[]),
            facts(
                21,
                20,
                "node.exe",
                "@modelcontextprotocol/server-pdf",
                111,
                &[],
            ),
        ]
    }

    #[test]
    fn the_forgotten_vite_is_found_and_named_with_its_launcher() {
        let found = forgotten(&the_live_machine());
        assert_eq!(found.len(), 1, "{found:?}");
        let vite = &found[0];
        assert_eq!(vite.label, "vite (stanica_club)");
        assert_eq!(vite.ports, vec![5173]);
        // Вместе с обёрткой npm run dev и оболочкой между ними — иначе
        // обёртка осталась бы висеть.
        assert_eq!(vite.close, vec![11, 12, 13]);
        assert_eq!(vite.launcher.as_deref(), Some("claude.exe"));
        assert_eq!(vite.bytes, Bytes((59 + 4 + 813) * MB));
    }

    #[test]
    fn a_server_in_use_is_left_alone() {
        // Браузер обращался к серверу десять минут назад.
        let mut processes = the_live_machine();
        processes[4].quiet_ms = 10 * 60 * 1000;
        assert!(forgotten(&processes).is_empty());
    }

    #[test]
    fn a_morning_server_is_not_forgotten_by_evening() {
        let mut processes = the_live_machine();
        processes[4].age_ms = 5 * HOUR;
        assert!(forgotten(&processes).is_empty());
    }

    #[test]
    fn a_busy_worker_keeps_its_server_alive() {
        // next dev: порт держит родитель, сборку делает дочерний процесс.
        let processes = vec![
            facts(1, 0, "Code.exe", "", 300, &[]),
            facts(2, 1, "node.exe", "next (shop)", 400, &[3000]),
            ServerFacts {
                quiet_ms: 2 * 60 * 1000,
                ..facts(3, 2, "node.exe", "next (shop)", 600, &[3000])
            },
        ];
        assert!(forgotten(&processes).is_empty());
    }

    #[test]
    fn a_server_with_workers_is_closed_whole() {
        let processes = vec![
            facts(1, 0, "WindowsTerminal.exe", "", 80, &[]),
            facts(2, 1, "node.exe", "next (shop)", 400, &[3000]),
            facts(3, 2, "node.exe", "next (shop)", 600, &[3000]),
        ];
        let found = forgotten(&processes);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].pid, 2);
        assert_eq!(found[0].close, vec![2, 3]);
        assert_eq!(found[0].bytes, Bytes(1000 * MB));
        assert_eq!(found[0].launcher.as_deref(), Some("WindowsTerminal.exe"));
    }

    #[test]
    fn the_terminal_tab_a_server_runs_in_is_not_closed() {
        // npm run dev, набранный руками в PowerShell: закрыть надо сервер,
        // а не вкладку терминала, где человек его набрал.
        let processes = vec![
            facts(1, 0, "WindowsTerminal.exe", "", 80, &[]),
            facts(2, 1, "powershell.exe", "", 70, &[]),
            facts(3, 2, "node.exe", "npm run dev", 59, &[]),
            facts(4, 3, "node.exe", "vite (shop)", 400, &[5173]),
        ];
        let found = forgotten(&processes);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].close, vec![3, 4]);
        assert_eq!(found[0].launcher.as_deref(), Some("WindowsTerminal.exe"));
    }

    #[test]
    fn a_spinning_server_is_found_even_though_someone_is_connected() {
        // Живой случай: vite четверо суток держал полтора ядра и 12 тысяч
        // дескрипторов, а к нему были подключены забытые вкладки.
        let mut processes = the_live_machine();
        processes[4].quiet_ms = 0;
        processes[4].busy_ms = 4 * 24 * HOUR;
        processes[4].cpu_percent = 143.0;
        processes[4].handles = 12_272;
        let found = forgotten(&processes);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].why, Why::Spinning { cores: 1.43 });
        assert_eq!(found[0].close, vec![11, 12, 13]);
    }

    #[test]
    fn a_long_rebuild_is_not_spinning() {
        // Полчаса сборки после большого обновления — работа.
        let mut processes = the_live_machine();
        processes[4].quiet_ms = 0;
        processes[4].busy_ms = 30 * 60 * 1000;
        processes[4].cpu_percent = 180.0;
        assert!(forgotten(&processes).is_empty());
    }

    #[test]
    fn a_small_server_is_not_worth_mentioning() {
        // php artisan serve держит двадцать мегабайт — памяти машине
        // он не вернёт, и шуметь о нём незачем.
        let processes = vec![facts(5, 0, "php.exe", "artisan serve", 20, &[8000])];
        assert!(forgotten(&processes).is_empty());
    }
}
