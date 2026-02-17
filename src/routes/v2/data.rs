use axum::{
    Extension, Json,
    body::{Body, Bytes},
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use tracing::error;

use equicloud::utils::CONFIG;
use equicloud::{DatabaseService, compute_checksum, validate_key};

pub async fn get_data(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = validate_key(&key) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.message()})),
        )
            .into_response();
    }

    if !CONFIG.datastore_enabled && key.starts_with("dataStore/") {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "DataStore sync is disabled"})),
        )
            .into_response();
    }

    let entry = match db.get_data_key(&user_id, &key).await {
        Ok(Some(e)) => e,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, HeaderMap::new(), Body::empty()).into_response();
        }
        Err(e) => {
            error!("Failed to get data key: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                HeaderMap::new(),
                Body::empty(),
            )
                .into_response();
        }
    };

    if let Some(if_none_match) = headers.get("if-none-match")
        && if_none_match.to_str().unwrap_or("") == entry.checksum
    {
        return (StatusCode::NOT_MODIFIED, HeaderMap::new(), Body::empty()).into_response();
    }

    let mut response_headers = HeaderMap::new();
    if let Ok(v) = "application/octet-stream".parse() {
        response_headers.insert("Content-Type", v);
    }
    if let Ok(v) = entry.checksum.parse() {
        response_headers.insert("ETag", v);
    }
    if let Ok(v) = entry.version.to_string().parse() {
        response_headers.insert("X-Version", v);
    }

    (StatusCode::OK, response_headers, Body::from(entry.value)).into_response()
}

pub async fn put_data(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = validate_key(&key) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.message()})),
        )
            .into_response();
    }

    if !CONFIG.datastore_enabled && key.starts_with("dataStore/") {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "DataStore sync is disabled"})),
        )
            .into_response();
    }

    if headers.get("content-type").and_then(|h| h.to_str().ok()) != Some("application/octet-stream")
    {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(serde_json::json!({"error": "Content type must be application/octet-stream"})),
        )
            .into_response();
    }

    let max_size = if key.starts_with("dataStore/") {
        CONFIG.max_datastore_key_size_bytes
    } else {
        CONFIG.max_key_size_bytes
    };

    if body.len() > max_size {
        let limit_mb = max_size / 1024 / 1024;
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": format!("Value exceeds {}MB limit", limit_mb)})),
        )
            .into_response();
    }

    let checksum = compute_checksum(&body);

    match db
        .save_data_key_with_quota_check(
            &user_id,
            &key,
            body.into(),
            &checksum,
            CONFIG.max_backup_size_bytes as i64,
        )
        .await
    {
        Ok(Some((version, updated_at))) => Json(serde_json::json!({
            "version": version,
            "checksum": checksum,
            "updated_at": updated_at
        }))
        .into_response(),
        Ok(None) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": "Total storage limit exceeded"})),
        )
            .into_response(),
        Err(e) => {
            error!("Failed to save data key: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Failed to save data"})),
            )
                .into_response()
        }
    }
}

pub async fn delete_data(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
    Path(key): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = validate_key(&key) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.message()})),
        )
            .into_response();
    }

    if !CONFIG.datastore_enabled && key.starts_with("dataStore/") {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "DataStore sync is disabled"})),
        )
            .into_response();
    }

    match db.delete_data_key(&user_id, &key).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            error!("Failed to delete data key: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
