use anyhow::Result;

use crate::logger::log_line;
use crate::patching::{
    BundleDecision, BundleOutcome, BundleSkipReason, BundleStatus, Stage2PatchBundle,
    Stage2PatchContext,
};

#[derive(Debug, Clone, Copy)]
pub struct ImageAssetLimits {
    pub img_enabled: bool,
    pub img_disabled_by_user: bool,
    pub img_value: u32,
    pub png_value: u32,
}

impl Stage2PatchBundle for ImageAssetLimits {
    fn key(&self) -> &'static str {
        "image_asset_limits"
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
        if self.img_enabled && !self.img_disabled_by_user {
            crate::patching::patch::patch_img_limit(ctx.h_process, self.img_value)?;
            log_line!(
                "[stage2] post-start IMG limit patch value={}",
                self.img_value
            );
        } else if self.img_disabled_by_user {
            log_line!("[stage2] IMG limit DISABLED by user (marker file / env)");
        } else {
            log_line!("[stage2] IMG limit disabled by config");
        }

        match crate::patching::patch::patch_png_limit(ctx.h_process, self.png_value) {
            Ok(()) => log_line!("[stage2] post-start PNG limit patch applied"),
            Err(e) => log_line!("[stage2] PNG limit patch skipped: {e:#}"),
        }

        Ok(BundleOutcome {
            key: self.key(),
            status: BundleStatus::Installed,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct InventoryLimit {
    pub enabled: bool,
    pub value: u32,
}

impl Stage2PatchBundle for InventoryLimit {
    fn key(&self) -> &'static str {
        "inventory_limit"
    }

    fn decision(&self) -> BundleDecision {
        if self.enabled {
            BundleDecision::Install
        } else {
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        }
    }

    fn install(&self, ctx: &Stage2PatchContext<'_>) -> Result<()> {
        crate::patching::patch::patch_inventory_limit(ctx.h_process, self.value)
    }

    fn log_installed(&self) {
        log_line!(
            "[stage2] post-start inventory limit patch value={}",
            self.value
        );
    }

    fn log_skipped(&self, _reason: BundleSkipReason) {
        log_line!("[stage2] inventory limit disabled by config");
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EquipUi {
    pub enabled: bool,
    pub disabled_by_user: bool,
}

impl Stage2PatchBundle for EquipUi {
    fn key(&self) -> &'static str {
        "equip_ui"
    }

    fn decision(&self) -> BundleDecision {
        if self.disabled_by_user {
            BundleDecision::Skip(BundleSkipReason::UserDisabled)
        } else if !self.enabled {
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        } else {
            BundleDecision::Install
        }
    }

    fn install(&self, ctx: &Stage2PatchContext<'_>) -> Result<()> {
        crate::patching::equip_ui::install_equip_ui_patches(ctx.h_process, ctx.pid)
    }

    fn log_installed(&self) {
        log_line!("[stage2] equip UI patches installed");
    }

    fn log_skipped(&self, reason: BundleSkipReason) {
        match reason {
            BundleSkipReason::UserDisabled => {
                log_line!("[stage2] equip UI patches DISABLED by user (marker file / env)")
            }
            BundleSkipReason::ConfigDisabled => log_line!("[stage2] equip UI disabled by config"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HpMpLimit {
    pub enabled: bool,
    pub disabled_by_user: bool,
}

impl Stage2PatchBundle for HpMpLimit {
    fn key(&self) -> &'static str {
        "hp_mp_limit"
    }

    fn decision(&self) -> BundleDecision {
        if self.disabled_by_user {
            BundleDecision::Skip(BundleSkipReason::UserDisabled)
        } else if !self.enabled {
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        } else {
            BundleDecision::Install
        }
    }

    fn install(&self, ctx: &Stage2PatchContext<'_>) -> Result<()> {
        crate::patching::hp_mp_patch::install_hp_mp_patches(ctx.h_process, ctx.pid)
    }

    fn log_installed(&self) {
        log_line!("[stage2] HP/MP limit patches installed");
    }

    fn log_skipped(&self, reason: BundleSkipReason) {
        match reason {
            BundleSkipReason::UserDisabled => {
                log_line!("[stage2] HP/MP limit patches DISABLED by user (marker file / env)")
            }
            BundleSkipReason::ConfigDisabled => {
                log_line!("[stage2] HP/MP limit disabled by config")
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AcMrLimit {
    pub enabled: bool,
}

impl Stage2PatchBundle for AcMrLimit {
    fn key(&self) -> &'static str {
        "ac_mr_limit"
    }

    fn decision(&self) -> BundleDecision {
        if self.enabled {
            BundleDecision::Install
        } else {
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        }
    }

    fn install(&self, ctx: &Stage2PatchContext<'_>) -> Result<()> {
        crate::patching::ac_mr_patch::install_ac_mr_patches(ctx.h_process, ctx.pid)
    }

    fn log_installed(&self) {
        log_line!("[stage2] AC/MR limit patches installed");
    }

    fn log_skipped(&self, _reason: BundleSkipReason) {
        log_line!("[stage2] AC/MR limit disabled by config");
    }
}
