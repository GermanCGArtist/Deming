//! JSON-RPC 2.0 protocol framing and stdio transport for the MCP server.

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::tools::tool_definitions;
use super::McpServer;

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
