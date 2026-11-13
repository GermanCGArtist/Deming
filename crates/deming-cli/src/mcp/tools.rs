//! JSON Schema tool definitions exposed by MCP `tools/list`.

use serde_json::{json, Value};

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
            "description": "Submit and execute a TestJobSpec end-to-end (supports dry_run).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "job": { "type": "object", "description": "TestJobSpec JSON object" },
                    "dry_run": { "type": "boolean" }
                },
                "required": ["job"]
            }
        }),
        json!({
            "name": "apply_modpack",
            "description": "Sync a deming.toml or mods.json manifest into a sandboxed instance directory (supports dry_run).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "manifest_path": { "type": "string" },
                    "instance_dir": { "type": "string" },
                    "dry_run": { "type": "boolean" }
                },
                "required": ["manifest_path", "instance_dir"]
            }
        }),
        json!({
            "name": "update_mod_state",
            "description": "Enable, disable, or remove a mod JAR inside a sandboxed instance directory (supports dry_run).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "instance_dir": { "type": "string" },
                    "filename": { "type": "string" },
                    "state": { "type": "string", "enum": ["enable", "disable", "remove"] },
                    "dry_run": { "type": "boolean" }
                },
                "required": ["instance_dir", "filename", "state"]
            }
        }),
        json!({
            "name": "delete_instance",
            "description": "Delete a sandboxed instance directory inside the workspace (supports dry_run).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "instance_dir": { "type": "string" },
                    "dry_run": { "type": "boolean" }
                },
                "required": ["instance_dir"]
            }
        }),
        json!({
            "name": "analyze_crash_log",
            "description": "Analyze latest.log and crash-reports/*.txt in a sandboxed directory (or raw log_text) to extract exception summary and implicated mods.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "instance_dir": { "type": "string" },
                    "log_text": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "batch_update_mods",
            "description": "Add, remove, enable, or disable multiple mods in a single call (supports dry_run).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "instance_dir": { "type": "string" },
                    "manifest_path": { "type": "string" },
                    "mc_version": { "type": "string" },
                    "dry_run": { "type": "boolean" },
                    "operations": { "type": "array" }
                },
                "required": ["instance_dir", "operations"]
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
