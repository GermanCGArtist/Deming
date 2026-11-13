//! Minecraft client JVM launch, auto-connect flags, stdout/stderr streaming, and timeout enforcement.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use deming_core::{AccountRef, DemingError, InstanceSpec, InstanceState, ProcessPriority, Result, RunEvent};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::scheduler::AbortHandle;
use crate::win_job::{configure_tokio_child, JobHandle};
use crate::{absolutize_executable, absolutize_path};

/// Determines whether the given Minecraft version supports `--quickPlayMultiplayer` (`>= 1.20`).
pub fn supports_quick_play(mc_version: &str) -> bool {
    let v = mc_version.trim();
    if v.is_empty() {
        return false;
    }

    // Handle Mojang snapshot format like "23w14a" (1.20 snapshot week 14 of 2023) or "24w01a".
    if let Some((year_str, rest)) = v.split_once('w') {
        if let Ok(year) = year_str.parse::<u32>() {
            let week_digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(week) = week_digits.parse::<u32>() {
                return year > 23 || (year == 23 && week >= 14);
            }
        }
    }

    // Strip pre-release / rc / qualifier suffixes (e.g. "1.20-pre1", "1.21.4-rc1").
    let core = v.split(['-', '_', '+']).next().unwrap_or(v);
    let mut parts = core.split('.');
    let major = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    // Mojang switched to year-based major versions starting with 26.x (all >= 1.20).
    major > 1 || (major == 1 && minor >= 20)
}

/// Builds the auto-connect game arguments for connecting to `host:port` based on `mc_version`.
pub fn build_connect_args(mc_version: &str, host: &str, port: u16) -> Vec<String> {
    if supports_quick_play(mc_version) {
        vec![
            "--quickPlayMultiplayer".to_string(),
            format!("{host}:{port}"),
        ]
    } else {
        vec![
            "--server".to_string(),
            host.to_string(),
            "--port".to_string(),
            port.to_string(),
        ]
    }
}

/// Configuration for launching a single Minecraft client instance.
#[derive(Debug, Clone)]
pub struct ClientLaunchConfig {
    /// Run ID for emitted [`RunEvent`]s.
    pub run_id: Uuid,
    /// Path to the `java` executable (or custom executable when testing).
    pub java_exe: PathBuf,
    /// Instance working directory (`--gameDir`).
    pub instance_dir: PathBuf,
    /// Instance specification.
    pub spec: InstanceSpec,
    /// Main class to invoke (e.g., `net.minecraft.client.main.Main`).
    /// When `None`, `classpath` / `jar` is omitted unless specified, allowing direct command testing.
    pub main_class: Option<String>,
    /// Classpath entries joined with the OS path separator (`;` on Windows, `:` elsewhere).
    pub classpath: Vec<PathBuf>,
    /// Optional natives directory (`-Djava.library.path=...`).
    pub natives_dir: Option<PathBuf>,
    /// Optional assets directory (`--assetsDir`).
    pub assets_dir: Option<PathBuf>,
    /// Optional asset index name (`--assetIndex`).
    pub asset_index: Option<String>,
    /// Optional access token for authenticated Microsoft launches (`--accessToken`).
    pub access_token: Option<String>,
    /// Resolved RAM budget in MiB (`-Xmx{ram_mb}M`).
    pub ram_mb: u64,
    /// Resolved CPU core affinity limit.
    pub cpu_cores: Option<usize>,
    /// OS scheduling priority.
    pub priority: ProcessPriority,
    /// Optional `(host, port)` to auto-connect to on startup.
    pub connect_server: Option<(String, u16)>,
    /// Execution timeout in seconds (`0` disables the timeout).
    pub timeout_secs: u64,
    /// Additional JVM flags appended after `-Xmx` and `spec.jvm_args`.
    pub extra_jvm_args: Vec<String>,
    /// Additional game arguments appended after `spec.game_args`.
    pub extra_game_args: Vec<String>,
    /// When `true` on Windows, watches for the spawned client window and cloaks/hides it
    /// from the desktop and taskbar (`DWMWA_CLOAK` + `WS_EX_TOOLWINDOW`) so live frames
    /// can be captured invisibly in the background.
    pub cloak_window: bool,
}

/// Filters out unexpanded Mojang `${...}` placeholder arguments and their preceding flags
/// (such as `-cp ${classpath}`, `--username ${auth_player_name}`, `-Djava.library.path=${natives_directory}`)
/// so raw placeholders are never passed to the JVM after Deming's resolved flags.
pub fn strip_mojang_placeholders(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg.contains("${") {
            i += 1;
            continue;
        }
        let is_value_flag = arg.starts_with('-') && !arg.contains('=');
        if is_value_flag
            && i + 1 < args.len()
            && !args[i + 1].starts_with('-')
            && args[i + 1].contains("${")
        {
            i += 2;
            continue;
        }
        out.push(arg.clone());
        i += 1;
    }
    out
}

