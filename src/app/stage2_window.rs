use crate::logger::log_line;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

const STAGE2_WINDOW_WAIT_TIMEOUT_MS: u64 = 60_000;
const STAGE2_WINDOW_WAIT_POLL_MS: u64 = 100;

/// Finds the first visible top-level window owned by the game process.
fn find_visible_window_for_pid(pid: u32) -> Option<(HWND, String)> {
    struct Search {
        pid: u32,
        found: Option<(HWND, String)>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }

        let mut window_pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut window_pid));
        if window_pid != search.pid {
            return true.into();
        }

        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, &mut buf);
        let title = String::from_utf16_lossy(&buf[..copied as usize]);
        if !title.trim().is_empty() {
            search.found = Some((hwnd, title));
            return false.into();
        }
        true.into()
    }

    let mut search = Search { pid, found: None };
    unsafe {
        let _ = EnumWindows(
            Some(enum_proc),
            LPARAM((&mut search as *mut Search) as isize),
        );
    }
    search.found
}

pub(crate) fn wait_for_visible_window(pid: u32, label: &str) -> bool {
    let start = Instant::now();
    let timeout = Duration::from_millis(STAGE2_WINDOW_WAIT_TIMEOUT_MS);
    let poll = Duration::from_millis(STAGE2_WINDOW_WAIT_POLL_MS);
    let mut next_log = Duration::from_secs(5);

    log_line!("[stage2] {label}: waiting visible game window");
    while start.elapsed() < timeout {
        if let Some((hwnd, title)) = find_visible_window_for_pid(pid) {
            let hwnd_value = hwnd.0 as usize;
            log_line!(
                "[stage2] {label}: visible after {:.3}s hwnd=0x{hwnd_value:X} title={title}",
                start.elapsed().as_secs_f64()
            );
            return true;
        }

        if start.elapsed() >= next_log {
            log_line!(
                "[stage2] {label}: still waiting visible game window {:.3}s",
                start.elapsed().as_secs_f64()
            );
            next_log += Duration::from_secs(5);
        }
        std::thread::sleep(poll);
    }

    log_line!(
        "[stage2] {label}: visible wait timeout {:.3}s; fallback attach",
        start.elapsed().as_secs_f64()
    );
    false
}
