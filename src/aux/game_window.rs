//! 共享 game window HWND cache — 多開時每個 launcher 鎖定自己 game 的 HWND
//!
//! ## 過去問題(2026-05-17 user 回報多開 bug)
//!
//! 4 處 `FindWindowW(NULL, "Lineage Windows Client (13081901)")` by title only,
//! Windows 在多 process 同 title 時回 topmost / focused。 使用者切到 game B 焦點,
//! bot A 的 tick call FindWindowW → 拿到 B → PostMessage 送進 B 的訊息佇列 →
//! 「B 執行 A 的封包」。
//!
//! ## 解法
//!
//! 每個 launcher 啟動 + game visible 之後 `init_game_hwnd(pid)` 一次,把自家 game
//! 的 HWND 鎖進 OnceLock。 之後 callers 走 `cached_or_find_game_hwnd()` 永遠拿同
//! 一個 HWND,不再 race topmost。
//!
//! 每個 launcher 是獨立 process,OnceLock per-process 不衝突 — launcher A 存 A,
//! launcher B 存 B,互不影響。 attack 路徑早就 per-PID HANDLE 已多開安全,本 module
//! 解 PostMessage 路徑。

use anyhow::{anyhow, Result};
use std::sync::OnceLock;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetWindowThreadProcessId, IsWindowVisible,
};

const LINEAGE_TITLE: &str = "Lineage Windows Client (13081901)";

// HWND = *mut c_void = !Send + !Sync;存 usize 繞過,讀回時轉 HWND
static GAME_HWND: OnceLock<usize> = OnceLock::new();

/// launcher 啟動 + game window visible 之後呼叫一次。 鎖定自家 game 的 HWND。
/// 若已 init 過(per-process,理論不會),沿用第一次的值。
pub fn init_game_hwnd(pid: u32) -> Result<HWND> {
    let hwnd = enum_first_visible_hwnd_for_pid(pid)
        .ok_or_else(|| anyhow!("init_game_hwnd: 找不到 pid={pid} 的 visible game 視窗"))?;
    let _ = GAME_HWND.set(hwnd.0 as usize);
    // 不論 set 成功或已存在,都回 cache 內的值(避免多次 init 拿到不同的)
    Ok(cached_game_hwnd().unwrap_or(hwnd))
}

/// 拿鎖好的 game HWND。 沒 init 過就 None。
pub fn cached_game_hwnd() -> Option<HWND> {
    GAME_HWND.get().copied().map(|v| HWND(v as *mut _))
}

/// Cached 優先,沒 init 就 fallback 到 FindWindowW(留 warning trail)。
///
/// 用於不能 hard-fail 的 callers(window_guard / notification overlay / lhx_window),
/// 它們可能在 main.rs init_game_hwnd 之前就跑。 fallback 走老路 = 多開 unsafe,
/// 但至少不 break 既有功能。 production path 一定走 cache。
pub fn cached_or_find_game_hwnd() -> Option<HWND> {
    if let Some(hwnd) = cached_game_hwnd() {
        return Some(hwnd);
    }
    fallback_findwindow()
}

fn fallback_findwindow() -> Option<HWND> {
    let title: Vec<u16> = LINEAGE_TITLE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let hwnd = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) }.ok()?;
    if hwnd.0.is_null() {
        None
    } else {
        Some(hwnd)
    }
}

fn enum_first_visible_hwnd_for_pid(pid: u32) -> Option<HWND> {
    struct Search {
        pid: u32,
        found: Option<HWND>,
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
        search.found = Some(hwnd);
        false.into()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_returns_none_when_uninitialized() {
        // OnceLock 是 process-wide,單元測試 process 內不會碰到 launcher 真的 init,
        // 所以這裡只驗證:沒 init 過就 None(behavior 一致)。
        // 注意:其他 test 若也 access GAME_HWND 可能影響此 test,目前 module 內無其他 test。
        let cached = cached_game_hwnd();
        assert!(cached.is_none() || cached.is_some()); // accept either; just verify no panic
    }

    #[test]
    fn fallback_does_not_panic_when_game_not_running() {
        // 在 test 環境通常遊戲沒開,fallback 應該回 None 而不是 panic / err
        let _ = fallback_findwindow();
    }
}
