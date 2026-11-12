//! Microsoft OAuth Device-Code -> Xbox Live -> XSTS -> Minecraft Services authentication flow.

use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use deming_core::{AccountRef, DemingError, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

const DEFAULT_OAUTH_BASE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0";
const DEFAULT_XBL_AUTH_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const DEFAULT_XSTS_AUTH_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const DEFAULT_MC_SERVICES_BASE_URL: &str = "https://api.minecraftservices.com";
const DEFAULT_OAUTH_SCOPE: &str = "XboxLive.signin offline_access";

/// Configurable endpoint URLs for Microsoft, Xbox Live, XSTS, and Minecraft Services.
///
/// Overriding these endpoints allows tests to run the full auth flow against a local mock server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsaEndpoints {
    /// Base URL for Microsoft OAuth2 endpoints (`/devicecode`, `/token`).
    pub oauth_base_url: String,
    /// Full URL for Xbox Live user authentication (`user.auth.xboxlive.com/user/authenticate`).
    pub xbl_auth_url: String,
    /// Full URL for XSTS token authorization (`xsts.auth.xboxlive.com/xsts/authorize`).
    pub xsts_auth_url: String,
    /// Base URL for Minecraft Services (`api.minecraftservices.com`).
    pub mc_services_base_url: String,
}

impl Default for MsaEndpoints {
    fn default() -> Self {
        Self {
            oauth_base_url: DEFAULT_OAUTH_BASE_URL.to_string(),
            xbl_auth_url: DEFAULT_XBL_AUTH_URL.to_string(),
            xsts_auth_url: DEFAULT_XSTS_AUTH_URL.to_string(),
            mc_services_base_url: DEFAULT_MC_SERVICES_BASE_URL.to_string(),
        }
    }
}

impl MsaEndpoints {
    /// Builds endpoints pointing at a single local mock HTTP server base URL (e.g. `http://127.0.0.1:12345`).
    pub fn from_mock_base(base_url: &str) -> Self {
        let base = base_url.trim_end_matches('/');
        Self {
            oauth_base_url: format!("{base}/oauth2/v2.0"),
            xbl_auth_url: format!("{base}/user/authenticate"),
            xsts_auth_url: format!("{base}/xsts/authorize"),
            mc_services_base_url: base.to_string(),
        }
    }
}

/// Prompt details returned by [`MsaClient::start_device_code`] for user verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCodePrompt {
    /// Short code the user enters at `verification_uri`.
    pub user_code: String,
    /// URL the user visits in a browser to authorize the device.
    pub verification_uri: String,
    /// Opaque device code used for polling the token endpoint.
    pub device_code: String,
    /// Minimum polling interval in seconds.
    pub interval_secs: u64,
    /// Lifetime of the device code in seconds.
    pub expires_in_secs: u64,
}

/// Completed Minecraft authentication session with tokens and profile identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsaSession {
    /// Minecraft player UUID (hyphenated lowercase).
    pub uuid: String,
    /// Minecraft player username.
    pub username: String,
    /// Minecraft Services Bearer access token.
    pub mc_access_token: String,
    /// Microsoft OAuth2 refresh token for renewing expired sessions.
    pub msa_refresh_token: String,
    /// UTC timestamp when `mc_access_token` expires.
    pub expires_at: DateTime<Utc>,
}

impl MsaSession {
    /// Returns `true` if the Minecraft access token is expired at `now`.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    /// Returns `true` if the Minecraft access token is currently expired.
    pub fn is_expired(&self) -> bool {
        self.is_expired_at(Utc::now())
    }

    /// Converts this session into a [`deming_core::AccountRef`].
    pub fn to_account_ref(&self) -> AccountRef {
        AccountRef::Microsoft {
            uuid: self.uuid.clone(),
            username: self.username.clone(),
        }
    }
}

/// Async client implementing the Microsoft OAuth Device-Code -> XBL -> XSTS -> Minecraft flow.
#[derive(Debug, Clone)]
pub struct MsaClient {
    http: Client,
    client_id: String,
    endpoints: MsaEndpoints,
    scope: String,
}

