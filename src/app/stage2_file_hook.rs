use crate::app::launch_config::file_hook_disabled_by_env;
use crate::logger::log_line;
use crate::platform::inject;
use anyhow::Result;
use windows::Win32::Foundation::HANDLE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileHookStartTiming {
    Skipped,
    SpawnImmediatelyAfterAttach,
}

pub(crate) struct EarlyFileHookWorker {
    _join: std::thread::JoinHandle<()>,
}

pub(crate) fn file_hook_start_timing(
    inject_path: Option<&str>,
    path_exists: impl FnOnce(&str) -> bool,
) -> FileHookStartTiming {
    match inject_path {
        Some(path) if path_exists(path) => FileHookStartTiming::SpawnImmediatelyAfterAttach,
        _ => FileHookStartTiming::Skipped,
    }
}

pub(crate) fn spawn_early_file_hook_worker(
    h_process: HANDLE,
    pid: u32,
    inject_path: Option<&str>,
) -> Result<Option<EarlyFileHookWorker>> {
    if file_hook_disabled_by_env() {
        log_line!("[stage2] FileHook disabled by marker/env; early FileHook skipped");
        return Ok(None);
    }

    let Some(path) = inject_path else {
        log_line!("[stage2] no inject file path; early FileHook skipped");
        return Ok(None);
    };

    if file_hook_start_timing(Some(path), |p| std::path::Path::new(p).exists())
        == FileHookStartTiming::Skipped
    {
        log_line!("[stage2] inject file not found, early FileHook skipped: {path}");
        return Ok(None);
    }

    let buffer = inject::load_inject_file(path)?;
    let h_raw = h_process.0 as usize;
    log_line!("[stage2] FileHook worker spawned immediately after attach");
    let join = std::thread::spawn(move || {
        let h_process = HANDLE(h_raw as *mut _);
        match inject::start_file_hook_worker(h_process, pid, &buffer) {
            Ok(()) => log_line!("[stage2] FileHook installed by early worker"),
            Err(e) => log_line!("[stage2] FileHook worker failed: {e:#}"),
        }
    });
    Ok(Some(EarlyFileHookWorker { _join: join }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_hook_worker_starts_immediately_after_stage2_attach_when_inject_exists() {
        assert_eq!(
            file_hook_start_timing(Some(r"D:\lineage3.81C\TW13081901.pak"), |_| true),
            FileHookStartTiming::SpawnImmediatelyAfterAttach
        );
    }
}
