use anyhow::Result;
use std::path::Path;
use windows::Win32::Foundation::HANDLE;

pub(crate) mod ac_mr_patch;
pub mod bundles;
pub(crate) mod dpi_override;
pub(crate) mod equip_ui;
pub(crate) mod hook;
pub(crate) mod hp_mp_patch;
pub(crate) mod img_hover;
pub(crate) mod packet_proxy;
pub(crate) mod patch;
pub(crate) mod smooth_run_hook;

#[derive(Debug, Clone, Copy)]
pub struct Stage2PatchContext<'a> {
    pub h_process: HANDLE,
    pub pid: u32,
    pub game_dir: &'a Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleDecision {
    Install,
    Skip(BundleSkipReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleSkipReason {
    ConfigDisabled,
    UserDisabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleStatus {
    Installed,
    Skipped(BundleSkipReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleOutcome {
    pub key: &'static str,
    pub status: BundleStatus,
}

pub trait Stage2PatchBundle {
    fn key(&self) -> &'static str;
    fn decision(&self) -> BundleDecision;
    fn install(&self, ctx: &Stage2PatchContext<'_>) -> Result<()>;
    fn log_installed(&self);
    fn log_skipped(&self, reason: BundleSkipReason);

    fn apply(&self, ctx: &Stage2PatchContext<'_>) -> Result<BundleOutcome> {
        match self.decision() {
            BundleDecision::Install => {
                self.install(ctx)?;
                self.log_installed();
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

#[cfg(test)]
mod tests {
    use super::bundles::{
        AcMrLimit, DynamicDialog, EquipUi, HpMpLimit, InventoryLimit, RuntimeHooks,
    };
    use super::{BundleDecision, BundleSkipReason, Stage2PatchBundle};

    #[test]
    fn hp_mp_bundle_decision_prefers_user_disable() {
        let bundle = HpMpLimit {
            enabled: true,
            disabled_by_user: true,
        };

        assert_eq!(
            bundle.decision(),
            BundleDecision::Skip(BundleSkipReason::UserDisabled)
        );
    }

    #[test]
    fn hp_mp_bundle_decision_installs_when_enabled() {
        let bundle = HpMpLimit {
            enabled: true,
            disabled_by_user: false,
        };

        assert_eq!(bundle.decision(), BundleDecision::Install);
    }

    #[test]
    fn ac_mr_bundle_decision_tracks_config() {
        assert_eq!(
            AcMrLimit { enabled: false }.decision(),
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        );
        assert_eq!(
            AcMrLimit { enabled: true }.decision(),
            BundleDecision::Install
        );
    }

    #[test]
    fn inventory_limit_bundle_decision_tracks_config() {
        assert_eq!(
            InventoryLimit {
                enabled: false,
                value: 255,
            }
            .decision(),
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        );
        assert_eq!(
            InventoryLimit {
                enabled: true,
                value: 255,
            }
            .decision(),
            BundleDecision::Install
        );
    }

    #[test]
    fn dynamic_dialog_hover_decision_prefers_user_disable() {
        assert_eq!(
            DynamicDialog {
                enabled: false,
                hover_disabled_by_user: true,
            }
            .hover_decision(),
            BundleDecision::Skip(BundleSkipReason::UserDisabled)
        );
    }

    #[test]
    fn dynamic_dialog_dialog_decision_tracks_config() {
        assert_eq!(
            DynamicDialog {
                enabled: false,
                hover_disabled_by_user: false,
            }
            .dialog_decision(),
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        );
        assert_eq!(
            DynamicDialog {
                enabled: true,
                hover_disabled_by_user: true,
            }
            .dialog_decision(),
            BundleDecision::Install
        );
    }

    #[test]
    fn equip_ui_bundle_decision_prefers_user_disable() {
        assert_eq!(
            EquipUi {
                enabled: true,
                disabled_by_user: true,
            }
            .decision(),
            BundleDecision::Skip(BundleSkipReason::UserDisabled)
        );
    }

    #[test]
    fn runtime_hooks_smooth_run_decision_prefers_user_disable() {
        let bundle = RuntimeHooks {
            file_hook_installed: false,
            morph_preprocess_enabled: false,
            smooth_run_disabled_by_user: true,
            send_packet_spy_enabled: false,
            move_packet_no_encrypt: false,
        };

        assert_eq!(
            bundle.smooth_run_decision(),
            BundleDecision::Skip(BundleSkipReason::UserDisabled)
        );
    }

    #[test]
    fn runtime_hooks_smooth_run_decision_requires_file_hook_and_morph() {
        let mut bundle = RuntimeHooks {
            file_hook_installed: false,
            morph_preprocess_enabled: true,
            smooth_run_disabled_by_user: false,
            send_packet_spy_enabled: false,
            move_packet_no_encrypt: false,
        };

        assert_eq!(
            bundle.smooth_run_decision(),
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        );

        bundle.file_hook_installed = true;
        assert_eq!(bundle.smooth_run_decision(), BundleDecision::Install);
    }

    #[test]
    fn runtime_hooks_send_packet_spy_decision_tracks_flag() {
        let mut bundle = RuntimeHooks {
            file_hook_installed: false,
            morph_preprocess_enabled: false,
            smooth_run_disabled_by_user: false,
            send_packet_spy_enabled: false,
            move_packet_no_encrypt: false,
        };

        assert_eq!(
            bundle.send_packet_spy_decision(),
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        );

        bundle.send_packet_spy_enabled = true;
        assert_eq!(bundle.send_packet_spy_decision(), BundleDecision::Install);
    }
}
