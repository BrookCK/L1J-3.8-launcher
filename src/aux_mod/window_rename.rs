//! 視窗改亂碼名(防外掛)— 把本登入器啟動的 game 主視窗標題改成隨機英數,
//! 斷掉外部外掛 FindWindow(title) 定位。只動本進程 game HWND(走 game_window cache)。

use anyhow::{anyhow, Result};
use std::iter::once;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::WindowsAndMessaging::SetWindowTextW;

const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// 由 seed 產生 8-16 字的隨機 [A-Za-z0-9] 標題(xorshift64,無外部 rng 依賴)。
pub fn random_window_title(seed: u64) -> String {
    let mut x = seed | 1; // 避免 0 → xorshift 全 0 卡死
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let len = 8 + (next() % 9) as usize; // 8..=16
    let mut s = String::with_capacity(len);
    for _ in 0..len {
        let idx = (next() % CHARSET.len() as u64) as usize;
        s.push(CHARSET[idx] as char);
    }
    s
}

/// 改寫指定 HWND 標題為隨機亂碼,回傳實際寫入的標題。
pub fn apply_random_title(hwnd: HWND, pid: u32) -> Result<String> {
    let seed = seed_from(pid);
    let title = random_window_title(seed);
    let wide: Vec<u16> = title.encode_utf16().chain(once(0)).collect();
    unsafe {
        SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()))
            .map_err(|e| anyhow!("SetWindowTextW 失敗: {e:#}"))?;
    }
    Ok(title)
}

fn seed_from(pid: u32) -> u64 {
    let tick = unsafe { GetTickCount() };
    ((pid as u64) << 32) ^ (tick as u64) ^ 0x9E37_79B9_7F4A_7C15
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_length_in_range() {
        for seed in [1u64, 42, 99999, 0xDEAD_BEEF, u64::MAX] {
            let t = random_window_title(seed);
            assert!(t.len() >= 8 && t.len() <= 16, "len={} seed={seed}", t.len());
        }
    }

    #[test]
    fn title_charset_is_ascii_alnum() {
        let t = random_window_title(0x1234_5678);
        assert!(t.chars().all(|c| c.is_ascii_alphanumeric()), "got {t}");
    }

    #[test]
    fn same_seed_is_deterministic_diff_seed_differs() {
        assert_eq!(random_window_title(7), random_window_title(7));
        assert_ne!(random_window_title(7), random_window_title(8));
    }
}
