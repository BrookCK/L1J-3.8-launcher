use anyhow::Result;

use crate::logger::log_line;
use crate::patching::{
    BundleDecision, BundleOutcome, BundleSkipReason, BundleStatus, Stage2PatchBundle,
    Stage2PatchContext,
};

#[derive(Debug, Clone, Copy)]
pub struct ItemDescription;

impl Stage2PatchBundle for ItemDescription {
    fn key(&self) -> &'static str {
        "item_description"
    }

    fn decision(&self) -> BundleDecision {
        BundleDecision::Install
    }

    fn install(&self, _ctx: &Stage2PatchContext<'_>) -> Result<()> {
        Ok(())
    }

    fn log_installed(&self) {}

    fn log_skipped(&self, _reason: BundleSkipReason) {}

    fn apply(&self, ctx: &Stage2PatchContext<'_>) -> Result<BundleOutcome> {
        match crate::aux::item_desc_color::install(ctx.h_process) {
            Ok(()) => log_line!("[stage2] item desc color installed"),
            Err(e) => log_line!("[stage2] item desc color skipped: {e:#}"),
        }

        match crate::aux::item_desc_length::install(ctx.h_process) {
            Ok(()) => log_line!("[stage2] item desc length sidecar installed"),
            Err(e) => log_line!("[stage2] item desc length skipped: {e:#}"),
        }

        match crate::aux::item_desc_length::install_custom_opcode_242(ctx.h_process) {
            Ok(()) => log_line!("[stage2] item custom opcode 242 mux installed"),
            Err(e) => log_line!("[stage2] item custom opcode 242 mux skipped: {e:#}"),
        }

        Ok(BundleOutcome {
            key: self.key(),
            status: BundleStatus::Installed,
        })
    }
}
