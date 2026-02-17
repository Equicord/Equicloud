use axum::{
    Extension, Router,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
};
use serde_json::json;
use std::env;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::error;

use equicloud::DatabaseService;
use equicloud::constants::{MS_PER_DAY, MS_PER_MONTH, MS_PER_WEEK};

static START_TIME: OnceLock<u64> = OnceLock::new();

pub fn register() -> Router {
    START_TIME.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before UNIX epoch")
            .as_secs()
    });

    Router::new().route("/metrics", get(get_metrics))
}

async fn get_metrics(Extension(db): Extension<DatabaseService>) -> impl IntoResponse {
    let metrics_enabled = env::var("METRICS_ENABLED")
        .unwrap_or_else(|_| "false".to_string())
        .parse::<bool>()
        .unwrap_or(false);

    if !metrics_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_secs(0))
        .as_secs();

    let start_time = *START_TIME.get().unwrap_or(&0);
    let uptime = now - start_time;

    let user_counts = match get_user_counts(&db).await {
        Ok(counts) => counts,
        Err(e) => {
            error!("Failed to get user counts for metrics: {}", e);
            UserCounts::default()
        }
    };

    Json(json!({
        "users_day": user_counts.day,
        "users_week": user_counts.week,
        "users_month": user_counts.month,
        "users_total": user_counts.total,
        "uptime_seconds": uptime,
        "timestamp": chrono::Utc::now().timestamp()
    }))
    .into_response()
}

#[derive(Default)]
struct UserCounts {
    total: u64,
    week: u64,
    month: u64,
    day: u64,
}

async fn get_user_counts(db: &DatabaseService) -> Result<UserCounts, anyhow::Error> {
    let now = chrono::Utc::now().timestamp_millis();
    let day_ago = now - MS_PER_DAY;
    let week_ago = now - MS_PER_WEEK;
    let month_ago = now - MS_PER_MONTH;

    let total = query_total_count(db).await?;
    let day = query_count_since(db, day_ago).await?;
    let week = query_count_since(db, week_ago).await?;
    let month = query_count_since(db, month_ago).await?;

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
            "SELECT COUNT(*) FROM users WHERE updated_at > ?",
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
