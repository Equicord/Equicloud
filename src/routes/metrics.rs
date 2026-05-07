use axum::{
    Extension, Router,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::get,
};
use serde_json::json;
use std::env;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tracing::{error, warn};

use equicloud::DatabaseService;
use equicloud::constants::{MS_PER_DAY, MS_PER_MONTH, MS_PER_WEEK};

const CACHE_TTL_SECS: u64 = 300;

struct MetricsConfig {
    enabled: bool,
    token: Option<String>,
}

static METRICS_CONFIG: OnceLock<MetricsConfig> = OnceLock::new();
static START_TIME: OnceLock<u64> = OnceLock::new();
static CACHED_COUNTS: OnceLock<Mutex<CachedUserCounts>> = OnceLock::new();
static REFRESH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct CachedUserCounts {
    counts: UserCounts,
    fetched_at: u64,
}

pub fn register() -> Router {
    let enabled = env::var("METRICS_ENABLED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(false);

    let token = env::var("METRICS_TOKEN").ok().filter(|s| !s.is_empty());

    let effective_enabled = if enabled && token.is_none() {
        error!(
            "METRICS_ENABLED=true but METRICS_TOKEN is unset; metrics endpoint will be disabled. \
             Set METRICS_TOKEN to a long random string to enable."
        );
        false
    } else {
        enabled
    };

    METRICS_CONFIG.get_or_init(|| MetricsConfig {
        enabled: effective_enabled,
        token,
    });

    START_TIME.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before UNIX epoch")
            .as_secs()
    });

    CACHED_COUNTS.get_or_init(|| {
        Mutex::new(CachedUserCounts {
            counts: UserCounts::default(),
            fetched_at: 0,
        })
    });

    REFRESH_LOCK.get_or_init(|| Mutex::new(()));

    Router::new().route("/metrics", get(get_metrics))
}

async fn get_metrics(
    Extension(db): Extension<DatabaseService>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(config) = METRICS_CONFIG.get() else {
        // Should be unreachable — register() runs before serve. Be defensive.
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    if !config.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }

    let expected_token = match &config.token {
        Some(t) => t,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let provided_token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    let provided = match provided_token {
        Some(t) => t,
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };

    if !constant_time_eq(provided.as_bytes(), expected_token.as_bytes()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_secs(0))
        .as_secs();

    let start_time = *START_TIME.get().unwrap_or(&0);
    let uptime = now.saturating_sub(start_time);

    let user_counts = get_cached_user_counts(&db, now).await;

    Json(json!({
        "users_day": user_counts.day,
        "users_week": user_counts.week,
        "users_month": user_counts.month,
        "users_total": user_counts.total,
        "uptime_seconds": uptime,
        "timestamp_ms": chrono::Utc::now().timestamp_millis()
    }))
    .into_response()
}

#[inline]
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[derive(Default, Clone)]
struct UserCounts {
    total: u64,
    week: u64,
    month: u64,
    day: u64,
}

async fn get_cached_user_counts(db: &DatabaseService, now: u64) -> UserCounts {
    let cache = CACHED_COUNTS.get().unwrap();

    let (cached_counts, age) = {
        let cached = cache.lock().await;
        (
            cached.counts.clone(),
            now.saturating_sub(cached.fetched_at),
        )
    };

    if age < CACHE_TTL_SECS {
        return cached_counts;
    }

    let refresh_lock = REFRESH_LOCK.get().unwrap();
    let _guard = match refresh_lock.try_lock() {
        Ok(g) => g,
        Err(_) => {
            // Another task is refreshing; serve stale.
            return cached_counts;
        }
    };

    // Re-check inside the refresh guard in case the prior holder just finished.
    {
        let cached = cache.lock().await;
        if now.saturating_sub(cached.fetched_at) < CACHE_TTL_SECS {
            return cached.counts.clone();
        }
    }

    match get_user_counts(db).await {
        Ok(counts) => {
            let mut cached = cache.lock().await;
            cached.counts = counts.clone();
            cached.fetched_at = now;
            counts
        }
        Err(e) => {
            warn!("Failed to refresh metrics user counts: {}", e);
            cached_counts
        }
    }
}

async fn get_user_counts(db: &DatabaseService) -> Result<UserCounts, anyhow::Error> {
    let now = chrono::Utc::now().timestamp_millis();
    let day_ago = now - MS_PER_DAY;
    let week_ago = now - MS_PER_WEEK;
    let month_ago = now - MS_PER_MONTH;

    let (total, day, week, month) = tokio::try_join!(
        query_total_count(db),
        query_count_since(db, day_ago),
        query_count_since(db, week_ago),
        query_count_since(db, month_ago),
    )?;

    Ok(UserCounts {
        total,
        day,
        week,
        month,
    })
}

async fn query_total_count(db: &DatabaseService) -> Result<u64, anyhow::Error> {
    let result = db
        .session()
        .query_unpaged("SELECT COUNT(*) FROM users", &[])
        .await?;

    let count = result
        .into_rows_result()?
        .rows::<(i64,)>()?
        .next()
        .transpose()?
        .map(|row| row.0 as u64)
        .unwrap_or(0);

    Ok(count)
}

async fn query_count_since(db: &DatabaseService, timestamp: i64) -> Result<u64, anyhow::Error> {
    let result = db
        .session()
        .query_unpaged(
            "SELECT COUNT(*) FROM users WHERE updated_at > ? ALLOW FILTERING",
            (timestamp,),
        )
        .await?;

    let count = result
        .into_rows_result()?
        .rows::<(i64,)>()?
        .next()
        .transpose()?
        .map(|row| row.0 as u64)
        .unwrap_or(0);

    Ok(count)
}
