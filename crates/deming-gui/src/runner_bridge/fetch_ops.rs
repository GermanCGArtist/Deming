//! Async Microsoft OAuth authentication flows and modpack/mod-sync operations.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use deming_auth::{AccountStore, MsaClient, StoredAccount};
use deming_core::{DemingConfig, DemingLockfile, LockedMod, ModSource, ModsManifest};
use deming_fetch::{
    export_mrpack, import_curseforge_zip, import_mrpack, link_or_copy, sync_instance_mods,
    CurseForgeClient,
};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::login_webview::is_oauth_callback_url;

use super::{emit_ui, BridgeMessage, GuiBridge, ImportedPackPayload, DEFAULT_MSA_CLIENT_ID};

impl GuiBridge {
    /// Starts a Microsoft OAuth Device-Code flow and polls in background until authorized or cancelled.
    pub fn start_msa_login(&self, config: DemingConfig, client_id: String) {
        self.cancel_msa_login();
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        if let Ok(mut g) = self.msa_cancel.lock() {
            *g = Some(cancel_tx);
        }

        let ui_tx = self.ui_tx.clone();
        let egui_ctx = Arc::clone(&self.egui_ctx);

        self.runtime.spawn(async move {
            let client = MsaClient::new(client_id);
            let prompt = match client.start_device_code().await {
                Ok(p) => {
                    emit_ui(&ui_tx, &egui_ctx, BridgeMessage::MsaDeviceCodeReady(Ok(p.clone())));
                    p
                }
                Err(e) => {
                    emit_ui(
                        &ui_tx,
                        &egui_ctx,
                        BridgeMessage::MsaDeviceCodeReady(Err(e.to_string())),
                    );
                    return;
                }
            };

            let poll_fut = client.poll_and_complete_login(&prompt);
            tokio::select! {
                _ = &mut cancel_rx => {
                    emit_ui(
                        &ui_tx,
                        &egui_ctx,
                        BridgeMessage::MsaLoginFinished(Err("Microsoft login cancelled".to_string())),
                    );
                }
                res = poll_fut => {
                    match res {
                        Ok(session) => {
                            let store = AccountStore::from_config(&config);
                            if let Err(e) = store.upsert(StoredAccount::from_msa(&session)) {
                                emit_ui(
                                    &ui_tx,
                                    &egui_ctx,
                                    BridgeMessage::MsaLoginFinished(Err(format!(
                                        "Login succeeded but failed to save account: {e}"
                                    ))),
                                );
                            } else {
                                emit_ui(&ui_tx, &egui_ctx, BridgeMessage::MsaLoginFinished(Ok(session)));
                            }
                        }
                        Err(e) => {
                            emit_ui(&ui_tx, &egui_ctx, BridgeMessage::MsaLoginFinished(Err(e.to_string())));
                        }
                    }
                }
            }
        });
    }

    /// Completes a Microsoft Browser OAuth (`login.live.com`) login from a pasted redirect URL or authorization code.
    pub fn complete_browser_login(&self, config: DemingConfig, code_or_url: String) {
        let ui_tx = self.ui_tx.clone();
        let egui_ctx = Arc::clone(&self.egui_ctx);

        self.runtime.spawn(async move {
            let client = MsaClient::new(DEFAULT_MSA_CLIENT_ID);
            match client.complete_browser_login(&code_or_url).await {
                Ok(session) => {
                    let store = AccountStore::from_config(&config);
                    if let Err(e) = store.upsert(StoredAccount::from_msa(&session)) {
                        emit_ui(
                            &ui_tx,
                            &egui_ctx,
                            BridgeMessage::MsaBrowserLoginFinished(Err(format!(
                                "Login succeeded but failed to save account: {e}"
                            ))),
                        );
                    } else {
                        emit_ui(
                            &ui_tx,
                            &egui_ctx,
                            BridgeMessage::MsaBrowserLoginFinished(Ok(session)),
                        );
                    }
                }
                Err(e) => {
                    emit_ui(
                        &ui_tx,
                        &egui_ctx,
                        BridgeMessage::MsaBrowserLoginFinished(Err(e.to_string())),
                    );
                }
            }
        });
    }

