use serde::Deserialize;

#[derive(Deserialize)]
pub struct OAuthCallback {
    pub code: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
pub struct DiscordAccessTokenResult {
    pub access_token: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub token_type: String,
}

#[derive(Deserialize)]
pub struct DiscordUserResult {
    pub id: String,
}
