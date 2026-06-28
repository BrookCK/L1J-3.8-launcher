//! Minimap Phase 1 — 獨立浮動視窗顯示當前 map 牆壁 + 玩家 + 怪物。
//!
//! 模組布局 + 設計詳見 `docs/superpowers/specs/2026-05-14-minimap-design.md`。
//!
//! Phase 1 對外只暴露 `show_minimap()`(從 BotWindow 點按鈕觸發)— launcher 主程式
//! 不必呼 shutdown,視窗關閉時 NWG dispatch 結束,thread 自然退出。

pub mod cache;
pub mod coord;
pub mod map_loader;
pub mod nav_grid;
pub mod nav_profile;
pub mod renderer;
pub mod s32_parser;
pub mod snapshot;
pub mod window;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use windows::Win32::Foundation::HANDLE;

use crate::log_line;

use self::map_loader::Map;

/// 視窗是否開著 — 避免 BotWindow 重複按按鈕多開
static IS_OPEN: AtomicBool = AtomicBool::new(false);

/// 取得目前地圖資料；cache miss 時直接從 client map 檔載入。
///
/// bot 與小地圖 UI 都應走這個入口，讓尋路資料來源跟 UI 是否開啟解耦。
pub fn get_or_load_map(map_id: u32) -> Result<Arc<Map>> {
    cache::global().get_or_load(map_id, || map_loader::load(map_id))
}

pub fn set_game_dir(game_dir: impl Into<std::path::PathBuf>) {
    map_loader::set_game_root(game_dir);
}

/// 從 BotWindow「開啟小地圖」按鈕呼。 若已開 → log + no-op。
pub fn show_minimap(h: HANDLE) {
    if IS_OPEN.swap(true, Ordering::AcqRel) {
        log_line!("[minimap] 視窗已開,no-op");
        return;
    }
    let h_raw = h.0 as usize;
    std::thread::spawn(move || {
        window::run_window(h_raw);
        IS_OPEN.store(false, Ordering::Release);
    });
}
