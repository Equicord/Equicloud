use axum::{Extension, Json, http::StatusCode, response::IntoResponse};
use tracing::error;

use equicloud::DatabaseService;
use equicloud::types::ManifestResponse;
use equicloud::utils::{CONFIG, error_response};

pub async fn get_manifest(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
) -> impl IntoResponse {
    let mut entries = match db.get_data_manifest(&user_id).await {
        Ok(e) => e,
        Err(e) => {
            error!("Failed to get manifest: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(error_response("Failed to get manifest")),
            )
                .into_response();
        }
    };

    if !CONFIG.datastore_enabled {
        entries.retain(|e| !e.key.starts_with("dataStore/"));
    }

    let total_size: i64 = entries.iter().map(|e| e.size_bytes as i64).sum();

    Json(ManifestResponse {
        entries,
        total_size,
    })
    .into_response()
}
