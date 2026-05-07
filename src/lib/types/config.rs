use std::env;
use std::fmt::{self, Display};
use std::str::FromStr;

use tracing::error;

use crate::constants::{
    DEFAULT_COMPRESSION_ENABLED, DEFAULT_DATASTORE_ENABLED, DEFAULT_MAX_BACKUP_SIZE,
    DEFAULT_MAX_SYNC_MANIFEST_ENTRIES, DEFAULT_MAX_SYNC_UPLOADS, DEFAULT_ZSTD_COMPRESSION_LEVEL,
    MAX_DATASTORE_KEY_SIZE, MAX_KEY_SIZE,
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
                "Key contains invalid characters (allowed: alphanumeric, _, -, ., /; no `..` traversal)"
            }
        }
    }
}

impl Display for KeyValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for KeyValidationError {}

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
    pub discord_allow_all_users: bool,
    pub max_sync_uploads: usize,
    pub max_sync_manifest_entries: usize,
}

impl Config {
    pub fn from_env() -> Self {
        let compression_level: i32 =
            parse_env("COMPRESSION_LEVEL", DEFAULT_ZSTD_COMPRESSION_LEVEL).clamp(1, 22);

        Self {
            max_backup_size_bytes: parse_env("MAX_BACKUP_SIZE_BYTES", DEFAULT_MAX_BACKUP_SIZE),
            max_key_size_bytes: parse_env("MAX_KEY_SIZE_BYTES", MAX_KEY_SIZE),
            max_datastore_key_size_bytes: parse_env(
                "MAX_DATASTORE_KEY_SIZE_BYTES",
                MAX_DATASTORE_KEY_SIZE,
            ),
            compression_enabled: parse_env("COMPRESSION_ENABLED", DEFAULT_COMPRESSION_ENABLED),
            compression_level,
            datastore_enabled: parse_env("DATASTORE_ENABLED", DEFAULT_DATASTORE_ENABLED),
            discord_client_id: env::var("DISCORD_CLIENT_ID").unwrap_or_default(),
            discord_client_secret: env::var("DISCORD_CLIENT_SECRET").unwrap_or_default(),
            server_fqdn: env::var("SERVER_FQDN").unwrap_or_default(),
            discord_allowed_user_ids: env::var("DISCORD_ALLOWED_USER_IDS").ok(),
            discord_allow_all_users: parse_env("DISCORD_ALLOW_ALL_USERS", false),
            max_sync_uploads: parse_env("MAX_SYNC_UPLOADS", DEFAULT_MAX_SYNC_UPLOADS),
            max_sync_manifest_entries: parse_env(
                "MAX_SYNC_MANIFEST_ENTRIES",
                DEFAULT_MAX_SYNC_MANIFEST_ENTRIES,
            ),
        }
    }

    pub fn redirect_uri(&self) -> String {
        format!("{}/v1/oauth/callback", self.server_fqdn)
    }
}

fn parse_env<T>(name: &str, default: T) -> T
where
    T: FromStr,
    T::Err: Display,
{
    match env::var(name) {
        Ok(v) if !v.is_empty() => match v.parse() {
            Ok(parsed) => parsed,
            Err(e) => {
                error!(
                    "Invalid value for {}={:?}: {}; falling back to default",
                    name, v, e
                );
                default
            }
        },
        _ => default,
    }
}
