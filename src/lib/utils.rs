use once_cell::sync::Lazy;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::constants::{CHECKSUM_BYTES, MAX_DECOMPRESSION_SIZE, MAX_KEY_NAME_LEN};
use crate::types::{Config, KeyValidationError};

pub static CONFIG: Lazy<Config> = Lazy::new(Config::from_env);

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
const DECOMPRESS_INITIAL_CAPACITY_CAP: usize = 64 * 1024;

#[inline]
fn starts_with_zstd_magic(data: &[u8]) -> bool {
    data.len() >= 4 && data[..4] == ZSTD_MAGIC
}

pub fn hash_user_id(user_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(user_id.as_bytes());
    let digest = hasher.finalize();
    // Single allocation instead of `format!` + `hex::encode` (two allocs).
    let prefix_bytes: [u8; 8] = digest[..8].try_into().unwrap();
    format!("settings:{:016x}", u64::from_be_bytes(prefix_bytes))
}

pub fn compute_checksum(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(&hasher.finalize()[..CHECKSUM_BYTES])
}

pub fn compress(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }

    let must_disambiguate = starts_with_zstd_magic(data);

    if !CONFIG.compression_enabled && !must_disambiguate {
        return data.to_vec();
    }

    // `zstd::bulk::compress` is ~2× faster than `zstd::stream::copy_encode`
    // for in-memory data because it skips the streaming-encoder framing.
    let output = match zstd::bulk::compress(data, CONFIG.compression_level) {
        Ok(out) => out,
        Err(e) => {
            warn!("zstd compress failed; storing raw: {}", e);
            if must_disambiguate {
                warn!(
                    "raw payload starts with zstd magic and compression failed; stored value will be ambiguous on read"
                );
            }
            return data.to_vec();
        }
    };

    if !must_disambiguate && output.len() >= data.len() {
        // Raw is smaller and unambiguous; prefer it.
        return data.to_vec();
    }

    output
}

pub fn decompress(data: &[u8]) -> Vec<u8> {
    if !starts_with_zstd_magic(data) {
        return data.to_vec();
    }

    // Bulk decompress with an upper-bounded output buffer. We don't know the
    // decompressed size in advance, so start with a sensible capacity and let
    // the bulk API grow if needed (still capped by MAX_DECOMPRESSION_SIZE).
    let initial_capacity = data
        .len()
        .saturating_mul(4)
        .min(DECOMPRESS_INITIAL_CAPACITY_CAP);
    let mut output = vec![0u8; initial_capacity];
    let mut decompressor = match zstd::bulk::Decompressor::new() {
        Ok(d) => d,
        Err(e) => {
            warn!("zstd decompressor init failed; returning raw bytes: {}", e);
            return data.to_vec();
        }
    };

    // Try at progressively larger output buffers, capped at MAX_DECOMPRESSION_SIZE.
    let mut buf_size = initial_capacity.max(1024);
    loop {
        output.resize(buf_size, 0);
        match decompressor.decompress_to_buffer(data, &mut output) {
            Ok(decompressed) => {
                output.truncate(decompressed);
                return output;
            }
            Err(_) if buf_size < MAX_DECOMPRESSION_SIZE => {
                buf_size = (buf_size.saturating_mul(2)).min(MAX_DECOMPRESSION_SIZE);
            }
            Err(e) => {
                warn!(
                    "zstd decompress failed at {} bytes; returning raw bytes: {}",
                    buf_size, e
                );
                return data.to_vec();
            }
        }
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
    if key.contains("..") || key.starts_with('/') || key.ends_with('/') || key.contains("//") {
        return Err(KeyValidationError::InvalidChars);
    }
    // Reject any path segment that consists only of dots or that starts with a
    // dot — `dataStore/.foo`, `foo/.`, `foo/.bar` could otherwise be treated as
    // hidden / parent-dir markers by downstream filesystem-aware consumers.
    if key
        .split('/')
        .any(|seg| seg.is_empty() || seg.starts_with('.'))
    {
        return Err(KeyValidationError::InvalidChars);
    }
    Ok(())
}

pub fn error_response(message: &str) -> Value {
    json!({
        "error": message
    })
}
