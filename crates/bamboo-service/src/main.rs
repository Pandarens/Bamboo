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
#[cfg(windows)]
mod execute;

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
        // Запуск диспетчером служб. Этим аргументом брокер прописан
        // в команде запуска службы.
        "service" => {
            if let Err(error) = bamboo_sys::service::run_as_service(broker::run_as_service) {
                eprintln!("не удалось запуститься как служба: {error}");
                std::process::exit(1);
            }
        }
        "install" => match install() {
            Ok(()) => println!("Служба Bamboo установлена и запустится при следующей загрузке."),
            Err(error) => {
                eprintln!("установка не удалась: {error}");
                eprintln!("Установка требует прав администратора.");
                std::process::exit(1);
            }
        },
        "uninstall" => match bamboo_sys::uninstall_service() {
            Ok(()) => println!("Служба Bamboo удалена."),
            Err(error) => {
                eprintln!("удаление не удалось: {error}");
                std::process::exit(1);
            }
        },
        "help" | "--help" => usage(),
        other => {
            eprintln!("неизвестный режим: {other}\n");
            usage();
            std::process::exit(2);
        }
    }
}

#[cfg(windows)]
fn install() -> Result<(), bamboo_core::Error> {
    let exe = std::env::current_exe()
        .map_err(|_| bamboo_core::Error::Unsupported("не удалось определить путь к себе"))?;
    bamboo_sys::install_service(&exe.to_string_lossy())
}

#[cfg(windows)]
fn usage() {
    println!(
        "Брокер Bamboo\n\n\
         bamboo-service console     запустить в консоли (для отладки)\n\
         bamboo-service install     установить как службу (нужны права администратора)\n\
         bamboo-service uninstall   удалить службу\n\n\
         В роли службы брокер запускается диспетчером служб сам."
    );
}
