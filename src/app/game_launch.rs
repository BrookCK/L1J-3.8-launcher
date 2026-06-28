use crate::app::launch_config::{
    connect_hook_enabled, connect_target_for_launch, create_game_suspended_requested,
    game_ip_arg_enabled, ipv4_decimal_arg, keep_launcher_alive_after_stage2_enabled,
    load_aux_config, packet_encrypt_requires_startup_hook,
};
use crate::app::lineage_cfg;
use crate::app::stage2_spawn::spawn_delayed_stage2_self;
use crate::aux;
use crate::logger::log_line;
use crate::patching::{dpi_override, packet_proxy};
use crate::platform::process;
use anyhow::{bail, Result};
use std::time::Instant;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};

pub(crate) const GAME_EXE: &str = "TW13081901.bin";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PacketEncryptConfig {
    pub rsa_d: u32,
    pub rsa_n: u32,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn launch_game(
    ip: &str,
    port: u16,
    game_dir: &str,
    no_connect: bool,
    inject_buffer: Option<Vec<u8>>,
    inject_source_path: Option<String>,
    packet_encrypt: Option<PacketEncryptConfig>,
    windowed: bool,
    window_mode: crate::app::config::WindowMode,
    on_started: Option<Box<dyn FnOnce() + Send>>,
) -> Result<()> {
    let launch_start = Instant::now();
    let exe_path = format!("{game_dir}\\{GAME_EXE}");
    if !std::path::Path::new(&exe_path).exists() {
        bail!("game executable not found: {exe_path}");
    }

    let aux_for_limit = load_aux_config();
    let multi_limit = aux::multi_limit::effective_limit(
        aux_for_limit.multi_instance,
        aux_for_limit.multi_instance_limit,
    );
    let multi_slot = match aux::multi_limit::acquire_launch_slot(multi_limit) {
        aux::multi_limit::SlotOutcome::Unlimited => None,
        aux::multi_limit::SlotOutcome::Acquired(guard) => Some(guard),
        aux::multi_limit::SlotOutcome::Full(n) => {
            log_line!("[multi-limit] launch limit reached: {n}");
            aux::multi_limit::show_limit_reached_message(n);
            return Ok(());
        }
    };

    log_line!("========================================");
    log_line!("Lineage 3.8 Rust launcher");
    log_line!("launch exe: {exe_path}");
    log_line!("connect target: {ip}:{port}");
    log_line!(
        "display mode: {} WindowMode={}",
        if windowed { "windowed" } else { "fullscreen" },
        window_mode.as_raw()
    );
    log_line!("[launch-time] launch_game start");
    let startup_hook_required = packet_encrypt_requires_startup_hook(packet_encrypt.is_some());
    log_line!("[StartupHook] pre-resume hook disabled; no DLL launch path");

    if let Some(cfg) = packet_encrypt {
        log_line!(
            "[PacketEncrypt] enabled rsa_d={} rsa_n={}",
            cfg.rsa_d,
            cfg.rsa_n
        );
    } else {
        log_line!("[PacketEncrypt] disabled");
    }

    let packet_proxy_endpoint = if let Some(cfg) = packet_encrypt {
        Some(packet_proxy::start_packet_encrypt_proxy(
            packet_proxy::PacketProxyConfig {
                server_ip: ip.to_string(),
                server_port: port,
                packet_encrypt: cfg,
            },
        )?)
    } else {
        None
    };
    let connect_target = connect_target_for_launch(
        ip,
        port,
        packet_proxy_endpoint
            .as_ref()
            .map(|endpoint| (endpoint.ip.as_str(), endpoint.port)),
    );
    if packet_proxy_endpoint.is_some() {
        log_line!(
            "[NetProxy] PacketEncrypt proxy route: game -> {}:{} -> {ip}:{port}",
            connect_target.ip,
            connect_target.port
        );
    } else {
        log_line!("[NetProxy] disabled: game connects directly to {ip}:{port}");
    }

    let connect_hook_enabled = connect_hook_enabled(no_connect);
    let patch_no_connect = !connect_hook_enabled;
    if connect_hook_enabled {
        log_line!("[ConnectHook] post-start connect hook enabled by env");
    } else {
        log_line!("[ConnectHook] direct IP mode; connect hook disabled by default");
    }

    if inject_buffer.is_some() && inject_source_path.is_none() {
        log_line!("[stage2] inject buffer has no source path; FileHook requires source path");
    }

    let game_ip_arg = if game_ip_arg_enabled() {
        let arg = ipv4_decimal_arg(&connect_target.ip);
        if let Some(arg) = &arg {
            log_line!(
                "[launch-time] game IPv4 arg={arg} ({}:{})",
                connect_target.ip,
                connect_target.port
            );
        }
        arg
    } else {
        log_line!("[launch-time] game IPv4 arg disabled by LOGIN38_GAME_IP_ARG");
        None
    };

    let create_suspended = create_game_suspended_requested(startup_hook_required);
    if create_suspended {
        log_line!("[launch-time] CreateProcess suspended for pre-resume DLL install");
    } else {
        log_line!("[launch-time] direct-bin CreateProcess without CREATE_SUSPENDED");
    }

    let _ = inject_buffer;

    apply_display_mode_config(game_dir, windowed, window_mode);

    if windowed {
        log_line!("[compat] windowed launch; fullscreen optimization flag skipped");
    } else if let Err(e) = dpi_override::ensure_disable_fullscreen_optimizations(&exe_path) {
        log_line!("[compat] disable fullscreen optimizations failed: {e:#}");
    }

    let (h_process, h_thread, pid) = process::create_game_with_args(
        &exe_path,
        game_dir,
        create_suspended,
        game_ip_arg.as_deref(),
    )?;
    log_line!(
        "[launch-time] CreateProcess reached {:.3}s",
        launch_start.elapsed().as_secs_f64()
    );
    log_line!("[OK] game process started PID={pid}");

    if create_suspended {
        log_line!("[StartupHook] no startup_hook DLL before ResumeThread");
        process::resume_main_thread(h_thread);
        log_line!(
            "[launch-time] ResumeThread done {:.3}s",
            launch_start.elapsed().as_secs_f64()
        );
    } else {
        unsafe {
            let _ = CloseHandle(h_thread);
        }
        log_line!(
            "[launch-time] CreateProcess returned running process {:.3}s",
            launch_start.elapsed().as_secs_f64()
        );
    }

    if let Some(cb) = on_started {
        cb();
    }

    if let Err(e) = spawn_delayed_stage2_self(
        pid,
        &connect_target.ip,
        connect_target.port,
        game_dir,
        patch_no_connect,
        inject_source_path.as_deref(),
        windowed,
    ) {
        log_line!("[stage2] schedule failed: {e:#}");
    }

    if keep_launcher_alive_after_stage2_enabled()
        || packet_proxy_endpoint.is_some()
        || multi_slot.is_some()
    {
        log_line!("[StartupHook] fast stage1 mode: keep launcher alive for game process");
        unsafe {
            WaitForSingleObject(h_process, INFINITE);
            let _ = CloseHandle(h_process);
        }
        log_line!("[StartupHook] fast stage1 mode: game exited, launcher exits");
    } else {
        log_line!("[StartupHook] fast stage1 mode: stage2 scheduled, launcher exits immediately");
        unsafe {
            let _ = CloseHandle(h_process);
        }
    }

    drop(multi_slot);

    Ok(())
}

fn apply_display_mode_config(
    game_dir: &str,
    windowed: bool,
    window_mode: crate::app::config::WindowMode,
) {
    let fullscreen = if windowed { 0 } else { 1 };
    if let Err(e) = lineage_cfg::set_fullscreen(game_dir, fullscreen) {
        log_line!("[cfg] set FullScreen={fullscreen} failed: {e:#}");
        return;
    }

    if windowed {
        if let Err(e) = lineage_cfg::set_window_mode(game_dir, window_mode) {
            log_line!(
                "[cfg] set WindowMode={} failed: {e:#}",
                window_mode.as_raw()
            );
            return;
        }
    }

    log_line!(
        "[cfg] display mode applied: {} WindowMode={}",
        if windowed { "windowed" } else { "fullscreen" },
        window_mode.as_raw()
    );
}
