//! Цикл брокера: приём подключений, валидация, исполнение.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bamboo_actuate::{Executor, RestorePointNet, SystemBackend};
use bamboo_core::Result;
use bamboo_ipc::{pipe_name, ErrorCode, Request, Response};
use bamboo_journal::Journal;
use bamboo_policy::UserWhitelist;
use bamboo_sys::pipe::{client_is_same_image, PipeServer};

use crate::validate::{validate, BrokerPolicy, ClientFacts, Verdict};
use crate::{execute, snapshot};

/// Сколько времени ждать между попытками пересоздать канал после сбоя.
const RETRY_PAUSE: std::time::Duration = std::time::Duration::from_secs(1);

/// Тело службы. Крутит тот же цикл, что и консольный режим, но остановку
/// получает от диспетчера служб, а не с клавиатуры.
///
/// Обязано завершиться при `stop.stop_requested()`, иначе служба зависнет
/// в состоянии остановки.
pub fn run_as_service(stop: bamboo_sys::StopSignal) {
    let _ = bamboo_sys::apply_self_limits();
    let name = pipe_name(current_session_id());
    let policy = BrokerPolicy::default();

    let journal = open_broker_journal();
    // Со страховкой: перед остановкой и отключением службы брокер
    // оставляет точку восстановления, а если оставить её не вышло —
    // отказывается действовать. Свой откат чинит то, что сделали мы;
    // точка страхует случай, когда сломалось что-то ещё.
    let executor = Executor::with_safety_net(&journal, SystemBackend, RestorePointNet);
    let whitelist = UserWhitelist::new();

    while !stop.stop_requested() {
        // Один клиент за раз; при сбое канала пересоздаём после паузы.
        if serve_one_client(&name, &policy, &executor, &whitelist).is_err() {
            std::thread::sleep(RETRY_PAUSE);
        }
    }
}

/// Запускает брокер в консольном режиме.
pub fn run_console() -> Result<()> {
    println!("Брокер Bamboo запускается.");

    // Собственный бюджет: брокер тоже применяет к себе EcoQoS.
    let _ = bamboo_sys::apply_self_limits();

    let session = current_session_id();
    let name = pipe_name(session);
    println!("Слушаю канал {name}");
    println!("Журнал действий: {}", journal_path().display());
    println!("Остановка — Ctrl+C.\n");

    let stop = Arc::new(AtomicBool::new(false));
    install_ctrl_c(Arc::clone(&stop));

    let policy = BrokerPolicy::default();

    let journal = open_broker_journal();
    // Со страховкой: перед остановкой и отключением службы брокер
    // оставляет точку восстановления, а если оставить её не вышло —
    // отказывается действовать. Свой откат чинит то, что сделали мы;
    // точка страхует случай, когда сломалось что-то ещё.
    let executor = Executor::with_safety_net(&journal, SystemBackend, RestorePointNet);
    let whitelist = UserWhitelist::new();

    while !stop.load(Ordering::Relaxed) {
        match serve_one_client(&name, &policy, &executor, &whitelist) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("канал сорвался: {error}. Пересоздаю через секунду.");
                std::thread::sleep(RETRY_PAUSE);
            }
        }
    }

    println!("Брокер остановлен.");
    Ok(())
}

