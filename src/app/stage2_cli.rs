use crate::app::launch_config::{
    force_simplified_text_locale_enabled_by_env, load_aux_config, stage2_pre_visible_attach_enabled,
};
use crate::app::lhx_runtime::run_lhx_aux_until_game_exit;
use crate::app::stage2_file_hook::spawn_early_file_hook_worker;
use crate::app::stage2_patches::{
    install_stage2_connect_hook, run_stage2_feature_patches, Stage2Patches,
};
use crate::app::stage2_window::wait_for_visible_window;
use crate::aux;
use crate::logger::log_line;
use crate::patching::patch;
use crate::platform::process;
use anyhow::{bail, Context, Result};
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE};

struct Stage2CliOptions {
    pid: u32,
    ip: String,
    port: u16,
    game_dir: String,
    no_connect: bool,
    inject_path: Option<String>,
    delay_ms: u64,
}

pub(crate) fn run_stage2_cli(args: &[String]) -> Result<()> {
    let opts = parse_stage2_cli_args(args)?;
    sleep_before_stage2_attach(opts.delay_ms);
    log_stage2_attach_start(&opts);

    let h_process = open_stage2_process(&opts)?;
    let (connect_hook_installed, file_hook_requested) =
        run_stage2_startup_patches(h_process, &opts)?;

    run_stage2_feature_patches(Stage2Patches {
        h_process,
        pid: opts.pid,
        ip: &opts.ip,
        port: opts.port,
        no_connect: opts.no_connect,
        connect_hook_installed,
        file_hook_installed: file_hook_requested,
        game_dir: &opts.game_dir,
    })?;

    finish_stage2_window_setup(h_process, opts.pid);
    let kept_alive_for_aux = run_lhx_aux_until_game_exit(h_process, opts.pid)?;
    close_stage2_process(h_process, kept_alive_for_aux);
    Ok(())
}

fn parse_stage2_cli_args(args: &[String]) -> Result<Stage2CliOptions> {
    if args.len() < 6 {
        bail!("--stage2 requires PID IP PORT GAME_DIR");
    }

    let pid: u32 = args[2].parse().context("--stage2 pid parse failed")?;
    let ip = args[3].clone();
    let port: u16 = args[4].parse().context("--stage2 port parse failed")?;
    let game_dir = args[5].clone();
    let mut no_connect = false;
    let mut inject_path: Option<String> = None;
    let mut delay_ms = 0_u64;

    let mut i = 6;
    while i < args.len() {
        match args[i].as_str() {
            "--no-connect" => no_connect = true,
            "--windowed" | "--fullscreen" => {}
            "--delay-ms" => {
                i += 1;
                if i >= args.len() {
                    bail!("--stage2 --delay-ms requires a value");
                }
                delay_ms = args[i]
                    .parse()
                    .context("--stage2 --delay-ms parse failed")?;
            }
            "--inject" => {
                i += 1;
                if i >= args.len() {
                    bail!("--stage2 --inject requires a file path");
                }
                inject_path = Some(args[i].clone());
            }
            other => log_line!("[stage2] ignore unknown arg: {other}"),
        }
        i += 1;
    }

    Ok(Stage2CliOptions {
        pid,
        ip,
        port,
        game_dir,
        no_connect,
        inject_path,
        delay_ms,
    })
}

fn sleep_before_stage2_attach(delay_ms: u64) {
    if delay_ms > 0 {
        log_line!("[stage2] sleep {delay_ms}ms before attach");
        std::thread::sleep(Duration::from_millis(delay_ms));
    }

    if stage2_pre_visible_attach_enabled() {
        log_line!("[stage2] pre-visible attach enabled by env");
    }
}

fn log_stage2_attach_start(opts: &Stage2CliOptions) {
    log_line!(
        "[stage2] attach pid={} target={}:{} game_dir={}",
        opts.pid,
        opts.ip,
        opts.port,
        opts.game_dir
    );
}

