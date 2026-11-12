//! Deming CLI, job orchestrator, and stdio MCP server.

pub mod cli;
pub mod mcp;
pub mod orchestrator;

pub use orchestrator::execute_job;
