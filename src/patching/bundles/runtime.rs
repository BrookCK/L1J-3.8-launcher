use windows::Win32::Foundation::HANDLE;

use crate::logger::log_line;
use crate::patching::{BundleDecision, BundleSkipReason, Stage2PatchContext};

#[derive(Debug, Clone, Copy)]
pub struct RuntimeHooks {
    pub file_hook_installed: bool,
    pub morph_preprocess_enabled: bool,
    pub smooth_run_disabled_by_user: bool,
    pub send_packet_spy_enabled: bool,
    pub move_packet_no_encrypt: bool,
}

impl RuntimeHooks {
    pub fn smooth_run_decision(&self) -> BundleDecision {
        if self.smooth_run_disabled_by_user {
            BundleDecision::Skip(BundleSkipReason::UserDisabled)
        } else if !self.file_hook_installed || !self.morph_preprocess_enabled {
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        } else {
            BundleDecision::Install
        }
    }

    pub fn send_packet_spy_decision(&self) -> BundleDecision {
        if self.send_packet_spy_enabled {
            BundleDecision::Install
        } else {
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled)
        }
    }

    pub(crate) fn apply(&self, ctx: &Stage2PatchContext<'_>) {
        match self.smooth_run_decision() {
            BundleDecision::Install => {
                match crate::patching::smooth_run_hook::install_smooth_run_hook(
                    ctx.h_process,
                    ctx.pid,
                ) {
                    Ok(()) => {
                        log_line!("[stage2] smooth run hook installed (per-entity @ 0x00449776)")
                    }
                    Err(e) => log_line!("[stage2] smooth run hook failed: {e}"),
                }
            }
            BundleDecision::Skip(BundleSkipReason::UserDisabled) => {
                log_line!("[stage2] smooth run hook DISABLED by user (env/CLI/marker file)");
            }
            BundleDecision::Skip(BundleSkipReason::ConfigDisabled) => {
                if self.file_hook_installed {
                    log_line!("[stage2] smooth run hook skipped; morph preprocess disabled");
                }
            }
        }

        if self.file_hook_installed {
            if let Err(e) = crate::aux::poison_hook::install_poison_hook(ctx.h_process, ctx.pid) {
                log_line!("[stage2] poison hook failed: {e}");
            }
        }

        let _ = crate::aux::notification::install(ctx.h_process, ctx.pid, ctx.game_dir)
            .map_err(|e| log_line!("[notification] install skipped: {e:#}"));

        match self.send_packet_spy_decision() {
            BundleDecision::Install => {
                let _ = crate::aux::use_item_spy::install_send_packet_spy(ctx.h_process)
                    .map_err(|e| log_line!("[spy] SendPacketData install skipped: {e:#}"));
            }
            BundleDecision::Skip(_) => {
                log_line!("[spy] SendPacketData skipped; opt-in diagnostic hook disabled");
            }
        }

        if self.move_packet_no_encrypt {
            spawn_delayed_move_packet_no_encrypt_patch(ctx.h_process);
        }
    }
}

fn spawn_delayed_move_packet_no_encrypt_patch(h: HANDLE) {
    let h_raw = h.0 as usize;
    std::thread::spawn(move || {
        let h = HANDLE(h_raw as *mut _);
        log_line!("[MoveNoEncrypt] wait for in-game state before patch");
        for _ in 0..6000 {
            if crate::platform::memory::read_u32(h, crate::aux::address::G_GAME_STATE).ok()
                == Some(3)
            {
                match crate::patching::patch::patch_move_packet_no_encrypt(h) {
                    Ok(()) => log_line!("[MoveNoEncrypt] patch installed"),
                    Err(e) => log_line!("[MoveNoEncrypt] patch failed: {e:#}"),
                }
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        log_line!("[MoveNoEncrypt] timeout before in-game state");
    });
}
