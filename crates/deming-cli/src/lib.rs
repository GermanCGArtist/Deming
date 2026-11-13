//! Deming CLI, job orchestrator, and stdio MCP server.

pub mod cli;
pub mod crash_analyzer;
pub mod mcp;
pub mod orchestrator;
pub mod pack_ops;

pub use orchestrator::execute_job;