fn open_stage2_process(opts: &Stage2CliOptions) -> Result<HANDLE> {
    process::open_game_process(opts.pid)
}

fn run_stage2_startup_patches(h_process: HANDLE, opts: &Stage2CliOptions) -> Result<(bool, bool)> {
    if force_simplified_text_locale_enabled_by_env() {
        patch::spawn_force_simplified_text_locale_worker(h_process);
    }
    patch::spawn_simplified_status_tooltip_encoding_worker(h_process);

    let connect_hook_installed = install_stage2_connect_hook(
        h_process,
        opts.pid,
        &opts.ip,
        opts.port,
        opts.no_connect,
        "pre-patch",
    )?;

    let file_hook_worker =
        spawn_early_file_hook_worker(h_process, opts.pid, opts.inject_path.as_deref())?;
    let file_hook_requested = file_hook_worker.is_some();

    log_line!("[stage2] time protection bypass: wait_and_patch");
    patch::wait_and_patch(h_process, opts.pid)?;
    log_line!("[stage2] time protection bypass patched");

    Ok((connect_hook_installed, file_hook_requested))
}

fn finish_stage2_window_setup(h_process: HANDLE, pid: u32) {
    let _ = wait_for_visible_window(pid, "post-start feature patch");
    lock_stage2_game_window(pid);
    let ddraw_inproc_active = install_stage2_ddraw_inproc(h_process);
    apply_stage2_window_fallback(ddraw_inproc_active);
}

fn lock_stage2_game_window(pid: u32) {
    // Cache the actual game HWND before input_sim, overlay, and lhx_window need it.
    // Keep failure warning-only because callers still have their existing FindWindowW fallback paths.
    match aux::game_window::init_game_hwnd(pid) {
        Ok(hwnd) => log_line!(
            "[stage2] game HWND locked pid={pid} hwnd=0x{:X}",
            hwnd.0 as usize
        ),
        Err(e) => {
            log_line!("[stage2] WARN init_game_hwnd failed; callers may use fallback lookup: {e:#}")
        }
    }

    // Apply anti-cheat title randomization only after the real game HWND is cached.
    if load_aux_config().anti_cheat_basic {
        if let Some(hwnd) = aux::game_window::cached_game_hwnd() {
            match aux::window_rename::apply_random_title(hwnd, pid) {
                Ok(t) => log_line!("[anti-cheat] randomized window title: {t}"),
                Err(e) => log_line!("[anti-cheat] WARN randomized window title failed: {e:#}"),
            }
        }
    }
}

fn install_stage2_ddraw_inproc(h_process: HANDLE) -> bool {
    // DDraw in-process present takeover. If injection fails, fall back to the legacy window style patch.
    if aux::ddraw_inproc::disabled_by_env() {
        return false;
    }

    match aux::ddraw_inproc::install(h_process) {
        Ok(()) => {
            log_line!("[stage2] DDraw in-process present takeover installed");
            true
        }
        Err(e) => {
            log_line!("[stage2] WARN DDraw in-process install failed, fallback: {e:#}");
            false
        }
    }
}

fn apply_stage2_window_fallback(ddraw_inproc_active: bool) {
    // Legacy fallback when the in-process present takeover is unavailable.
    if ddraw_inproc_active {
        return;
    }

    if let Some(hwnd) = aux::game_window::cached_game_hwnd() {
        match aux::game_window::enable_clip_children(hwnd) {
            Ok(()) => log_line!("[stage2] WS_CLIPCHILDREN enabled on game window"),
            Err(e) => log_line!("[stage2] WARN WS_CLIPCHILDREN failed: {e:#}"),
        }
    }
}

fn close_stage2_process(h_process: HANDLE, kept_alive_for_aux: bool) {
    unsafe {
        let _ = CloseHandle(h_process);
    }
    if kept_alive_for_aux {
        log_line!("[stage2] done after LHX aux shutdown");
    } else {
        log_line!("[stage2] done");
    }
}
