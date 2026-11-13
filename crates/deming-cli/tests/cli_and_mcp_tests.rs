use std::path::PathBuf;

use chrono::Utc;
use clap::Parser;
use deming_archive::RunArchiveWriter;
use deming_cli::cli::{
    apply_config_updates, AccountAction, Cli, Commands, ConfigAction, LoaderArg, PriorityArg,
    RunAction, SourceArg,
};
use deming_cli::crash_analyzer::analyze_crash_text;
use deming_cli::mcp::{resolve_sandboxed_path, McpServer};
use deming_core::{DemingConfig, ProcessPriority, RunManifest, RunStatus, TestJobSpec};
use serde_json::{json, Value};
use uuid::Uuid;

fn temp_config_path() -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("deming-cli-test-{}", Uuid::new_v4()));
    let cfg = base.join("config.json");
    (base, cfg)
}

#[test]
fn cli_parses_config_and_account_commands() {
    let cli = Cli::try_parse_from([
        "deming",
        "--json",
        "--config",
        "C:/tmp/cfg.json",
        "config",
        "set",
        "--workspace",
        "C:/ws",
        "--max-ram-mb",
        "8192",
        "--max-concurrent",
        "3",
        "--cpu-cores",
        "4",
        "--default-ram-mb",
        "1024",
        "--priority",
        "below-normal",
    ])
    .expect("parse config set");

    assert!(cli.json);
    assert_eq!(cli.config, Some(PathBuf::from("C:/tmp/cfg.json")));
    match cli.command.expect("command") {
        Commands::Config(cmd) => match cmd.action {
            ConfigAction::Set(args) => {
                assert_eq!(args.workspace, Some(PathBuf::from("C:/ws")));
                assert_eq!(args.max_ram_mb, Some(8192));
                assert_eq!(args.max_concurrent, Some(3));
                assert_eq!(args.cpu_cores, Some(4));
                assert_eq!(args.default_ram_mb, Some(1024));
                assert_eq!(args.priority, Some(PriorityArg::BelowNormal));

                let mut cfg = DemingConfig::default();
                apply_config_updates(&mut cfg, &args);
                assert_eq!(cfg.workspace_dir, PathBuf::from("C:/ws"));
                assert_eq!(cfg.global_limits.max_total_ram_mb, 8192);
                assert_eq!(cfg.global_limits.max_concurrent_instances, 3);
                assert_eq!(cfg.global_limits.default_cpu_cores_per_instance, 4);
                assert_eq!(cfg.global_limits.default_instance_ram_mb, 1024);
                assert_eq!(
                    cfg.global_limits.process_priority,
                    ProcessPriority::BelowNormal
                );
            }
            _ => panic!("expected ConfigAction::Set"),
        },
        _ => panic!("expected Commands::Config"),
    }

    let acc = Cli::try_parse_from(["deming", "account", "add-offline", "Tester01"])
        .expect("parse add-offline");
    assert!(matches!(
        acc.command,
        Some(Commands::Account(c)) if matches!(c.action, AccountAction::AddOffline { ref username } if username == "Tester01")
    ));

    let rm = Cli::try_parse_from(["deming", "account", "remove", "1234-5678"])
        .expect("parse account remove");
    assert!(matches!(
        rm.command,
        Some(Commands::Account(c)) if matches!(c.action, AccountAction::Remove { ref uuid } if uuid == "1234-5678")
    ));
}

