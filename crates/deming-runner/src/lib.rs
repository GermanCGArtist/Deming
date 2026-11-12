//! Process execution, Windows Job Object containment, resource scheduling,
//! completion rule evaluation, and native screenshot capture for Deming.

use std::path::{Path, PathBuf};

pub mod client;
pub mod evaluator;
pub mod scheduler;
pub mod screenshot;
pub mod server;
pub mod win_job;

pub use client::{
    build_connect_args, run_client, strip_mojang_placeholders, supports_quick_play,
    ClientLaunchConfig, ClientRunOutcome,
};
pub use evaluator::{evaluate_rule, EvaluationContext, EvaluationResult};
pub use scheduler::{AbortHandle, ResourcePermit, ResourceScheduler};
pub use screenshot::{capture_instance_screenshot, capture_pid_to_png, find_windows_for_pid, index_screenshots};
pub use server::{pick_free_port, write_eula, write_server_properties, ServerLaunchConfig, ServerProcess};
pub use win_job::{configure_pid, configure_tokio_child, set_cpu_affinity_and_priority, JobHandle};

/// Converts a filesystem path into a clean absolute path without requiring it to exist
/// and without adding Windows `\\?\` verbatim prefixes.
pub(crate) fn absolutize_path(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Absolutizes an executable path only when it contains directory components (e.g. `./jre/bin/java`),
/// leaving bare command names (e.g. `java`, `cmd.exe`) intact for `PATH` lookup.
pub(crate) fn absolutize_executable(path: &Path) -> PathBuf {
    let has_parent = path
        .parent()
        .map(|p| !p.as_os_str().is_empty())
        .unwrap_or(false);
    if path.is_relative() && has_parent {
        absolutize_path(path)
    } else {
        path.to_path_buf()
    }
}
