//! Stdio JSON-RPC 2.0 MCP (Model Context Protocol) server for Deming.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use deming_archive::RunArchiveReader;
use deming_auth::{create_offline_account, AccountStore, StoredAccount};
use deming_core::{DemingConfig, ProcessPriority, TestJobSpec};
use deming_ipc::{IpcClient, IpcMessage};
use deming_runner::AbortHandle;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::orchestrator::execute_job;

/// JSON-RPC 2.0 MCP server backed by a Deming configuration file.
#[derive(Debug, Clone)]
pub struct McpServer {
    config_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
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

    /// Handles a single parsed JSON-RPC 2.0 request.
    ///
    /// Returns `None` for notifications (requests without an `id`).
    pub async fn handle_request(&self, raw: Value) -> Option<Value> {
        let req: JsonRpcRequest = match serde_json::from_value(raw) {
            Ok(r) => r,
            Err(e) => {
                return Some(
                    serde_json::to_value(JsonRpcResponse {
                        jsonrpc: "2.0",
                        id: Value::Null,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32600,
                            message: format!("Invalid Request: {e}"),
                        }),
                    })
                    .unwrap_or(Value::Null),
                );
            }
        };

        let id = req.id?;

        let response = match req.method.as_str() {
            "initialize" => JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "deming-mcp",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                })),
                error: None,
            },
            "ping" => JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(json!({})),
                error: None,
            },
            "tools/list" => JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(json!({
                    "tools": tool_definitions()
                })),
                error: None,
            },
            "tools/call" => {
                let name = req
                    .params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = req
                    .params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));

                if name.is_empty() {
                    JsonRpcResponse {
                        jsonrpc: "2.0",
                        id,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32602,
                            message: "Missing tool 'name' in params".to_string(),
                        }),
                    }
                } else {
                    match self.call_tool(name, args).await {
                        Ok(payload) => JsonRpcResponse {
                            jsonrpc: "2.0",
                            id,
                            result: Some(json!({
                                "content": [{
                                    "type": "text",
                                    "text": serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string())
                                }],
                                "isError": false
                            })),
                            error: None,
                        },
                        Err(err) => JsonRpcResponse {
                            jsonrpc: "2.0",
                            id,
                            result: Some(json!({
                                "content": [{
                                    "type": "text",
                                    "text": format!("{err:#}")
                                }],
                                "isError": true
                            })),
                            error: None,
                        },
                    }
                }
            }
            other => JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {other}"),
                }),
            },
        };

        serde_json::to_value(response).ok()
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<Value> {
        match name {
            "get_config" => {
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                Ok(serde_json::to_value(cfg)?)
            }
            "set_config" => {
                let mut cfg = DemingConfig::load_or_default(&self.config_path)?;
                if let Some(ws) = args.get("workspace_dir").and_then(|v| v.as_str()) {
                    cfg.workspace_dir = PathBuf::from(ws);
                }
                if let Some(v) = args.get("max_total_ram_mb").and_then(|v| v.as_u64()) {
                    cfg.global_limits.max_total_ram_mb = v;
                }
                if let Some(v) = args
                    .get("max_concurrent_instances")
                    .and_then(|v| v.as_u64())
                {
                    cfg.global_limits.max_concurrent_instances = v as usize;
                }
                if let Some(v) = args.get("default_instance_ram_mb").and_then(|v| v.as_u64()) {
                    cfg.global_limits.default_instance_ram_mb = v;
                }
                if let Some(v) = args
                    .get("default_cpu_cores_per_instance")
                    .and_then(|v| v.as_u64())
                {
                    cfg.global_limits.default_cpu_cores_per_instance = v as usize;
                }
                if let Some(p) = args.get("process_priority").and_then(|v| v.as_str()) {
                    cfg.global_limits.process_priority = match p {
                        "below_normal" | "below-normal" => ProcessPriority::BelowNormal,
                        "normal" => ProcessPriority::Normal,
                        "above_normal" | "above-normal" => ProcessPriority::AboveNormal,
                        "high" => ProcessPriority::High,
                        other => anyhow::bail!("invalid process_priority '{other}'"),
                    };
                }
                cfg.save(&self.config_path)?;
                Ok(serde_json::to_value(cfg)?)
            }
            "list_accounts" => {
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let store = AccountStore::from_config(&cfg);
                let accounts = store.load()?;
                Ok(serde_json::to_value(accounts)?)
            }
            "add_offline_account" => {
                let username = args
                    .get("username")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'username'"))?;
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let store = AccountStore::from_config(&cfg);
                let offline = create_offline_account(username)?;
                let stored = StoredAccount::from_offline(&offline);
                store.upsert(stored.clone())?;
                Ok(serde_json::to_value(stored)?)
            }
            "submit_test_run" => {
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let job_val = args.get("job").cloned().unwrap_or(args);
                let job: TestJobSpec = serde_json::from_value(job_val)?;
                let abort = Arc::new(AbortHandle::new());
                let ipc = IpcClient::connect(&cfg.workspace_dir).await.ok();
                let manifest = execute_job(&cfg, &job, abort, ipc.as_ref()).await?;
                Ok(serde_json::to_value(manifest)?)
            }
            "get_run_status" => {
                let run_id = args
                    .get("run_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'run_id'"))?;
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let reader = RunArchiveReader::new(cfg.runs_dir());
                let run = reader.find_run(run_id)?;
                let manifest = reader.load_manifest(&run.path)?;
                Ok(serde_json::to_value(manifest)?)
            }
            "list_runs" => {
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let reader = RunArchiveReader::new(cfg.runs_dir());
                let runs = reader.list_runs()?;
                Ok(serde_json::to_value(runs)?)
            }
            "tail_instance_logs" => {
                let run_id = args
                    .get("run_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'run_id'"))?;
                let instance_id = args
                    .get("instance_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("server");
                let log_file = args.get("log_file").and_then(|v| v.as_str());
                let max_lines = args
                    .get("max_lines")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(100) as usize;

                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let reader = RunArchiveReader::new(cfg.runs_dir());
                let run = reader.find_run(run_id)?;
                let lines = reader.tail_logs(&run.path, instance_id, log_file, max_lines)?;
                Ok(json!({
                    "run_id": run_id,
                    "source": instance_id,
                    "lines": lines,
                }))
            }
            "capture_screenshot" => {
                let run_id = args
                    .get("run_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'run_id'"))?;
                let instance_id = args
                    .get("instance_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'instance_id'"))?;

                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                if let Ok(ipc) = IpcClient::connect(&cfg.workspace_dir).await {
                    if ipc.is_connected() {
                        let _ = ipc
                            .send(&IpcMessage::RequestScreenshot {
                                run_id: run_id.to_string(),
                                instance_id: instance_id.to_string(),
                            })
                            .await;
                        if let Ok(Some(IpcMessage::ScreenshotResult { path, error, .. })) =
                            ipc.recv_timeout(Duration::from_secs(5)).await
                        {
                            if let Some(err) = error {
                                anyhow::bail!("screenshot capture failed: {err}");
                            }
                            if let Some(p) = path {
                                return Ok(json!({
                                    "run_id": run_id,
                                    "instance_id": instance_id,
                                    "path": p,
                                }));
                            }
                        }
                    }
                }

                let reader = RunArchiveReader::new(cfg.runs_dir());
                let run = reader.find_run(run_id)?;
                let shots = reader.list_screenshots(&run.path, Some(instance_id))?;
                Ok(json!({
                    "run_id": run_id,
                    "instance_id": instance_id,
                    "screenshots": shots,
                }))
            }
            "abort_run" => {
                let run_id = args.get("run_id").and_then(|v| v.as_str());
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let ipc = IpcClient::connect(&cfg.workspace_dir).await?;
                let sent = if let Some(rid) = run_id {
                    ipc.abort_run(rid).await?
                } else {
                    ipc.abort_all().await?
                };
                Ok(json!({
                    "aborted": sent,
                    "run_id": run_id,
                }))
            }
            unknown => anyhow::bail!("unknown tool: {unknown}"),
        }
    }
}

