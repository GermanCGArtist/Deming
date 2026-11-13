//! Shared helper utilities for job orchestration.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use deming_auth::{AccountStore, MsaClient, StoredAccount};
use deming_core::{AccountRef, InstanceResult, InstanceState, RunEvent};
use deming_fetch::ProgressCallback;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Default Microsoft OAuth2 public client ID used when refreshing MSA tokens.
pub const DEFAULT_MSA_CLIENT_ID: &str = deming_auth::msa::MOJANG_CLIENT_ID;

/// Returns the default configuration file path (`<local_data_dir>/deming/config.json`).
pub fn default_config_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("deming")
        .join("config.json")
}

/// Resolves an [`AccountRef`] into an optional Minecraft access token, refreshing expired Microsoft tokens.
pub async fn resolve_account_token(
    store: &AccountStore,
    account_ref: &AccountRef,
) -> Result<Option<String>> {
    match account_ref {
        AccountRef::Offline { .. } => Ok(None),
        AccountRef::Microsoft { uuid, username } => {
            let stored = store.find_by_ref(account_ref)?.with_context(|| {
                format!("Microsoft account {username} ({uuid}) not found in account store")
            })?;
            match stored {
                StoredAccount::Microsoft {
                    mc_access_token,
                    msa_refresh_token,
                    expires_at,
                    ..
                } => {
                    if Utc::now() >= expires_at {
                        let client = MsaClient::new(DEFAULT_MSA_CLIENT_ID);
                        let refreshed = client
                            .refresh_token(&msa_refresh_token)
                            .await
                            .context("failed to refresh expired Microsoft account session")?;
                        store.upsert(StoredAccount::from_msa(&refreshed))?;
                        Ok(Some(refreshed.mc_access_token))
                    } else {
                        Ok(Some(mc_access_token))
                    }
                }
                StoredAccount::Offline { .. } => Ok(None),
            }
        }
    }
}

/// Creates a throttled [`ProgressCallback`] that emits [`RunEvent::Progress`] at most every 250ms or on completion.
pub fn make_progress_callback(
    event_tx: mpsc::UnboundedSender<RunEvent>,
    run_id: Uuid,
    instance_id: Option<String>,
    phase: &'static str,
) -> ProgressCallback {
    let last_emit = Arc::new(std::sync::Mutex::new(Option::<std::time::Instant>::None));
    let run_id_str = run_id.to_string();
    Arc::new(move |completed, total, message| {
        let now = std::time::Instant::now();
        let is_done = total > 0 && completed >= total;
        let should_emit = {
            let mut guard = last_emit.lock().unwrap_or_else(|e| e.into_inner());
            let elapsed_ok = guard
                .map(|prev| now.duration_since(prev) >= Duration::from_millis(250))
                .unwrap_or(true);
            if is_done || elapsed_ok {
                *guard = Some(now);
                true
            } else {
                false
            }
        };
        if should_emit {
            let _ = event_tx.send(RunEvent::Progress {
                timestamp: Utc::now(),
                run_id: run_id_str.clone(),
                instance_id: instance_id.clone(),
                phase: phase.to_string(),
                completed,
                total,
                message: Some(message.to_string()),
            });
        }
    })
}

/// Strips unexpanded Mojang `${...}` template placeholders (and preceding `--flag` pairs) from argument lists.
pub fn strip_unexpanded_mojang_args(args: &[String]) -> Vec<String> {
    let mut cleaned = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg.contains("${") {
            i += 1;
            continue;
        }
        // Also drop `--flag ${placeholder}` or `-cp ${classpath}` pairs (but not if the next arg is its own `-` flag).
        if (arg.starts_with('-') || arg.starts_with("--"))
            && i + 1 < args.len()
            && !args[i + 1].starts_with('-')
            && args[i + 1].contains("${")
        {
            i += 2;
            continue;
        }
        cleaned.push(arg.clone());
        i += 1;
    }
    cleaned
}

pub(crate) fn emit_instance_state(
    event_tx: &mpsc::UnboundedSender<RunEvent>,
    run_id: Uuid,
    instance_id: &str,
    state: InstanceState,
    message: Option<String>,
) {
    let _ = event_tx.send(RunEvent::InstanceStateChanged {
        run_id,
        instance_id: instance_id.to_string(),
        state,
        message,
        timestamp: Utc::now(),
    });
}

pub(crate) fn fail_result(
    event_tx: &mpsc::UnboundedSender<RunEvent>,
    run_id: Uuid,
    instance_id: &str,
    reason: String,
) -> InstanceResult {
    emit_instance_state(
        event_tx,
        run_id,
        instance_id,
        InstanceState::Failed,
        Some(reason.clone()),
    );
    InstanceResult {
        instance_id: instance_id.to_string(),
        state: InstanceState::Failed,
        exit_code: None,
        passed: false,
        reason: Some(reason),
        started_at: None,
        finished_at: Some(Utc::now()),
        log_path: None,
        screenshots: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_unexpanded_mojang_args_removes_placeholders_and_flag_pairs() {
        let input = vec![
            "-XX:+UseG1GC".to_string(),
            "-Djava.library.path=${natives_directory}".to_string(),
            "--version".to_string(),
            "${version_name}".to_string(),
            "--width".to_string(),
            "1280".to_string(),
        ];
        let cleaned = strip_unexpanded_mojang_args(&input);
        assert_eq!(cleaned, vec!["-XX:+UseG1GC", "--width", "1280"]);
    }

    #[test]
    fn progress_callback_emits_initial_and_completion_events() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let run_id = Uuid::new_v4();
        let cb = make_progress_callback(tx, run_id, Some("c1".to_string()), "mods");

        cb(0, 10, "Starting");
        cb(5, 10, "Halfway"); // throttled (<250ms and not done)
        cb(10, 10, "Complete"); // completion always emits

        let first = rx.try_recv().expect("initial progress event");
        let second = rx.try_recv().expect("completion progress event");
        assert!(rx.try_recv().is_err());

        match first {
            RunEvent::Progress {
                completed,
                total,
                phase,
                instance_id,
                ..
            } => {
                assert_eq!(completed, 0);
                assert_eq!(total, 10);
                assert_eq!(phase, "mods");
                assert_eq!(instance_id.as_deref(), Some("c1"));
            }
            other => panic!("expected Progress, got {other:?}"),
        }

        match second {
            RunEvent::Progress { completed, total, .. } => {
                assert_eq!(completed, 10);
                assert_eq!(total, 10);
            }
            other => panic!("expected Progress, got {other:?}"),
        }
    }
}
