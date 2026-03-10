use serde::Deserialize;

#[derive(Deserialize)]
pub struct OAuthCallback {
    pub code: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
pub struct DiscordAccessTokenResult {
    pub access_token: String,
}

#[derive(Deserialize)]
pub struct DiscordUserResult {
    pub id: String,
}
