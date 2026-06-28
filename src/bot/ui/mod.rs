//! 內掛 UI — 獨立 NWG top-level 視窗,跟 LHX 完全切開。
//!
//! ## 啟動入口
//!
//! 遊戲內按 **INS** 鍵 → 開啟 BotWindow(由 `bot::hotkey` 偵測 + 呼 `show_bot_window`)。
//! 視窗在獨立 thread 跑 NWG dispatch loop,關閉時 thread 結束。
//!
//! ## Phase 1 範圍
//!
//! 最簡 NWG 對話框,**不追求視覺仿真**(操8.8 風格仿真留到 Phase 6)。 控件:
//! - master enable checkbox
//! - 攻擊技能名 text input
//! - 怪物白名單 multi-line text(每行一個)
//! - cooldown ms text input
//! - 儲存 / 關閉 按鈕

pub mod window;

pub use window::is_bot_window_open;
pub use window::show_bot_window;
