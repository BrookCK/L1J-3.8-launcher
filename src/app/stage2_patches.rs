use crate::app::hover_polling::spawn_hover_poll;
use crate::app::launch_config::{
    equip_ui_disabled_by_env, hp_mp_limit_disabled_by_env, img_hover_disabled_by_env,
    img_limit_disabled_by_env, load_aux_config, send_packet_spy_enabled_by_env,
    smooth_run_hook_disabled_by_env, stage2_remaining_patch_delay_ms,
};
use crate::app::login;
use crate::aux;
use crate::logger::log_line;
use crate::patching::{self, hook, Stage2PatchBundle};
use crate::platform::inject;
use anyhow::Result;
use std::path::Path;
use std::time::Duration;
use windows::Win32::Foundation::HANDLE;

pub(crate) struct Stage2Patches<'a> {
    pub(crate) h_process: HANDLE,
    pub(crate) pid: u32,
    pub(crate) ip: &'a str,
    pub(crate) port: u16,
    pub(crate) no_connect: bool,
    pub(crate) connect_hook_installed: bool,
    pub(crate) file_hook_installed: bool,
    pub(crate) game_dir: &'a str,
}

pub(crate) fn install_stage2_connect_hook(
    h_process: HANDLE,
    pid: u32,
    ip: &str,
    port: u16,
    no_connect: bool,
    phase: &str,
) -> Result<bool> {
    if no_connect {
        log_line!("[stage2] {phase} connect hook skipped by --no-connect");
        return Ok(false);
    }

    hook::hook_connect(h_process, pid, ip, port, 0)?;
    log_line!("[stage2] {phase} connect hook installed");
    Ok(true)
}

pub(crate) fn run_stage2_feature_patches(args: Stage2Patches) -> Result<()> {
    let Stage2Patches {
        h_process,
        pid,
        ip,
        port,
        no_connect,
        connect_hook_installed,
        file_hook_installed,
        game_dir,
    } = args;
    let aux_cfg = load_aux_config();
    let patch_ctx = patching::Stage2PatchContext {
        h_process,
        pid,
        game_dir: Path::new(game_dir),
    };
    crate::aux::notification::set_enabled(aux_cfg.pickup_toast_enabled, aux_cfg.exp_drift_enabled);

    if connect_hook_installed {
        log_line!("[stage2] early connect hook already installed before time patch");
    } else {
        install_stage2_connect_hook(h_process, pid, ip, port, no_connect, "early")?;
    }

    login::install_login_hooks(h_process, pid)?;
    log_line!("[stage2] early login hooks installed");

    patching::bundles::ClientHardening.apply(&patch_ctx)?;

    patching::bundles::ImageAssetLimits {
        img_enabled: aux_cfg.img_limit_enabled,
        img_disabled_by_user: img_limit_disabled_by_env(),
        img_value: aux_cfg.img_limit_value,
        png_value: 100_000,
    }
    .apply(&patch_ctx)?;

    patching::bundles::DynamicIcon {
        enabled: aux_cfg.dynamic_icon_enabled,
        pak_name: &aux_cfg.dynamic_icon_pak_name,
    }
    .apply(&patch_ctx)?;

    patching::bundles::ItemDescription.apply(&patch_ctx)?;

    // Surface/input patch bundle.
    patching::bundles::SurfaceInput {
        ddraw_inproc_disabled: aux::ddraw_inproc::disabled_by_env(),
    }
    .apply(&patch_ctx)?;

    patching::bundles::InventoryLimit {
        enabled: aux_cfg.inventory_limit_enabled,
        value: aux_cfg.inventory_limit_value,
    }
    .apply(&patch_ctx)?;

    let dynamic_dialog = patching::bundles::DynamicDialog {
        enabled: aux_cfg.dynamic_dialog_enabled,
        hover_disabled_by_user: img_hover_disabled_by_env(),
    }
    .apply(&patch_ctx);
    if let Some(result) = dynamic_dialog.hover_poll {
        spawn_hover_poll(result);
    }

    patching::bundles::EquipUi {
        enabled: aux_cfg.equip_ui_enabled,
        disabled_by_user: equip_ui_disabled_by_env(),
    }
    .apply(&patch_ctx)?;

    patching::bundles::HpMpLimit {
        enabled: aux_cfg.hp_mp_limit_enabled,
        disabled_by_user: hp_mp_limit_disabled_by_env(),
    }
    .apply(&patch_ctx)?;
    patching::bundles::AcMrLimit {
        enabled: aux_cfg.ac_mr_limit_enabled,
    }
    .apply(&patch_ctx)?;

    let remaining_delay_ms = stage2_remaining_patch_delay_ms();
    if remaining_delay_ms > 0 {
        log_line!("[stage2] sleep {remaining_delay_ms}ms before remaining patches");
        std::thread::sleep(Duration::from_millis(remaining_delay_ms));
    }

    patching::bundles::RuntimeHooks {
        file_hook_installed,
        morph_preprocess_enabled: inject::morph_preprocess_enabled(),
        smooth_run_disabled_by_user: smooth_run_hook_disabled_by_env(),
        send_packet_spy_enabled: send_packet_spy_enabled_by_env(),
        move_packet_no_encrypt: aux_cfg.move_packet_no_encrypt,
    }
    .apply(&patch_ctx);

    log_line!("[stage2] all patches done");
    Ok(())
}
