//! Crash log and stack trace analyzer for AI diagnostics.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Structured summary extracted from Minecraft logs and crash reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashAnalysis {
    /// Primary exception or failure headline, if detected.
    pub summary: Option<String>,
    /// Filtered relevant error lines and top stack frames (noise stripped).
    pub key_lines: Vec<String>,
    /// Implicated mod IDs or `.jar` filenames mentioned in the crash.
    pub implicated_mods: Vec<String>,
    /// Crash report files inspected.
    pub crash_reports: Vec<String>,
}

/// Inspects `latest.log` (or `logs/latest.log`) and `crash-reports/*.txt` inside `dir`.
pub fn analyze_instance_crash(dir: &Path) -> Result<CrashAnalysis> {
    let mut combined = String::new();
    let mut crash_reports = Vec::new();

    for candidate in [
        dir.join("latest.log"),
        dir.join("logs").join("latest.log"),
        dir.join("stderr.log"),
    ] {
        if candidate.is_file() {
            if let Ok(text) = fs::read_to_string(&candidate) {
                combined.push_str(&text);
                combined.push('\n');
            }
        }
    }

    let crash_dir = dir.join("crash-reports");
    if crash_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&crash_dir) {
            let mut paths: Vec<_> = entries
                .filter_map(|e| e.ok().map(|entry| entry.path()))
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("txt"))
                .collect();
            paths.sort();
            for p in paths {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    crash_reports.push(name.to_string());
                }
                if let Ok(text) = fs::read_to_string(&p) {
                    combined.push_str(&text);
                    combined.push('\n');
                }
            }
        }
    }

    let mut analysis = analyze_crash_text(&combined);
    analysis.crash_reports = crash_reports;
    Ok(analysis)
}

/// Parses raw Minecraft log / crash report text, stripping repetitive stack trace noise.
pub fn analyze_crash_text(raw: &str) -> CrashAnalysis {
    let mut summary: Option<String> = None;
    let mut key_lines: Vec<String> = Vec::new();
    let mut implicated: BTreeSet<String> = BTreeSet::new();
    let mut consecutive_frames = 0usize;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        extract_implicated_mods(trimmed, &mut implicated);

        let is_stack_frame = trimmed.starts_with("at ") || trimmed.starts_with("... ");
        let is_noisy_frame = is_stack_frame
            && (trimmed.contains("java.base/")
                || trimmed.contains("jdk.internal.")
                || trimmed.contains("sun.reflect.")
                || trimmed.contains("org.lwjgl.")
                || trimmed.starts_with("... "));

        if is_stack_frame {
            consecutive_frames += 1;
            if is_noisy_frame || consecutive_frames > 4 {
                continue;
            }
            key_lines.push(trimmed.to_string());
            continue;
        }

        consecutive_frames = 0;

        let is_error_headline = trimmed.contains("Exception")
            || trimmed.contains("Error:")
            || trimmed.starts_with("Caused by:")
            || trimmed.contains("FATAL")
            || trimmed.contains("ERROR]")
            || trimmed.contains("Mixin apply")
            || trimmed.contains("ModResolutionException")
            || trimmed.contains("Description:");

        if is_error_headline {
            if summary.is_none()
                && (trimmed.contains("Exception")
                    || trimmed.contains("Error")
                    || trimmed.starts_with("Caused by:")
                    || trimmed.contains("Description:"))
            {
                summary = Some(trimmed.to_string());
            }
            if key_lines.len() < 60 {
                key_lines.push(trimmed.to_string());
            }
        }
    }

    if summary.is_none() {
        summary = key_lines.first().cloned();
    }

    CrashAnalysis {
        summary,
        key_lines,
        implicated_mods: implicated.into_iter().collect(),
        crash_reports: Vec::new(),
    }
}

fn extract_implicated_mods(line: &str, out: &mut BTreeSet<String>) {
    // Extract `.jar` filenames mentioned in logs/stacktraces.
    for token in line.split(|c: char| {
        c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | '\'' | '"' | ',' | ';' | ':')
    }) {
        if token.ends_with(".jar") && token.len() > 4 {
            let fname = token.rsplit(['/', '\\']).next().unwrap_or(token);
            out.insert(fname.to_string());
        }
    }

    // Extract `Mod ID: '<id>'` or `mod '<id>'` patterns common in Fabric/NeoForge/Forge crash logs.
    let lower = line.to_ascii_lowercase();
    for marker in ["mod id: '", "mod '", "from mod "] {
        if let Some(pos) = lower.find(marker) {
            let rest = &line[pos + marker.len()..];
            let mod_id: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if !mod_id.is_empty() && mod_id != "minecraft" && mod_id != "java" {
                out.insert(mod_id);
            }
        }
    }
}
