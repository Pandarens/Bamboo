//! Брокер Bamboo.
//!
//! Служба под SYSTEM, выполняющая привилегированные операции по запросу
//! агента. Здесь — точка входа и консольный режим для отладки; регистрация
//! как службы Windows появится следующим шагом (пока брокер запускается
//! из консоли от администратора).
//!
//! Сама привилегированная работа и валидация запросов лежат в модулях
//! без `unsafe`: `validate` целиком чистый и проверяется тестами.

#![cfg_attr(not(windows), allow(unused))]

mod validate;

#[cfg(windows)]
mod broker;

#[cfg(not(windows))]
fn main() {
    eprintln!("Брокер Bamboo работает только на Windows.");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();

    match mode.as_str() {
        // Консольный режим: брокер работает в переднем плане, логи в консоль.
        // Так его удобно отлаживать до превращения в службу.
        "console" | "" => {
            if let Err(error) = broker::run_console() {
                eprintln!("брокер завершился с ошибкой: {error}");
                std::process::exit(1);
            }
        }
        "help" | "--help" => {
            println!(
                "Брокер Bamboo\n\n\
                 bamboo-service console   запустить в консоли (для отладки)\n\n\
                 Требует прав администратора: под обычным пользователем\n\
                 привилегированные операции недоступны."
            );
        }
        other => {
            eprintln!("неизвестный режим: {other}");
            std::process::exit(2);
        }
    }
}
