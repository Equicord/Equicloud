use axum::{
    Extension,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde_json::json;
use tracing::error;

use equicloud::DatabaseService;

pub async fn get_user_info() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "timestamp": chrono::Utc::now().timestamp(),
        "service": "equicloud"
    }))
}

pub async fn delete_all_user_data(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
) -> impl IntoResponse {
    if let Err(e) = db.delete_user_settings(&user_id).await {
        error!("Failed to delete user settings: {}", e);
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    if let Err(e) = db.delete_all_data(&user_id).await {
        error!("Failed to delete user data: {}", e);
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    StatusCode::NO_CONTENT
}
