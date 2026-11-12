//! Core domain types, configuration, specifications, and event schemas for Deming.

pub mod config;
pub mod error;
pub mod events;
pub mod spec;

pub use config::{DemingConfig, GlobalResourceLimits, ProcessPriority, RetentionPolicy};
pub use error::{DemingError, Result};
pub use events::{InstanceResult, InstanceState, RunEvent, RunManifest, RunStatus};
pub use spec::{
    AccountRef, InstanceSpec, ModLoader, ModSource, ServerSpec, TestCompletionRule, TestJobSpec,
};