impl ClientLaunchConfig {
    /// Builds the full argument vector for launching the client JVM.
    pub fn build_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        if self.main_class.is_some() {
            if self.ram_mb > 0 {
                let xms = self.ram_mb.min(512).max(128.min(self.ram_mb));
                args.push(format!("-Xms{xms}M"));
                args.push(format!("-Xmx{}M", self.ram_mb));
            }
            if let Some(natives) = &self.natives_dir {
                let abs_natives = absolutize_path(natives);
                args.push(format!("-Djava.library.path={}", abs_natives.display()));
            }
        }

        args.extend(strip_mojang_placeholders(&self.spec.jvm_args));
        args.extend(strip_mojang_placeholders(&self.extra_jvm_args));

        if !self.classpath.is_empty() {
            let sep = if cfg!(windows) { ";" } else { ":" };
            let cp = self
                .classpath
                .iter()
                .map(|p| absolutize_path(p).to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(sep);
            args.push("-cp".to_string());
            args.push(cp);
        }

        if let Some(main_class) = &self.main_class {
            let abs_instance_dir = absolutize_path(&self.instance_dir);
            args.push(main_class.clone());
            args.push("--version".to_string());
            args.push(self.spec.mc_version.clone());
            args.push("--gameDir".to_string());
            args.push(abs_instance_dir.to_string_lossy().into_owned());

            if let Some(assets_dir) = &self.assets_dir {
                let abs_assets = absolutize_path(assets_dir);
                args.push("--assetsDir".to_string());
                args.push(abs_assets.to_string_lossy().into_owned());
            }
            if let Some(asset_index) = &self.asset_index {
                args.push("--assetIndex".to_string());
                args.push(asset_index.clone());
            }

            match &self.spec.account {
                AccountRef::Offline { username } => {
                    args.push("--username".to_string());
                    args.push(username.clone());
                    args.push("--userType".to_string());
                    args.push("legacy".to_string());
                    args.push("--accessToken".to_string());
                    args.push(self.access_token.clone().unwrap_or_else(|| "0".to_string()));
                }
                AccountRef::Microsoft { uuid, username } => {
                    args.push("--username".to_string());
                    args.push(username.clone());
                    args.push("--uuid".to_string());
                    args.push(uuid.clone());
                    args.push("--userType".to_string());
                    args.push("msa".to_string());
                    if let Some(token) = &self.access_token {
                        args.push("--accessToken".to_string());
                        args.push(token.clone());
                    }
                }
            }
        }

        if let Some((host, port)) = &self.connect_server {
            args.extend(build_connect_args(&self.spec.mc_version, host, *port));
        }

        args.extend(strip_mojang_placeholders(&self.spec.game_args));
        args.extend(strip_mojang_placeholders(&self.extra_game_args));

        args
    }
}

/// Captured execution details from running a client instance to completion, timeout, or abort.
#[derive(Debug, Clone)]
pub struct ClientRunOutcome {
    /// Instance identifier.
    pub instance_id: String,
    /// OS process ID assigned to the launched process, if available.
    pub pid: Option<u32>,
    /// Exit code returned by the process, if it exited cleanly or with a code.
    pub exit_code: Option<i32>,
    /// `true` if the client exceeded `timeout_secs` and was terminated.
    pub timed_out: bool,
    /// `true` if the run was aborted via [`AbortHandle`].
    pub aborted: bool,
    /// Captured stdout lines in order.
    pub stdout_lines: Vec<String>,
    /// Captured stderr lines in order.
    pub stderr_lines: Vec<String>,
    /// Combined stdout + stderr lines in arrival order.
    pub combined_logs: Vec<String>,
    /// Timestamp when the process was spawned.
    pub started_at: DateTime<Utc>,
    /// Timestamp when the process exited or was terminated.
    pub finished_at: DateTime<Utc>,
}

