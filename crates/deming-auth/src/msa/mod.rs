//! Microsoft OAuth Device-Code & Browser OAuth -> Xbox Live -> XSTS -> Minecraft Services authentication flow.

mod minecraft;
mod oauth;
mod xbox;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use deming_core::{AccountRef, DemingError, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use self::oauth::RawMsaTokenResponse;

pub(crate) const DEFAULT_OAUTH_BASE_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0";
pub(crate) const DEFAULT_LIVE_AUTHORIZE_URL: &str = "https://login.live.com/oauth20_authorize.srf";
pub(crate) const DEFAULT_LIVE_TOKEN_URL: &str = "https://login.live.com/oauth20_token.srf";
pub(crate) const DEFAULT_LIVE_REDIRECT_URI: &str = "https://login.live.com/oauth20_desktop.srf";
pub(crate) const DEFAULT_XBL_AUTH_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
pub(crate) const DEFAULT_XSTS_AUTH_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
pub(crate) const DEFAULT_MC_SERVICES_BASE_URL: &str = "https://api.minecraftservices.com";
pub(crate) const DEFAULT_OAUTH_SCOPE: &str = "XboxLive.signin offline_access";

/// Mojang's public Xbox/Minecraft client ID for `login.live.com` browser OAuth.
pub const MOJANG_CLIENT_ID: &str = "00000000402b5328";

/// Configurable endpoint URLs for Microsoft, Xbox Live, XSTS, and Minecraft Services.
///
/// Overriding these endpoints allows tests to run the full auth flow against a local mock server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MsaEndpoints {
    /// Base URL for Microsoft OAuth2 endpoints (`/devicecode`, `/token`).
    pub oauth_base_url: String,
    /// Full URL for Live Connect browser authorization (`login.live.com/oauth20_authorize.srf`).
    pub live_authorize_url: String,
    /// Full URL for Live Connect token exchange/refresh (`login.live.com/oauth20_token.srf`).
    pub live_token_url: String,
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
            live_authorize_url: DEFAULT_LIVE_AUTHORIZE_URL.to_string(),
            live_token_url: DEFAULT_LIVE_TOKEN_URL.to_string(),
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
            live_authorize_url: format!("{base}/oauth20_authorize.srf"),
            live_token_url: format!("{base}/oauth20_token.srf"),
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

/// Async client implementing the Microsoft OAuth -> XBL -> XSTS -> Minecraft flow.
#[derive(Debug, Clone)]
pub struct MsaClient {
    pub(crate) http: Client,
    pub(crate) client_id: String,
    pub(crate) endpoints: MsaEndpoints,
    pub(crate) scope: String,
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

    pub(crate) async fn complete_login_with_msa_tokens(
        &self,
        msa_token: RawMsaTokenResponse,
    ) -> Result<MsaSession> {
        let msa_refresh_token = msa_token.refresh_token.ok_or_else(|| {
            DemingError::Auth("microsoft token response missing refresh_token".to_string())
        })?;

        let (xbl_token, _) =
            xbox::authenticate_xbl(&self.http, &self.endpoints.xbl_auth_url, &msa_token.access_token)
                .await?;
        let (xsts_token, uhs) =
            xbox::authenticate_xsts(&self.http, &self.endpoints.xsts_auth_url, &xbl_token).await?;
        let mc_token = minecraft::login_minecraft_with_xbox(
            &self.http,
            &self.endpoints.mc_services_base_url,
            &uhs,
            &xsts_token,
        )
        .await?;
        let profile = minecraft::fetch_minecraft_profile(
            &self.http,
            &self.endpoints.mc_services_base_url,
            &mc_token.access_token,
        )
        .await?;

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
}