/// Runs the stdio JSON-RPC 2.0 MCP server loop until EOF.
pub async fn run_stdio(config_path: PathBuf) -> Result<()> {
    let server = McpServer::new(config_path);
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(val) => server.handle_request(val).await,
            Err(e) => Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": {
                    "code": -32700,
                    "message": format!("Parse error: {e}")
                }
            })),
        };

        if let Some(resp) = response {
            let mut out = serde_json::to_string(&resp)?;
            out.push('\n');
            stdout.write_all(out.as_bytes()).await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

/// Returns the JSON tool definitions exposed by `tools/list`.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "get_config",
            "description": "Get the current Deming configuration (workspace, resource limits, retention).",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "set_config",
            "description": "Update Deming configuration fields and persist to disk.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace_dir": { "type": "string" },
                    "max_total_ram_mb": { "type": "integer", "minimum": 128 },
                    "max_concurrent_instances": { "type": "integer", "minimum": 1 },
                    "default_instance_ram_mb": { "type": "integer", "minimum": 128 },
                    "default_cpu_cores_per_instance": { "type": "integer", "minimum": 1 },
                    "process_priority": {
                        "type": "string",
                        "enum": ["below_normal", "normal", "above_normal", "high"]
                    }
                }
            }
        }),
        json!({
            "name": "list_accounts",
            "description": "List all stored offline and Microsoft accounts.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "add_offline_account",
            "description": "Create and store a deterministic offline/dev Minecraft account.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "username": { "type": "string", "minLength": 1, "maxLength": 16 }
                },
                "required": ["username"]
            }
        }),
        json!({
            "name": "submit_test_run",
            "description": "Submit and execute a TestJobSpec end-to-end.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "job": { "type": "object", "description": "TestJobSpec JSON object" }
                },
                "required": ["job"]
            }
        }),
        json!({
            "name": "get_run_status",
            "description": "Get the full RunManifest for a specific run ID or archive name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string" }
                },
                "required": ["run_id"]
            }
        }),
        json!({
            "name": "list_runs",
            "description": "List archived and recent test runs.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "tail_instance_logs",
            "description": "Tail recent log lines for an instance or server from a run.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string" },
                    "instance_id": { "type": "string" },
                    "log_file": { "type": "string" },
                    "max_lines": { "type": "integer", "minimum": 1 }
                },
                "required": ["run_id"]
            }
        }),
        json!({
            "name": "capture_screenshot",
            "description": "Request a live screenshot via IPC or list archived screenshots for an instance.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string" },
                    "instance_id": { "type": "string" }
                },
                "required": ["run_id", "instance_id"]
            }
        }),
        json!({
            "name": "abort_run",
            "description": "Abort a specific active run or all active runs via IPC.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string" }
                }
            }
        }),
    ]
}
