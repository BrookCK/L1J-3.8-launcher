//! 遊戲右鍵選單注入(Phase 2)
//!
//! 對齊使用者要求 — 右鍵物品時,在原生「鑑定 / 溶解」之後 append:
//! - `加到溶解名單` → push 進 `AuxSettings.dissolve_list`
//! - `加到刪除名單` → push 進 `AuxSettings.delete_list`
//! - `名稱複製` → 把物品名抓進系統剪貼簿(`arboard`)
//!
//! Phase 2 的 `install` / `poll_ring` 骨架(RE 未完成,長期回 bail / 回 0)已於
//! 死碼清理時移除;之後真正實作右鍵選單注入時再依設計文件重建。
//! 設計細節見 docs/superpowers/specs/2026-04-27-lhx-window-skeleton-design.md。
