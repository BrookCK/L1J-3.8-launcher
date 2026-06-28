use anyhow::Result;

use crate::logger::log_line;
use crate::patching::{
    BundleDecision, BundleOutcome, BundleSkipReason, BundleStatus, Stage2PatchBundle,
    Stage2PatchContext,
};

#[derive(Debug, Clone, Copy)]
pub struct SurfaceInput {
    pub ddraw_inproc_disabled: bool,
}

impl Stage2PatchBundle for SurfaceInput {
    fn key(&self) -> &'static str {
        "surface_input"
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
        match crate::patching::patch::patch_surface_pixel_format(ctx.h_process, ctx.pid) {
            Ok(()) => log_line!("[stage2] surface pixel format patch applied"),
            Err(e) => log_line!("[stage2] surface pixel format patch skipped: {e:#}"),
        }

        if self.ddraw_inproc_disabled {
            match crate::patching::patch::patch_input_box_offscreen(ctx.h_process, ctx.pid) {
                Ok(()) => log_line!("[stage2] input box offscreen background patch applied"),
                Err(e) => log_line!("[stage2] input box offscreen background patch skipped: {e:#}"),
            }
        } else {
            log_line!("[stage2] input box offscreen skipped; ddraw in-process owns background");
        }

        Ok(BundleOutcome {
            key: self.key(),
            status: BundleStatus::Installed,
        })
    }
}
