use std::fs;
use std::path::PathBuf;

use chrono::{Duration as ChronoDuration, Utc};
use deming_core::{
    AccountRef, InstanceResult, InstanceSpec, InstanceState, ModLoader, RetentionPolicy,
    RunEvent, RunManifest, RunStatus, TestCompletionRule, TestJobSpec,
};
use uuid::Uuid;

use deming_archive::{
    apply_retention, LogSearchFilter, RunArchiveReader, RunArchiveWriter,
};

fn create_temp_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("deming-archive-test-{}-{}", prefix, Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample_manifest(run_id: Uuid, job_name: &str, status: RunStatus) -> RunManifest {
    let now = Utc::now();
    RunManifest {
        run_id,
        job_name: job_name.to_string(),
        status,
        created_at: now,
        started_at: Some(now),
        finished_at: Some(now),
        spec: TestJobSpec {
            name: job_name.to_string(),
            instances: vec![InstanceSpec {
                id: "client-1".to_string(),
                mc_version: "1.21.4".to_string(),
                loader: ModLoader::Fabric,
                loader_version: None,
                mods: vec![],
                account: AccountRef::Offline {
                    username: "Tester1".to_string(),
                },
                ram_mb: Some(2048),
                cpu_cores: Some(2),
                jvm_args: vec![],
                game_args: vec![],
                test_rule: TestCompletionRule::ExitCodeZero,
            }],
            server: None,
            timeout_secs: 60,
        },
        instances: vec![InstanceResult {
            instance_id: "client-1".to_string(),
            state: InstanceState::Passed,
            exit_code: Some(0),
            passed: true,
            reason: Some("Process exited with 0".to_string()),
            started_at: Some(now),
            finished_at: Some(now),
            log_path: Some(PathBuf::from("instances/client-1/latest.log")),
            screenshots: vec![PathBuf::from("instances/client-1/screenshots/finish.png")],
        }],
        server_log_path: Some(PathBuf::from("server/latest.log")),
        bundle_path: None,
    }
}

#[test]
fn test_writer_creates_structure_and_reader_loads_artifacts() {
    let temp_dir = create_temp_dir("writer-structure");
    let run_id = Uuid::new_v4();
    let mut writer = RunArchiveWriter::new(&temp_dir, run_id).unwrap();

    let manifest = sample_manifest(run_id, "SmokeTest", RunStatus::Passed);
    writer.write_manifest(&manifest).unwrap();

    // Append JSON lines events
    let now = Utc::now();
    writer
        .append_event(&RunEvent::RunStarted {
            run_id,
            job_name: "SmokeTest".to_string(),
            timestamp: now,
        })
        .unwrap();

    writer
        .append_event(&RunEvent::InstanceStateChanged {
            run_id,
            instance_id: "client-1".to_string(),
            state: InstanceState::Running,
            message: Some("Launched client".to_string()),
            timestamp: now,
        })
        .unwrap();

    writer
        .append_event(&RunEvent::RunFinished {
            run_id,
            status: RunStatus::Passed,
            timestamp: now,
        })
        .unwrap();

    // Write server & instance logs
    writer.append_server_line("[INFO] Server starting...").unwrap();
    writer.append_server_line("[INFO] Done (2.1s)! For help, type \"help\"").unwrap();

    writer.append_instance_stdout("client-1", "Setting up GL context").unwrap();
    writer.append_instance_stderr("client-1", "OpenGL debug message").unwrap();
    writer.append_instance_latest_log("client-1", "[Client thread/INFO]: Connecting to localhost").unwrap();
    writer.append_instance_latest_log("client-1", "[Client thread/INFO]: Joined game").unwrap();

    // Save screenshot and crash report
    let dummy_img = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]; // PNG header
    let shot_path = writer.save_screenshot("client-1", "finish.png", &dummy_img).unwrap();
    assert!(shot_path.exists());

    let report_path = writer
        .save_crash_report("client-1", "crash.txt", "No crash detected, all tests passed.")
        .unwrap();
    assert!(report_path.exists());

    // Verify Reader
    let reader = RunArchiveReader::new(&temp_dir);
    let runs = reader.list_runs().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, Some(run_id));
    assert_eq!(runs[0].job_name.as_deref(), Some("SmokeTest"));
    assert_eq!(runs[0].status, Some(RunStatus::Passed));
    assert!(!runs[0].is_compressed);

    // Load manifest
    let loaded_manifest = reader.load_manifest(writer.run_dir()).unwrap();
    assert_eq!(loaded_manifest.run_id, run_id);
    assert_eq!(loaded_manifest.instances.len(), 1);

    // Read events
    let events = reader.read_events(writer.run_dir()).unwrap();
    assert_eq!(events.len(), 3);
    match &events[0] {
        RunEvent::RunStarted { job_name, .. } => assert_eq!(job_name, "SmokeTest"),
        _ => panic!("Expected RunStarted"),
    }

    // Tail logs
    let server_lines = reader.tail_logs(writer.run_dir(), "server", None, 1).unwrap();
    assert_eq!(server_lines.len(), 1);
    assert!(server_lines[0].contains("Done"));

    let client_lines = reader.tail_logs(writer.run_dir(), "client-1", Some("latest.log"), 2).unwrap();
    assert_eq!(client_lines.len(), 2);
    assert!(client_lines[1].contains("Joined game"));

    // List screenshots
    let screenshots = reader.list_screenshots(writer.run_dir(), None).unwrap();
    assert_eq!(screenshots.len(), 1);
    assert_eq!(screenshots[0].instance_id, "client-1");
    assert_eq!(screenshots[0].filename, "finish.png");
    assert_eq!(screenshots[0].size_bytes, dummy_img.len() as u64);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_log_searching_with_regex_and_instance_filters() {
    let temp_dir = create_temp_dir("log-search");
    let run_id = Uuid::new_v4();
    let mut writer = RunArchiveWriter::new(&temp_dir, run_id).unwrap();

    writer.append_server_line("[INFO] Server tick time 15ms").unwrap();
    writer.append_server_line("[WARN] Can't keep up! Is the server overloaded?").unwrap();
    writer.append_server_line("[ERROR] Server connection timed out").unwrap();

    writer.append_instance_latest_log("client-alpha", "[12:00:00] [INFO] Loading assets").unwrap();
    writer.append_instance_latest_log("client-alpha", "[12:00:01] [ERROR] OpenGL shader compilation failed").unwrap();

    writer.append_instance_latest_log("client-beta", "[12:00:00] [INFO] Connected to dedicated server").unwrap();
    writer.append_instance_latest_log("client-beta", "[12:00:02] [INFO] Player achieved victory").unwrap();

    let reader = RunArchiveReader::new(&temp_dir);

    // 1. Search for ERROR across all instances and server
    let filter = LogSearchFilter {
        regex: Some("ERROR".to_string()),
        include_server: true,
        ..Default::default()
    };
    let matches = reader.search_logs(writer.run_dir(), &filter).unwrap();
    assert_eq!(matches.len(), 2);
    let sources: Vec<String> = matches.iter().map(|m| m.source.clone()).collect();
    assert!(sources.contains(&"server".to_string()));
    assert!(sources.contains(&"client-alpha".to_string()));

    // 2. Search for ERROR on client-alpha only
    let filter_alpha = LogSearchFilter {
        instance_id: Some("client-alpha".to_string()),
        regex: Some("ERROR".to_string()),
        ..Default::default()
    };
    let alpha_matches = reader.search_logs(writer.run_dir(), &filter_alpha).unwrap();
    assert_eq!(alpha_matches.len(), 1);
    assert_eq!(alpha_matches[0].source, "client-alpha");
    assert_eq!(alpha_matches[0].line_number, 2);

    // 3. Search case-insensitive regex
    let filter_case = LogSearchFilter {
        regex: Some("can't keep up".to_string()),
        include_server: true,
        case_sensitive: false,
        ..Default::default()
    };
    let case_matches = reader.search_logs(writer.run_dir(), &filter_case).unwrap();
    assert_eq!(case_matches.len(), 1);
    assert_eq!(case_matches[0].source, "server");

    // 4. Test search limit
    let filter_limit = LogSearchFilter {
        regex: Some("INFO".to_string()),
        include_server: true,
        limit: Some(2),
        ..Default::default()
    };
    let limit_matches = reader.search_logs(writer.run_dir(), &filter_limit).unwrap();
    assert_eq!(limit_matches.len(), 2);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_zip_compression_and_reading_from_zip() {
    let temp_dir = create_temp_dir("zip-test");
    let run_id = Uuid::new_v4();
    let mut writer = RunArchiveWriter::new(&temp_dir, run_id).unwrap();

    let manifest = sample_manifest(run_id, "ZipJob", RunStatus::Passed);
    writer.write_manifest(&manifest).unwrap();

    writer
        .append_event(&RunEvent::RunStarted {
            run_id,
            job_name: "ZipJob".to_string(),
            timestamp: Utc::now(),
        })
        .unwrap();

    writer.append_server_line("[INFO] Server ready inside zip").unwrap();
    writer.append_instance_latest_log("client-1", "[INFO] Client connected inside zip").unwrap();
    writer.save_screenshot("client-1", "zip_shot.png", b"ZIP_SCREENSHOT_BYTES").unwrap();

    let run_folder = writer.run_dir().to_path_buf();
    assert!(run_folder.is_dir());

    // Compress folder to zip
    let zip_path = deming_archive::retention::compress_run_dir(&run_folder).unwrap();
    assert!(zip_path.exists());
    assert!(!run_folder.exists()); // Original uncompressed directory must be deleted

    // Now inspect via RunArchiveReader directly from the .zip file!
    let reader = RunArchiveReader::new(&temp_dir);
    let runs = reader.list_runs().unwrap();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].is_compressed);
    assert_eq!(runs[0].id, Some(run_id));
    assert_eq!(runs[0].job_name.as_deref(), Some("ZipJob"));

    // Read manifest from zip
    let loaded_manifest = reader.load_manifest(&zip_path).unwrap();
    assert_eq!(loaded_manifest.run_id, run_id);
    assert_eq!(loaded_manifest.job_name, "ZipJob");

    // Read events from zip
    let events = reader.read_events(&zip_path).unwrap();
    assert_eq!(events.len(), 1);

    // Search logs in zip
    let filter = LogSearchFilter {
        regex: Some("inside zip".to_string()),
        include_server: true,
        ..Default::default()
    };
    let matches = reader.search_logs(&zip_path, &filter).unwrap();
    assert_eq!(matches.len(), 2);

    // Tail logs from zip
    let tail = reader.tail_logs(&zip_path, "server", None, 5).unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0], "[INFO] Server ready inside zip");

    // List screenshots from zip
    let shots = reader.list_screenshots(&zip_path, None).unwrap();
    assert_eq!(shots.len(), 1);
    assert_eq!(shots[0].instance_id, "client-1");
    assert_eq!(shots[0].filename, "zip_shot.png");
    assert_eq!(shots[0].size_bytes, b"ZIP_SCREENSHOT_BYTES".len() as u64);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_retention_policy_compression_and_pruning() {
    let temp_dir = create_temp_dir("retention-policy");
    let base_time = Utc::now();

    // Create 6 runs spaced out in time:
    // run 0 (oldest: base - 50m)
    // run 1 (base - 40m)
    // run 2 (base - 30m)
    // run 3 (base - 20m)
    // run 4 (base - 10m)
    // run 5 (newest: base)
    let mut run_dirs = Vec::new();
    for i in 0..6 {
        let ts = base_time - ChronoDuration::minutes((5 - i) * 10);
        let id = Uuid::new_v4();
        let writer = RunArchiveWriter::with_timestamp(&temp_dir, id, ts).unwrap();
        let manifest = sample_manifest(id, &format!("Job-{}", i), RunStatus::Passed);
        writer.write_manifest(&manifest).unwrap();
        run_dirs.push(writer.run_dir().to_path_buf());
    }

    // Verify 6 uncompressed directories exist
    let reader = RunArchiveReader::new(&temp_dir);
    let initial_runs = reader.list_runs().unwrap();
    assert_eq!(initial_runs.len(), 6);
    assert!(initial_runs.iter().all(|r| !r.is_compressed));

    // Policy: max_runs = 4, compress_after_runs = 2, auto_prune = true
    // When sorted descending (newest to oldest):
    // index 0: run 5 (newest) -> keep uncompressed
    // index 1: run 4 -> keep uncompressed
    // index 2: run 3 -> compress to zip
    // index 3: run 2 -> compress to zip
    // index 4: run 1 -> prune
    // index 5: run 0 -> prune
    let policy = RetentionPolicy {
        max_runs: 4,
        compress_after_runs: 2,
        auto_prune: true,
    };

    let report = apply_retention(&temp_dir, &policy).unwrap();
    assert_eq!(report.pruned.len(), 2, "Expected 2 oldest runs pruned");
    assert_eq!(report.compressed.len(), 2, "Expected 2 older runs compressed");

    // Verify current state in runs_dir
    let runs_after = reader.list_runs().unwrap();
    assert_eq!(runs_after.len(), 4, "Total retained runs should be 4");

    // The two newest runs should be uncompressed
    assert!(!runs_after[0].is_compressed);
    assert!(!runs_after[1].is_compressed);

    // The next two runs should be compressed .zip files
    assert!(runs_after[2].is_compressed);
    assert!(runs_after[3].is_compressed);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_retention_policy_respects_auto_prune_false() {
    let temp_dir = create_temp_dir("retention-no-prune");
    let base_time = Utc::now();

    // Create 4 runs
    for i in 0..4 {
        let ts = base_time - ChronoDuration::minutes((3 - i) * 10);
        let id = Uuid::new_v4();
        let writer = RunArchiveWriter::with_timestamp(&temp_dir, id, ts).unwrap();
        let manifest = sample_manifest(id, &format!("Job-{}", i), RunStatus::Passed);
        writer.write_manifest(&manifest).unwrap();
    }

    let policy = RetentionPolicy {
        max_runs: 2,
        compress_after_runs: 1,
        auto_prune: false, // Do not delete
    };

    let report = apply_retention(&temp_dir, &policy).unwrap();
    assert_eq!(report.pruned.len(), 0);
    // Index 1, 2, 3 should be compressed (3 runs)
    assert_eq!(report.compressed.len(), 3);

    let reader = RunArchiveReader::new(&temp_dir);
    let runs_after = reader.list_runs().unwrap();
    assert_eq!(runs_after.len(), 4);
    assert!(!runs_after[0].is_compressed);
    assert!(runs_after[1].is_compressed);
    assert!(runs_after[2].is_compressed);
    assert!(runs_after[3].is_compressed);

    let _ = fs::remove_dir_all(&temp_dir);
}
