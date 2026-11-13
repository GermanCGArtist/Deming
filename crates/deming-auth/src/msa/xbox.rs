//! Xbox Live (XBL) and XSTS token exchange helpers.

use deming_core::{DemingError, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct RawXboxTokenResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: RawDisplayClaims,
}

#[derive(Debug, Deserialize)]
struct RawDisplayClaims {
    xui: Vec<RawXuiClaim>,
}

#[derive(Debug, Deserialize)]
struct RawXuiClaim {
    uhs: String,
}

#[derive(Debug, Deserialize)]
struct RawXstsErrorResponse {
    #[serde(rename = "XErr")]
    xerr: Option<u64>,
    #[serde(rename = "Message")]
    message: Option<String>,
}

pub(crate) async fn authenticate_xbl(
    http: &Client,
    xbl_auth_url: &str,
    msa_access_token: &str,
) -> Result<(String, String)> {
    let payload = json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            "RpsTicket": format!("d={msa_access_token}")
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT"
    });

    let resp = http
        .post(xbl_auth_url)
        .header("Accept", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| DemingError::Network(format!("XBL auth request failed: {e}")))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| DemingError::Network(format!("failed reading XBL body: {e}")))?;

    if !status.is_success() {
        return Err(DemingError::Auth(format!(
            "XBL auth failed with HTTP {status}: {body}"
        )));
    }

    let parsed: RawXboxTokenResponse = serde_json::from_str(&body)?;
    extract_xbox_token_and_uhs(parsed, "XBL")
}

pub(crate) async fn authenticate_xsts(
    http: &Client,
    xsts_auth_url: &str,
    xbl_token: &str,
) -> Result<(String, String)> {
    let payload = json!({
        "Properties": {
            "SandboxId": "RETAIL",
            "UserTokens": [xbl_token]
        },
        "RelyingParty": "rp://api.minecraftservices.com/",
        "TokenType": "JWT"
    });

    let resp = http
        .post(xsts_auth_url)
        .header("Accept", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| DemingError::Network(format!("XSTS auth request failed: {e}")))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| DemingError::Network(format!("failed reading XSTS body: {e}")))?;

    if !status.is_success() {
        if let Ok(err) = serde_json::from_str::<RawXstsErrorResponse>(&body) {
            let reason = match err.xerr {
                Some(2148916233) => "Microsoft account has no Xbox profile",
                Some(2148916235) => "Xbox Live is unavailable in this country/region",
                Some(2148916238) => "Child account must be added to a Family by an adult",
                _ => "XSTS authorization rejected",
            };
            return Err(DemingError::Auth(format!(
                "{reason} (XErr={:?}): {}",
                err.xerr,
                err.message.unwrap_or_default()
            )));
        }
        return Err(DemingError::Auth(format!(
            "XSTS auth failed with HTTP {status}: {body}"
        )));
    }

    let parsed: RawXboxTokenResponse = serde_json::from_str(&body)?;
    extract_xbox_token_and_uhs(parsed, "XSTS")
}

fn extract_xbox_token_and_uhs(
    parsed: RawXboxTokenResponse,
    stage: &str,
) -> Result<(String, String)> {
    let uhs = parsed
        .display_claims
        .xui
        .into_iter()
        .next()
        .map(|x| x.uhs)
        .ok_or_else(|| DemingError::Auth(format!("{stage} response missing xui userhash")))?;
    Ok((parsed.token, uhs))
}