/// Обслуживает одно подключение от начала до конца.
fn serve_one_client(
    name: &str,
    policy: &BrokerPolicy,
    executor: &Executor<'_, SystemBackend, RestorePointNet>,
    whitelist: &UserWhitelist,
) -> Result<()> {
    let server = PipeServer::create(name)?;
    let client_pid = server.accept()?;

    // Два независимых рубежа проверки клиента (ТЗ, раздел 3.2): совпадает ли
    // образ и из той ли он сессии. Имя канала уже привязано к сессии, но
    // полагаться на одно имя — значит доверять; сессию клиента проверяем
    // отдельным вызовом. REJECT_REMOTE_CLIENTS отсекает удалённых на уровне
    // системы, поэтому remote здесь заведомо ложь.
    let image_matches = client_is_same_image(client_pid).unwrap_or(false);
    let same_session = bamboo_sys::pipe::client_in_session(client_pid, current_session_id());
    let client = ClientFacts {
        image_matches,
        same_session,
        remote: false,
    };

    let mut buffer = vec![0u8; bamboo_ipc::MAX_FRAME_BYTES];
    let read = server.read(&mut buffer)?;

    let response = match bamboo_ipc::decode(&buffer[..read]) {
        Ok(Some((body, _))) => handle_frame(body, &client, policy, executor, whitelist),
        Ok(None) => {
            eprintln!("клиент прислал неполный кадр");
            Response::Error {
                code: ErrorCode::Malformed,
                detail: "кадр пришёл не целиком".into(),
            }
        }
        Err(error) => {
            eprintln!("кадр не разобрался: {error}");
            Response::Error {
                code: ErrorCode::Malformed,
                detail: error.to_string(),
            }
        }
    };

    let frame = bamboo_ipc::encode_message(&response)?;
    server.write(&frame)?;

    server.disconnect()?;
    Ok(())
}

/// Разбирает тело кадра в запрос, валидирует и исполняет.
fn handle_frame(
    body: &[u8],
    client: &ClientFacts,
    policy: &BrokerPolicy,
    executor: &Executor<'_, SystemBackend, RestorePointNet>,
    whitelist: &UserWhitelist,
) -> Response {
    let request: Request = match bamboo_ipc::decode_message(body) {
        Ok(request) => request,
        Err(error) => {
            // Клиент прислал что-то, что не разбирается в запрос. Молча
            // не проглатываем — отвечаем понятным кодом.
            return Response::Error {
                code: ErrorCode::Malformed,
                detail: error.to_string(),
            };
        }
    };

    match validate(client, &request, policy) {
        Verdict::Allow => {
            println!("запрос {request:?} — разрешён, исполняю");
            match &request {
                // Читающий снимок брокер обслуживает сам через коллектор.
                Request::QuerySnapshot { scope } => snapshot::query_snapshot(*scope),
                // Всё изменяющее идёт через того же исполнителя, что и CLI:
                // политика, журнал, действие, подтверждение.
                _ => {
                    let now = bamboo_core::SampleTime::wall_clock_now();
                    execute::run(&request, executor, whitelist, now)
                }
            }
        }
        Verdict::Deny(code, detail) => {
            println!("запрос {request:?} — отказ: {detail}");
            Response::Error { code, detail }
        }
    }
}

/// Идентификатор сессии, которую обслуживает брокер.
///
/// В консольном режиме это собственная сессия брокера — та же, где работает
/// агент пользователя, поэтому берём её напрямую. Реальная служба под SYSTEM
/// живёт в сессии 0 и должна определять активную сессию пользователя через
/// `WTSGetActiveConsoleSessionId` — это отдельный шаг для служебного пути.
fn current_session_id() -> u32 {
    bamboo_sys::pipe::process_session_id(std::process::id()).unwrap_or(1)
}

fn journal_path() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("Bamboo").join("journal.db")
}

/// Открывает журнал брокера. Под SYSTEM путь в ProgramData доступен всегда;
/// в консольном режиме без прав он может не открыться — тогда падаем на
/// журнал в памяти, чтобы отладка не требовала администратора. Действия при
/// этом применяются и откатываются, но между запусками не сохраняются.
fn open_broker_journal() -> Journal {
    let path = journal_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match Journal::open(&path) {
        Ok(journal) => journal,
        Err(error) => {
            eprintln!(
                "журнал {} не открылся ({error}); работаю с журналом в памяти",
                path.display()
            );
            Journal::in_memory().expect("журнал в памяти обязан открыться")
        }
    }
}

/// Ставит обработчик Ctrl+C, который просит цикл остановиться.
fn install_ctrl_c(stop: Arc<AtomicBool>) {
    // Простейший вариант без внешних зависимостей: отдельный поток,
    // читающий строку. Реальная служба реагирует на SERVICE_CONTROL_STOP.
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        stop.store(true, Ordering::Relaxed);
    });
}
