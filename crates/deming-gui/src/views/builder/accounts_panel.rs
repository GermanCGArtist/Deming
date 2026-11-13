//! Account selection sub-panel for client instances in the Test Builder.

use deming_auth::StoredAccount;
use egui::RichText;

use crate::preset::AccountDraft;
use crate::theme::TEXT_MUTED;

pub(super) fn render_instance_account_selector(
    ui: &mut egui::Ui,
    idx: usize,
    account: &mut AccountDraft,
    stored_accounts: &[StoredAccount],
) {
    ui.label(RichText::new("Account:").color(TEXT_MUTED));
    egui::ComboBox::from_id_salt(format!("inst_account_{idx}"))
        .selected_text(account.display_label())
        .show_ui(ui, |ui| {
            for acc in stored_accounts {
                let draft = AccountDraft::from_stored(acc);
                let label = draft.display_label();
                if ui
                    .selectable_label(*account == draft, label)
                    .clicked()
                {
                    *account = draft;
                }
            }
            if stored_accounts.is_empty() {
                ui.label(
                    RichText::new("No saved accounts (add in Accounts tab)")
                        .size(11.5)
                        .color(TEXT_MUTED),
                );
            }
        });
    if !account.is_microsoft {
        ui.add(
            egui::TextEdit::singleline(&mut account.username)
                .desired_width(110.0)
                .hint_text("Offline username"),
        );
    }
}
