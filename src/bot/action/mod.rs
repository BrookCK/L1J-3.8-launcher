//! 動作層 — bot 主動送封包 / 呼叫遊戲函數的入口。
//!
//! ## 為什麼跟 `aux/drink_hook` 共用 SendPacketData / DrinkHandle 但持自己的 instance
//!
//! bot 跟既有 drink_hook 喝水助手要**同時運作互不干涉**。 共用底層 `DrinkHandle` API
//! 沒問題(它是 thread-safe + 走 RemoteThread),但兩邊應該各持一個 `DrinkHandle`
//! instance,各自管理:
//! - **last_drink cooldown**(避免抖動)
//! - **prologue_snapshot**(packer 重新加密 USE_ITEM 時各自警示)
//!
//! 共用 instance 會讓兩邊 cooldown 干擾(例如 aux 剛喝完 0.5s 內,bot 想再喝就被擋)。
//!
//! ## Phase 進度
//!
//! - **skill.rs**: C_SKILL 攻擊 / buff(Phase 1)
//! - **attack.rs**: C_ATTACK / C_FAR_ATTACK 物理攻擊(Phase 1,2026-05-13 完成 — 對齊 L1JGO
//!   Whale server `internal/handler/attack.go` 開出的 opcode 229 / 123)
//! - consume.rs: HP/MP 補品(Phase 2)
//! - teleport.rs: 回家卷 + 變身卷(Phase 2-3)
//! - shop.rs / storage.rs: NPC 互動(Phase 4)

pub mod attack;
pub mod screen_target;
pub mod skill;
pub mod walk;

use once_cell::sync::Lazy;

use crate::aux::drink_hook::DrinkHandle;

/// 與 LHX 完全分離的 USE_ITEM 位址 — 跟 `main.rs::LHX_USE_ITEM_ADDR` 同值。
///
/// 不去 import 既有常數是避免 bot 模組綁死 main.rs;3.8 client 這個位址固定,
/// 已驗證 ★★★★★(MEMORY.md / drink_hook.rs:21)。
const BOT_USE_ITEM_ADDR: u32 = 0x004B3EE0;

/// bot 專屬 DrinkHandle — 全域 lazy,首次 cast 時建立。
///
/// 跟既有 LHX 用的 DrinkHandle 是兩個獨立 instance:cooldown / prologue_snapshot
/// 各自管理,兩邊同時噴封包不會互相 cooldown lockout。
pub fn bot_drink_handle() -> &'static DrinkHandle {
    static HANDLE: Lazy<DrinkHandle> = Lazy::new(|| DrinkHandle::new(BOT_USE_ITEM_ADDR));
    &HANDLE
}