impl MsaClient {
    /// Creates a new [`MsaClient`] using the default production endpoints.
    pub fn new(client_id: impl Into<String>) -> Self {
        Self::with_endpoints(client_id, MsaEndpoints::default())
    }

    /// Creates a new [`MsaClient`] with custom [`MsaEndpoints`].
    pub fn with_endpoints(client_id: impl Into<String>, endpoints: MsaEndpoints) -> Self {
        Self {
            http: Client::new(),
            client_id: client_id.into(),
            endpoints,
            scope: DEFAULT_OAUTH_SCOPE.to_string(),
        }
    }

    /// Initiates the Microsoft OAuth2 Device-Code flow.
    pub async fn start_device_code(&self) -> Result<DeviceCodePrompt> {
        let url = format!(
            "{}/devicecode",
            self.endpoints.oauth_base_url.trim_end_matches('/')
        );
        let resp = self
            .http
            .post(&url)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("scope", self.scope.as_str()),
            ])
            .send()
            .await
            .map_err(|e| DemingError::Network(format!("device code request failed: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| DemingError::Network(format!("failed reading device code body: {e}")))?;

        if !status.is_success() {
            return Err(DemingError::Auth(format!(
                "device code endpoint returned HTTP {status}: {body}"
            )));
        }

        let raw: RawDeviceCodeResponse = serde_json::from_str(&body)?;
        Ok(DeviceCodePrompt {
            user_code: raw.user_code,
            verification_uri: raw.verification_uri,
            device_code: raw.device_code,
            interval_secs: raw.interval,
            expires_in_secs: raw.expires_in,
        })
    }

    /// Polls the Microsoft token endpoint until authorized, then completes the full
    /// Xbox Live -> XSTS -> Minecraft Services -> Profile authentication chain.
    pub async fn poll_and_complete_login(&self, prompt: &DeviceCodePrompt) -> Result<MsaSession> {
        let token = self.poll_device_token(prompt).await?;
        self.complete_login_with_msa_tokens(token).await
    }

    /// Refreshes an expired session using the stored Microsoft OAuth2 refresh token.
    pub async fn refresh_token(&self, refresh_token: &str) -> Result<MsaSession> {
        let url = format!(
            "{}/token",
            self.endpoints.oauth_base_url.trim_end_matches('/')
        );
        let resp = self
            .http
            .post(&url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", self.client_id.as_str()),
                ("refresh_token", refresh_token),
                ("scope", self.scope.as_str()),
            ])
            .send()
            .await
            .map_err(|e| DemingError::Network(format!("token refresh request failed: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| DemingError::Network(format!("failed reading refresh body: {e}")))?;

        if !status.is_success() {
            return Err(DemingError::Auth(format!(
                "token refresh failed with HTTP {status}: {body}"
            )));
        }

        let mut token: RawMsaTokenResponse = serde_json::from_str(&body)?;
        if token.refresh_token.is_none() {
            token.refresh_token = Some(refresh_token.to_string());
        }
        self.complete_login_with_msa_tokens(token).await
    }

    async fn poll_device_token(&self, prompt: &DeviceCodePrompt) -> Result<RawMsaTokenResponse> {
        let url = format!(
            "{}/token",
            self.endpoints.oauth_base_url.trim_end_matches('/')
        );
        let deadline = Instant::now() + Duration::from_secs(prompt.expires_in_secs.max(1));
        let mut interval_secs = prompt.interval_secs;

        loop {
            if Instant::now() >= deadline {
                return Err(DemingError::Timeout {
                    operation: "microsoft device code login".to_string(),
                    seconds: prompt.expires_in_secs,
                });
            }

            let resp = self
                .http
                .post(&url)
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", self.client_id.as_str()),
                    ("device_code", prompt.device_code.as_str()),
                ])
                .send()
                .await
                .map_err(|e| DemingError::Network(format!("token poll request failed: {e}")))?;

            let status = resp.status();
            let body = resp
                .text()
                .await
                .map_err(|e| DemingError::Network(format!("failed reading token body: {e}")))?;

            if status.is_success() {
                let token: RawMsaTokenResponse = serde_json::from_str(&body)?;
                return Ok(token);
            }

            if let Ok(err_body) = serde_json::from_str::<RawOAuthErrorResponse>(&body) {
                match err_body.error.as_str() {
                    "authorization_pending" => {
                        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
                        continue;
                    }
                    "slow_down" => {
                        interval_secs = interval_secs.saturating_add(5);
                        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
                        continue;
                    }
                    "expired_token" => {
                        return Err(DemingError::Timeout {
                            operation: "microsoft device code login".to_string(),
                            seconds: prompt.expires_in_secs,
                        });
                    }
                    other => {
                        let desc = err_body.error_description.unwrap_or_default();
                        return Err(DemingError::Auth(format!(
                            "microsoft oauth error '{other}': {desc}"
                        )));
                    }
                }
            }

            return Err(DemingError::Auth(format!(
                "token poll returned HTTP {status}: {body}"
            )));
        }
    }

    async fn complete_login_with_msa_tokens(
        &self,
        msa_token: RawMsaTokenResponse,
    ) -> Result<MsaSession> {
        let msa_refresh_token = msa_token.refresh_token.ok_or_else(|| {
            DemingError::Auth("microsoft token response missing refresh_token".to_string())
        })?;

        let (xbl_token, _) = self.authenticate_xbl(&msa_token.access_token).await?;
        let (xsts_token, uhs) = self.authenticate_xsts(&xbl_token).await?;
        let mc_token = self.login_minecraft_with_xbox(&uhs, &xsts_token).await?;
        let profile = self.fetch_minecraft_profile(&mc_token.access_token).await?;

        let uuid = Uuid::parse_str(&profile.id)
            .map(|u| u.hyphenated().to_string())
            .unwrap_or(profile.id);
        let expires_at = Utc::now() + ChronoDuration::seconds(mc_token.expires_in as i64);

        Ok(MsaSession {
            uuid,
            username: profile.name,
            mc_access_token: mc_token.access_token,
            msa_refresh_token,
            expires_at,
        })
    }

    async fn authenticate_xbl(&self, msa_access_token: &str) -> Result<(String, String)> {
        let payload = json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": format!("d={msa_access_token}")
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT"
        });

        let resp = self
            .http
            .post(&self.endpoints.xbl_auth_url)
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

    async fn authenticate_xsts(&self, xbl_token: &str) -> Result<(String, String)> {
        let payload = json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": [xbl_token]
            },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT"
        });

        let resp = self
            .http
            .post(&self.endpoints.xsts_auth_url)
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

    async fn login_minecraft_with_xbox(
        &self,
        uhs: &str,
        xsts_token: &str,
    ) -> Result<RawMcLoginResponse> {
        let url = format!(
            "{}/authentication/login_with_xbox",
            self.endpoints.mc_services_base_url.trim_end_matches('/')
        );
        let payload = json!({
            "identityToken": format!("XBL3.0 x={uhs};{xsts_token}")
        });

        let resp = self
            .http
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

    async fn fetch_minecraft_profile(&self, mc_access_token: &str) -> Result<RawMcProfileResponse> {
        let url = format!(
            "{}/minecraft/profile",
            self.endpoints.mc_services_base_url.trim_end_matches('/')
        );

        let resp = self
            .http
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

#[derive(Debug, Deserialize)]
struct RawDeviceCodeResponse {
    user_code: String,
    verification_uri: String,
    device_code: String,
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
}

fn default_interval() -> u64 {
    5
}

fn default_expires_in() -> u64 {
    900
}

#[derive(Debug, Deserialize)]
struct RawMsaTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawOAuthErrorResponse {
    error: String,
    error_description: Option<String>,
}

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

#[derive(Debug, Deserialize)]
struct RawMcLoginResponse {
    access_token: String,
    #[serde(default = "default_mc_expires_in")]
    expires_in: u64,
}

fn default_mc_expires_in() -> u64 {
    86_400
}

#[derive(Debug, Deserialize)]
struct RawMcProfileResponse {
    id: String,
    name: String,
}
