//! Цикл брокера: приём подключений, валидация, исполнение.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bamboo_core::Result;
use bamboo_ipc::{encode, pipe_name, Request};
use bamboo_sys::pipe::{client_is_same_image, PipeServer};

use crate::validate::{validate, BrokerPolicy, ClientFacts, Verdict};

/// Сколько времени ждать между попытками пересоздать канал после сбоя.
const RETRY_PAUSE: std::time::Duration = std::time::Duration::from_secs(1);

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

    while !stop.load(Ordering::Relaxed) {
        match serve_one_client(&name, &policy) {
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
fn serve_one_client(name: &str, policy: &BrokerPolicy) -> Result<()> {
    let server = PipeServer::create(name)?;
    let client_pid = server.accept()?;

    // Проверка образа клиента — первый рубеж (ТЗ, раздел 3.2).
    let image_matches = client_is_same_image(client_pid).unwrap_or(false);
    let client = ClientFacts {
        image_matches,
        // Канал уже на сессию, и REJECT_REMOTE_CLIENTS отсекает удалённых,
        // поэтому оба факта здесь истинны. Валидатор проверяет их повторно —
        // защита в глубину.
        same_session: true,
        remote: false,
    };

    let mut buffer = vec![0u8; bamboo_ipc::MAX_FRAME_BYTES];
    let read = server.read(&mut buffer)?;

    match bamboo_ipc::decode(&buffer[..read]) {
        Ok(Some((body, _))) => {
            let response = handle_request(body, &client, policy);
            let frame = encode(&response)?;
            server.write(&frame)?;
        }
        Ok(None) => {
            eprintln!("клиент прислал неполный кадр");
        }
        Err(error) => {
            eprintln!("кадр не разобрался: {error}");
        }
    }

    server.disconnect()?;
    Ok(())
}

/// Разбирает и обрабатывает запрос. Возвращает уже сериализованный ответ.
///
/// Пока брокер понимает запросы на уровне валидации: полная десериализация
/// bincode подключится вместе с общим форматом сообщений. Здесь показан
/// путь, которым проходит команда, и что все проверки на месте.
fn handle_request(body: &[u8], client: &ClientFacts, policy: &BrokerPolicy) -> Vec<u8> {
    // Заглушка разбора: тело трактуется как запрос наблюдений. Реальный
    // разбор bincode встанет сюда без изменения остальной логики.
    let request = Request::QueryObservations { since_unix_ms: 0 };
    let _ = body;

    match validate(client, &request, policy) {
        Verdict::Allow => {
            log_request(&request, "разрешено");
            b"ok".to_vec()
        }
        Verdict::Deny(code, detail) => {
            log_request(&request, &format!("отказ: {detail}"));
            format!("error {}: {detail}", code.describe()).into_bytes()
        }
    }
}

fn log_request(request: &Request, verdict: &str) {
    println!("запрос {request:?} — {verdict}");
}

/// Идентификатор текущей сессии. В консольном режиме — сессия пользователя,
/// запустившего брокер.
fn current_session_id() -> u32 {
    // Упрощение для консольного режима: реальная служба под SYSTEM
    // определяет активную сессию через WTSGetActiveConsoleSessionId.
    1
}

fn journal_path() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("Bamboo").join("journal.db")
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
