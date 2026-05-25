//! sprite_id → 物件類型(`102.type`)對照表。
//!
//! 對照 `SPR.txt`(== 遊戲內 `list.spz` 明文)第 `102.(N)` 指令的數字:
//!
//! | 值 | 類型 | bot 行為 |
//! |----|------|---------|
//! | 0  | 影子(人物/怪物影子、法術)| 跳過 |
//! | 1  | 裝飾品(聖誕樹、雪人、帳篷、船)| 跳過 |
//! | 5  | 玩家 / 不可對話 NPC | 跳過 |
//! | 6  | 可對話 NPC | 跳過 |
//! | 7  | 寶箱、開關 | 跳過 |
//! | 8  | 可打開的門 | 跳過 |
//! | 9  | 可撿取物品 | 跳過 |
//! | 10 | **MOB 怪物**(會出現攻擊符號) | **打** |
//! | 11 | 城牆、城門 | 跳過 |
//! | 12 | 新版可對話 NPC | 跳過 |
//! | 14/15/16/18-22 | 告示牌/門/未知 | 跳過 |
//!
//! 表格由 `build.rs` 在編譯期讀 `SPR.txt` 直接 codegen 成 static `[u8; N]`,runtime
//! 零 IO、零 parse、O(1) 查詢。 `0xFF` = SPR.txt 沒寫該 sprite_id(視作未知,**不打**)。

include!(concat!(env!("OUT_DIR"), "/sprite_types.rs"));

#[cfg(test)]
const TYPE_SHADOW: u8 = 0;
pub const TYPE_MOB: u8 = 10;
pub const TYPE_UNKNOWN: u8 = 0xFF;

/// 查 sprite_id 對應的 102.type。 出範圍 / SPR.txt 沒寫 → `None`。
#[inline]
pub fn sprite_type(sprite_id: u16) -> Option<u8> {
    let idx = sprite_id as usize;
    if idx >= SPRITE_TYPES_LEN {
        return None;
    }
    let t = SPRITE_TYPES[idx];
    if t == TYPE_UNKNOWN {
        None
    } else {
        Some(t)
    }
}

/// bot 過濾用 — 是否為 MOB 怪物(可攻擊)。 未知 / 影子 / 玩家 / 裝飾全回 `false`。
#[inline]
pub fn is_mob(sprite_id: u16) -> bool {
    sprite_type(sprite_id) == Some(TYPE_MOB)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 路燈 sprite_id=85(dump 驗證,`#81177:85`) — SPR.txt 寫 `102.(0)` 影子類
    #[test]
    fn torch_sprite_85_is_shadow_not_mob() {
        assert_eq!(sprite_type(85), Some(TYPE_SHADOW));
        assert!(!is_mob(85));
    }

    /// 真怪 sprite_id=2489(dump 驗證) — SPR.txt 寫 `102.(10)` MOB
    #[test]
    fn sprite_2489_is_mob() {
        assert_eq!(sprite_type(2489), Some(TYPE_MOB));
        assert!(is_mob(2489));
    }

    /// 出範圍 sprite_id(SPR.txt 不存在)→ None,bot 不該打
    #[test]
    fn out_of_range_sprite_returns_none() {
        assert_eq!(sprite_type(u16::MAX), None);
        assert!(!is_mob(u16::MAX));
    }

    /// 表至少含 1 萬筆,確認 build.rs codegen 真有跑
    #[test]
    fn table_has_substantial_entries() {
        assert!(SPRITE_TYPES_LEN >= 10_000, "SPR.txt parse 應產出 30000+ 條");
    }
}
