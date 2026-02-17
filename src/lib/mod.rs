use anyhow::Result;
use scylla::client::PoolSize;
use scylla::client::execution_profile::ExecutionProfile;
use scylla::client::session::Session;
use scylla::client::session_builder::SessionBuilder;
use scylla::frame::Compression;
use scylla::policies::load_balancing::DefaultPolicy;
use scylla::policies::retry::DefaultRetryPolicy;
use std::env;
use std::sync::Arc;
use std::time::Duration;

pub mod constants;
pub mod database;
pub mod hash_migration;
pub mod migrations;
pub mod utils;

pub use database::{DataEntry, DataManifestEntry, DatabaseService};
pub use migrations::MigrationRunner;
pub use utils::{KeyValidationError, compress, compute_checksum, decompress, validate_key};

pub async fn create_database_connection() -> Result<Session> {
    let uri = env::var("SCYLLA_URI").unwrap_or_else(|_| constants::DEFAULT_SCYLLA_URI.to_string());
    let username = env::var("SCYLLA_USERNAME").ok();
    let password = env::var("SCYLLA_PASSWORD").ok();

    let pool_size: usize = env::var("SCYLLA_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);

    let connection_timeout: u64 = env::var("SCYLLA_CONNECTION_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5000);

    let load_balancing = DefaultPolicy::builder().build();

    let mut session_builder = SessionBuilder::new()
        .known_node(&uri)
        .connection_timeout(Duration::from_millis(connection_timeout))
        .pool_size(PoolSize::PerShard(
            std::num::NonZeroUsize::new(pool_size).expect("pool size must be > 0"),
        ))
        .default_execution_profile_handle(
            ExecutionProfile::builder()
                .load_balancing_policy(load_balancing)
                .retry_policy(Arc::new(DefaultRetryPolicy::new()))
                .request_timeout(Some(Duration::from_secs(30)))
                .build()
                .into_handle(),
        )
        .compression(Some(Compression::Lz4))
        .tcp_nodelay(true);

    if let (Some(user), Some(pass)) = (username, password) {
        session_builder = session_builder.user(user, pass);
    }

    let session = session_builder.build().await?;
    Ok(session)
}
