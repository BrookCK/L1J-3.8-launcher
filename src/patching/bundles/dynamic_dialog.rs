use crate::logger::log_line;
use crate::patching::{BundleDecision, BundleSkipReason, Stage2PatchContext};

#[derive(Debug, Clone, Copy)]
pub struct DynamicDialog {
    pub enabled: bool,
    pub hover_disabled_by_user: bool,
}

pub(crate) struct DynamicDialogOutcome {
    pub(crate) hover_poll: Option<crate::patching::img_hover::HoverHookResult>,
}

impl DynamicDialog {
    pub fn hover_decision(&self) -> BundleDecision {
        if self.hover_disabled_by_user {
            BundleDecision::Skip(BundleSkipReason::UserDisabled)
        } else if !self.enabled {
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        } else {
            BundleDecision::Install
        }
    }

    pub fn dialog_decision(&self) -> BundleDecision {
        if self.enabled {
            BundleDecision::Install
        } else {
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        }
    }

    pub(crate) fn apply(&self, ctx: &Stage2PatchContext<'_>) -> DynamicDialogOutcome {
        let hover_poll = match self.hover_decision() {
            BundleDecision::Install => {
                match crate::patching::img_hover::install_img_hover_hook(ctx.h_process, ctx.pid) {
                    Ok(Some(result)) => {
                        log_line!("[stage2] img hover hook installed");
                        Some(result)
                    }
                    Ok(None) => {
                        log_line!("[stage2] img hover hook skipped");
                        None
                    }
                    Err(e) => {
                        log_line!("[stage2] img hover hook failed: {e}");
                        None
                    }
                }
            }
            BundleDecision::Skip(BundleSkipReason::UserDisabled) => {
                log_line!("[stage2] img hover hook DISABLED by user (marker file / env)");
                None
            }
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled) => {
                log_line!("[stage2] dynamic dialog disabled by config");
                None
            }
        };

        if let BundleDecision::Install = self.dialog_decision() {
            match crate::aux::dynamic_dialog_hook::install(ctx.h_process, ctx.pid) {
                Ok(_) => log_line!("[stage2] dynamic dialog hook installed"),
                Err(e) => log_line!("[stage2] dynamic dialog hook failed: {e:#}"),
            }
        }

        DynamicDialogOutcome { hover_poll }
    }
}