#[test]
fn cli_parses_run_and_mcp_commands() {
    let logs = Cli::try_parse_from([
        "deming",
        "run",
        "logs",
        "abc-123",
        "--instance",
        "client-1",
        "--grep",
        "ERROR",
    ])
    .expect("parse run logs");
    match logs.command.expect("command") {
        Commands::Run(r) => match r.action {
            RunAction::Logs {
                run_id,
                instance,
                grep,
            } => {
                assert_eq!(run_id, "abc-123");
                assert_eq!(instance.as_deref(), Some("client-1"));
                assert_eq!(grep.as_deref(), Some("ERROR"));
            }
            _ => panic!("expected RunAction::Logs"),
        },
        _ => panic!("expected Commands::Run"),
    }

    let abort_all = Cli::try_parse_from(["deming", "run", "abort"]).expect("parse run abort");
    assert!(matches!(
        abort_all.command,
        Some(Commands::Run(r)) if matches!(r.action, RunAction::Abort { run_id: None })
    ));

    let shot = Cli::try_parse_from(["deming", "run", "screenshot", "run-1", "client-2"])
        .expect("parse run screenshot");
    assert!(matches!(
        shot.command,
        Some(Commands::Run(r)) if matches!(
            r.action,
            RunAction::Screenshot { ref run_id, ref instance_id }
            if run_id == "run-1" && instance_id == "client-2"
        )
    ));

    let submit_cloak = Cli::try_parse_from(["deming", "run", "submit", "job.json", "--cloak"])
        .expect("parse run submit --cloak");
    assert!(matches!(
        submit_cloak.command,
        Some(Commands::Run(r)) if matches!(
            r.action,
            RunAction::Submit { ref job_file, cloak: true } if job_file == &PathBuf::from("job.json")
        )
    ));

    let mcp = Cli::try_parse_from(["deming", "mcp"]).expect("parse mcp");
    assert!(matches!(mcp.command, Some(Commands::Mcp)));

    let no_sub = Cli::try_parse_from(["deming"]).expect("parse bare deming");
    assert!(no_sub.command.is_none());

    let search = Cli::try_parse_from([
        "deming",
        "search",
        "sodium",
        "--mc-version",
        "1.21.4",
        "--loader",
        "fabric",
        "--source",
        "curseforge",
    ])
    .expect("parse search");
    match search.command {
        Some(Commands::Search(s)) => {
            assert_eq!(s.query, "sodium");
            assert_eq!(s.mc_version.as_deref(), Some("1.21.4"));
            assert_eq!(s.loader, Some(LoaderArg::Fabric));
            assert_eq!(s.source, SourceArg::CurseForge);
        }
        _ => panic!("expected Commands::Search"),
    }

    let add = Cli::try_parse_from([
        "deming",
        "add",
        "fabric-api",
        "--manifest",
        "deming.toml",
        "--mc-version",
        "1.21.4",
        "--loader",
        "fabric",
    ])
    .expect("parse add");
    match add.command {
        Some(Commands::Add(a)) => {
            assert_eq!(a.slug, "fabric-api");
            assert_eq!(a.manifest, Some(PathBuf::from("deming.toml")));
            assert_eq!(a.mc_version.as_deref(), Some("1.21.4"));
            assert_eq!(a.loader, Some(LoaderArg::Fabric));
        }
        _ => panic!("expected Commands::Add"),
    }

    let sync = Cli::try_parse_from([
        "deming",
        "sync",
        "--manifest",
        "deming.toml",
        "--instance",
        "instances/test",
        "--dry-run",
    ])
    .expect("parse sync");
    match sync.command {
        Some(Commands::Sync(s)) => {
            assert_eq!(s.manifest, Some(PathBuf::from("deming.toml")));
            assert_eq!(s.instance, Some(PathBuf::from("instances/test")));
            assert!(s.dry_run);
        }
        _ => panic!("expected Commands::Sync"),
    }
}

