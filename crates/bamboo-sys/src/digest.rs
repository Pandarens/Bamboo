//! Подсчёт SHA-256 средствами Windows.
//!
//! Нужен, чтобы проверить скачанное обновление. Своей реализации хеша здесь
//! не будет: в системе есть проверенная, а самодельная криптография в
//! программе, которая скачивает и запускает файлы, — плохая затея даже
//! когда написана правильно.
//!
//! Что эта проверка даёт и чего не даёт, стоит сказать прямо. Она ловит
//! оборванную и повреждённую загрузку — то, что случается само собой.
//! От подмены она не защищает: тот, кто сможет подменить файл выпуска,
//! подменит рядом и его хеш. Настоящая защита от подмены — подпись, и её
//! здесь нет.

use bamboo_core::{Error, Result};
use windows_sys::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash,
    BCryptHashData, BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE,
};

/// Длина SHA-256 в байтах.
const SHA256_LENGTH: usize = 32;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Считает SHA-256 и возвращает его строкой из шестнадцатеричных цифр
/// в нижнем регистре — в том же виде, в каком его печатают все прочие
/// средства, чтобы значения можно было сверить глазами.
pub fn sha256_hex(data: &[u8]) -> Result<String> {
    let mut algorithm: BCRYPT_ALG_HANDLE = core::ptr::null_mut();
    let status = unsafe {
        BCryptOpenAlgorithmProvider(
            &mut algorithm,
            wide("SHA256").as_ptr(),
            core::ptr::null(),
            0,
        )
    };
    if status != 0 {
        return Err(Error::Nt {
            call: "BCryptOpenAlgorithmProvider",
            status,
        });
    }

    let digest = hash_with(algorithm, data);
    unsafe { BCryptCloseAlgorithmProvider(algorithm, 0) };

    let digest = digest?;
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn hash_with(algorithm: BCRYPT_ALG_HANDLE, data: &[u8]) -> Result<[u8; SHA256_LENGTH]> {
    let mut hash: BCRYPT_HASH_HANDLE = core::ptr::null_mut();
    let status = unsafe {
        BCryptCreateHash(
            algorithm,
            &mut hash,
            core::ptr::null_mut(),
            0,
            core::ptr::null(),
            0,
            0,
        )
    };
    if status != 0 {
        return Err(Error::Nt {
            call: "BCryptCreateHash",
            status,
        });
    }

    let result = feed_and_finish(hash, data);
    unsafe { BCryptDestroyHash(hash) };
    result
}

fn feed_and_finish(hash: BCRYPT_HASH_HANDLE, data: &[u8]) -> Result<[u8; SHA256_LENGTH]> {
    // Пустые данные скармливать нельзя: BCryptHashData не принимает нулевой
    // указатель. Хеш пустой строки при этом вполне определён, и получается
    // он сам собой — достаточно ничего не добавлять.
    if !data.is_empty() {
        let status = unsafe { BCryptHashData(hash, data.as_ptr(), data.len() as u32, 0) };
        if status != 0 {
            return Err(Error::Nt {
                call: "BCryptHashData",
                status,
            });
        }
    }

    let mut digest = [0u8; SHA256_LENGTH];
    let status = unsafe { BCryptFinishHash(hash, digest.as_mut_ptr(), SHA256_LENGTH as u32, 0) };
    if status != 0 {
        return Err(Error::Nt {
            call: "BCryptFinishHash",
            status,
        });
    }
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values_match() {
        // Проверочные значения из стандарта: если сойдутся они, сойдётся
        // и всё остальное.
        assert_eq!(
            sha256_hex(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"").unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn a_single_changed_byte_changes_everything() {
        // Ради этого хеш и считается: оборванная загрузка обязана
        // отличаться от целой.
        let one = sha256_hex(b"bamboo").unwrap();
        let other = sha256_hex(b"bambop").unwrap();
        assert_ne!(one, other);
    }

    #[test]
    fn the_result_is_lowercase_hex_of_the_right_length() {
        let digest = sha256_hex("что угодно".as_bytes()).unwrap();
        assert_eq!(digest.len(), SHA256_LENGTH * 2);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(digest, digest.to_lowercase());
    }

    #[test]
    fn large_input_is_handled() {
        let big = vec![7u8; 8 << 20];
        assert_eq!(sha256_hex(&big).unwrap().len(), SHA256_LENGTH * 2);
    }
}
