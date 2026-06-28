//! 技能攻擊 — Phase 1 走 `AttackEntity` 模式:寫 `[0x97C90C] = target_id` + `spell_book_cast`。
//!
//! Phase 1 限制:**只支援技能攻擊**(法師類 / 精靈技能職可玩)。 物理基本攻擊
//! C_ATTACK 0x1F 等 Phase 5 Frida spy RE 完成後才上。
//!
//! ## 為什麼每次都 rebuild SpellBook
//!
//! `SpellBook::build` 是一次 ReadProcessMemory + 字串解碼,成本約 10ms 內。 bot tick
//! 500ms,實際 skill cast 至少間隔 1-2s(技能 cooldown 制),負擔可忽略。

use anyhow::{anyhow, Result};
use windows::Win32::Foundation::HANDLE;

use crate::aux::drink_hook::SkillTargetMode;
use crate::aux::spell_book::SpellBook;

use super::bot_drink_handle;

/// 對指定怪物施放命名技能(C_SKILL 攻擊)。
///
/// 走 `SkillTargetMode::AttackEntity(target_id)`:shellcode 寫 `[0x97C90C] = target_id`
/// 後 call `spell_book_cast`。 client 端 dispatcher 看到 `[0x97C90C]!=0` → 走 `cccdhh`
/// 攻擊路徑 → 跑動畫 + 冷卻追蹤 + 送出跟玩家手動點怪 1:1 等價的封包。
///
/// ## 2026-05-13 RE 結論(踩過的坑)
///
/// 1. `Explicit(id)` 寫 `[0x97C910]` — **「請選擇目標」對話框** ← `0x97C910` 是物品 target,
///    對怪施法走那個 global 找不到 entity 就跳對話框。
/// 2. `ForceTargetPacket(id)` 送純 `cccd`(7B 封包,無座標) — **完全沒反應** ←
///    server 視作 inventory item 施法路徑。
/// 3. `ForceTargetPacketWithXY(id, x, y)` 送 `cccdhh`(11B) raw packet — bypass
///    spell_book_cast,**沒動畫**;server 似乎也沒接受(live test 沒反應)。
/// 4. **本 `AttackEntity(id)`(目前用)**:寫 `[0x97C90C] = id` 再 call spell_book_cast
///    讓客戶端自己組封包送 — 跟玩家手動點怪行為 1:1 等價,server 必接受。
///
/// 失敗情境:
/// - skill_name 不在玩家技能書 → `Err`
/// - SpellBook build 失敗(未進場 / 記憶體讀錯)→ `Err`
/// - RemoteThread 執行失敗 → `Err`
pub fn cast_damage_skill_at(h: HANDLE, skill_name: &str, target_id: u32) -> Result<()> {
    let book = SpellBook::build(h)?;
    let packed = book
        .lookup(skill_name)
        .ok_or_else(|| anyhow!("技能 {:?} 不在玩家技能書", skill_name))?;
    bot_drink_handle().execute_skill(h, packed, SkillTargetMode::AttackEntity(target_id))
}

/// 對自己施放自身 buff(走 ForceSelfPacket bypass spell_book_cast 內部 target 解析)。
///
/// 用途:Phase 2+ 自動 buff 補(順跑術 / 火焰武器 / 保護罩等)。 Phase 1 step 4 暫無
/// caller,先把 API 補齊以維持對稱性。
#[cfg(test)]
pub fn cast_self_buff(h: HANDLE, skill_name: &str) -> Result<()> {
    let book = SpellBook::build(h)?;
    let packed = book
        .lookup(skill_name)
        .ok_or_else(|| anyhow!("自身 buff 技能 {:?} 不在玩家技能書", skill_name))?;
    bot_drink_handle().execute_skill(h, packed, SkillTargetMode::ForceSelfPacket)
}
