use axum::{
    Extension, Json,
    body::{Body, Bytes},
    extract::Path,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use tracing::error;

use equicloud::utils::{CONFIG, error_response};
use equicloud::{DatabaseService, compute_checksum, validate_key};

const OCTET_STREAM: HeaderValue = HeaderValue::from_static("application/octet-stream");

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(error_response(message))).into_response()
}

fn datastore_blocked(key: &str) -> Option<Response> {
    if !CONFIG.datastore_enabled && key.starts_with("dataStore/") {
        Some(err(StatusCode::FORBIDDEN, "DataStore sync is disabled"))
    } else {
        None
    }
}

fn max_size_for_key(key: &str) -> usize {
    if key.starts_with("dataStore/") {
        CONFIG.max_datastore_key_size_bytes
    } else {
        CONFIG.max_key_size_bytes
    }
}

fn content_type_is_octet_stream(headers: &HeaderMap) -> bool {
    headers
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .map(|s| {
            let primary = s.split(';').next().unwrap_or("").trim();
            primary.eq_ignore_ascii_case("application/octet-stream")
        })
        .unwrap_or(false)
}

fn header_str_eq(headers: &HeaderMap, name: &str, expected: &str) -> bool {
    headers
        .get(name)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|v| v == expected)
}

pub async fn get_data(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = validate_key(&key) {
        return err(StatusCode::BAD_REQUEST, e.message());
    }

    if let Some(r) = datastore_blocked(&key) {
        return r;
    }

    // Short-circuit conditional GETs: only fetch the (potentially MB-sized) value
    // when the client's cached ETag doesn't match.
    if headers.get("if-none-match").is_some() {
        match db.get_data_key_metadata(&user_id, &key).await {
            Ok(Some((version, checksum, _size, _updated))) => {
                if header_str_eq(&headers, "if-none-match", &checksum) {
                    let mut response_headers = HeaderMap::new();
                    if let Ok(v) = HeaderValue::from_str(&checksum) {
                        response_headers.insert("ETag", v);
                    }
                    let mut buf = itoa::Buffer::new();
                    if let Ok(v) = HeaderValue::from_str(buf.format(version)) {
                        response_headers.insert("X-Version", v);
                    }
                    return (StatusCode::NOT_MODIFIED, response_headers, Body::empty())
                        .into_response();
                }
            }
            Ok(None) => {
                return (StatusCode::NOT_FOUND, HeaderMap::new(), Body::empty()).into_response();
            }
            Err(e) => {
                error!("Failed to read data key metadata: {}", e);
                return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read data");
            }
        }
    }

    let entry = match db.get_data_key(&user_id, &key).await {
        Ok(Some(e)) => e,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, HeaderMap::new(), Body::empty()).into_response();
        }
        Err(e) => {
            error!("Failed to get data key: {}", e);
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read data");
        }
    };

    let mut response_headers = HeaderMap::new();
    response_headers.insert("Content-Type", OCTET_STREAM);
    if let Ok(v) = HeaderValue::from_str(&entry.checksum) {
        response_headers.insert("ETag", v);
    }
    let mut buf = itoa::Buffer::new();
    if let Ok(v) = HeaderValue::from_str(buf.format(entry.version)) {
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
) -> Response {
    if let Err(e) = validate_key(&key) {
        return err(StatusCode::BAD_REQUEST, e.message());
    }

    if let Some(r) = datastore_blocked(&key) {
        return r;
    }

    if !content_type_is_octet_stream(&headers) {
        return err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content type must be application/octet-stream",
        );
    }

    let max_size = max_size_for_key(&key);
    if body.len() > max_size {
        let limit_mb = max_size / 1024 / 1024;
        return err(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("Value exceeds {}MB limit", limit_mb),
        );
    }

    // SHA-256 of a multi-MB body is CPU-bound and can stall the async runtime
    // worker for tens-to-hundreds of milliseconds. Run it on the blocking pool
    // for values above 64 KB; inline for smaller payloads where the spawn
    // overhead would dominate.
    let computed_checksum = if body.len() > 64 * 1024 {
        let body_for_hash = body.clone();
        match tokio::task::spawn_blocking(move || compute_checksum(&body_for_hash)).await {
            Ok(c) => c,
            Err(e) => {
                error!("checksum task panicked: {}", e);
                return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to compute checksum");
            }
        }
    } else {
        compute_checksum(&body)
    };

    if let Some(raw) = headers.get("x-checksum") {
        match raw.to_str() {
            Ok(provided) if provided == computed_checksum => {}
            Ok(_) => return err(StatusCode::BAD_REQUEST, "Checksum mismatch"),
            Err(_) => {
                return err(StatusCode::BAD_REQUEST, "Malformed X-Checksum header");
            }
        }
    }

    match db
        .save_data_key_with_quota_check(
            &user_id,
            &key,
            body.into(),
            &computed_checksum,
            CONFIG.max_backup_size_bytes as i64,
        )
        .await
    {
        Ok(Some((version, updated_at))) => Json(serde_json::json!({
            "version": version,
            "checksum": computed_checksum,
            "updated_at": updated_at
        }))
        .into_response(),
        Ok(None) => err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Total storage limit exceeded",
        ),
        Err(e) => {
            error!("Failed to save data key: {}", e);
            err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save data")
        }
    }
}

pub async fn delete_data(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
    Path(key): Path<String>,
) -> Response {
    if let Err(e) = validate_key(&key) {
        return err(StatusCode::BAD_REQUEST, e.message());
    }

    if let Some(r) = datastore_blocked(&key) {
        return r;
    }

    match db.delete_data_key(&user_id, &key).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            error!("Failed to delete data key: {}", e);
            err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete")
        }
    }
}
