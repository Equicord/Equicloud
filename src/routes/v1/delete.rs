use axum::{
    Extension,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;
use tracing::error;

use crate::middleware::auth::invalidate_user_cache;
use equicloud::DatabaseService;
use equicloud::utils::error_response;

pub async fn v1_status_pong() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "equicloud",
    }))
}

pub async fn delete_all_user_data(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
) -> Response {
    match db.delete_user_account(&user_id).await {
        Ok(_) => {
            invalidate_user_cache(&user_id).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            error!("Failed to delete user account: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(error_response("Failed to delete account")),
            )
                .into_response()
        }
    }
}
