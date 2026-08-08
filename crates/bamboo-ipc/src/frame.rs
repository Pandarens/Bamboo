//! Кадрирование сообщений (ТЗ, раздел 13.1).
//!
//! Длина в четырёх байтах, затем тело. Ограничение размера кадра —
//! не удобство, а защита: без него клиент, объявивший кадр на четыре
//! гигабайта, заставит брокер выделить эту память. Брокер работает
//! под SYSTEM, и такой подарок ему делать нельзя.

use bamboo_core::{Error, Result};
use serde::{de::DeserializeOwned, Serialize};

/// Максимальный размер кадра.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Сериализует сообщение в готовый к отправке кадр.
///
/// Тело — bincode, снаружи — кадрирование с префиксом длины. Ограничение
/// размера защищает брокер: он под SYSTEM, и заставить его выделить кадр
/// произвольного размера нельзя.
pub fn encode_message<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    let body =
        bincode::serialize(message).map_err(|_| Error::Malformed("сообщение не сериализуется"))?;
    encode(&body)
}

/// Разбирает сообщение из тела кадра.
pub fn decode_message<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    bincode::deserialize(body).map_err(|_| Error::Malformed("сообщение не разбирается"))
}

/// Длина заголовка.
pub const HEADER_BYTES: usize = 4;

/// Упаковывает тело в кадр.
pub fn encode(body: &[u8]) -> Result<Vec<u8>> {
    if body.len() > MAX_FRAME_BYTES {
        return Err(Error::Malformed(
            "сообщение больше предельного размера кадра",
        ));
    }

    let mut frame = Vec::with_capacity(HEADER_BYTES + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(body);
    Ok(frame)
}

/// Читает объявленную длину из заголовка и проверяет её.
pub fn decode_length(header: &[u8]) -> Result<usize> {
    if header.len() < HEADER_BYTES {
        return Err(Error::Malformed("заголовок кадра неполный"));
    }

    let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if length > MAX_FRAME_BYTES {
        // Именно здесь обрывается попытка заставить брокер выделить
        // произвольный объём памяти.
        return Err(Error::Malformed("объявленная длина кадра превышает предел"));
    }
    Ok(length)
}

/// Разбирает кадр целиком. Возвращает тело и сколько байт израсходовано.
pub fn decode(buffer: &[u8]) -> Result<Option<(&[u8], usize)>> {
    if buffer.len() < HEADER_BYTES {
        return Ok(None);
    }

    let length = decode_length(buffer)?;
    let total = HEADER_BYTES + length;
    if buffer.len() < total {
        // Кадр ещё не пришёл целиком — это нормально для потока.
        return Ok(None);
    }
    Ok(Some((&buffer[HEADER_BYTES..total], total)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_survives_a_round_trip() {
        let frame = encode(b"hello").unwrap();
        let (body, used) = decode(&frame).unwrap().unwrap();
        assert_eq!(body, b"hello");
        assert_eq!(used, frame.len());
    }

    #[test]
    fn an_empty_body_is_a_valid_frame() {
        let frame = encode(&[]).unwrap();
        assert_eq!(frame.len(), HEADER_BYTES);
        let (body, _) = decode(&frame).unwrap().unwrap();
        assert!(body.is_empty());
    }

    #[test]
    fn a_partial_frame_is_not_an_error() {
        // Поток мог принести половину кадра — это обычное дело.
        let frame = encode("длинное сообщение".as_bytes()).unwrap();
        assert!(decode(&frame[..2]).unwrap().is_none());
        assert!(decode(&frame[..frame.len() - 1]).unwrap().is_none());
    }

    #[test]
    fn several_frames_are_read_one_by_one() {
        let mut stream = encode(b"first").unwrap();
        stream.extend(encode(b"second").unwrap());

        let (first, used) = decode(&stream).unwrap().unwrap();
        assert_eq!(first, b"first");
        let (second, _) = decode(&stream[used..]).unwrap().unwrap();
        assert_eq!(second, b"second");
    }

    #[test]
    fn an_oversized_declared_length_is_refused_before_allocating() {
        // Клиент объявил кадр на четыре гигабайта. Брокер работает
        // под SYSTEM, и выделять эту память он не станет.
        let header = u32::MAX.to_le_bytes();
        assert!(decode_length(&header).is_err());
        assert!(decode(&header).is_err());
    }

    #[test]
    fn an_oversized_body_cannot_even_be_encoded() {
        let huge = vec![0u8; MAX_FRAME_BYTES + 1];
        assert!(encode(&huge).is_err());
    }

    #[test]
    fn a_frame_at_the_limit_is_allowed() {
        let body = vec![7u8; MAX_FRAME_BYTES];
        let frame = encode(&body).unwrap();
        let (decoded, _) = decode(&frame).unwrap().unwrap();
        assert_eq!(decoded.len(), MAX_FRAME_BYTES);
    }

    #[test]
    fn a_request_survives_a_full_round_trip() {
        use crate::message::Request;
        use bamboo_policy::Action;

        let request = Request::Apply {
            action: Action::FreezeProcess,
            app_key: "slack.exe".into(),
            pid: Some(4242),
            dry_run: false,
        };

        // Кодируем как на отправку, читаем как на приёме.
        let frame = encode_message(&request).unwrap();
        let (body, _) = decode(&frame).unwrap().unwrap();
        let back: Request = decode_message(body).unwrap();

        assert_eq!(back, request);
    }

    #[test]
    fn a_response_survives_a_full_round_trip() {
        use crate::message::{ErrorCode, Response};

        let response = Response::Error {
            code: ErrorCode::RefusedByPolicy,
            detail: "приложение в белом списке".into(),
        };
        let frame = encode_message(&response).unwrap();
        let (body, _) = decode(&frame).unwrap().unwrap();
        let back: Response = decode_message(body).unwrap();

        assert_eq!(back, response);
    }

    #[test]
    fn garbage_body_is_a_clean_error_not_a_panic() {
        use crate::message::Request;
        // Случайные байты не должны разбираться в запрос и не должны
        // ронять процесс.
        let result: Result<Request> = decode_message(&[0xFF, 0x00, 0x13, 0x37]);
        assert!(result.is_err());
    }
}
