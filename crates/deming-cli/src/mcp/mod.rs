//! Stdio JSON-RPC 2.0 MCP (Model Context Protocol) server for Deming.

pub mod handlers;
pub mod protocol;
pub mod tools;

use std::path::{Path, PathBuf};

pub use handlers::{resolve_sandboxed_path, BatchModOp};
pub use protocol::run_stdio;
pub use tools::tool_definitions;

/// JSON-RPC 2.0 MCP server backed by a Deming configuration file.
#[derive(Debug, Clone)]
pub struct McpServer {
    pub(crate) config_path: PathBuf,
}

impl McpServer {
    /// Creates a new [`McpServer`] using `config_path`.
    pub fn new(config_path: impl Into<PathBuf>) -> Self {
        Self {
            config_path: config_path.into(),
        }
    }

    /// Returns the configured path to `config.json`.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }
}
