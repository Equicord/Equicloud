use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use base64::prelude::*;
use moka::future::Cache;
use once_cell::sync::Lazy;
use std::sync::Arc;
use std::time::Duration;

use equicloud::DatabaseService;
use equicloud::utils::error_response;

const AUTH_CACHE_TTL_SECS: u64 = 60;
const AUTH_CACHE_CAPACITY: u64 = 10_000;
const MAX_AUTH_HEADER_LEN: usize = 256;
const MAX_DECODED_TOKEN_LEN: usize = 192;

/// Cache key: token bytes (Arc-shared, no full-String alloc per cache hit).
/// Cache value: discord_user_id as `Arc<str>`. Negative results are not
/// cached so a stolen-token scan can't pollute the cache.
static AUTH_CACHE: Lazy<Cache<Arc<str>, Arc<str>>> = Lazy::new(|| {
    Cache::builder()
        .max_capacity(AUTH_CACHE_CAPACITY)
        .time_to_live(Duration::from_secs(AUTH_CACHE_TTL_SECS))
        .build()
});

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

#[cold]
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(error_response("Unauthorized")),
    )
        .into_response()
}

#[cold]
fn server_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(error_response("Server misconfigured")),
    )
        .into_response()
}

pub async fn auth_middleware(mut request: Request, next: Next) -> Response {
    let Some(auth_header) = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
    else {
        return unauthorized();
    };

    // Reject obviously-bad headers before any work.
    if auth_header.is_empty() || auth_header.len() > MAX_AUTH_HEADER_LEN {
        return unauthorized();
    }

    let token = auth_header.strip_prefix("Bearer ").unwrap_or(auth_header);

    // Cache lookup borrows the &str without allocating.
    if let Some(user_id) = AUTH_CACHE.get(token).await {
        request.extensions_mut().insert(user_id.as_ref().to_string());
        return next.run(request).await;
    }

    let Some(db) = request.extensions().get::<DatabaseService>().cloned() else {
        return server_error();
    };

    match verify_token(token, &db).await {
        Some(user_id) => {
            let token_arc: Arc<str> = Arc::from(token);
            AUTH_CACHE.insert(token_arc, Arc::clone(&user_id)).await;
            request.extensions_mut().insert(user_id.as_ref().to_string());
            next.run(request).await
        }
        None => unauthorized(),
    }
}

async fn verify_token(token: &str, db: &DatabaseService) -> Option<Arc<str>> {
    // Decode into a stack buffer; real tokens decode to ~64 bytes.
    let mut buf = [0u8; MAX_DECODED_TOKEN_LEN];
    let decoded_len = BASE64_STANDARD.decode_slice(token, &mut buf).ok()?;
    let decoded = &buf[..decoded_len];

    let token_str = std::str::from_utf8(decoded).ok()?;
    let (provided_secret, discord_user_id) = token_str.split_once(':')?;

    if discord_user_id.is_empty() || provided_secret.is_empty() {
        return None;
    }

    let stored_secret = db.get_user_auth_secret(discord_user_id).await.ok()??;

    if !constant_time_eq(provided_secret.as_bytes(), stored_secret.as_bytes()) {
        return None;
    }

    Some(Arc::from(discord_user_id))
}

/// Best-effort invalidate the cache entry for a given Discord user id.
pub async fn invalidate_user_cache(user_id: &str) {
    let mut to_evict: Vec<Arc<str>> = Vec::new();
    for entry in AUTH_CACHE.iter() {
        if entry.1.as_ref() == user_id {
            to_evict.push(Arc::clone(&entry.0));
        }
    }
    for key in to_evict {
        AUTH_CACHE.invalidate(&key).await;
    }
}
