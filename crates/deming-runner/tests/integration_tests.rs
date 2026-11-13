use std::collections::BTreeMap;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

use deming_core::TestCompletionRule;
#[cfg(windows)]
use deming_core::{AccountRef, InstanceSpec, ModLoader, ProcessPriority, RunEvent, ServerSpec};
use deming_runner::client::{build_connect_args, supports_quick_play};
#[cfg(windows)]
use deming_runner::client::{run_client, ClientLaunchConfig};
use deming_runner::evaluator::{evaluate_rule, EvaluationContext};
use deming_runner::scheduler::{AbortHandle, ResourceScheduler};
use deming_runner::screenshot::index_screenshots;
use deming_runner::server::{pick_free_port, write_eula, write_server_properties};
#[cfg(windows)]
use deming_runner::server::{ServerLaunchConfig, ServerProcess};
#[cfg(windows)]
use deming_runner::win_job::{set_cpu_affinity_and_priority, JobHandle};
#[cfg(windows)]
use tokio::sync::mpsc;
use uuid::Uuid;

fn temp_test_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("deming-runner-{prefix}-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn eval_exit_code_zero_rules() {
    let rule = TestCompletionRule::ExitCodeZero;

    let pass = evaluate_rule(
        &rule,
        &EvaluationContext {
            exit_code: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(pass.passed, "expected pass: {}", pass.reason);

    let fail_nonzero = evaluate_rule(
        &rule,
        &EvaluationContext {
            exit_code: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!fail_nonzero.passed);

    let fail_timeout = evaluate_rule(
        &rule,
        &EvaluationContext {
            exit_code: Some(0),
            timed_out: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!fail_timeout.passed);

    let fail_abort = evaluate_rule(
        &rule,
        &EvaluationContext {
            exit_code: Some(0),
            aborted: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!fail_abort.passed);
}

#[test]
fn eval_log_pattern_rules() {
    let rule = TestCompletionRule::LogPattern {
        success_regex: r"All \d+ GameTests passed".to_string(),
        failure_regex: Some(r"FATAL|AssertionError".to_string()),
    };

    let pass = evaluate_rule(
        &rule,
        &EvaluationContext {
            logs: vec![
                "[main/INFO]: Loading world".to_string(),
                "[Server thread/INFO]: All 12 GameTests passed".to_string(),
            ],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(pass.passed, "expected pass: {}", pass.reason);

    let fail_on_failure_regex = evaluate_rule(
        &rule,
        &EvaluationContext {
            logs: vec![
                "[main/ERROR]: AssertionError: expected 1 got 2".to_string(),
                "[Server thread/INFO]: All 12 GameTests passed".to_string(),
            ],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!fail_on_failure_regex.passed);
    assert!(fail_on_failure_regex.reason.contains("failure_regex"));

    let fail_missing_success = evaluate_rule(
        &rule,
        &EvaluationContext {
            logs: vec!["[main/INFO]: Still booting".to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!fail_missing_success.passed);
}

#[test]
fn eval_gametest_report_xml_and_json_and_composite() {
    let dir = temp_test_dir("gametest-eval");
    let reports_dir = dir.join("build").join("test-results");
    fs::create_dir_all(&reports_dir).unwrap();

    fs::write(
        reports_dir.join("TEST-gametest.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="ExampleGameTests" tests="4" failures="0" errors="0" skipped="0">
  <testcase name="testBlockPlace" classname="ExampleGameTests" time="0.12"/>
</testsuite>"#,
    )
    .unwrap();

    fs::write(
        reports_dir.join("summary.json"),
        r#"{"passed": true, "tests": 4, "failures": 0}"#,
    )
    .unwrap();

    let xml_rule = TestCompletionRule::GameTestReport {
        report_path_glob: "**/TEST-*.xml".to_string(),
    };
    let res = evaluate_rule(
        &xml_rule,
        &EvaluationContext {
            instance_dir: dir.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(res.passed, "expected XML report pass: {}", res.reason);

    let json_rule = TestCompletionRule::GameTestReport {
        report_path_glob: "build/test-results/*.json".to_string(),
    };
    let res_json = evaluate_rule(
        &json_rule,
        &EvaluationContext {
            instance_dir: dir.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(res_json.passed, "expected JSON report pass: {}", res_json.reason);

    let composite = TestCompletionRule::Composite(vec![
        TestCompletionRule::ExitCodeZero,
        TestCompletionRule::LogPattern {
            success_regex: "Tests complete".to_string(),
            failure_regex: Some("CRASH".to_string()),
        },
        xml_rule.clone(),
    ]);
    let comp_pass = evaluate_rule(
        &composite,
        &EvaluationContext {
            exit_code: Some(0),
            logs: vec!["Tests complete".to_string()],
            instance_dir: dir.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(comp_pass.passed, "expected composite pass: {}", comp_pass.reason);

    // Add a failing XML report and verify composite fails.
    fs::write(
        reports_dir.join("TEST-failed.xml"),
        r#"<testsuite name="FailingSuite" tests="2" failures="1" errors="0">
  <testcase name="broken"><failure message="boom"/></testcase>
</testsuite>"#,
    )
    .unwrap();

    let comp_fail = evaluate_rule(
        &composite,
        &EvaluationContext {
            exit_code: Some(0),
            logs: vec!["Tests complete".to_string()],
            instance_dir: dir.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!comp_fail.passed);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ephemeral_port_and_server_properties_generation() {
    let port = pick_free_port().unwrap();
    assert!(port > 0);
    // Verify the returned port can actually be bound.
    let bound = TcpListener::bind(("127.0.0.1", port)).unwrap();
    assert_eq!(bound.local_addr().unwrap().port(), port);
    drop(bound);

    let dir = temp_test_dir("server-props");
    let eula_path = write_eula(&dir, true).unwrap();
    assert_eq!(
        fs::read_to_string(&eula_path).unwrap(),
        "# Generated by Deming\neula=true\n"
    );

    let mut custom = BTreeMap::new();
    custom.insert("gamemode".to_string(), "creative".to_string());
    custom.insert("difficulty".to_string(), "peaceful".to_string());
    custom.insert("max-players".to_string(), "8".to_string());

    let props_path = write_server_properties(&dir, port, false, &custom).unwrap();
    let content = fs::read_to_string(&props_path).unwrap();

    assert!(content.contains(&format!("server-port={port}")));
    assert!(content.contains("online-mode=false"));
    assert!(content.contains("gamemode=creative"));
    assert!(content.contains("difficulty=peaceful"));
    assert!(content.contains("max-players=8"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn client_auto_connect_flags_by_version() {
    assert!(supports_quick_play("1.20"));
    assert!(supports_quick_play("1.20.1"));
    assert!(supports_quick_play("1.21.4"));
    assert!(supports_quick_play("1.20-pre1"));
    assert!(supports_quick_play("23w14a"));
    assert!(supports_quick_play("26.1"));
    assert!(supports_quick_play("26.1.1"));
    assert!(!supports_quick_play("1.19.4"));
    assert!(!supports_quick_play("1.18.2"));
    assert!(!supports_quick_play("1.12.2"));
    assert!(!supports_quick_play("23w13a"));

    assert_eq!(
        build_connect_args("1.21.4", "127.0.0.1", 25565),
        vec!["--quickPlayMultiplayer", "127.0.0.1:25565"]
    );
    assert_eq!(
        build_connect_args("26.1.1", "127.0.0.1", 52082),
        vec!["--quickPlayMultiplayer", "127.0.0.1:52082"]
    );
    assert_eq!(
        build_connect_args("1.19.4", "127.0.0.1", 25565),
        vec!["--server", "127.0.0.1", "--port", "25565"]
    );
}

#[test]
fn screenshot_indexing_filters_and_sorts_pngs() {
    let dir = temp_test_dir("screenshots");
    let shots = dir.join("screenshots");
    fs::create_dir_all(&shots).unwrap();

    fs::write(shots.join("002_end.PNG"), b"png").unwrap();
    fs::write(shots.join("001_start.png"), b"png").unwrap();
    fs::write(shots.join("notes.txt"), b"ignore").unwrap();

    let indexed = index_screenshots(&dir).unwrap();
    let names: Vec<String> = indexed
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["001_start.png", "002_end.PNG"]);

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn scheduler_enforces_ram_and_concurrency_and_abort() {
    let scheduler = ResourceScheduler::new(4096, 2);

    let p1 = scheduler.try_acquire(2048).unwrap().expect("p1");
    let p2 = scheduler.try_acquire(2048).unwrap().expect("p2");
    assert_eq!(scheduler.active_instances(), 2);
    assert_eq!(scheduler.used_ram_mb(), 4096);
    assert_eq!(scheduler.available_ram_mb(), 0);

    // Budget exhausted -> try_acquire returns None.
    assert!(scheduler.try_acquire(1024).unwrap().is_none());

    // Dropping p1 frees both a concurrency slot and 2048 MiB.
    drop(p1);
    assert_eq!(scheduler.active_instances(), 1);
    assert_eq!(scheduler.used_ram_mb(), 2048);

    let p3 = scheduler.try_acquire(1024).unwrap().expect("p3");
    assert_eq!(scheduler.used_ram_mb(), 3072);

    // Requesting more than max_total_ram_mb errors immediately instead of hanging.
    let err = scheduler.acquire(8192, None).await.unwrap_err();
    assert!(err.to_string().contains("exceeds global max_total_ram_mb"));

    // Abort wakes a waiting acquire() call.
    let abort = AbortHandle::new();
    let sched_clone = scheduler.clone();
    let abort_clone = abort.clone();
    let waiter = tokio::spawn(async move { sched_clone.acquire(2048, Some(&abort_clone)).await });

    tokio::time::sleep(Duration::from_millis(50)).await;
    abort.abort().unwrap();

    let wait_res = waiter.await.unwrap();
    assert!(wait_res.is_err());

    drop(p2);
    drop(p3);
    assert_eq!(scheduler.active_instances(), 0);
    assert_eq!(scheduler.used_ram_mb(), 0);
}

#[cfg(windows)]
#[test]
fn windows_job_object_spawn_and_terminate_all_kills_immediately() {
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, Stdio};

    let job = JobHandle::new().expect("create JobHandle");

    // Spawn a long-lived Windows process tree (`cmd /c ping -n 30 127.0.0.1 >nul`).
    let mut child = Command::new("cmd.exe")
        .args(["/C", "ping", "-n", "30", "127.0.0.1", ">nul"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cmd.exe");

    job.assign_std_child(&child)
        .expect("assign child to JobHandle");
    set_cpu_affinity_and_priority(child.as_raw_handle(), Some(1), ProcessPriority::BelowNormal)
        .expect("set affinity and priority");

    // Give it a brief moment to ensure it's running and not already exited.
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        child.try_wait().unwrap().is_none(),
        "child should still be running before terminate_all()"
    );

    let start = Instant::now();
    job.terminate_all().expect("terminate_all");
    let status = child.wait().expect("wait after terminate_all");
    let elapsed = start.elapsed();

    assert!(job.is_terminated());
    assert!(!status.success());
    assert!(
        elapsed < Duration::from_secs(2),
        "expected immediate kill via TerminateJobObject, took {elapsed:?}"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn server_lifecycle_ready_pattern_and_client_timeout_on_windows() {
    let server_dir = temp_test_dir("win-server-life");
    let abort = AbortHandle::new();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let server_bat = server_dir.join("mock_server.bat");
    fs::write(
        &server_bat,
        "@echo off\r\necho [Server thread/INFO]: Done (0.42s)! For help, type \"help\"\r\nping -n 15 127.0.0.1 >nul\r\n",
    )
    .unwrap();

    let server_cfg = ServerLaunchConfig {
        run_id: Uuid::new_v4(),
        java_exe: PathBuf::from("cmd.exe"),
        server_dir: server_dir.clone(),
        server_jar: None,
        jvm_args: vec![],
        program_args: vec!["/C".to_string(), server_bat.to_string_lossy().into_owned()],
        spec: ServerSpec {
            mc_version: "1.21.4".to_string(),
            loader: ModLoader::Vanilla,
            loader_version: None,
            mods: vec![],
            online_mode: false,
            port: None,
            server_properties: BTreeMap::new(),
            ready_pattern: r#"Done \([0-9.]+s\)! For help, type "help""#.to_string(),
            auto_connect_clients: true,
            ram_mb: Some(1024),
        },
        ram_mb: 1024,
        cpu_cores: Some(1),
        priority: ProcessPriority::BelowNormal,
        startup_timeout_secs: 5,
    };

    let mut srv = ServerProcess::start(server_cfg, &abort, Some(tx.clone()))
        .await
        .expect("server should reach ready_pattern");

    assert!(srv.port() > 0);
    assert!(server_dir.join("eula.txt").exists());
    assert!(server_dir.join("server.properties").exists());

    // Confirm ServerReady event was emitted.
    let mut saw_ready = false;
    while let Ok(ev) = rx.try_recv() {
        if let RunEvent::ServerReady { port, .. } = ev {
            assert_eq!(port, srv.port());
            saw_ready = true;
        }
    }
    assert!(saw_ready, "expected RunEvent::ServerReady");

    // Kill the server cleanly via Job Object.
    srv.kill().await.expect("kill server");

    // Now test client timeout enforcement.
    let client_dir = temp_test_dir("win-client-timeout");
    let client_bat = client_dir.join("mock_client.bat");
    fs::write(
        &client_bat,
        "@echo off\r\necho booting client\r\nping -n 20 127.0.0.1 >nul\r\n",
    )
    .unwrap();

    let client_cfg = ClientLaunchConfig {
        run_id: Uuid::new_v4(),
        java_exe: PathBuf::from("cmd.exe"),
        instance_dir: client_dir.clone(),
        spec: InstanceSpec {
            id: "client-timeout-test".to_string(),
            mc_version: "1.21.4".to_string(),
            loader: ModLoader::Vanilla,
            loader_version: None,
            mods: vec![],
            account: AccountRef::Offline {
                username: "Tester".to_string(),
            },
            ram_mb: Some(1024),
            cpu_cores: Some(1),
            jvm_args: vec![],
            game_args: vec![],
            test_rule: TestCompletionRule::ExitCodeZero,
        },
        main_class: None,
        classpath: vec![],
        natives_dir: None,
        assets_dir: None,
        asset_index: None,
        access_token: None,
        ram_mb: 1024,
        cpu_cores: Some(1),
        priority: ProcessPriority::BelowNormal,
        connect_server: None,
        timeout_secs: 1,
        extra_jvm_args: vec![],
        extra_game_args: vec!["/C".to_string(), client_bat.to_string_lossy().into_owned()],
        on_pid: None,
    };

    let outcome = run_client(client_cfg, &abort, Some(tx))
        .await
        .expect("run_client should return outcome on timeout");
    assert!(outcome.timed_out);
    assert!(!outcome.aborted);
    assert!(outcome.combined_logs.iter().any(|l| l.contains("booting client")));

    let _ = fs::remove_dir_all(&server_dir);
    let _ = fs::remove_dir_all(&client_dir);
}

#[cfg(windows)]
#[tokio::test]
async fn client_on_pid_and_early_pass_log_pattern_terminates_before_timeout() {
    use std::sync::{Arc, Mutex};

    let client_dir = temp_test_dir("win-client-early");
    let client_bat = client_dir.join("early_pass.bat");
    fs::write(
        &client_bat,
        "@echo off\r\necho Sound engine started\r\nping -n 30 127.0.0.1 >nul\r\n",
    )
    .unwrap();

    let abort = AbortHandle::new();
    let seen_pid = Arc::new(Mutex::new(None::<u32>));
    let seen_pid_cb = Arc::clone(&seen_pid);

    let rule = TestCompletionRule::LogPattern {
        success_regex: "Sound engine started".to_string(),
        failure_regex: Some("FATAL".to_string()),
    };

    let client_cfg = ClientLaunchConfig {
        run_id: Uuid::new_v4(),
        java_exe: PathBuf::from("cmd.exe"),
        instance_dir: client_dir.clone(),
        spec: InstanceSpec {
            id: "client-early-pass".to_string(),
            mc_version: "26.1.1".to_string(),
            loader: ModLoader::Vanilla,
            loader_version: None,
            mods: vec![],
            account: AccountRef::Offline {
                username: "Tester".to_string(),
            },
            ram_mb: Some(1024),
            cpu_cores: Some(1),
            jvm_args: vec![],
            game_args: vec![],
            test_rule: rule.clone(),
        },
        main_class: None,
        classpath: vec![],
        natives_dir: None,
        assets_dir: None,
        asset_index: None,
        access_token: None,
        ram_mb: 1024,
        cpu_cores: Some(1),
        priority: ProcessPriority::BelowNormal,
        connect_server: None,
        timeout_secs: 20,
        extra_jvm_args: vec![],
        extra_game_args: vec!["/C".to_string(), client_bat.to_string_lossy().into_owned()],
        on_pid: Some(Arc::new(move |p| {
            *seen_pid_cb.lock().unwrap() = Some(p);
        })),
    };

    let start = Instant::now();
    let outcome = run_client(client_cfg, &abort, None)
        .await
        .expect("run_client should finish cleanly on early pass");
    let elapsed = start.elapsed();

    assert!(!outcome.timed_out);
    assert!(!outcome.aborted);
    assert_eq!(*seen_pid.lock().unwrap(), outcome.pid);
    assert!(outcome.pid.is_some());
    assert!(
        elapsed < Duration::from_secs(5),
        "expected early termination well before 20s timeout, took {elapsed:?}"
    );

    let eval = evaluate_rule(
        &rule,
        &EvaluationContext {
            exit_code: outcome.exit_code,
            timed_out: outcome.timed_out,
            aborted: outcome.aborted,
            logs: outcome.combined_logs.clone(),
            instance_dir: client_dir.clone(),
        },
    )
    .unwrap();
    assert!(eval.passed, "expected Passed: true, got: {}", eval.reason);

    let _ = fs::remove_dir_all(&client_dir);
}

#[cfg(windows)]
#[test]
fn window_capture_pid_to_png_writes_valid_nonzero_png() {
    use std::mem::zeroed;
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread::JoinHandle;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, UpdateWindow, PAINTSTRUCT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW,
        RegisterClassW, ShowWindow, TranslateMessage, MSG, PM_REMOVE, SW_SHOWNOACTIVATE, WM_PAINT,
        WNDCLASSW, WS_EX_NOACTIVATE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    unsafe extern "system" fn test_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_PAINT {
            let mut ps: PAINTSTRUCT = zeroed();
            let hdc = BeginPaint(hwnd, &raw mut ps);
            // Solid bright orange/blue BGR fill so captured pixels are guaranteed non-zero RGB.
            let brush = CreateSolidBrush(0x00FF_8822);
            FillRect(hdc, &raw const ps.rcPaint, brush);
            DeleteObject(brush);
            EndPaint(hwnd, &raw const ps);
            return 0;
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    struct WindowGuard {
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl Drop for WindowGuard {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let (tx, rx) = mpsc::channel::<isize>();

    let handle = std::thread::spawn(move || {
        let class_name: Vec<u16> = "DemingPngCaptureTestWnd\0".encode_utf16().collect();
        let title: Vec<u16> = "Deming Capture Test\0".encode_utf16().collect();

        unsafe {
            let mut wc: WNDCLASSW = zeroed();
            wc.lpfnWndProc = Some(test_wnd_proc);
            wc.lpszClassName = class_name.as_ptr();
            RegisterClassW(&raw const wc);

            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                120,
                120,
                320,
                240,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            );
            assert!(!hwnd.is_null(), "CreateWindowExW failed");

            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            UpdateWindow(hwnd);
            let _ = tx.send(hwnd as isize);

            let mut msg: MSG = zeroed();
            while !stop_thread.load(Ordering::Acquire) {
                while PeekMessageW(&raw mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&raw const msg);
                    DispatchMessageW(&raw const msg);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            DestroyWindow(hwnd);
        }
    });

    let _guard = WindowGuard {
        stop,
        handle: Some(handle),
    };

    let hwnd_raw = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("test window should be created");
    std::thread::sleep(Duration::from_millis(60));

    let pid = std::process::id();
    let windows = deming_runner::screenshot::find_windows_for_pid(pid);
    assert!(
        windows.contains(&hwnd_raw),
        "expected test window {hwnd_raw} in find_windows_for_pid({pid})"
    );

    let dir = temp_test_dir("win-png-capture");
    let png_path = dir.join("frame.png");
    deming_runner::screenshot::capture_pid_to_png(pid, &png_path)
        .expect("capture_pid_to_png should succeed");
    assert!(png_path.exists());

    let img = image::open(&png_path)
        .expect("captured PNG should decode")
        .to_rgba8();
    assert!(img.width() > 0 && img.height() > 0);
    assert!(
        img.as_raw()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|px| px[0] > 0 || px[1] > 0 || px[2] > 0),
        "expected captured PNG to contain non-zero RGB pixels"
    );

    let _ = fs::remove_dir_all(&dir);
}
