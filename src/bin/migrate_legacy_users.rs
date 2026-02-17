//! Migration tool for cleaning up legacy CRC32-hashed user data
//!
//! Usage:
//!   cargo run --bin migrate_legacy_users -- [--dry-run] [--delete-legacy]
//!
//! This tool will:
//! 1. Scan the database for entries using legacy CRC32 hash format
//! 2. Report on any legacy entries found
//! 3. Optionally delete legacy entries (with --delete-legacy flag)

use dotenv::dotenv;
use equicloud::constants::DEFAULT_SCYLLA_URI;
use equicloud::hash_migration::is_legacy_key;
use scylla::client::session::Session;
use std::env;
use tracing::{error, info};

#[tokio::main]
async fn main() {
    dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = env::args().collect();
    let delete_flag = "--delete-legacy".to_string();
    let delete_legacy = args.contains(&delete_flag);
    let dry_run = !delete_legacy;

    if dry_run {
        info!("Running in DRY-RUN mode - no data will be deleted");
        info!("Use --delete-legacy flag to actually delete legacy entries");
    }

    info!("Connecting to database...");
    let session = match create_database_connection().await {
        Ok(session) => session,
        Err(e) => {
            error!("Failed to connect to database: {}", e);
            std::process::exit(1);
        }
    };

    info!("Connected to database");

    match session.use_keyspace("equicloud", false).await {
        Ok(_) => info!("Using keyspace: equicloud"),
        Err(e) => {
            error!("Failed to use keyspace: {}", e);
            std::process::exit(1);
        }
    }

    let query = "SELECT id FROM users";
    let result = match session.query_unpaged(query, &[]).await {
        Ok(result) => result,
        Err(e) => {
            error!("Failed to query users: {}", e);
            std::process::exit(1);
        }
    };

    let rows_result = match result.into_rows_result() {
        Ok(rows) => rows,
        Err(e) => {
            error!("Failed to get rows: {}", e);
            std::process::exit(1);
        }
    };

    let mut total_users = 0;
    let mut legacy_users = 0;
    let mut deleted_count = 0;

    info!("Scanning for legacy entries...");

    let rows = match rows_result.rows::<(String,)>() {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to parse rows: {}", e);
            std::process::exit(1);
        }
    };

    for row in rows {
        let (user_id,) = match row {
            Ok(id) => id,
            Err(e) => {
                error!("Failed to parse row: {}", e);
                continue;
            }
        };

        total_users += 1;

        if is_legacy_key(&user_id) {
            legacy_users += 1;
            info!("Found legacy entry: {}", user_id);

            if delete_legacy {
                info!("Deleting legacy entry: {}", user_id);
                let delete_query = "DELETE FROM users WHERE id = ?";
                match session.query_unpaged(delete_query, (&user_id,)).await {
                    Ok(_) => {
                        deleted_count += 1;
                        info!("Deleted: {}", user_id);
                    }
                    Err(e) => {
                        error!("Failed to delete {}: {}", user_id, e);
                    }
                }
            }
        }
    }

    info!("Scan complete!");
    info!("Total entries: {}", total_users);
    info!("Legacy entries found: {}", legacy_users);

    if delete_legacy {
        info!("Legacy entries deleted: {}", deleted_count);
    } else {
        info!("Run with --delete-legacy to remove these entries");
    }

    if legacy_users > 0 && !delete_legacy {
        info!("\nLegacy entries detected. These entries use the old CRC32 hash format.");
        info!("They will be automatically migrated when users next access their settings.");
        info!(
            "To clean up orphaned entries now, run: cargo run --bin migrate_legacy_users -- --delete-legacy"
        );
    } else if legacy_users == 0 {
        info!("No legacy entries found - migration complete!");
    }
}

async fn create_database_connection() -> anyhow::Result<Session> {
    let uri = env::var("SCYLLA_URI").unwrap_or_else(|_| DEFAULT_SCYLLA_URI.to_string());
    let username = env::var("SCYLLA_USERNAME").ok();
    let password = env::var("SCYLLA_PASSWORD").ok();

    let session_builder = scylla::client::session_builder::SessionBuilder::new().known_node(uri);

    let session = if let (Some(user), Some(pass)) = (username, password) {
        session_builder.user(user, pass).build().await?
    } else {
        session_builder.build().await?
    };
    Ok(session)
}
