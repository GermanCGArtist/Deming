//! Persistent account store for offline and Microsoft accounts.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use deming_core::{AccountRef, DemingConfig, Result};
use serde::{Deserialize, Serialize};

use crate::msa::MsaSession;
use crate::offline::OfflineAccount;

/// Persisted account record stored in `config.accounts_file()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoredAccount {
    /// Offline/development account for `online-mode=false` servers.
    Offline { username: String, uuid: String },
    /// Authenticated Microsoft/Minecraft account with tokens and expiration.
    Microsoft {
        uuid: String,
        username: String,
        mc_access_token: String,
        msa_refresh_token: String,
        expires_at: DateTime<Utc>,
    },
}

impl StoredAccount {
    /// Creates a [`StoredAccount::Offline`] from an [`OfflineAccount`].
    pub fn from_offline(account: &OfflineAccount) -> Self {
        Self::Offline {
            username: account.username.clone(),
            uuid: account.uuid_hyphenated(),
        }
    }

    /// Creates a [`StoredAccount::Microsoft`] from a completed [`MsaSession`].
    pub fn from_msa(session: &MsaSession) -> Self {
        Self::Microsoft {
            uuid: session.uuid.clone(),
            username: session.username.clone(),
            mc_access_token: session.mc_access_token.clone(),
            msa_refresh_token: session.msa_refresh_token.clone(),
            expires_at: session.expires_at,
        }
    }

    /// Returns the account's username.
    pub fn username(&self) -> &str {
        match self {
            Self::Offline { username, .. } | Self::Microsoft { username, .. } => username,
        }
    }

    /// Returns the account's UUID string.
    pub fn uuid(&self) -> &str {
        match self {
            Self::Offline { uuid, .. } | Self::Microsoft { uuid, .. } => uuid,
        }
    }

