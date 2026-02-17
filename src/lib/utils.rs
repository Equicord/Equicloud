use once_cell::sync::Lazy;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::env;

use crate::constants::{
    CHECKSUM_BYTES, DEFAULT_COMPRESSION_ENABLED, DEFAULT_DATASTORE_ENABLED,
    DEFAULT_MAX_BACKUP_SIZE, DEFAULT_ZSTD_COMPRESSION_LEVEL, MAX_DATASTORE_KEY_SIZE,
    MAX_DECOMPRESSION_SIZE, MAX_KEY_NAME_LEN, MAX_KEY_SIZE,
};
use crate::hash_migration::sha256;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyValidationError {
    Empty,
    TooLong,
    InvalidChars,
}

impl KeyValidationError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "Key cannot be empty",
            Self::TooLong => "Key name exceeds 256 characters",
            Self::InvalidChars => {
                "Key contains invalid characters (allowed: alphanumeric, _, -, ., /)"
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
    Ok(())
}

#[derive(Clone)]
pub struct Config {
    pub max_backup_size_bytes: usize,
    pub max_key_size_bytes: usize,
    pub max_datastore_key_size_bytes: usize,
    pub compression_enabled: bool,
    pub compression_level: i32,
    pub datastore_enabled: bool,
    pub discord_client_id: String,
    pub discord_client_secret: String,
    pub server_fqdn: String,
    pub discord_allowed_user_ids: Option<String>,
    pub cors_allowed_origins: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            max_backup_size_bytes: env::var("MAX_BACKUP_SIZE_BYTES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_MAX_BACKUP_SIZE),
            max_key_size_bytes: env::var("MAX_KEY_SIZE_BYTES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(MAX_KEY_SIZE),
            max_datastore_key_size_bytes: env::var("MAX_DATASTORE_KEY_SIZE_BYTES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(MAX_DATASTORE_KEY_SIZE),
            compression_enabled: env::var("COMPRESSION_ENABLED")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_COMPRESSION_ENABLED),
            compression_level: env::var("COMPRESSION_LEVEL")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_ZSTD_COMPRESSION_LEVEL),
            datastore_enabled: env::var("DATASTORE_ENABLED")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_DATASTORE_ENABLED),
            discord_client_id: env::var("DISCORD_CLIENT_ID").unwrap_or_default(),
            discord_client_secret: env::var("DISCORD_CLIENT_SECRET").unwrap_or_default(),
            server_fqdn: env::var("SERVER_FQDN").unwrap_or_default(),
            discord_allowed_user_ids: env::var("DISCORD_ALLOWED_USER_IDS").ok(),
            cors_allowed_origins: env::var("CORS_ALLOWED_ORIGINS").ok(),
        }
    }

    pub fn redirect_uri(&self) -> String {
        format!("{}/v1/oauth/callback", self.server_fqdn)
    }
}

pub static CONFIG: Lazy<Config> = Lazy::new(Config::from_env);

pub fn error_response(message: &str) -> Value {
    json!({
        "error": message
    })
}
