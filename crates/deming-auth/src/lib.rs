//! Authentication and account management for Deming.
//!
//! Supports deterministic offline/dev accounts (`offline`) as well as the full
//! Microsoft OAuth Device-Code -> Xbox Live -> XSTS -> Minecraft Services flow
//! (`msa`) and persistent account storage (`store`).

pub mod msa;
pub mod offline;
pub mod store;

pub use msa::{DeviceCodePrompt, MsaClient, MsaEndpoints, MsaSession};
pub use offline::{create_offline_account, offline_uuid, OfflineAccount};
pub use store::{AccountStore, StoredAccount};
