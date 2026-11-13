//! Accounts tab: manage Offline/Dev accounts and Microsoft OAuth login.

use chrono::Utc;
use deming_auth::{create_offline_account, AccountStore, MsaClient, StoredAccount};
use egui::{RichText, ScrollArea};

use crate::runner_bridge::{GuiBridge, DEFAULT_MSA_CLIENT_ID};
use crate::state::{StatusToast, UiState};
use crate::theme::{
    card_frame, danger_button, primary_button, status_badge, sub_card_frame, ACCENT_EMERALD,
    DANGER_RED, INFO_BLUE, TEXT_MUTED, TEXT_PRIMARY, WARN_AMBER,
};

pub fn refresh_accounts(state: &mut UiState) {
    let store = AccountStore::from_config(&state.config);
    match store.load() {
        Ok(accts) => state.accounts.accounts = accts,
        Err(e) => {
            state.toast = Some(StatusToast {
                is_error: true,
                text: format!("Failed to load accounts: {e}"),
                created_at: Utc::now(),
            });
        }
    }
}

pub fn render(ui: &mut egui::Ui, state: &mut UiState, bridge: &GuiBridge) {
    ui.columns(2, |cols| {
        // Left column: Add Offline Account + Microsoft OAuth Login
        card_frame().show(&mut cols[0], |ui| {
            ScrollArea::vertical()
                .id_salt("accounts_left_scroll")
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Add Offline / Dev Account")
                            .size(15.5)
                            .strong()
                            .color(ACCENT_EMERALD),
                    );
                    ui.label(
                        RichText::new(
                            "Deterministic UUID v3 for online-mode=false local test servers.",
                        )
                        .size(11.5)
                        .color(TEXT_MUTED),
                    );
                    ui.add_space(6.0);

                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut state.accounts.offline_username)
                                .desired_width(200.0)
                                .hint_text("Username (1-16 chars, A-Z, 0-9, _)"),
                        );

                        if ui.add(primary_button("Add Offline Account")).clicked() {
                            match create_offline_account(state.accounts.offline_username.clone()) {
                                Ok(acct) => {
                                    let store = AccountStore::from_config(&state.config);
                                    match store.upsert(StoredAccount::from_offline(&acct)) {
                                        Ok(()) => {
                                            state.accounts.offline_username.clear();
                                            refresh_accounts(state);
                                            state.toast = Some(StatusToast {
                                                is_error: false,
                                                text: format!(
                                                    "Added offline account '{}' ({})",
                                                    acct.username,
                                                    acct.uuid_hyphenated()
                                                ),
                                                created_at: Utc::now(),
                                            });
                                        }
                                        Err(e) => {
                                            state.toast = Some(StatusToast {
                                                is_error: true,
                                                text: format!("Failed to save account: {e}"),
                                                created_at: Utc::now(),
                                            });
                                        }
                                    }
                                }
                                Err(e) => {
                                    state.toast = Some(StatusToast {
                                        is_error: true,
                                        text: e.to_string(),
                                        created_at: Utc::now(),
                                    });
                                }
                            }
                        }
                    });

                    ui.add_space(16.0);
                    ui.separator();
                    ui.add_space(12.0);

                    // Primary Microsoft One-Click WebView Popup Login
                    ui.label(
                        RichText::new("Microsoft Account (Xbox Live / Minecraft)")
                            .size(15.5)
                            .strong()
                            .color(ACCENT_EMERALD),
                    );
                    ui.label(
                        RichText::new(format!(
                            "Sign in with Microsoft using Mojang's default Client ID ({DEFAULT_MSA_CLIENT_ID}) — no Azure App Registration needed."
                        ))
                        .size(11.5)
                        .color(TEXT_MUTED),
                    );
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        let popup_btn = primary_button("Sign In with Microsoft (One-Click Popup)");
                        if ui
                            .add_enabled(!state.accounts.popup_login_in_flight, popup_btn)
                            .clicked()
                        {
                            state.accounts.popup_login_in_flight = true;
                            state.accounts.popup_is_error = false;
                            state.accounts.popup_status_msg = Some(
                                "Sign-in popup open — complete Microsoft login in the popup window."
                                    .to_string(),
                            );
                            bridge.start_popup_login(state.config.clone());
                        }
                    });

                    if state.accounts.popup_login_in_flight {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new("Waiting for Microsoft sign-in popup...")
                                    .color(INFO_BLUE),
                            );
                        });
                    }

                    if let Some(ref status) = state.accounts.popup_status_msg {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(status)
                                .size(12.0)
                                .color(if state.accounts.popup_is_error {
                                    DANGER_RED
                                } else {
                                    TEXT_MUTED
                                }),
                        );
                    }

                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // Secondary collapsible Manual Browser URL Paste
                    egui::CollapsingHeader::new(
                        RichText::new("Manual Browser Login (Paste Redirect URL)")
                            .size(13.5)
                            .strong()
                            .color(TEXT_PRIMARY),
                    )
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(
                                "1. Open Microsoft login in your browser and sign in.\n\
                                 2. After reaching the blank page, copy the full URL from your address bar (or the ?code= value) and paste it below.",
                            )
                            .size(11.5)
                            .color(TEXT_MUTED),
                        );
                        ui.add_space(8.0);

                        ui.horizontal(|ui| {
                            let auth_url =
                                MsaClient::new(DEFAULT_MSA_CLIENT_ID).browser_authorize_url();
                            if ui
                                .add(primary_button("Open Microsoft Login in Browser"))
                                .clicked()
                            {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(auth_url.clone()));
                                state.accounts.browser_is_error = false;
                                state.accounts.browser_status_msg = Some(
                                    "Browser opened. Complete sign-in, then paste the redirect URL below."
                                        .to_string(),
                                );
                            }
                            if ui.button("Copy Login URL").clicked() {
                                ui.ctx().copy_text(auth_url);
                                state.toast = Some(StatusToast {
                                    is_error: false,
                                    text: "Copied Microsoft login URL".to_string(),
                                    created_at: Utc::now(),
                                });
                            }
                        });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut state.accounts.browser_code_input)
                                    .desired_width(260.0)
                                    .hint_text(
                                        "https://login.live.com/oauth20_desktop.srf?code=... or code",
                                    ),
                            );

                            let can_complete = !state.accounts.browser_login_in_flight
                                && !state.accounts.browser_code_input.trim().is_empty();
                            if ui
                                .add_enabled(can_complete, primary_button("Complete Sign-In"))
                                .clicked()
                            {
                                state.accounts.browser_login_in_flight = true;
                                state.accounts.browser_is_error = false;
                                state.accounts.browser_status_msg = Some(
                                    "Exchanging authorization code with Microsoft / Xbox / Minecraft..."
                                        .to_string(),
                                );
                                bridge.complete_browser_login(
                                    state.config.clone(),
                                    state.accounts.browser_code_input.trim().to_string(),
                                );
                            }
                        });

                        if state.accounts.browser_login_in_flight {
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(
                                    RichText::new("Completing Microsoft sign-in...")
                                        .color(INFO_BLUE),
                                );
                            });
                        }

                        if let Some(ref status) = state.accounts.browser_status_msg {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(status)
                                    .size(12.0)
                                    .color(if state.accounts.browser_is_error {
                                        DANGER_RED
                                    } else {
                                        TEXT_MUTED
                                    }),
                            );
                        }
                    });

                    ui.add_space(8.0);

                    // Secondary collapsible Device-Code Flow
                    egui::CollapsingHeader::new(
                        RichText::new("Device-Code Login (Custom Azure Client ID)")
                            .size(13.5)
                            .strong()
                            .color(TEXT_PRIMARY),
                    )
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(
                                "Use this if you have registered your own Azure AD Public Client Application ID.",
                            )
                            .size(11.5)
                            .color(TEXT_MUTED),
                        );
                        ui.add_space(6.0);

                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Client ID:").color(TEXT_MUTED));
                            ui.add(
                                egui::TextEdit::singleline(&mut state.accounts.msa_client_id)
                                    .desired_width(240.0),
                            );
                        });

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let start_enabled = !state.accounts.msa_polling
                                && !state.accounts.msa_client_id.trim().is_empty();
                            if ui
                                .add_enabled(
                                    start_enabled,
                                    primary_button("Start Device-Code Login"),
                                )
                                .clicked()
                            {
                                state.accounts.msa_polling = true;
                                state.accounts.msa_prompt = None;
                                state.accounts.msa_is_error = false;
                                state.accounts.msa_status_msg =
                                    Some("Requesting device code from Microsoft...".to_string());
                                bridge.start_msa_login(
                                    state.config.clone(),
                                    state.accounts.msa_client_id.trim().to_string(),
                                );
                            }

                            if state.accounts.msa_polling && ui.button("Cancel Login").clicked() {
                                bridge.cancel_msa_login();
                                state.accounts.msa_polling = false;
                                state.accounts.msa_prompt = None;
                                state.accounts.msa_status_msg = Some("Cancelled".to_string());
                            }
                        });

                        if let Some(ref prompt) = state.accounts.msa_prompt {
                            ui.add_space(10.0);
                            sub_card_frame().show(ui, |ui| {
                                ui.label(
                                    RichText::new("Enter this code in your browser:")
                                        .strong()
                                        .color(TEXT_PRIMARY),
                                );
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(&prompt.user_code)
                                            .monospace()
                                            .size(22.0)
                                            .strong()
                                            .color(ACCENT_EMERALD),
                                    );
                                    if ui.button("Copy Code").clicked() {
                                        ui.ctx().copy_text(prompt.user_code.clone());
                                        state.toast = Some(StatusToast {
                                            is_error: false,
                                            text: format!("Copied user code '{}'", prompt.user_code),
                                            created_at: Utc::now(),
                                        });
                                    }
                                });
                                ui.add_space(4.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(RichText::new("Verification URL:").color(TEXT_MUTED));
                                    ui.hyperlink_to(
                                        &prompt.verification_uri,
                                        &prompt.verification_uri,
                                    );
                                    if ui.button("Copy URL").clicked() {
                                        ui.ctx().copy_text(prompt.verification_uri.clone());
                                        state.toast = Some(StatusToast {
                                            is_error: false,
                                            text: "Copied verification URL".to_string(),
                                            created_at: Utc::now(),
                                        });
                                    }
                                });
                                if state.accounts.msa_polling {
                                    ui.add_space(6.0);
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(
                                            RichText::new(
                                                "Waiting for Microsoft authorization...",
                                            )
                                            .color(INFO_BLUE),
                                        );
                                    });
                                }
                            });
                        }

                        if let Some(ref status) = state.accounts.msa_status_msg {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(status)
                                    .size(12.0)
                                    .color(if state.accounts.msa_is_error {
                                        DANGER_RED
                                    } else {
                                        TEXT_MUTED
                                    }),
                            );
                        }
                    });
                });
        });

        // Right column: Stored Accounts List
        card_frame().show(&mut cols[1], |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Stored Accounts ({})", state.accounts.accounts.len()))
                        .size(15.5)
                        .strong()
                        .color(ACCENT_EMERALD),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Refresh").clicked() {
                        refresh_accounts(state);
                    }
                });
            });

            ui.add_space(8.0);

            if state.accounts.accounts.is_empty() {
                ui.label(
                    RichText::new("No stored accounts yet. Add an offline or Microsoft account on the left.")
                        .italics()
                        .color(TEXT_MUTED),
                );
                return;
            }

            let accounts_snapshot = state.accounts.accounts.clone();
            let mut remove_uuid: Option<String> = None;

            ScrollArea::vertical()
                .id_salt("accounts_list_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for acc in &accounts_snapshot {
                        sub_card_frame().show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(acc.username())
                                        .size(14.0)
                                        .strong()
                                        .color(TEXT_PRIMARY),
                                );
                                match acc {
                                    StoredAccount::Offline { .. } => {
                                        status_badge(ui, "Offline", TEXT_MUTED);
                                    }
                                    StoredAccount::Microsoft { .. } => {
                                        status_badge(ui, "Microsoft", ACCENT_EMERALD);
                                        if acc.is_expired() {
                                            status_badge(ui, "Token Expired", WARN_AMBER);
                                        } else {
                                            status_badge(ui, "Active", INFO_BLUE);
                                        }
                                    }
                                }

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.add(danger_button("Remove")).clicked() {
                                            remove_uuid = Some(acc.uuid().to_string());
                                        }
                                    },
                                );
                            });
                            ui.label(
                                RichText::new(format!("UUID: {}", acc.uuid()))
                                    .monospace()
                                    .size(11.0)
                                    .color(TEXT_MUTED),
                            );
                        });
                        ui.add_space(4.0);
                    }
                });

            if let Some(uuid) = remove_uuid {
                let store = AccountStore::from_config(&state.config);
                if let Err(e) = store.remove_by_uuid(&uuid) {
                    state.toast = Some(StatusToast {
                        is_error: true,
                        text: format!("Failed to remove account: {e}"),
                        created_at: Utc::now(),
                    });
                } else {
                    refresh_accounts(state);
                }
            }
        });
    });
}