/// Spawns a client JVM inside a Windows Job Object, streams stdout/stderr,
/// applies CPU affinity/priority, and enforces `config.timeout_secs` and `abort`.
pub async fn run_client(
    config: ClientLaunchConfig,
    abort: &AbortHandle,
    events: Option<mpsc::UnboundedSender<RunEvent>>,
) -> Result<ClientRunOutcome> {
    if abort.is_aborted() {
        return Err(DemingError::Process(format!(
            "client '{}' skipped because run is aborted",
            config.spec.id
        )));
    }

    let instance_dir = absolutize_path(&config.instance_dir);
    let java_exe = absolutize_executable(&config.java_exe);

    std::fs::create_dir_all(&instance_dir)?;
    ensure_default_options_txt(&instance_dir)?;

    let job = Arc::new(JobHandle::new()?);
    abort.register_job(Arc::clone(&job))?;

    emit_state(
        &events,
        config.run_id,
        &config.spec.id,
        InstanceState::Starting,
        Some("spawning client process".to_string()),
    );

    let args = config.build_args();
    let mut cmd = Command::new(&java_exe);
    cmd.args(&args)
        .current_dir(&instance_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let started_at = Utc::now();
    let mut child = cmd.spawn().map_err(|e| {
        DemingError::Process(format!(
            "failed to spawn client '{}' ({}): {e}",
            config.spec.id,
            config.java_exe.display()
        ))
    })?;

    let pid = child.id();

    if let Err(err) = job.assign_tokio_child(&child) {
        let _ = child.start_kill();
        let _ = job.terminate_all();
        return Err(err);
    }

    if let Err(err) = configure_tokio_child(&child, config.cpu_cores, config.priority) {
        let _ = job.terminate_all();
        return Err(err);
    }

    emit_state(
        &events,
        config.run_id,
        &config.spec.id,
        InstanceState::Running,
        pid.map(|p| format!("running (pid {p})")),
    );

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_buf = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::new()));
    let combined_buf = Arc::new(Mutex::new(Vec::new()));

    let mut reader_tasks = Vec::new();

    let cloak_task = if config.cloak_window {
        pid.map(|target_pid| {
            tokio::spawn(async move {
                let start = tokio::time::Instant::now();
                let max_watch = Duration::from_secs(30);
                while start.elapsed() < max_watch {
                    if let Ok(updated) =
                        crate::screenshot::cloak_and_hide_from_taskbar(target_pid, true)
                    {
                        if updated > 0 {
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            })
        })
    } else {
        None
    };

    if let Some(out) = stdout {
        let out_ref = Arc::clone(&stdout_buf);
        let comb_ref = Arc::clone(&combined_buf);
        let events_ref = events.clone();
        let run_id = config.run_id;
        let source = config.spec.id.clone();
        reader_tasks.push(tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(mut g) = out_ref.lock() {
                    g.push(line.clone());
                }
                if let Ok(mut g) = comb_ref.lock() {
                    g.push(line.clone());
                }
                if let Some(tx) = &events_ref {
                    let _ = tx.send(RunEvent::LogLine {
                        run_id,
                        source: source.clone(),
                        line,
                        timestamp: Utc::now(),
                    });
                }
            }
        }));
    }

    if let Some(err_stream) = stderr {
        let err_ref = Arc::clone(&stderr_buf);
        let comb_ref = Arc::clone(&combined_buf);
        let events_ref = events.clone();
        let run_id = config.run_id;
        let source = config.spec.id.clone();
        reader_tasks.push(tokio::spawn(async move {
            let mut lines = BufReader::new(err_stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(mut g) = err_ref.lock() {
                    g.push(line.clone());
                }
                if let Ok(mut g) = comb_ref.lock() {
                    g.push(line.clone());
                }
                if let Some(tx) = &events_ref {
                    let _ = tx.send(RunEvent::LogLine {
                        run_id,
                        source: source.clone(),
                        line,
                        timestamp: Utc::now(),
                    });
                }
            }
        }));
    }

    let mut timed_out = false;
    let mut aborted = false;
    let mut exit_code = None;

    if config.timeout_secs > 0 {
        let sleep = tokio::time::sleep(Duration::from_secs(config.timeout_secs));
        tokio::pin!(sleep);

        tokio::select! {
            status = child.wait() => {
                exit_code = status?.code();
            }
            _ = abort.cancelled() => {
                aborted = true;
                let _ = job.terminate_all();
                let _ = child.wait().await;
            }
            _ = &mut sleep => {
                timed_out = true;
                let _ = job.terminate_all();
                let _ = child.wait().await;
            }
        }
    } else {
        tokio::select! {
            status = child.wait() => {
                exit_code = status?.code();
            }
            _ = abort.cancelled() => {
                aborted = true;
                let _ = job.terminate_all();
                let _ = child.wait().await;
            }
        }
    }

    if let Some(task) = cloak_task {
        task.abort();
        let _ = task.await;
    }

    for task in reader_tasks {
        let _ = task.await;
    }

    let finished_at = Utc::now();
    let stdout_lines = stdout_buf.lock().map(|g| g.clone()).unwrap_or_default();
    let stderr_lines = stderr_buf.lock().map(|g| g.clone()).unwrap_or_default();
    let combined_logs = combined_buf.lock().map(|g| g.clone()).unwrap_or_default();

    Ok(ClientRunOutcome {
        instance_id: config.spec.id,
        pid,
        exit_code,
        timed_out,
        aborted,
        stdout_lines,
        stderr_lines,
        combined_logs,
        started_at,
        finished_at,
    })
}

