use once_cell::sync::Lazy;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::constants::{CHECKSUM_BYTES, MAX_DECOMPRESSION_SIZE, MAX_KEY_NAME_LEN};
use crate::hash_migration::sha256;
use crate::types::{Config, KeyValidationError};

pub fn hash_user_id(user_id: &str) -> String {
    sha256::hash_user_id(user_id)
}

pub fn get_user_secret(user_id: &str) -> String {
    sha256::get_user_secret(user_id)
}

pub fn compute_checksum(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(&hasher.finalize()[..CHECKSUM_BYTES])
}

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

pub fn compress(data: &[u8]) -> Vec<u8> {
    if !CONFIG.compression_enabled || data.is_empty() {
        return data.to_vec();
    }
    let capacity = zstd::zstd_safe::compress_bound(data.len());
    let mut output = Vec::with_capacity(capacity);

    if zstd::stream::copy_encode(data, &mut output, CONFIG.compression_level).is_err() {
        return data.to_vec();
    }

    if output.len() < data.len() {
        output
    } else {
        data.to_vec()
    }
}

pub fn decompress(data: &[u8]) -> Vec<u8> {
    if data.len() < 4 || data[..4] != ZSTD_MAGIC {
        return data.to_vec();
    }

    let mut decoder = match zstd::stream::Decoder::new(data) {
        Ok(d) => d,
        Err(_) => return data.to_vec(),
    };

    let estimated_size = data.len().saturating_mul(4).min(MAX_DECOMPRESSION_SIZE);
    let mut output = Vec::with_capacity(estimated_size);

    use std::io::Read;
    let limit = MAX_DECOMPRESSION_SIZE as u64 + 1;
    let mut limited_reader = (&mut decoder).take(limit);

    if limited_reader.read_to_end(&mut output).is_ok() {
        if output.len() > MAX_DECOMPRESSION_SIZE {
            return data.to_vec();
        }
        output
    } else {
        data.to_vec()
    }
}

pub fn validate_key(key: &str) -> Result<(), KeyValidationError> {
    if key.is_empty() {
        return Err(KeyValidationError::Empty);
    }
    if key.len() > MAX_KEY_NAME_LEN {
        return Err(KeyValidationError::TooLong);
    }
    if !key
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' || b == b'/')
    {
        return Err(KeyValidationError::InvalidChars);
    }
    Ok(())
}

pub static CONFIG: Lazy<Config> = Lazy::new(Config::from_env);

pub fn error_response(message: &str) -> Value {
    json!({
        "error": message
    })
}
