use anyhow::Result;
use scylla::client::PoolSize;
use scylla::client::execution_profile::ExecutionProfile;
use scylla::client::session::Session;
use scylla::client::session_builder::SessionBuilder;
use scylla::frame::Compression;
use scylla::policies::load_balancing::DefaultPolicy;
use scylla::policies::retry::DefaultRetryPolicy;
use scylla::policies::speculative_execution::SimpleSpeculativeExecutionPolicy;
use scylla::statement::{Consistency, SerialConsistency};
use std::env;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

pub mod constants;
pub mod database;
pub mod migrations;
pub mod types;
pub mod utils;

pub use database::DatabaseService;
pub use migrations::MigrationRunner;
pub use types::{DataEntry, DataManifestEntry, KeyValidationError};
pub use utils::{compress, compute_checksum, decompress, error_response, validate_key};

pub async fn create_database_connection() -> Result<Session> {
    let uri = env::var("SCYLLA_URI").unwrap_or_else(|_| constants::DEFAULT_SCYLLA_URI.to_string());
    let username = env::var("SCYLLA_USERNAME").ok();
    let password = env::var("SCYLLA_PASSWORD").ok();

    let pool_size = parse_env_usize("SCYLLA_POOL_SIZE", 4);
    let pool_size = match NonZeroUsize::new(pool_size) {
        Some(n) => n,
        None => {
            warn!("SCYLLA_POOL_SIZE must be > 0; falling back to 4");
            NonZeroUsize::new(4).expect("4 is non-zero")
        }
    };

    let connection_timeout: u64 = parse_env_u64("SCYLLA_CONNECTION_TIMEOUT_MS", 5000);
    let request_timeout_ms: u64 = parse_env_u64("SCYLLA_REQUEST_TIMEOUT_MS", 5000);

    // Build a load-balancing policy with explicit DC preference. Reads
    // `SCYLLA_LOCAL_DC` from env; if unset, the policy still works (driver
    // falls back to the contact-point DC) but multi-DC deployments should
    // set this to avoid surprise cross-DC routing.
    let lb_builder = DefaultPolicy::builder().permit_dc_failover(false);
    let load_balancing = match env::var("SCYLLA_LOCAL_DC")
        .ok()
        .filter(|s| !s.is_empty())
    {
        Some(dc) => lb_builder.prefer_datacenter(dc).build(),
        None => lb_builder.build(),
    };

    // Speculative execution: on slow coordinator responses, send the same
    // (idempotent) query to a second replica. Two extra attempts at 50 ms
    // intervals catches stragglers without amplifying load.
    let spec_exec = SimpleSpeculativeExecutionPolicy {
        max_retry_count: 2,
        retry_interval: Duration::from_millis(50),
    };

    let mut session_builder = SessionBuilder::new()
        .known_node(&uri)
        .connection_timeout(Duration::from_millis(connection_timeout))
        .pool_size(PoolSize::PerShard(pool_size))
        .default_execution_profile_handle(
            ExecutionProfile::builder()
                .load_balancing_policy(load_balancing)
                .retry_policy(Arc::new(DefaultRetryPolicy::new()))
                .speculative_execution_policy(Some(Arc::new(spec_exec)))
                // Tighter than the 30s driver default — bounds tail latency
                // when a coordinator is slow. Configurable per-deployment.
                .request_timeout(Some(Duration::from_millis(request_timeout_ms)))
                .consistency(Consistency::LocalQuorum)
                .serial_consistency(Some(SerialConsistency::LocalSerial))
                .build()
                .into_handle(),
        )
        .compression(Some(Compression::Lz4))
        .tcp_nodelay(true)
        // Avoid first-query stalls after NAT/load-balancer idle-kill.
        .keepalive_interval(Duration::from_secs(30))
        .keepalive_timeout(Duration::from_secs(10));

    if let (Some(user), Some(pass)) = (username, password) {
        session_builder = session_builder.user(user, pass);
    }

    let session = session_builder.build().await?;
    Ok(session)
}

fn parse_env_usize(name: &str, default: usize) -> usize {
    parse_env_inner(name, default)
}

fn parse_env_u64(name: &str, default: u64) -> u64 {
    parse_env_inner(name, default)
}

fn parse_env_inner<T>(name: &str, default: T) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(name) {
        Ok(v) if !v.is_empty() => match v.parse() {
            Ok(parsed) => parsed,
            Err(e) => {
                tracing::error!(
                    "Invalid value for {}={:?}: {}; falling back to default",
                    name,
                    v,
                    e
                );
                default
            }
        },
        _ => default,
    }
}
