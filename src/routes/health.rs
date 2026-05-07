use axum::{
    Extension, Router,
    http::StatusCode,
    response::{IntoResponse, Json, Redirect, Response},
    routing::get,
};
use serde_json::json;
use std::env;
use std::sync::OnceLock;
use tracing::debug;

use equicloud::DatabaseService;

static REDIRECT_URL: OnceLock<Option<String>> = OnceLock::new();

pub fn register() -> Router {
    REDIRECT_URL.get_or_init(|| {
        env::var("API_ROOT_REDIRECT_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .filter(|s| is_safe_redirect_url(s))
    });

    Router::new()
        .route("/health", get(health_check))
        .route("/", get(root_redirect))
}

async fn health_check(Extension(db): Extension<DatabaseService>) -> Response {
    match db.health_check().await {
        Ok(()) => (StatusCode::OK, Json(json!({"status": "ok"}))).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "unavailable"})),
        )
            .into_response(),
    }
}

fn is_safe_redirect_url(url: &str) -> bool {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return false;
    }
    !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

async fn root_redirect() -> Response {
    if let Some(Some(redirect_url)) = REDIRECT_URL.get() {
        debug!("Redirecting / to configured URL");
        return Redirect::temporary(redirect_url).into_response();
    }

    Json(json!({
        "service": "equicloud",
    }))
    .into_response()
}
