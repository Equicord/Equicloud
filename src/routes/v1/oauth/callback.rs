use axum::{
    Extension,
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use once_cell::sync::Lazy;
use reqwest::Client;
use serde_json::{Value, json};
use std::time::Duration;
use tracing::{error, info, warn};

use equicloud::DatabaseService;
use equicloud::constants::{DISCORD_TOKEN_REVOKE_URL, DISCORD_TOKEN_URL, DISCORD_USER_URL};
use equicloud::types::oauth::{DiscordAccessTokenResult, DiscordUserResult, OAuthCallback};
use equicloud::utils::{CONFIG, error_response};

static OAUTH_HTTP_CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .pool_max_idle_per_host(8)
        .build()
        .expect("failed to build OAuth HTTP client")
});

const REQUIRED_SCOPE: &str = "identify";

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(error_response(message))).into_response()
}

pub async fn oauth_callback(
    Extension(db): Extension<DatabaseService>,
    Query(params): Query<OAuthCallback>,
) -> Response {
    if let Some(provider_error) = params.error {
        error!("OAuth provider returned error: {:?}", provider_error);
        return err(StatusCode::BAD_REQUEST, "Authorization failed");
    }

    let code = match params.code {
        Some(code) if !code.is_empty() => code,
        _ => return err(StatusCode::BAD_REQUEST, "Missing code"),
    };

    let redirect_uri = CONFIG.redirect_uri();

    let token_response = OAUTH_HTTP_CLIENT
        .post(DISCORD_TOKEN_URL)
        .form(&[
            ("client_id", CONFIG.discord_client_id.as_str()),
            ("client_secret", CONFIG.discord_client_secret.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .send()
        .await;

    let token_response = match token_response {
        Ok(response) => response,
        Err(e) => {
            error!("Failed to request access token: {}", e);
            return err(StatusCode::BAD_GATEWAY, "Failed to request access token");
        }
    };

    if !token_response.status().is_success() {
        let status = token_response.status();
        error!("Discord token exchange failed (HTTP {})", status);
        return err(StatusCode::BAD_REQUEST, "Invalid code");
    }

    let token_result: DiscordAccessTokenResult = match token_response.json().await {
        Ok(result) => result,
        Err(e) => {
            error!("Failed to parse token response: {}", e);
            return err(StatusCode::BAD_GATEWAY, "Failed to parse token response");
        }
    };

    if !scope_contains(&token_result.scope, REQUIRED_SCOPE) {
        error!(
            "Discord granted insufficient scope (expected `{}`)",
            REQUIRED_SCOPE
        );
        revoke_discord_token_in_background(&token_result.access_token);
        return err(StatusCode::BAD_REQUEST, "Insufficient OAuth scope");
    }

    if !token_result.token_type.eq_ignore_ascii_case("Bearer") {
        error!(
            "Discord returned unexpected token_type {:?}",
            token_result.token_type
        );
        revoke_discord_token_in_background(&token_result.access_token);
        return err(StatusCode::BAD_GATEWAY, "Unexpected token type");
    }

    let user_response = OAUTH_HTTP_CLIENT
        .get(DISCORD_USER_URL)
        .header(
            "Authorization",
            format!("Bearer {}", token_result.access_token),
        )
        .send()
        .await;

    let user_response = match user_response {
        Ok(response) => response,
        Err(e) => {
            error!("Failed to request user: {}", e);
            revoke_discord_token_in_background(&token_result.access_token);
            return err(StatusCode::BAD_GATEWAY, "Failed to request user");
        }
    };

    if !user_response.status().is_success() {
        let status = user_response.status();
        error!("Discord user request failed (HTTP {})", status);
        revoke_discord_token_in_background(&token_result.access_token);
        return err(StatusCode::BAD_GATEWAY, "Failed to request user");
    }

    let user_result: DiscordUserResult = match user_response.json().await {
        Ok(result) => result,
        Err(e) => {
            error!("Failed to parse user response: {}", e);
            revoke_discord_token_in_background(&token_result.access_token);
            return err(StatusCode::BAD_GATEWAY, "Failed to parse user response");
        }
    };

    let user_id = user_result.id;

    let whitelist: Option<Vec<&str>> = CONFIG
        .discord_allowed_user_ids
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .filter(|list: &Vec<&str>| !list.is_empty());

    match (&whitelist, CONFIG.discord_allow_all_users) {
        (Some(list), _) => {
            if !list.contains(&user_id.as_str()) {
                revoke_discord_token_in_background(&token_result.access_token);
                return err(StatusCode::FORBIDDEN, "User is not whitelisted");
            }
        }
        (None, true) => {}
        (None, false) => {
            error!(
                "OAuth refused: neither DISCORD_ALLOWED_USER_IDS nor DISCORD_ALLOW_ALL_USERS is set"
            );
            revoke_discord_token_in_background(&token_result.access_token);
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "Server is not configured to accept OAuth",
            );
        }
    }

    let secret = match db.get_or_create_user_auth_secret(&user_id).await {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to issue auth secret: {}", e);
            revoke_discord_token_in_background(&token_result.access_token);
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to issue authentication",
            );
        }
    };

    revoke_discord_token(&token_result.access_token).await;

    info!("OAuth callback completed successfully");

    let body: Value = json!({ "secret": secret });
    (StatusCode::OK, Json(body)).into_response()
}

fn scope_contains(scopes: &str, required: &str) -> bool {
    scopes.split_whitespace().any(|s| s == required)
}

/// Fire-and-forget revocation: spawn the request so error responses don't
/// block on the Discord round-trip. Failures are logged via `revoke_discord_token`.
fn revoke_discord_token_in_background(access_token: &str) {
    let token = access_token.to_owned();
    tokio::spawn(async move {
        revoke_discord_token(&token).await;
    });
}

async fn revoke_discord_token(access_token: &str) {
    let response = OAUTH_HTTP_CLIENT
        .post(DISCORD_TOKEN_REVOKE_URL)
        .form(&[
            ("client_id", CONFIG.discord_client_id.as_str()),
            ("client_secret", CONFIG.discord_client_secret.as_str()),
            ("token", access_token),
            ("token_type_hint", "access_token"),
        ])
        .send()
        .await;

    match response {
        Ok(r) if r.status().is_success() => {}
        Ok(r) => warn!("Discord token revocation returned HTTP {}", r.status()),
        Err(e) => warn!("Discord token revocation request failed: {}", e),
    }
}
