use axum::{
    Extension,
    body::{Body, Bytes},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
};
use serde_json::json;
use tracing::error;

use equicloud::DatabaseService;
use equicloud::utils::{CONFIG, error_response};

const OCTET_STREAM: HeaderValue = HeaderValue::from_static("application/octet-stream");

fn etag_header_value(written: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(written).ok()
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

fn etag_matches(header_value: Option<&str>, written: &str) -> bool {
    let Some(raw) = header_value else {
        return false;
    };
    raw.split(',').any(|tag| {
        let t = tag.trim();
        t == "*" || t == written
    })
}

fn if_none_match_matches(headers: &HeaderMap, written: &str) -> bool {
    etag_matches(
        headers.get("if-none-match").and_then(|h| h.to_str().ok()),
        written,
    )
}

fn if_match_matches(headers: &HeaderMap, written: &str) -> bool {
    etag_matches(
        headers.get("if-match").and_then(|h| h.to_str().ok()),
        written,
    )
}

pub async fn head_settings(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    match db.get_settings_metadata(&user_id).await {
        Ok(Some(written)) => {
            if if_none_match_matches(&headers, &written) {
                return (StatusCode::NOT_MODIFIED, HeaderMap::new());
            }
            let mut response_headers = HeaderMap::new();
            if let Some(v) = etag_header_value(&written) {
                response_headers.insert("ETag", v);
            }
            (StatusCode::NO_CONTENT, response_headers)
        }
        Ok(None) => (StatusCode::NOT_FOUND, HeaderMap::new()),
        Err(e) => {
            error!("Database error in head_settings: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new())
        }
    }
}

pub async fn get_settings(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let metadata = match db.get_settings_metadata(&user_id).await {
        Ok(m) => m,
        Err(e) => {
            error!("Database error in get_settings (metadata): {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(error_response("Failed to retrieve settings")),
            )
                .into_response();
        }
    };

    let written = match metadata {
        Some(w) => w,
        None => return (StatusCode::NOT_FOUND, HeaderMap::new(), Body::empty()).into_response(),
    };

    if if_none_match_matches(&headers, &written) {
        let mut response_headers = HeaderMap::new();
        if let Some(v) = etag_header_value(&written) {
            response_headers.insert("ETag", v);
        }
        return (StatusCode::NOT_MODIFIED, response_headers, Body::empty()).into_response();
    }

    match db.get_user_settings(&user_id).await {
        Ok(Some((value, written))) => {
            let mut response_headers = HeaderMap::new();
            response_headers.insert("Content-Type", OCTET_STREAM);
            if let Some(v) = etag_header_value(&written) {
                response_headers.insert("ETag", v);
            }
            (StatusCode::OK, response_headers, Body::from(value)).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, HeaderMap::new(), Body::empty()).into_response(),
        Err(e) => {
            error!("Database error in get_settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(error_response("Failed to retrieve settings")),
            )
                .into_response()
        }
    }
}

pub async fn put_settings(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if !content_type_is_octet_stream(&headers) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            axum::Json(error_response(
                "Content type must be application/octet-stream",
            )),
        )
            .into_response();
    }

    let size_limit = CONFIG.max_backup_size_bytes;

    if body.len() > size_limit {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            axum::Json(error_response("Settings are too large")),
        )
            .into_response();
    }

    if headers.contains_key("if-match") {
        let current = match db.get_settings_metadata(&user_id).await {
            Ok(c) => c,
            Err(e) => {
                error!("Database error reading metadata for If-Match: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(error_response("Failed to read current settings")),
                )
                    .into_response();
            }
        };
        match current {
            Some(current) if if_match_matches(&headers, &current) => {}
            Some(_) => {
                return (
                    StatusCode::PRECONDITION_FAILED,
                    axum::Json(error_response("Settings have changed since last read")),
                )
                    .into_response();
            }
            None => {
                return (
                    StatusCode::PRECONDITION_FAILED,
                    axum::Json(error_response("No existing settings to match against")),
                )
                    .into_response();
            }
        }
    }

    match db.save_user_settings(&user_id, body.to_vec()).await {
        Ok(written) => {
            let mut response_headers = HeaderMap::new();
            let mut buf = itoa::Buffer::new();
            if let Some(v) = etag_header_value(buf.format(written)) {
                response_headers.insert("ETag", v);
            }
            (
                StatusCode::OK,
                response_headers,
                axum::Json(json!({ "written": written })),
            )
                .into_response()
        }
        Err(e) => {
            error!("Database error in put_settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(error_response("Failed to save settings")),
            )
                .into_response()
        }
    }
}

pub async fn delete_settings(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
) -> impl IntoResponse {
    match db.delete_user_settings(&user_id).await {
        Ok(_) => (StatusCode::NO_CONTENT, HeaderMap::new()).into_response(),
        Err(e) => {
            error!("Database error in delete_settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(error_response("Failed to delete settings")),
            )
                .into_response()
        }
    }
}
