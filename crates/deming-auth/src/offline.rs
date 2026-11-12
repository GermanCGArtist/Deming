//! Offline/development account creation with deterministic UUID v3 generation.

use deming_core::{AccountRef, DemingError, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Prefix used by Minecraft when deriving deterministic offline player UUIDs.
const OFFLINE_PLAYER_PREFIX: &str = "OfflinePlayer:";

/// An offline/development account for `online-mode=false` local servers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfflineAccount {
    /// Player username.
    pub username: String,
    /// Deterministic UUID v3 derived from `OfflinePlayer:<username>`.
    pub uuid: Uuid,
}

impl OfflineAccount {
    /// Returns the hyphenated lowercase UUID string.
    pub fn uuid_hyphenated(&self) -> String {
        self.uuid.hyphenated().to_string()
    }

    /// Returns the unhyphenated (simple) lowercase UUID string.
    pub fn uuid_simple(&self) -> String {
        self.uuid.simple().to_string()
    }

    /// Converts this offline account into a [`deming_core::AccountRef`].
    pub fn to_account_ref(&self) -> AccountRef {
        AccountRef::Offline {
            username: self.username.clone(),
        }
    }
}

/// Computes a deterministic UUID v3 for `OfflinePlayer:<username>`.
pub fn offline_uuid(username: &str) -> Uuid {
    let seed = format!("{OFFLINE_PLAYER_PREFIX}{username}");
    Uuid::new_v3(&Uuid::nil(), seed.as_bytes())
}

/// Creates a validated [`OfflineAccount`] with a deterministic UUID v3.
pub fn create_offline_account(username: impl Into<String>) -> Result<OfflineAccount> {
    let username = username.into();
    let trimmed = username.trim();
    if trimmed.is_empty() {
        return Err(DemingError::Auth(
            "offline username cannot be empty".to_string(),
        ));
    }
    if trimmed != username {
        return Err(DemingError::Auth(
            "offline username cannot have leading or trailing whitespace".to_string(),
        ));
    }
    if username.len() > 16 {
        return Err(DemingError::Auth(format!(
            "offline username '{username}' exceeds 16 characters"
        )));
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(DemingError::Auth(format!(
            "offline username '{username}' contains invalid characters (allowed: ASCII alphanumeric and '_')"
        )));
    }

    let uuid = offline_uuid(&username);
    Ok(OfflineAccount { username, uuid })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Version;

    #[test]
    fn offline_uuid_is_deterministic_v3() {
        let first = offline_uuid("DemingBot");
        let second = offline_uuid("DemingBot");
        assert_eq!(first, second);
        assert_eq!(first.get_version(), Some(Version::Md5));
        assert_ne!(first, offline_uuid("OtherBot"));
    }

    #[test]
    fn create_offline_account_validates_username() {
        let acct = create_offline_account("Player_01").expect("valid username");
        assert_eq!(acct.username, "Player_01");
        assert_eq!(acct.uuid, offline_uuid("Player_01"));
        assert_eq!(
            acct.to_account_ref(),
            AccountRef::Offline {
                username: "Player_01".to_string()
            }
        );

        assert!(create_offline_account("").is_err());
        assert!(create_offline_account("  ").is_err());
        assert!(create_offline_account("Player With Space").is_err());
        assert!(create_offline_account("UsernameWayTooLong1").is_err());
    }
}
