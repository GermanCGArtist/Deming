//! Test Builder UI state, validation, and bidirectional conversion to/from [`TestJobSpec`].

mod io;
mod schema;

pub use schema::{
    mod_loader_label, AccountDraft, BuilderState, CompletionRuleDraft, CompletionRuleKind,
    InstanceDraft, ModEntryDraft, ServerDraft, ALL_MOD_LOADERS,
};
