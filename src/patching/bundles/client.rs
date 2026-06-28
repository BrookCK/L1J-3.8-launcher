use anyhow::Result;

use crate::logger::log_line;
use crate::patching::{
    BundleDecision, BundleOutcome, BundleSkipReason, BundleStatus, Stage2PatchBundle,
    Stage2PatchContext,
};

#[derive(Debug, Clone, Copy)]
pub struct ClientHardening;

impl Stage2PatchBundle for ClientHardening {
    fn key(&self) -> &'static str {
        "client_hardening"
    }

    fn decision(&self) -> BundleDecision {
        BundleDecision::Install
    }

    fn install(&self, ctx: &Stage2PatchContext<'_>) -> Result<()> {
        crate::patching::patch::patch_ac_check(ctx.h_process)?;
        log_line!("[stage2] AC check bypass patched");

        crate::patching::patch::patch_crt_watson(ctx.h_process, ctx.pid)?;
        log_line!("[stage2] CRT Watson bypass patched");

        if let Err(e) = crate::aux::chat_width::install_chat_width_patch(ctx.h_process) {
            log_line!("[stage2] chat width patch skipped: {e:#}");
        }

        Ok(())
    }

    fn log_installed(&self) {}

    fn log_skipped(&self, _reason: BundleSkipReason) {}
}

#[derive(Debug, Clone, Copy)]
pub struct DynamicIcon<'a> {
    pub enabled: bool,
    pub pak_name: &'a str,
}

impl Stage2PatchBundle for DynamicIcon<'_> {
    fn key(&self) -> &'static str {
        "dynamic_icon"
    }

    fn decision(&self) -> BundleDecision {
        if self.enabled {
            BundleDecision::Install
        } else {
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        }
    }

    fn install(&self, ctx: &Stage2PatchContext<'_>) -> Result<()> {
        crate::aux::dynamic_icon::install(ctx.h_process, ctx.game_dir, self.pak_name)
    }

    fn log_installed(&self) {
        log_line!("[stage2] dynamic item icon installed");
    }

    fn log_skipped(&self, _reason: BundleSkipReason) {
        log_line!("[stage2] dynamic item icon disabled by config");
    }

    fn apply(&self, ctx: &Stage2PatchContext<'_>) -> Result<BundleOutcome> {
        match self.decision() {
            BundleDecision::Install => {
                match self.install(ctx) {
                    Ok(()) => self.log_installed(),
                    Err(e) => log_line!("[stage2] dynamic item icon skipped: {e:#}"),
                }
                Ok(BundleOutcome {
                    key: self.key(),
                    status: BundleStatus::Installed,
                })
            }
            BundleDecision::Skip(reason) => {
                self.log_skipped(reason);
                Ok(BundleOutcome {
                    key: self.key(),
                    status: BundleStatus::Skipped(reason),
                })
            }
        }
    }
}
