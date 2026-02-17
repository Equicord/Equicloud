use axum::{Extension, Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use tracing::error;

use equicloud::utils::CONFIG;
use equicloud::{DataManifestEntry, DatabaseService};

#[derive(Serialize)]
pub struct ManifestResponse {
    entries: Vec<DataManifestEntry>,
    total_size: i64,
}

pub async fn get_manifest(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
) -> impl IntoResponse {
    let entries = match db.get_data_manifest(&user_id).await {
        Ok(e) => e,
        Err(e) => {
            error!("Failed to get manifest: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Failed to get manifest"})),
            )
                .into_response();
        }
    };

    let entries: Vec<DataManifestEntry> = if CONFIG.datastore_enabled {
        entries
    } else {
        entries
            .into_iter()
            .filter(|e| !e.key.starts_with("dataStore/"))
            .collect()
    };

    let total_size: i64 = entries.iter().map(|e| e.size_bytes as i64).sum();

    Json(ManifestResponse {
        entries,
        total_size,
    })
    .into_response()
}