    /// Spawns a one-click native WebView popup (`--login-popup`) to authenticate via Microsoft,
    /// reads the captured redirect URL from stdout, exchanges the code, and saves the account.
    pub fn start_popup_login(&self, config: DemingConfig) {
        let ui_tx = self.ui_tx.clone();
        let egui_ctx = Arc::clone(&self.egui_ctx);

        self.runtime.spawn(async move {
            let exe = match std::env::current_exe() {
                Ok(p) => p,
                Err(e) => {
                    emit_ui(
                        &ui_tx,
                        &egui_ctx,
                        BridgeMessage::MsaPopupLoginFinished(Err(format!(
                            "Failed to locate current executable: {e}"
                        ))),
                    );
                    return;
                }
            };

            let output = match tokio::process::Command::new(exe)
                .arg("--login-popup")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
            {
                Ok(out) => out,
                Err(e) => {
                    emit_ui(
                        &ui_tx,
                        &egui_ctx,
                        BridgeMessage::MsaPopupLoginFinished(Err(format!(
                            "Failed to launch login popup process: {e}"
                        ))),
                    );
                    return;
                }
            };

            let stdout = String::from_utf8_lossy(&output.stdout);
            let captured = stdout
                .lines()
                .map(str::trim)
                .find(|line| is_oauth_callback_url(line))
                .map(str::to_string);

            let Some(callback_url) = captured else {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let msg = if !output.status.success() && !stderr.is_empty() {
                    format!("Microsoft login popup error: {stderr}")
                } else {
                    "Microsoft sign-in window closed before completing authentication."
                        .to_string()
                };
                emit_ui(
                    &ui_tx,
                    &egui_ctx,
                    BridgeMessage::MsaPopupLoginFinished(Err(msg)),
                );
                return;
            };

            let client = MsaClient::new(DEFAULT_MSA_CLIENT_ID);
            match client.complete_browser_login(&callback_url).await {
                Ok(session) => {
                    let store = AccountStore::from_config(&config);
                    if let Err(e) = store.upsert(StoredAccount::from_msa(&session)) {
                        emit_ui(
                            &ui_tx,
                            &egui_ctx,
                            BridgeMessage::MsaPopupLoginFinished(Err(format!(
                                "Login succeeded but failed to save account: {e}"
                            ))),
                        );
                    } else {
                        emit_ui(
                            &ui_tx,
                            &egui_ctx,
                            BridgeMessage::MsaPopupLoginFinished(Ok(session)),
                        );
                    }
                }
                Err(e) => {
                    emit_ui(
                        &ui_tx,
                        &egui_ctx,
                        BridgeMessage::MsaPopupLoginFinished(Err(e.to_string())),
                    );
                }
            }
        });
    }

