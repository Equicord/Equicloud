use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response};
use base64::prelude::*;
use tracing::warn;

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

pub async fn auth_middleware(mut request: Request, next: Next) -> Result<Response, StatusCode> {
    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token = auth_header.strip_prefix("Bearer ").unwrap_or(auth_header);

    let user_id = verify_token(token).ok_or(StatusCode::UNAUTHORIZED)?;
    request.extensions_mut().insert(user_id);
    Ok(next.run(request).await)
}

#[inline]
fn verify_token(token: &str) -> Option<String> {
    let decoded = BASE64_STANDARD.decode(token).ok()?;
    let token_str = String::from_utf8(decoded).ok()?;
    let (provided_secret, discord_user_id) = token_str.split_once(':')?;

    let expected_secret = equicloud::utils::get_user_secret(discord_user_id);
    if constant_time_eq(provided_secret.as_bytes(), expected_secret.as_bytes()) {
        return Some(discord_user_id.to_string());
    }

    let legacy_secret = equicloud::hash_migration::legacy::get_user_secret(discord_user_id);
    if constant_time_eq(provided_secret.as_bytes(), legacy_secret.as_bytes()) {
        warn!("User authenticated with legacy secret format");
        return Some(discord_user_id.to_string());
    }

    None
}