/// Writes `<instance_dir>/options.txt` with `onboardAccessibility:false\n` if missing,
/// so fresh test instances skip Minecraft's Accessibility Onboarding screen and allow
/// `--quickPlayMultiplayer` to auto-connect immediately.
pub fn ensure_default_options_txt(instance_dir: &Path) -> Result<()> {
    let options_path = instance_dir.join("options.txt");
    if !options_path.exists() {
        std::fs::write(&options_path, "onboardAccessibility:false\n")?;
    }
    Ok(())
}

fn emit_state(
    events: &Option<mpsc::UnboundedSender<RunEvent>>,
    run_id: Uuid,
    instance_id: &str,
    state: InstanceState,
    message: Option<String>,
) {
    if let Some(tx) = events {
        let _ = tx.send(RunEvent::InstanceStateChanged {
            run_id,
            instance_id: instance_id.to_string(),
            state,
            message,
            timestamp: Utc::now(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deming_core::{ModLoader, TestCompletionRule};
    use std::path::Path;

    #[test]
    fn build_args_includes_ram_classpath_account_and_connect_flags() {
        let cfg = ClientLaunchConfig {
            run_id: Uuid::new_v4(),
            java_exe: PathBuf::from("java"),
            instance_dir: PathBuf::from("instances/c1"),
            spec: InstanceSpec {
                id: "c1".to_string(),
                mc_version: "1.21.4".to_string(),
                loader: ModLoader::Fabric,
                loader_version: None,
                mods: vec![],
                account: AccountRef::Offline {
                    username: "TesterOne".to_string(),
                },
                ram_mb: Some(2048),
                cpu_cores: Some(2),
                jvm_args: vec!["-XX:+UseG1GC".to_string()],
                game_args: vec!["--width".to_string(), "1280".to_string()],
                test_rule: TestCompletionRule::ExitCodeZero,
            },
            main_class: Some("net.fabricmc.loader.impl.launch.knot.KnotClient".to_string()),
            classpath: vec![PathBuf::from("a.jar"), PathBuf::from("b.jar")],
            natives_dir: Some(PathBuf::from("natives")),
            assets_dir: Some(PathBuf::from("assets")),
            asset_index: Some("19".to_string()),
            access_token: None,
            ram_mb: 2048,
            cpu_cores: Some(2),
            priority: ProcessPriority::Normal,
            connect_server: Some(("127.0.0.1".to_string(), 25577)),
            timeout_secs: 60,
            extra_jvm_args: vec![],
            extra_game_args: vec![],
            cloak_window: false,
        };

        let args = cfg.build_args();
        assert!(args.contains(&"-Xmx2048M".to_string()));
        assert!(args.contains(&"-XX:+UseG1GC".to_string()));
        assert!(args.contains(&"--quickPlayMultiplayer".to_string()));
        assert!(args.contains(&"127.0.0.1:25577".to_string()));
        assert!(args.contains(&"--username".to_string()));
        assert!(args.contains(&"TesterOne".to_string()));
    }

    #[test]
    fn strip_mojang_placeholders_removes_unexpanded_tokens_and_pairs() {
        let input: Vec<String> = [
            "-Dos.name=Windows 10",
            "-Djava.library.path=${natives_directory}",
            "-Dminecraft.launcher.brand=${launcher_name}",
            "-DFabricMcEmu= net.minecraft.client.main.Main ",
            "-cp",
            "${classpath}",
            "--username",
            "${auth_player_name}",
            "--version",
            "${version_name}",
            "--gameDir",
            "${game_directory}",
            "--assetsDir",
            "${assets_root}",
            "--assetIndex",
            "${assets_index_name}",
            "--uuid",
            "${auth_uuid}",
            "--accessToken",
            "${auth_access_token}",
            "--clientId",
            "${clientid}",
            "--xuid",
            "${auth_xuid}",
            "--userType",
            "${user_type}",
            "--versionType",
            "${version_type}",
            "--demo",
            "--width",
            "1280",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let filtered = strip_mojang_placeholders(&input);
        assert_eq!(
            filtered,
            vec![
                "-Dos.name=Windows 10",
                "-DFabricMcEmu= net.minecraft.client.main.Main ",
                "--demo",
                "--width",
                "1280",
            ]
        );
    }

    #[test]
    fn client_build_args_absolutizes_paths_and_emits_single_natives_and_cp() {
        let cfg = ClientLaunchConfig {
            run_id: Uuid::new_v4(),
            java_exe: PathBuf::from(".deming-test/ws/cache/jre/bin/java.exe"),
            instance_dir: PathBuf::from(".deming-test/ws/instances/c1"),
            spec: InstanceSpec {
                id: "c1".to_string(),
                mc_version: "1.20.1".to_string(),
                loader: ModLoader::Fabric,
                loader_version: None,
                mods: vec![],
                account: AccountRef::Offline {
                    username: "TesterOne".to_string(),
                },
                ram_mb: Some(2048),
                cpu_cores: None,
                jvm_args: vec![],
                game_args: vec![],
                test_rule: TestCompletionRule::ExitCodeZero,
            },
            main_class: Some("net.fabricmc.loader.impl.launch.knot.KnotClient".to_string()),
            classpath: vec![
                PathBuf::from(".deming-test/ws/cache/a.jar"),
                PathBuf::from(".deming-test/ws/cache/b.jar"),
            ],
            natives_dir: Some(PathBuf::from(".deming-test/ws/cache/natives")),
            assets_dir: Some(PathBuf::from(".deming-test/ws/cache/assets")),
            asset_index: Some("5".to_string()),
            access_token: None,
            ram_mb: 2048,
            cpu_cores: None,
            priority: ProcessPriority::Normal,
            connect_server: None,
            timeout_secs: 60,
            extra_jvm_args: vec![
                "-XX:+UseG1GC".to_string(),
                "-Djava.library.path=${natives_directory}".to_string(),
                "-cp".to_string(),
                "${classpath}".to_string(),
            ],
            extra_game_args: vec![
                "--username".to_string(),
                "${auth_player_name}".to_string(),
                "--accessToken".to_string(),
                "${auth_access_token}".to_string(),
                "--demo".to_string(),
            ],
            cloak_window: false,
        };

        let args = cfg.build_args();

        // Zero unexpanded placeholders remain.
        assert!(
            !args.iter().any(|a| a.contains("${")),
            "found unexpanded placeholder in {args:?}"
        );

        // -Djava.library.path and -cp appear exactly once with absolute paths.
        let natives_flags: Vec<&String> = args
            .iter()
            .filter(|a| a.starts_with("-Djava.library.path="))
            .collect();
        assert_eq!(natives_flags.len(), 1);
        let natives_val = natives_flags[0].strip_prefix("-Djava.library.path=").unwrap();
        assert!(Path::new(natives_val).is_absolute());

        let cp_count = args.iter().filter(|a| *a == "-cp").count();
        assert_eq!(cp_count, 1);
        let cp_idx = args.iter().position(|a| a == "-cp").unwrap();
        let sep = if cfg!(windows) { ';' } else { ':' };
        for entry in args[cp_idx + 1].split(sep) {
            assert!(Path::new(entry).is_absolute(), "expected absolute cp entry: {entry}");
        }

        // --gameDir and --assetsDir are absolute.
        let game_dir_idx = args.iter().position(|a| a == "--gameDir").unwrap();
        assert!(Path::new(&args[game_dir_idx + 1]).is_absolute());
        let assets_dir_idx = args.iter().position(|a| a == "--assetsDir").unwrap();
        assert!(Path::new(&args[assets_dir_idx + 1]).is_absolute());

        // Preserved user/loader flags.
        assert!(args.contains(&"-XX:+UseG1GC".to_string()));
        assert!(args.contains(&"--demo".to_string()));

        // Relative executable with parent is absolutized; bare command is left intact.
        assert!(absolutize_executable(&cfg.java_exe).is_absolute());
        assert_eq!(absolutize_executable(Path::new("java")), PathBuf::from("java"));
    }

    #[test]
    fn ensure_default_options_txt_skips_onboarding_only_when_missing() {
        let dir = std::env::temp_dir().join(format!("deming-opts-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        ensure_default_options_txt(&dir).unwrap();
        let options_path = dir.join("options.txt");
        assert_eq!(
            std::fs::read_to_string(&options_path).unwrap(),
            "onboardAccessibility:false\n"
        );

        // Existing options.txt must be preserved untouched.
        std::fs::write(&options_path, "lang:en_us\nonboardAccessibility:true\n").unwrap();
        ensure_default_options_txt(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(&options_path).unwrap(),
            "lang:en_us\nonboardAccessibility:true\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
