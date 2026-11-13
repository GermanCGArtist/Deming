//! Microsoft OAuth2 browser (`login.live.com`) and Device-Code token flows.

use std::time::{Duration, Instant};

use deming_core::{DemingError, Result};
use serde::Deserialize;

use super::{
    DeviceCodePrompt, MsaClient, MsaSession, DEFAULT_LIVE_REDIRECT_URI, MOJANG_CLIENT_ID,
};

#[derive(Debug, Deserialize)]
pub(crate) struct RawMsaTokenResponse {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
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
struct RawOAuthErrorResponse {
    error: String,
    error_description: Option<String>,
}

impl MsaClient {
    /// Returns the `login.live.com` browser OAuth authorization URL for this client ID.
    pub fn browser_authorize_url(&self) -> String {
        format!(
            "{}?client_id={}&response_type=code&redirect_uri=https%3A%2F%2Flogin.live.com%2Foauth20_desktop.srf&scope=XboxLive.signin%20offline_access",
            self.endpoints.live_authorize_url.trim_end_matches('/'),
            self.client_id
        )
    }

    /// Extracts an OAuth authorization `code` from either a full redirect URL
    /// (`https://login.live.com/oauth20_desktop.srf?code=M.R3_BAY...&lc=...`) or a raw code string.
    pub fn extract_auth_code(input: &str) -> Result<String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(DemingError::Auth(
                "authorization code or redirect URL cannot be empty".to_string(),
            ));
        }

        if trimmed.contains('?') || trimmed.starts_with("http://") || trimmed.starts_with("https://")
        {
            let query = trimmed.split('?').nth(1).unwrap_or("");
            let query = query.split('#').next().unwrap_or("");

            let mut code: Option<String> = None;
            let mut error: Option<String> = None;
            let mut error_desc: Option<String> = None;

            for pair in query.split('&') {
                if pair.is_empty() {
                    continue;
                }
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                match k {
                    "code" if !v.is_empty() => code = Some(percent_decode(v)),
                    "error" if !v.is_empty() => error = Some(percent_decode(v)),
                    "error_description" if !v.is_empty() => error_desc = Some(percent_decode(v)),
                    _ => {}
                }
            }

            if let Some(err) = error {
                let desc = error_desc.unwrap_or_default();
                return Err(DemingError::Auth(format!(
                    "microsoft browser oauth returned error '{err}': {desc}"
                )));
            }

            return code.ok_or_else(|| {
                DemingError::Auth(
                    "redirect URL does not contain a 'code' query parameter".to_string(),
                )
            });
        }

        if trimmed.chars().any(char::is_whitespace) {
            return Err(DemingError::Auth(
                "authorization code cannot contain whitespace".to_string(),
            ));
        }

        Ok(trimmed.to_string())
    }

    /// Exchanges a browser OAuth redirect URL or raw authorization code at `login.live.com`
    /// and completes the Xbox Live -> XSTS -> Minecraft Services -> Profile authentication chain.
    pub async fn complete_browser_login(&self, code_or_url: &str) -> Result<MsaSession> {
        let code = Self::extract_auth_code(code_or_url)?;
        let resp = self
            .http
            .post(&self.endpoints.live_token_url)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("code", code.as_str()),
                ("grant_type", "authorization_code"),
                ("redirect_uri", DEFAULT_LIVE_REDIRECT_URI),
            ])
            .send()
            .await
            .map_err(|e| {
                DemingError::Network(format!("browser token exchange request failed: {e}"))
            })?;

        let status = resp.status();
        let body = resp.text().await.map_err(|e| {
            DemingError::Network(format!("failed reading browser token body: {e}"))
        })?;

        if !status.is_success() {
            return Err(DemingError::Auth(format!(
                "browser token exchange failed with HTTP {status}: {body}"
            )));
        }

        let token: RawMsaTokenResponse = serde_json::from_str(&body)?;
        self.complete_login_with_msa_tokens(token).await
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
    ///
    /// Uses `login.live.com` when `client_id` is Mojang's public client ID (`00000000402b5328`),
    /// and the configured Microsoft OAuth2 `/token` endpoint otherwise.
    pub async fn refresh_token(&self, refresh_token: &str) -> Result<MsaSession> {
        let resp = if self.client_id == MOJANG_CLIENT_ID {
            self.http
                .post(&self.endpoints.live_token_url)
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", self.client_id.as_str()),
                    ("refresh_token", refresh_token),
                    ("redirect_uri", DEFAULT_LIVE_REDIRECT_URI),
                ])
                .send()
                .await
        } else {
            let url = format!(
                "{}/token",
                self.endpoints.oauth_base_url.trim_end_matches('/')
            );
            self.http
                .post(&url)
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", self.client_id.as_str()),
                    ("refresh_token", refresh_token),
                    ("scope", self.scope.as_str()),
                ])
                .send()
                .await
        }
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
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex_str) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(val) = u8::from_str_radix(hex_str, 16) {
                    out.push(val);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_authorize_url_matches_live_oauth_format() {
        let client = MsaClient::new(MOJANG_CLIENT_ID);
        assert_eq!(
            client.browser_authorize_url(),
            "https://login.live.com/oauth20_authorize.srf?client_id=00000000402b5328&response_type=code&redirect_uri=https%3A%2F%2Flogin.live.com%2Foauth20_desktop.srf&scope=XboxLive.signin%20offline_access"
        );
    }

    #[test]
    fn extract_auth_code_handles_redirect_url_and_raw_code() {
        let url = "https://login.live.com/oauth20_desktop.srf?code=M.R3_BAY.abc-123_xyz&lc=1033";
        assert_eq!(
            MsaClient::extract_auth_code(url).expect("extract from URL"),
            "M.R3_BAY.abc-123_xyz"
        );

        let encoded = "https://login.live.com/oauth20_desktop.srf?lc=1033&code=M.R3_BAY%2Btest%3D1#frag";
        assert_eq!(
            MsaClient::extract_auth_code(encoded).expect("extract percent-encoded"),
            "M.R3_BAY+test=1"
        );

        assert_eq!(
            MsaClient::extract_auth_code("  M.R3_BAY.raw-code  ").expect("extract raw code"),
            "M.R3_BAY.raw-code"
        );

        assert!(MsaClient::extract_auth_code("").is_err());
        assert!(
            MsaClient::extract_auth_code("https://login.live.com/oauth20_desktop.srf?lc=1033")
                .is_err()
        );
        assert!(MsaClient::extract_auth_code("https://login.live.com/oauth20_desktop.srf?error=access_denied&error_description=User%20cancelled").is_err());
    }
}