    /// Returns `true` if this account has an access token that is expired at `now`.
    /// Offline accounts never expire.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        match self {
            Self::Offline { .. } => false,
            Self::Microsoft { expires_at, .. } => now >= *expires_at,
        }
    }

    /// Returns `true` if this account's token is currently expired.
    pub fn is_expired(&self) -> bool {
        self.is_expired_at(Utc::now())
    }

    /// Converts this stored account to a [`deming_core::AccountRef`].
    pub fn to_account_ref(&self) -> AccountRef {
        match self {
            Self::Offline { username, .. } => AccountRef::Offline {
                username: username.clone(),
            },
            Self::Microsoft { uuid, username, .. } => AccountRef::Microsoft {
                uuid: uuid.clone(),
                username: username.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct AccountsFilePayload {
    version: u32,
    accounts: Vec<StoredAccount>,
}

/// Manages reading and writing [`StoredAccount`] entries on disk.
#[derive(Debug, Clone)]
pub struct AccountStore {
    path: PathBuf,
}

impl AccountStore {
    /// Creates an [`AccountStore`] backed by `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Creates an [`AccountStore`] using `config.accounts_file()`.
    pub fn from_config(config: &DemingConfig) -> Self {
        Self::new(config.accounts_file())
    }

    /// Returns the path to the persisted accounts file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads all stored accounts from disk. Returns an empty `Vec` if the file does not exist.
    pub fn load(&self) -> Result<Vec<StoredAccount>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let raw = fs::read_to_string(&self.path)?;
        let payload: AccountsFilePayload = serde_json::from_str(&raw)?;
        Ok(payload.accounts)
    }

    /// Persists the given slice of accounts to disk, creating parent directories if needed.
    pub fn save(&self, accounts: &[StoredAccount]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = AccountsFilePayload {
            version: 1,
            accounts: accounts.to_vec(),
        };
        let json = serde_json::to_string_pretty(&payload)?;
        fs::write(&self.path, json)?;
        Ok(())
    }

    /// Inserts or replaces an account matching by variant and UUID (or username for Offline).
    pub fn upsert(&self, account: StoredAccount) -> Result<()> {
        let mut accounts = self.load()?;
        if let Some(existing) = accounts.iter_mut().find(|a| match (a, &account) {
            (
                StoredAccount::Offline { username: u1, .. },
                StoredAccount::Offline { username: u2, .. },
            ) => u1 == u2,
            (
                StoredAccount::Microsoft { uuid: id1, .. },
                StoredAccount::Microsoft { uuid: id2, .. },
            ) => id1 == id2,
            _ => false,
        }) {
            *existing = account;
        } else {
            accounts.push(account);
        }
        self.save(&accounts)
    }

    /// Removes any account matching `uuid`. Returns `true` if at least one account was removed.
    pub fn remove_by_uuid(&self, uuid: &str) -> Result<bool> {
        let mut accounts = self.load()?;
        let before = accounts.len();
        accounts.retain(|a| a.uuid() != uuid);
        let removed = accounts.len() != before;
        if removed {
            self.save(&accounts)?;
        }
        Ok(removed)
    }

    /// Finds a stored account matching an [`AccountRef`].
    pub fn find_by_ref(&self, account_ref: &AccountRef) -> Result<Option<StoredAccount>> {
        let accounts = self.load()?;
        Ok(accounts.into_iter().find(|a| match (a, account_ref) {
            (StoredAccount::Offline { username: u1, .. }, AccountRef::Offline { username: u2 }) => {
                u1 == u2
            }
            (
                StoredAccount::Microsoft { uuid: id1, .. },
                AccountRef::Microsoft { uuid: id2, .. },
            ) => id1 == id2,
            _ => false,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::create_offline_account;
    use chrono::Duration;
    use deming_core::{GlobalResourceLimits, RetentionPolicy};
    use uuid::Uuid;

    fn temp_workspace_config() -> DemingConfig {
        let dir = std::env::temp_dir().join(format!("deming-auth-test-{}", Uuid::new_v4()));
        DemingConfig {
            workspace_dir: dir,
            global_limits: GlobalResourceLimits::default(),
            retention: RetentionPolicy::default(),
            curseforge_api_key: None,
        }
    }

    #[test]
    fn account_store_save_and_load_round_trip() {
        let config = temp_workspace_config();
        let store = AccountStore::from_config(&config);
        assert!(store.load().expect("load empty").is_empty());

        let offline = StoredAccount::from_offline(
            &create_offline_account("DevPlayer").expect("valid offline"),
        );
        let now = Utc::now();
        let msa = StoredAccount::Microsoft {
            uuid: "12345678-1234-1234-1234-1234567890ab".to_string(),
            username: "RealPlayer".to_string(),
            mc_access_token: "mc_tok_123".to_string(),
            msa_refresh_token: "msa_ref_456".to_string(),
            expires_at: now + Duration::hours(12),
        };

        store.upsert(offline.clone()).expect("upsert offline");
        store.upsert(msa.clone()).expect("upsert msa");

        let loaded = store.load().expect("load saved");
        assert_eq!(loaded.len(), 2);
        assert_eq!(
            store
                .find_by_ref(&AccountRef::Offline {
                    username: "DevPlayer".to_string()
                })
                .expect("find offline"),
            Some(offline)
        );
        assert_eq!(
            store
                .find_by_ref(&AccountRef::Microsoft {
                    uuid: "12345678-1234-1234-1234-1234567890ab".to_string(),
                    username: "RealPlayer".to_string(),
                })
                .expect("find msa"),
            Some(msa.clone())
        );
        assert!(!msa.is_expired_at(now));
        assert!(msa.is_expired_at(now + Duration::hours(13)));

        assert!(store
            .remove_by_uuid("12345678-1234-1234-1234-1234567890ab")
            .expect("remove msa"));
        assert_eq!(store.load().expect("load after remove").len(), 1);

        let _ = fs::remove_dir_all(&config.workspace_dir);
    }
}