#[tokio::test]
async fn mcp_initialize_and_tools_list() {
    let (base, cfg_path) = temp_config_path();
    let server = McpServer::new(&cfg_path);

    let init_resp = server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .await
        .expect("initialize response");
    assert_eq!(init_resp["id"], 1);
    assert_eq!(init_resp["result"]["serverInfo"]["name"], "deming-mcp");
    assert!(init_resp["result"]["capabilities"]["tools"].is_object());

    // Notification without id should produce no response.
    let notif = server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .await;
    assert!(notif.is_none());

    let list_resp = server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .await
        .expect("tools/list response");

    let tools = list_resp["result"]["tools"]
        .as_array()
        .expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();
    for expected in [
        "get_config",
        "set_config",
        "list_accounts",
        "add_offline_account",
        "submit_test_run",
        "get_run_status",
        "list_runs",
        "tail_instance_logs",
        "capture_screenshot",
        "abort_run",
        "apply_modpack",
        "update_mod_state",
        "delete_instance",
        "analyze_crash_log",
        "batch_update_mods",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn sandbox_rejects_parent_traversal_and_external_paths() {
    let base = std::env::temp_dir().join(format!("deming-sandbox-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();

    let ok = resolve_sandboxed_path(&base, std::path::Path::new("instances/c1/mods"))
        .expect("inside workspace");
    assert!(ok.ends_with(std::path::Path::new("instances/c1/mods")));

    assert!(resolve_sandboxed_path(&base, std::path::Path::new("../outside")).is_err());
    assert!(resolve_sandboxed_path(&base, std::path::Path::new("instances/../../outside")).is_err());
    assert!(resolve_sandboxed_path(&base, std::path::Path::new("C:/Windows/System32")).is_err());

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn crash_analyzer_strips_noise_and_extracts_implicated_mods() {
    let raw = r#"
[12:00:01] [main/ERROR]: Minecraft has crashed!
java.lang.RuntimeException: Mixin apply failed for broken-mod.jar
    at net.fabricmc.loader.impl.FabricLoaderImpl.load(FabricLoaderImpl.java:100)
    at java.base/java.lang.Thread.run(Thread.java:833)
    ... 42 more
Caused by: org.spongepowered.asm.mixin.throwables.MixinApplyError: From mod 'brokenmod' (Mod ID: 'brokenmod')
"#;
    let analysis = analyze_crash_text(raw);
    assert!(analysis
        .summary
        .as_deref()
        .unwrap_or("")
        .contains("RuntimeException"));
    assert!(analysis
        .implicated_mods
        .contains(&"broken-mod.jar".to_string()));
    assert!(analysis.implicated_mods.contains(&"brokenmod".to_string()));
    assert!(!analysis.key_lines.iter().any(|l| l.contains("java.base/")));
    assert!(!analysis.key_lines.iter().any(|l| l.contains("... 42 more")));
}

#[tokio::test]
async fn mcp_batch_update_mods_and_analyze_crash_log() {
    let (base, cfg_path) = temp_config_path();
    let ws_dir = base.join("workspace");
    let inst_dir = ws_dir.join("instances").join("c1");
    let mods_dir = inst_dir.join("mods");
    std::fs::create_dir_all(&mods_dir).unwrap();
    std::fs::write(mods_dir.join("alpha.jar"), b"jar").unwrap();
    std::fs::write(
        inst_dir.join("latest.log"),
        "[12:00:00] [main/ERROR]: java.lang.IllegalStateException: crash in alpha.jar\n",
    )
    .unwrap();

    let server = McpServer::new(&cfg_path);
    server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "set_config",
                "arguments": { "workspace_dir": ws_dir.to_string_lossy() }
            }
        }))
        .await
        .unwrap();

    // Dry-run batch update should not rename alpha.jar
    let dry = server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "batch_update_mods",
                "arguments": {
                    "instance_dir": "instances/c1",
                    "manifest_path": "instances/c1/deming.toml",
                    "dry_run": true,
                    "operations": [
                        { "action": "add", "slug": "fabric-api" },
                        { "action": "disable", "filename": "alpha.jar" }
                    ]
                }
            }
        }))
        .await
        .unwrap();
    assert_eq!(dry["result"]["isError"], false);
    assert!(mods_dir.join("alpha.jar").exists());

    // Live batch update disables alpha.jar and saves manifest
    let live = server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "batch_update_mods",
                "arguments": {
                    "instance_dir": "instances/c1",
                    "manifest_path": "instances/c1/deming.toml",
                    "dry_run": false,
                    "operations": [
                        { "action": "add", "slug": "fabric-api" },
                        { "action": "disable", "filename": "alpha.jar" }
                    ]
                }
            }
        }))
        .await
        .unwrap();
    assert_eq!(live["result"]["isError"], false);
    assert!(mods_dir.join("alpha.jar.disabled").exists());
    assert!(inst_dir.join("deming.toml").exists());

    // analyze_crash_log on sandboxed instance_dir
    let crash = server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "analyze_crash_log",
                "arguments": { "instance_dir": "instances/c1" }
            }
        }))
        .await
        .unwrap();
    assert_eq!(crash["result"]["isError"], false);
    assert!(crash["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("alpha.jar"));

    // Sandbox traversal attempt must fail
    let escape = server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "analyze_crash_log",
                "arguments": { "instance_dir": "../../outside" }
            }
        }))
        .await
        .unwrap();
    assert_eq!(escape["result"]["isError"], true);

    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn mcp_tools_call_config_account_and_list_runs() {
    let (base, cfg_path) = temp_config_path();
    let ws_dir = base.join("workspace");
    let server = McpServer::new(&cfg_path);

    // 1. set_config
    let set_resp = server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "set_config",
                "arguments": {
                    "workspace_dir": ws_dir.to_string_lossy(),
                    "max_total_ram_mb": 12288,
                    "max_concurrent_instances": 3,
                    "process_priority": "below_normal"
                }
            }
        }))
        .await
        .expect("set_config response");
    assert_eq!(set_resp["result"]["isError"], false);

    // 2. get_config
    let get_resp = server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "get_config",
                "arguments": {}
            }
        }))
        .await
        .expect("get_config response");
    assert_eq!(get_resp["result"]["isError"], false);
    let cfg_text = get_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("config text");
    let parsed_cfg: DemingConfig = serde_json::from_str(cfg_text).expect("parse returned config");
    assert_eq!(parsed_cfg.workspace_dir, ws_dir);
    assert_eq!(parsed_cfg.global_limits.max_total_ram_mb, 12288);
    assert_eq!(parsed_cfg.global_limits.max_concurrent_instances, 3);
    assert_eq!(
        parsed_cfg.global_limits.process_priority,
        ProcessPriority::BelowNormal
    );

    // 3. add_offline_account
    let add_acc_resp = server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": {
                "name": "add_offline_account",
                "arguments": { "username": "McpTester" }
            }
        }))
        .await
        .expect("add_offline_account response");
    assert_eq!(add_acc_resp["result"]["isError"], false);
    let acc_text = add_acc_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("account text");
    let acc_val: Value = serde_json::from_str(acc_text).expect("parse account");
    assert_eq!(acc_val["username"], "McpTester");

    // 4. list_runs (empty first, then with one seeded run)
    let list_empty = server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "id": 13,
            "method": "tools/call",
            "params": {
                "name": "list_runs",
                "arguments": {}
            }
        }))
        .await
        .expect("list_runs empty");
    let runs_empty: Vec<Value> = serde_json::from_str(
        list_empty["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(runs_empty.is_empty());

    let run_id = Uuid::new_v4();
    let writer = RunArchiveWriter::new(parsed_cfg.runs_dir(), run_id).expect("create run archive");
    let manifest = RunManifest {
        run_id,
        job_name: "mcp-smoke".to_string(),
        status: RunStatus::Passed,
        created_at: Utc::now(),
        started_at: Some(Utc::now()),
        finished_at: Some(Utc::now()),
        spec: TestJobSpec {
            name: "mcp-smoke".to_string(),
            instances: vec![],
            server: None,
            timeout_secs: 60,
        },
        instances: vec![],
        server_log_path: None,
        bundle_path: None,
    };
    writer.write_manifest(&manifest).expect("write manifest");

    let list_one = server
        .handle_request(json!({
            "jsonrpc": "2.0",
            "id": 14,
            "method": "tools/call",
            "params": {
                "name": "list_runs",
                "arguments": {}
            }
        }))
        .await
        .expect("list_runs one");
    let runs_one: Vec<Value> = serde_json::from_str(
        list_one["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(runs_one.len(), 1);
    assert_eq!(runs_one[0]["job_name"], "mcp-smoke");

    let _ = std::fs::remove_dir_all(base);
}
