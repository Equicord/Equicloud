use std::env;

use crate::constants::{
    DEFAULT_COMPRESSION_ENABLED, DEFAULT_DATASTORE_ENABLED, DEFAULT_MAX_BACKUP_SIZE,
    DEFAULT_ZSTD_COMPRESSION_LEVEL, MAX_DATASTORE_KEY_SIZE, MAX_KEY_SIZE,
};

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
