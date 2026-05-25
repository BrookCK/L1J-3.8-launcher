//! BOT 全域熱鍵 — F8 toggle master enable, INS 開啟內掛設定視窗。
//!
//! ## 為什麼用 GetAsyncKeyState polling 而非 SetWindowsHookExW
//!
//! 既有 `aux::hotkey` 已經佔了一個 low-level keyboard hook(F1-F4 巨集);再裝一個
//! hook 會增加全 process 鍵盤事件 latency。 F8/INS 是低頻動作(每天按幾次),
//! 100ms polling 完全足夠,且不污染既有 hook chain。
//!
//! 跟 `img_hover.rs` 已驗證的 GetAsyncKeyState 模式相同。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::log_line;

use super::set_master_enabled;
use super::ui::show_bot_window;

/// VK_F8 — toggle master enable.
const VK_F8: i32 = 0x77;
/// VK_INSERT — open BotWindow settings.
const VK_INSERT: i32 = 0x2D;
const TOGGLE_MASTER_KEY_VK: i32 = VK_F8;
const TOGGLE_MASTER_KEY_LABEL: &str = "F8";
const OPEN_WINDOW_KEY_VK: i32 = VK_INSERT;
const OPEN_WINDOW_KEY_LABEL: &str = "INS";
/// poll 間隔(100ms,跟 img_hover 一致)
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[link(name = "user32")]
extern "system" {
    fn GetAsyncKeyState(vkey: i32) -> i16;
    fn GetForegroundWindow() -> isize;
    fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
}

pub fn spawn_toggle_listener(pid: u32, cancel: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        log_line!(
            "[bot/hotkey] hotkey listener 啟動({}=toggle master, {}=開設定視窗)",
            TOGGLE_MASTER_KEY_LABEL,
            OPEN_WINDOW_KEY_LABEL
        );
        let mut prev_toggle = false;
        let mut prev_open_window = false;
        let mut toggle_state = false;
        while !cancel.load(Ordering::Relaxed) {
            let toggle_down =
                unsafe { GetAsyncKeyState(TOGGLE_MASTER_KEY_VK) } as u16 & 0x8000 != 0;
            let open_window_down =
                unsafe { GetAsyncKeyState(OPEN_WINDOW_KEY_VK) } as u16 & 0x8000 != 0;
            let toggle_rising = toggle_down && !prev_toggle;
            let open_window_rising = open_window_down && !prev_open_window;
            prev_toggle = toggle_down;
            prev_open_window = open_window_down;

            if !is_game_foreground(pid) {
                thread::sleep(POLL_INTERVAL);
                continue;
            }

            if toggle_rising {
                toggle_state = !toggle_state;
                set_master_enabled(toggle_state);
                log_line!(
                    "[bot/hotkey] {} toggle → master {}",
                    TOGGLE_MASTER_KEY_LABEL,
                    if toggle_state { "ON" } else { "OFF" }
                );
            }
            if open_window_rising {
                log_line!(
                    "[bot/hotkey] {} pressed → 開設定視窗",
                    OPEN_WINDOW_KEY_LABEL
                );
                show_bot_window();
            }
            thread::sleep(POLL_INTERVAL);
        }
        log_line!("[bot/hotkey] hotkey listener 結束");
    })
}

/// 檢查當前前景視窗是否屬於遊戲 process。
fn is_game_foreground(target_pid: u32) -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd == 0 {
            return false;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        pid == target_pid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_toggle_hotkey_stays_f8() {
        assert_eq!(TOGGLE_MASTER_KEY_VK, VK_F8);
        assert_eq!(TOGGLE_MASTER_KEY_LABEL, "F8");
    }

    #[test]
    fn bot_window_hotkey_is_insert() {
        assert_eq!(OPEN_WINDOW_KEY_VK, VK_INSERT);
        assert_eq!(OPEN_WINDOW_KEY_LABEL, "INS");
    }
}
