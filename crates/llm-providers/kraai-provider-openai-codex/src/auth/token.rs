use std::io;

use base64::Engine;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub(super) struct StoredAuth {
    pub(super) tokens: StoredTokens,
    pub(super) claims: IdTokenClaims,
    pub(super) last_refresh_unix: u64,
    pub(super) generation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredTokens {
    pub(super) id_token: String,
    pub(super) access_token: String,
    pub(super) refresh_token: String,
    pub(super) account_id: String,
}

#[derive(Clone, Debug, Default)]
pub(super) struct IdTokenClaims {
    pub(super) email: Option<String>,
    pub(super) plan_type: Option<String>,
    pub(super) account_id: Option<String>,
}

#[derive(Deserialize)]
struct RootClaims {
    #[serde(default)]
    email: Option<String>,
    #[serde(rename = "https://api.openai.com/profile", default)]
    profile: Option<ProfileClaims>,
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: Option<AuthClaims>,
}

#[derive(Deserialize)]
struct ProfileClaims {
    #[serde(default)]
    email: Option<String>,
}

#[derive(Deserialize)]
struct AuthClaims {
    #[serde(default)]
    chatgpt_plan_type: Option<serde_json::Value>,
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}

pub(super) fn parse_id_token_claims(token: &str) -> io::Result<IdTokenClaims> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| io::Error::other("Invalid OpenAI id_token"))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(io::Error::other)?;
    let claims = serde_json::from_slice::<RootClaims>(&bytes).map_err(io::Error::other)?;
    let email = claims
        .email
        .or_else(|| claims.profile.and_then(|profile| profile.email));
    let (plan_type, account_id) = match claims.auth {
        Some(auth) => (
            auth.chatgpt_plan_type
                .map(|value| match value {
                    serde_json::Value::String(text) => text,
                    other => other.to_string(),
                })
                .map(|text| normalize_plan_type(&text)),
            auth.chatgpt_account_id,
        ),
        None => (None, None),
    };

    Ok(IdTokenClaims {
        email,
        plan_type,
        account_id,
    })
}

pub(super) fn normalize_plan_type(plan_type: &str) -> String {
    match plan_type.to_ascii_lowercase().as_str() {
        "free" => "Free".to_string(),
        "go" => "Go".to_string(),
        "plus" => "Plus".to_string(),
        "pro" => "Pro".to_string(),
        "team" => "Team".to_string(),
        "business" => "Business".to_string(),
        "enterprise" => "Enterprise".to_string(),
        "education" | "edu" => "Edu".to_string(),
        _ => plan_type.to_string(),
    }
}

pub(super) fn token_generation(refresh_token: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(refresh_token.as_bytes()))
}

pub(super) fn generate_generation() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