    /// Imports a `.mrpack` or CurseForge `.zip` modpack asynchronously and reports the resulting
    /// [`ModsManifest`] + [`DemingLockfile`] (plus any CurseForge resolution warnings).
    pub fn import_modpack(&self, config: DemingConfig, path: PathBuf) {
        let ui_tx = self.ui_tx.clone();
        let egui_ctx = Arc::clone(&self.egui_ctx);

        self.runtime.spawn(async move {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("imported-pack")
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect::<String>();
            let dest_dir = config.workspace_dir.join("packs").join(&stem);
            let is_mrpack = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("mrpack"))
                .unwrap_or(false);

            let res: Result<ImportedPackPayload, String> = if is_mrpack {
                import_mrpack(&path, &dest_dir, &config.cache_dir(), None)
                    .await
                    .map(|(manifest, lockfile)| {
                        let summary = format!(
                            "Imported .mrpack '{}' ({} {:?}, {} mods)",
                            stem,
                            manifest.mc_version,
                            manifest.loader,
                            lockfile.mods.len()
                        );
                        ImportedPackPayload {
                            pack_name: stem,
                            manifest,
                            lockfile: Some(lockfile),
                            summary,
                            warnings: Vec::new(),
                        }
                    })
                    .map_err(|e| e.to_string())
            } else {
                match import_curseforge_zip(&path, Some(&dest_dir)) {
                    Ok(pack) => {
                        let mut warnings = Vec::new();
                        let mut locked_mods: Vec<LockedMod> = Vec::new();
                        let mut mod_sources: Vec<ModSource> = Vec::new();

                        if !pack.files.is_empty() {
                            match CurseForgeClient::from_config(&config) {
                                Ok(cf) => {
                                    let mods_dir = dest_dir.join("mods");
                                    let _ = std::fs::create_dir_all(&mods_dir);
                                    for f in &pack.files {
                                        match cf
                                            .resolve_mod(
                                                f.project_id,
                                                Some(f.file_id),
                                                &pack.mc_version,
                                                pack.loader,
                                                &config.cache_dir(),
                                            )
                                            .await
                                        {
                                            Ok((locked, blob_path)) => {
                                                let target = mods_dir.join(&locked.filename);
                                                if let Err(e) = link_or_copy(&blob_path, &target) {
                                                    warnings.push(format!(
                                                        "Failed linking {} ({}): {e}",
                                                        locked.filename, f.project_id
                                                    ));
                                                } else {
                                                    mod_sources.push(ModSource::LocalPath(target));
                                                    locked_mods.push(locked);
                                                }
                                            }
                                            Err(e) => {
                                                warnings.push(format!(
                                                    "CurseForge mod {}/{} failed: {e}",
                                                    f.project_id, f.file_id
                                                ));
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    warnings.push(format!(
                                        "{} CurseForge mod(s) skipped — set CurseForge API Key in Settings to download ({e}).",
                                        pack.files.len()
                                    ));
                                }
                            }
                        }

                        // Include any JAR overrides extracted into dest_dir/mods
                        let override_mods_dir = dest_dir.join("mods");
                        if let Ok(rd) = std::fs::read_dir(&override_mods_dir) {
                            for entry in rd.flatten() {
                                let p = entry.path();
                                if p.is_file()
                                    && p.extension()
                                        .and_then(|s| s.to_str())
                                        .map(|s| s.eq_ignore_ascii_case("jar"))
                                        .unwrap_or(false)
                                {
                                    let src = ModSource::LocalPath(p);
                                    if !mod_sources.contains(&src) {
                                        mod_sources.push(src);
                                    }
                                }
                            }
                        }

                        let lockfile = if locked_mods.is_empty() {
                            None
                        } else {
                            Some(DemingLockfile::new(
                                pack.mc_version.clone(),
                                pack.loader,
                                pack.loader_version.clone(),
                                locked_mods,
                            ))
                        };
                        let manifest = ModsManifest {
                            mc_version: pack.mc_version.clone(),
                            loader: pack.loader,
                            loader_version: pack.loader_version.clone(),
                            mods: mod_sources,
                        };
                        let summary = format!(
                            "Imported CurseForge pack '{}' v{} ({} {:?}, {} mods, {} overrides)",
                            pack.name,
                            pack.version,
                            pack.mc_version,
                            pack.loader,
                            manifest.mods.len(),
                            pack.overrides_extracted
                        );
                        Ok(ImportedPackPayload {
                            pack_name: pack.name,
                            manifest,
                            lockfile,
                            summary,
                            warnings,
                        })
                    }
                    Err(e) => Err(e.to_string()),
                }
            };

            emit_ui(&ui_tx, &egui_ctx, BridgeMessage::ModpackImported(res));
        });
    }

    /// Exports a `.mrpack` bundle from the given [`ModsManifest`] and optional cached [`DemingLockfile`].
    pub fn export_mrpack_async(
        &self,
        config: DemingConfig,
        manifest: ModsManifest,
        existing_lockfile: Option<DemingLockfile>,
        out_path: PathBuf,
    ) {
        let ui_tx = self.ui_tx.clone();
        let egui_ctx = Arc::clone(&self.egui_ctx);

        self.runtime.spawn(async move {
            let lockfile_res: Result<DemingLockfile, String> = match existing_lockfile {
                Some(lf)
                    if lf.mc_version == manifest.mc_version
                        && lf.loader == manifest.loader
                        && lf.mods.len() == manifest.mods.len() =>
                {
                    Ok(lf)
                }
                _ => {
                    let stage_dir = config
                        .workspace_dir
                        .join(".tmp_export")
                        .join(Uuid::new_v4().to_string());
                    let synced = sync_instance_mods(&config, &manifest, &stage_dir, false, None)
                        .await
                        .map(|plan| plan.lockfile)
                        .map_err(|e| e.to_string());
                    let _ = tokio::fs::remove_dir_all(&stage_dir).await;
                    synced
                }
            };

            let res = match lockfile_res {
                Ok(lockfile) => export_mrpack(&manifest, &lockfile, None, &out_path)
                    .map(|()| out_path)
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            emit_ui(&ui_tx, &egui_ctx, BridgeMessage::ModpackExported(res));
        });
    }

    /// Runs [`sync_instance_mods`] (`dry_run = true` or `false`) for the target instance directory.
    pub fn sync_instance_mods_async(
        &self,
        config: DemingConfig,
        manifest: ModsManifest,
        instance_dir: PathBuf,
        dry_run: bool,
    ) {
        let ui_tx = self.ui_tx.clone();
        let egui_ctx = Arc::clone(&self.egui_ctx);

        self.runtime.spawn(async move {
            let res = sync_instance_mods(&config, &manifest, &instance_dir, dry_run, None)
                .await
                .map_err(|e| e.to_string());
            emit_ui(
                &ui_tx,
                &egui_ctx,
                BridgeMessage::ModsSynced {
                    dry_run,
                    result: res,
                },
            );
        });
    }

    pub fn cancel_msa_login(&self) {
        if let Ok(mut g) = self.msa_cancel.lock() {
            if let Some(tx) = g.take() {
                let _ = tx.send(());
            }
        }
    }
}
