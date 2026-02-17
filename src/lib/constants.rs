pub const DEFAULT_HOST: &str = "0.0.0.0";
pub const DEFAULT_PORT: &str = "8080";
pub const DEFAULT_SCYLLA_URI: &str = "127.0.0.1:9042";

pub const DEFAULT_MAX_BACKUP_SIZE: usize = 62_914_560; // 60 MB

pub const DISCORD_TOKEN_URL: &str = "https://discord.com/api/oauth2/token";
pub const DISCORD_USER_URL: &str = "https://discord.com/api/users/@me";

pub const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;
pub const MS_PER_WEEK: i64 = 7 * MS_PER_DAY;
pub const MS_PER_MONTH: i64 = 30 * MS_PER_DAY;

pub const DB_HEALTH_CHECK_INTERVAL_SECS: u64 = 30;

pub const MAX_KEY_SIZE: usize = 1_048_576; // 1 MB
pub const MAX_DATASTORE_KEY_SIZE: usize = 10_485_760; // 10 MB (compresses to <1MB)
pub const MAX_KEY_NAME_LEN: usize = 256;
pub const DEFAULT_DATASTORE_ENABLED: bool = false;

pub const DEFAULT_ZSTD_COMPRESSION_LEVEL: i32 = 3;
pub const CHECKSUM_BYTES: usize = 8;
pub const DEFAULT_COMPRESSION_ENABLED: bool = true;

pub const MAX_DECOMPRESSION_SIZE: usize = 10_485_760; // 10 MB
