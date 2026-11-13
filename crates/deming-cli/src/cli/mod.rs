//! Clap CLI definitions and command handlers for `deming`.

pub mod args;
pub mod commands;

pub use args::*;
pub use commands::{apply_config_updates, dispatch};
