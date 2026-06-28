//! Lineage 3.8 launcher.
//!
//! CLI launches `TW13081901.bin 2130706433` directly by default.
//! Startup hooks run on the suspended game process, then stage2 applies FileHook and feature patches.
#![windows_subsystem = "windows"]
#![allow(private_interfaces)]

mod app;
#[path = "aux_mod/mod.rs"]
mod aux;
#[path = "shared/i18n.rs"]
mod i18n;
#[path = "shared/legacy_text.rs"]
mod legacy_text;
#[path = "shared/logger.rs"]
mod logger;
mod patching;
mod platform;
#[path = "shared/smooth_run/mod.rs"]
mod smooth_run;

use crate::app::cli_entry::run_cli;
use crate::app::gui;
use crate::app::launch_config::should_attach_console;
use crate::app::stage2_cli::run_stage2_cli;
use crate::logger::log_line;
use windows::Win32::System::Console::{AllocConsole, AttachConsole, ATTACH_PARENT_PROCESS};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    std::panic::set_hook(Box::new(|info| {
        crate::logger::log(format!("[panic] {info}"));
    }));

    let stage2_mode = args.get(1).map(|s| s == "--stage2").unwrap_or(false);
    if should_attach_console(&args) {
        unsafe {
            if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
                let _ = AllocConsole();
            }
        }
    }

    let result = if stage2_mode {
        run_stage2_cli(&args)
    } else if args.len() <= 1 {
        gui::run_gui()
    } else {
        run_cli(&args)
    };

    if let Err(e) = result {
        crate::logger::log(format!("[error] {e:#}"));
        eprintln!("[error] {e:#}");
        std::process::exit(1);
    }
}
