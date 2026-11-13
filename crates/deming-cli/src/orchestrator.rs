//! Thin adapter re-exporting shared job orchestration from [`deming_engine`].

pub use deming_engine::{
    default_config_path, execute_job, execute_job_with_ipc, execute_job_with_ipc_options,
    execute_job_with_options, JobExecutionOptions, DEFAULT_MSA_CLIENT_ID,
};
