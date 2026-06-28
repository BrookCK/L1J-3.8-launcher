use crate::app::launch_config::stage2_delay_ms;
use crate::logger::log_line;
use anyhow::{Context, Result};
use std::os::windows::process::CommandExt;
use std::process::Command;

pub(crate) fn spawn_delayed_stage2_self(
    pid: u32,
    ip: &str,
    port: u16,
    game_dir: &str,
    no_connect: bool,
    inject_path: Option<&str>,
    windowed: bool,
) -> Result<()> {
    let exe = std::env::current_exe().context("current_exe failed")?;
    let delay_ms = stage2_delay_ms();
    let mut child = Command::new(&exe);
    child
        .arg("--stage2")
        .arg(pid.to_string())
        .arg(ip)
        .arg(port.to_string())
        .arg(game_dir)
        .arg("--delay-ms")
        .arg(delay_ms.to_string());
    child.arg(if windowed {
        "--windowed"
    } else {
        "--fullscreen"
    });
    if no_connect {
        child.arg("--no-connect");
    }
    if let Some(path) = inject_path {
        child.arg("--inject").arg(path);
    }

    child
        .creation_flags(0x0800_0000)
        .spawn()
        .context("spawn stage2 failed")?;

    log_line!("[StartupHook] scheduled same EXE stage2 attach after {delay_ms}ms");
    Ok(())
}
