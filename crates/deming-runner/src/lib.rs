//! Process execution, Windows Job Object containment, resource scheduling,
//! completion rule evaluation, and native screenshot capture for Deming.

pub mod client;
pub mod evaluator;
pub mod scheduler;
pub mod screenshot;
pub mod server;
pub mod win_job;

pub use client::{build_connect_args, run_client, supports_quick_play, ClientLaunchConfig, ClientRunOutcome};
pub use evaluator::{evaluate_rule, EvaluationContext, EvaluationResult};
pub use scheduler::{AbortHandle, ResourcePermit, ResourceScheduler};
pub use screenshot::{capture_instance_screenshot, capture_pid_to_png, find_windows_for_pid, index_screenshots};
pub use server::{pick_free_port, write_eula, write_server_properties, ServerLaunchConfig, ServerProcess};
pub use win_job::{configure_pid, configure_tokio_child, set_cpu_affinity_and_priority, JobHandle};
