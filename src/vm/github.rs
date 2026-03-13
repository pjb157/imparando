use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct AppJwtClaims {
    iat: i64,
    exp: i64,
    iss: String,
}

#[derive(Deserialize)]
struct InstallationTokenResponse {
    token: String,
}

pub fn is_github_repo_url(url: &str) -> bool {
    url.starts_with("https://github.com/")
        || url.starts_with("http://github.com/")
        || url.starts_with("git@github.com:")
}

pub async fn create_installation_token(
    app_id: u64,
    installation_id: u64,
    private_key_path: &Path,
) -> Result<String> {
    let pem = tokio::fs::read(private_key_path)
        .await
        .with_context(|| format!("reading GitHub App private key from {}", private_key_path.display()))?;

    let now = Utc::now().timestamp();
    let claims = AppJwtClaims {
        iat: now - 60,
        exp: now + 540,
        iss: app_id.to_string(),
    };

    let jwt = jsonwebtoken::encode(
        &Header::new(Algorithm::RS256),
        &claims,
        &EncodingKey::from_rsa_pem(&pem).context("parsing GitHub App private key PEM")?,
    )
    .context("signing GitHub App JWT")?;

    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "https://api.github.com/app/installations/{installation_id}/access_tokens"
        ))
        .header(reqwest::header::USER_AGENT, "imparando")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .bearer_auth(jwt)
        .send()
        .await
        .context("requesting GitHub installation token")?
        .error_for_status()
        .context("GitHub installation token request failed")?;

    let body: InstallationTokenResponse = response
        .json()
        .await
        .context("decoding GitHub installation token response")?;

    Ok(body.token)
}
