//! Minecraft Services authentication and profile fetching.

use deming_core::{DemingError, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
pub(crate) struct RawMcLoginResponse {
    pub(crate) access_token: String,
    #[serde(default = "default_mc_expires_in")]
    pub(crate) expires_in: u64,
}

fn default_mc_expires_in() -> u64 {
    86_400
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawMcProfileResponse {
    pub(crate) id: String,
    pub(crate) name: String,
}

pub(crate) async fn login_minecraft_with_xbox(
    http: &Client,
    mc_services_base_url: &str,
    uhs: &str,
    xsts_token: &str,
) -> Result<RawMcLoginResponse> {
    let url = format!(
        "{}/authentication/login_with_xbox",
        mc_services_base_url.trim_end_matches('/')
    );
    let payload = json!({
        "identityToken": format!("XBL3.0 x={uhs};{xsts_token}")
    });

    let resp = http
        .post(&url)
        .header("Accept", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| {
            DemingError::Network(format!("Minecraft login_with_xbox request failed: {e}"))
        })?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| DemingError::Network(format!("failed reading MC login body: {e}")))?;

    if !status.is_success() {
        return Err(DemingError::Auth(format!(
            "Minecraft login_with_xbox failed with HTTP {status}: {body}"
        )));
    }

    Ok(serde_json::from_str(&body)?)
}

pub(crate) async fn fetch_minecraft_profile(
    http: &Client,
    mc_services_base_url: &str,
    mc_access_token: &str,
) -> Result<RawMcProfileResponse> {
    let url = format!(
        "{}/minecraft/profile",
        mc_services_base_url.trim_end_matches('/')
    );

    let resp = http
        .get(&url)
        .bearer_auth(mc_access_token)
        .send()
        .await
        .map_err(|e| DemingError::Network(format!("Minecraft profile request failed: {e}")))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| DemingError::Network(format!("failed reading MC profile body: {e}")))?;

    if !status.is_success() {
        return Err(DemingError::Auth(format!(
            "Minecraft profile fetch failed with HTTP {status}: {body}"
        )));
    }

    Ok(serde_json::from_str(&body)?)
}
