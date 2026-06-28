//! 玩家裝備武器類型偵測 — 跨 process 讀「obfuscated weapon class」globals。
//!
//! ## 為什麼存在
//!
//! Bot 攻擊路徑要決定「該走到幾格內才開打」:melee 武器只能貼身打(range=1),
//! ranged 武器(弓/弩)可以在 range=8 fire。 過去 UI 讓使用者手選 melee/ranged,
//! 但「自己拿什麼武器」遊戲早就知道,沒理由再讓 user 動手。
//!
//! ## 遊戲怎麼判
//!
//! `click_attack` dispatcher @ `0x5A3770` 內(0x5A38EE-0x5A395C)有一段 4 次 Equals
//! 測試,跟 archery skill dispatcher @ `0x73F461` 用同一組 magic IDs:
//!
//! ```text
//! is_ranged =
//!     container@0xBDC7D4 == 0x14     // RANGED_WEAPON_CLASS_A
//!  || container@0xBDC7C8 == 0x448    // RANGED_WEAPON_CLASS_B
//!  || container@0xBDC7C8 == 0x465    // RANGED_WEAPON_CLASS_C
//!  || container@0xBDC7D4 == 0x3E     // RANGED_WEAPON_CLASS_D
//! ```
//!
//! 命中其中一個 → 送 `C_FAR_ATTACK` opcode `0x7B`;否則 → 送 `C_ATTACK` `0xE5`。
//!
//! 兩個 container address (`0xBDC7C8` / `0xBDC7D4`) 看起來是裝備武器在不同 ID space
//! 的 mirror — 一個放 item-class,一個放 weapon-category(僅推測,反正 4 個 magic ID
//! 都對應 ranged class,語意上等價於「有沒有裝備遠程武器」)。
//!
//! ## Container layout
//!
//! 每個 container 是 12 bytes 的 obfuscated single-int wrapper:
//!
//! ```text
//! [container+0]  (4B)  key1            (XOR with 0xC0017921 得 array index)
//! [container+4]  (4B)  data_ptr        (heap 上的 u32 陣列基址)
//! [container+8]  (4B)  xor_key         (data XOR'd 才是真實值)
//! ```
//!
//! 解碼公式: `value = *(u32*)(data_ptr + (key1 ^ 0xC0017921) * 4) ^ xor_key`
//!
//! 反組譯來源:`0x40A5A0`(Equals)/ `0x402800`(GetValue)— 兩個 getter 走同一條
//! XOR chain。 已知 `key1 ^ 0xC0017921 == 0` for `0xBDC7C8` / `0xBDC7D4`,
//! 但程式碼仍走通用公式以防其他 container 用上。
//!
//! ## Fail-soft
//!
//! 任何一步讀失敗(ReadProcessMemory 錯、data_ptr 為 NULL、heap 還沒分配)→ 視為
//! melee(range=1)。 理由:bot 寧可貼到 1 格範圍開打(對所有武器都會送出 packet,
//! ranged 武器只是 over-approached,不會打不到),不要錯誤回 ranged 讓 bow 站在 8 格
//! 卻拿劍打空。

use anyhow::{Context, Result};
use windows::Win32::Foundation::HANDLE;

use crate::memory::read_u32;

/// 兩個遊戲端用的武器類型 obfuscated container — `0x5A38EE` / `0x73F461` 都讀這兩格。
const WEAPON_CONTAINER_D4: u32 = 0x00BDC7D4;
const WEAPON_CONTAINER_C8: u32 = 0x00BDC7C8;

/// 解碼用常數 — 跟 `0x40A5A0` 內 `xor ecx, 0xC0017921` immediate 一致。
const KEY1_XOR: u32 = 0xC0017921;

/// 命中即視為 ranged 的 4 個 weapon-class magic ID(來自 dispatcher 內嵌立即數)。
const RANGED_CLASS_D4_A: u32 = 0x14;
const RANGED_CLASS_D4_B: u32 = 0x3E;
const RANGED_CLASS_C8_A: u32 = 0x448;
const RANGED_CLASS_C8_B: u32 = 0x465;

/// 偵測玩家目前是否裝備遠程武器 — 失敗(進場前/讀記憶體錯)一律回 `false` 視為 melee。
pub fn is_ranged_weapon_equipped(h: HANDLE) -> bool {
    let d4 = read_obfuscated_int(h, WEAPON_CONTAINER_D4).unwrap_or(0);
    let c8 = read_obfuscated_int(h, WEAPON_CONTAINER_C8).unwrap_or(0);
    matches!(d4, RANGED_CLASS_D4_A | RANGED_CLASS_D4_B)
        || matches!(c8, RANGED_CLASS_C8_A | RANGED_CLASS_C8_B)
}

/// 讀單個 obfuscated container — 給 diagnostic 跟 unit test 用,正常呼叫走上面 helper。
fn read_obfuscated_int(h: HANDLE, container_addr: u32) -> Result<u32> {
    let key1 = read_u32(h, container_addr)
        .with_context(|| format!("讀 container.key1 @ 0x{container_addr:08X}"))?;
    let data_ptr = read_u32(h, container_addr + 4)
        .with_context(|| format!("讀 container.data_ptr @ 0x{:08X}", container_addr + 4))?;
    let xor_key = read_u32(h, container_addr + 8)
        .with_context(|| format!("讀 container.xor_key @ 0x{:08X}", container_addr + 8))?;

    if data_ptr == 0 {
        anyhow::bail!("container @ 0x{container_addr:08X} 的 data_ptr 為 NULL — 玩家未進場");
    }
    let index = key1 ^ KEY1_XOR;
    let slot_addr = data_ptr.wrapping_add(index.wrapping_mul(4));
    let raw =
        read_u32(h, slot_addr).with_context(|| format!("讀 container.slot @ 0x{slot_addr:08X}"))?;
    Ok(raw ^ xor_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranged_magic_ids_match_dispatcher_immediates() {
        // 守住 4 個 magic ID 不被誤改 — 變動代表跟 0x5A38EE / 0x73F461 不同步。
        assert_eq!(RANGED_CLASS_D4_A, 0x14);
        assert_eq!(RANGED_CLASS_D4_B, 0x3E);
        assert_eq!(RANGED_CLASS_C8_A, 0x448);
        assert_eq!(RANGED_CLASS_C8_B, 0x465);
    }

    #[test]
    fn key1_xor_matches_dispatcher_immediate() {
        assert_eq!(KEY1_XOR, 0xC0017921);
    }
}
