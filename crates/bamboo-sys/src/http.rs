//! Загрузка по HTTPS через WinHTTP.
//!
//! Нужна ровно для одного: узнать у GitHub, не вышла ли новая версия, и
//! скачать её. Ради этого тащить в проект клиент HTTP со своим стеком TLS
//! незачем — в Windows он уже есть, работает через системное хранилище
//! сертификатов и обновляется вместе с системой. Свой пришлось бы обновлять
//! самим, а устаревший стек TLS в программе, которая скачивает и запускает
//! обновления, — худшее, что можно придумать.
//!
//! Только HTTPS. Обычный HTTP не поддерживается намеренно: по нему
//! обновление можно подменить по дороге.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetOption, URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

/// Больше этого не читаем ни при каких обстоятельствах.
///
/// Ответ сервера — чужие данные, и доверять их размеру нельзя. Без предела
/// сервер (или тот, кто им притворился) мог бы заставить нас читать
/// бесконечно, пока не кончится память.
const MAX_BODY: usize = 64 * 1024 * 1024;

/// Как представляемся серверу.
///
/// GitHub требует User-Agent и отвечает отказом без него.
const USER_AGENT: &str = "Bamboo";

struct OwnedHandle(*mut core::ffi::c_void);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

fn last_error(call: &'static str) -> Error {
    Error::Win32 {
        call,
        code: unsafe { windows_sys::Win32::Foundation::GetLastError() },
    }
}

/// Разобранный адрес: узел, порт и путь.
#[derive(Debug)]
struct Parts {
    host: Vec<u16>,
    port: u16,
    path: Vec<u16>,
}

/// Разбирает адрес силами самой Windows.
///
/// Свой разбор адресов писать не станем: он выглядит простым ровно до
/// первой попытки, а ошибка в нём означает обращение не туда, куда думали.
fn split_url(url: &str) -> Result<Parts> {
    if !url.starts_with("https://") {
        return Err(Error::Unsupported(
            "поддерживается только HTTPS: по обычному HTTP обновление можно подменить",
        ));
    }

    let wide_url = wide(url);
    let mut components: URL_COMPONENTS = unsafe { core::mem::zeroed() };
    components.dwStructSize = core::mem::size_of::<URL_COMPONENTS>() as u32;
    // Ненулевая длина при нулевом указателе означает «верни мне указатель
    // внутрь исходной строки и длину». Так адрес не копируется лишний раз.
    components.dwHostNameLength = u32::MAX;
    components.dwUrlPathLength = u32::MAX;
    components.dwExtraInfoLength = u32::MAX;

    let ok = unsafe { WinHttpCrackUrl(wide_url.as_ptr(), 0, 0, &mut components) };
    if ok == 0 {
        return Err(last_error("WinHttpCrackUrl"));
    }

    let host = unsafe {
        core::slice::from_raw_parts(
            components.lpszHostName,
            components.dwHostNameLength as usize,
        )
    };
    // Путь и хвост с параметрами лежат в исходной строке подряд, поэтому
    // берём их одним куском: разрывать и склеивать заново незачем.
    let path_len = components.dwUrlPathLength as usize + components.dwExtraInfoLength as usize;
    let path = unsafe { core::slice::from_raw_parts(components.lpszUrlPath, path_len) };

    Ok(Parts {
        host: host.iter().copied().chain(core::iter::once(0)).collect(),
        port: components.nPort,
        path: path.iter().copied().chain(core::iter::once(0)).collect(),
    })
}

