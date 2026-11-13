use std::fmt;
use std::path::{Path, PathBuf};

use deming_core::{AccountRef, InstanceSpec, ProcessPriority, Result};
use uuid::Uuid;

use crate::absolutize_path;

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
#[derive(Clone)]
pub struct ClientLaunchConfig {
    /// Run ID for emitted [`deming_core::RunEvent`]s.
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
    /// Optional callback invoked immediately with the child OS PID after spawn.
    pub on_pid: Option<std::sync::Arc<dyn Fn(u32) + Send + Sync>>,
}

impl fmt::Debug for ClientLaunchConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientLaunchConfig")
            .field("run_id", &self.run_id)
            .field("java_exe", &self.java_exe)
            .field("instance_dir", &self.instance_dir)
            .field("spec", &self.spec)
            .field("main_class", &self.main_class)
            .field("classpath", &self.classpath)
            .field("natives_dir", &self.natives_dir)
            .field("assets_dir", &self.assets_dir)
            .field("asset_index", &self.asset_index)
            .field("access_token", &self.access_token.as_ref().map(|_| "<redacted>"))
            .field("ram_mb", &self.ram_mb)
            .field("cpu_cores", &self.cpu_cores)
            .field("priority", &self.priority)
            .field("connect_server", &self.connect_server)
            .field("timeout_secs", &self.timeout_secs)
            .field("extra_jvm_args", &self.extra_jvm_args)
            .field("extra_game_args", &self.extra_game_args)
            .field("on_pid", &self.on_pid.as_ref().map(|_| "<callback>"))
            .finish()
    }
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

/// Writes `<instance_dir>/options.txt` with `onboardAccessibility:false`,
/// `pauseOnLostFocus:false`, and `rawMouseInput:false` if missing, so fresh test
/// instances skip Minecraft's Accessibility Onboarding screen and do not pause or grab
/// the cursor when unfocused.
pub fn ensure_default_options_txt(instance_dir: &Path) -> Result<()> {
    let options_path = instance_dir.join("options.txt");
    if !options_path.exists() {
        std::fs::write(
            &options_path,
            "onboardAccessibility:false\npauseOnLostFocus:false\nrawMouseInput:false\n",
        )?;
    }
    Ok(())
}
