//! Deming IPC bridge: Localhost TCP JSON-lines IPC protocol, server, and client.

pub mod client;
pub mod error;
pub mod message;
pub mod server;

use std::path::{Path, PathBuf};

pub use client::IpcClient;
pub use error::{IpcError, Result};
pub use message::IpcMessage;
pub use server::{IpcBroadcaster, IpcServer, PORT_FILE_NAME};

/// Computes the path to `deming-ipc.port` for the given workspace directory.
pub fn port_file_path(workspace_dir: impl AsRef<Path>) -> PathBuf {
    workspace_dir.as_ref().join(PORT_FILE_NAME)
}