/// Скачивает содержимое по адресу.
///
/// `accept` — что просим у сервера в заголовке `Accept`. GitHub по нему
/// различает описание выпуска и сам файл.
pub fn fetch(url: &str, accept: &str) -> Result<Vec<u8>> {
    let parts = split_url(url)?;

    let session = OwnedHandle(unsafe {
        WinHttpOpen(
            wide(USER_AGENT).as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            core::ptr::null(),
            core::ptr::null(),
            0,
        )
    });
    if session.0.is_null() {
        return Err(last_error("WinHttpOpen"));
    }

    // Переход с HTTPS на обычный HTTP запрещаем. Иначе перенаправление
    // увело бы загрузку обновления на незащищённое соединение, и вся
    // выгода от HTTPS пропала бы по дороге.
    let policy: u32 = WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP;
    unsafe {
        WinHttpSetOption(
            session.0,
            WINHTTP_OPTION_REDIRECT_POLICY,
            (&policy as *const u32).cast(),
            core::mem::size_of::<u32>() as u32,
        )
    };

    let connection =
        OwnedHandle(unsafe { WinHttpConnect(session.0, parts.host.as_ptr(), parts.port, 0) });
    if connection.0.is_null() {
        return Err(last_error("WinHttpConnect"));
    }

    // Строку держим в переменной: иначе она освободится в конце этого же
    // выражения, а указатель на неё останется в списке и уедет в WinHTTP.
    let accept_wide = wide(accept);
    let accept_list = [accept_wide.as_ptr(), core::ptr::null()];
    let request = OwnedHandle(unsafe {
        WinHttpOpenRequest(
            connection.0,
            wide("GET").as_ptr(),
            parts.path.as_ptr(),
            core::ptr::null(),
            core::ptr::null(),
            accept_list.as_ptr(),
            WINHTTP_FLAG_SECURE,
        )
    });
    if request.0.is_null() {
        return Err(last_error("WinHttpOpenRequest"));
    }

    let ok =
        unsafe { WinHttpSendRequest(request.0, core::ptr::null(), 0, core::ptr::null(), 0, 0, 0) };
    if ok == 0 {
        return Err(last_error("WinHttpSendRequest"));
    }

    let ok = unsafe { WinHttpReceiveResponse(request.0, core::ptr::null_mut()) };
    if ok == 0 {
        return Err(last_error("WinHttpReceiveResponse"));
    }

    let status = status_code(&request)?;
    if !(200..300).contains(&status) {
        // Отдельно про 404: у выпуска, которого нет, это обычный ответ,
        // а не поломка. Различать их полезно.
        return Err(Error::Win32 {
            call: "HTTP",
            code: status,
        });
    }

    read_body(&request)
}

fn status_code(request: &OwnedHandle) -> Result<u32> {
    let mut status: u32 = 0;
    let mut size = core::mem::size_of::<u32>() as u32;
    let ok = unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            core::ptr::null(),
            (&mut status as *mut u32).cast(),
            &mut size,
            core::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_error("WinHttpQueryHeaders"));
    }
    Ok(status)
}

fn read_body(request: &OwnedHandle) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut chunk = [0u8; 16 * 1024];

    loop {
        let mut read: u32 = 0;
        let ok = unsafe {
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )
        };
        if ok == 0 {
            return Err(last_error("WinHttpReadData"));
        }
        if read == 0 {
            break;
        }

        body.extend_from_slice(&chunk[..read as usize]);
        if body.len() > MAX_BODY {
            return Err(Error::Malformed("ответ сервера неправдоподобно велик"));
        }
    }

    Ok(body)
}

/// Скачивает текст.
pub fn fetch_text(url: &str, accept: &str) -> Result<String> {
    let bytes = fetch(url, accept)?;
    String::from_utf8(bytes).map_err(|_| Error::Malformed("ответ сервера не в UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_http_is_refused() {
        // Не придирка: по обычному HTTP обновление подменяется по дороге,
        // а мы его потом запускаем.
        let error = split_url("http://example.com/x").unwrap_err();
        assert!(error.to_string().contains("HTTPS"), "{error}");
    }

    #[test]
    fn a_url_splits_into_host_and_path() {
        let parts = split_url("https://api.github.com/repos/a/b/releases/latest").unwrap();
        assert_eq!(parts.port, 443);

        let host = String::from_utf16_lossy(&parts.host[..parts.host.len() - 1]);
        assert_eq!(host, "api.github.com");

        let path = String::from_utf16_lossy(&parts.path[..parts.path.len() - 1]);
        assert_eq!(path, "/repos/a/b/releases/latest");
    }

    #[test]
    fn query_parameters_stay_with_the_path() {
        // Хвост после вопросительного знака WinHttpCrackUrl отдаёт отдельно.
        // Потерять его значило бы запросить не тот адрес.
        let parts = split_url("https://example.com/file?token=abc").unwrap();
        let path = String::from_utf16_lossy(&parts.path[..parts.path.len() - 1]);
        assert_eq!(path, "/file?token=abc");
    }

    #[test]
    fn nonsense_is_refused_without_panicking() {
        assert!(split_url("").is_err());
        assert!(split_url("не адрес").is_err());
        assert!(split_url("ftp://example.com").is_err());
    }
}
