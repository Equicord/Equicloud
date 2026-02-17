use axum::{extract::Query, response::Json};
use reqwest;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{error, info};

use equicloud::constants::{DISCORD_TOKEN_URL, DISCORD_USER_URL};
use equicloud::utils::{CONFIG, error_response, get_user_secret, hash_user_id};

#[derive(Deserialize)]
pub struct OAuthCallback {
    pub code: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct DiscordAccessTokenResult {
    access_token: String,
}

#[derive(Deserialize)]
struct DiscordUserResult {
    id: String,
}

pub async fn oauth_callback(Query(params): Query<OAuthCallback>) -> Json<Value> {
    if let Some(error) = params.error {
        return Json(error_response(&error));
    }

    let code = match params.code {
        Some(code) => code,
        None => {
            return Json(error_response("Missing code"));
        }
    };

    let redirect_uri = CONFIG.redirect_uri();

    let client = reqwest::Client::new();

    let grant_type = "authorization_code";
    let scope = "identify";

    let token_response = client
        .post(DISCORD_TOKEN_URL)
        .form(&[
            ("client_id", CONFIG.discord_client_id.as_str()),
            ("client_secret", CONFIG.discord_client_secret.as_str()),
            ("grant_type", grant_type),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("scope", scope),
        ])
        .send()
        .await;

    let token_response = match token_response {
        Ok(response) => response,
        Err(err) => {
            error!("Failed to request access token: {}", err);
            return Json(error_response("Failed to request access token"));
        }
    };

    if !token_response.status().is_success() {
        return Json(error_response("Invalid code"));
    }

    let token_result: DiscordAccessTokenResult = match token_response.json().await {
        Ok(result) => result,
        Err(err) => {
            error!("Failed to parse token response: {}", err);
            return Json(error_response("Failed to parse token response"));
        }
    };

    let user_response = client
        .get(DISCORD_USER_URL)
        .header(
            "Authorization",
            format!("Bearer {}", token_result.access_token),
        )
        .send()
        .await;

    let user_response = match user_response {
        Ok(response) => response,
        Err(err) => {
            error!("Failed to request user: {}", err);
            return Json(error_response("Failed to request user"));
        }
    };

    if !user_response.status().is_success() {
        return Json(error_response("Failed to request user"));
    }

    let user_result: DiscordUserResult = match user_response.json().await {
        Ok(result) => result,
        Err(err) => {
            error!("Failed to parse user response: {}", err);
            return Json(error_response("Failed to parse user response"));
        }
    };

    let user_id = user_result.id;

    if let Some(allowed_users) = &CONFIG.discord_allowed_user_ids
        && !allowed_users.is_empty()
    {
        let allowed_list: Vec<&str> = allowed_users.split(',').map(|s| s.trim()).collect();
        if !allowed_list.contains(&user_id.as_str()) {
            return Json(error_response("User is not whitelisted"));
        }
    }

    let secret = get_user_secret(&user_id);
    let user_hash = hash_user_id(&user_id);

    info!("User {} authenticated successfully", &user_hash[..16]);

    Json(json!({
        "secret": secret
    }))
}
