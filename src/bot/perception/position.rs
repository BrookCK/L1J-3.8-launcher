//! 玩家座標觀察 — 讀 `[G_PLAYER_PTR]+0x34/+0x38`。
//!
//! ## offset 來源
//!
//! 2026-05-13 反組譯 `/loc` 指令的 sprintf caller `0x0040A849`:
//!
//! ```asm
//! mov edx, [0xc2d2b8]    ; G_PLAYER_PTR
//! mov eax, [edx+0x38]    ; Y(直接)
//! push eax
//! mov ecx, [0xc2d2b8]
//! mov edx, [ecx+0x34]    ; X_raw
//! sub edx, 0x8000
//! sar edx, 1              ; signed >> 1
//! add edx, 0x8000
//! push edx                ; X(已解碼)
//! push 0x8c8fa8           ; "location (%d, %d)"
//! ```
//!
//! 也就是 X 用「壓縮形式」存在 `+0x34`,顯示值 = `((raw - 0x8000) >> 1) + 0x8000`。
//! Y 直接讀。
//!
//! 解碼後跟 `/loc` 顯示值完全一致(實測 raw=33374 → display=33071)。
//!
//! ## 用途
//!
//! - Phase 3 移動:讀當前位置 → 跟目標距離計算 → 送 walk packet
//! - hunt 範圍判斷:卡牆 / 出範圍

use windows::Win32::Foundation::HANDLE;

use crate::aux::address::G_PLAYER_PTR;
use crate::memory::read_u32;

/// 玩家座標(已解碼為遊戲顯示值)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerPosition {
    pub x: i32,
    pub y: i32,
}

impl PlayerPosition {
    /// 從遊戲讀一次。 讀不到(未進場 / 指標 0)→ 回 None。
    pub fn read(h: HANDLE) -> Option<Self> {
        let player_ptr = read_u32(h, G_PLAYER_PTR).ok()?;
        if player_ptr == 0 {
            return None;
        }
        let x_raw = read_u32(h, player_ptr.wrapping_add(0x34)).ok()?;
        let y = read_u32(h, player_ptr.wrapping_add(0x38)).ok()?;
        Some(Self {
            x: decode_x(x_raw),
            y: y as i32,
        })
    }
}

/// 把 +0x34 的壓縮 X 解碼成 `/loc` 顯示值。
///
/// 公式:`display = ((raw - 0x8000) >> 1) + 0x8000`(算術右移,需保留正負號)
pub fn decode_x(raw_x: u32) -> i32 {
    let signed = raw_x as i32;
    ((signed.wrapping_sub(0x8000)) >> 1).wrapping_add(0x8000)
}

/// 反向 — 把遊戲顯示 X 編回 +0x34 的壓縮形式。
///
/// Phase 3 送 walk packet 時很可能需要,server 端可能用 raw 形式判斷距離。
/// 公式:`raw = ((display - 0x8000) << 1) + 0x8000`
#[cfg(test)]
pub fn encode_x(display_x: i32) -> u32 {
    (display_x
        .wrapping_sub(0x8000)
        .wrapping_shl(1)
        .wrapping_add(0x8000)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_x_matches_loc_output() {
        // 實測:raw=33374 → display=33071
        assert_eq!(decode_x(33374), 33071);
    }

    #[test]
    fn decode_then_encode_is_identity_on_origin() {
        // 0x8000(=32768)是 pivot,encode/decode 都不變
        assert_eq!(decode_x(0x8000), 0x8000);
        assert_eq!(encode_x(0x8000), 0x8000);
    }

    #[test]
    fn encode_decode_round_trip() {
        // 顯示座標 → raw → display 應該回到原值(偶數 offset)
        for display in [33071, 33000, 32768, 32500, 33500] {
            let raw = encode_x(display);
            assert_eq!(decode_x(raw), display, "round trip failed for {display}");
        }
    }

    #[test]
    fn decode_below_pivot() {
        // raw < 0x8000 應該回到 < 0x8000(壓縮是線性的)
        // raw=0x7000(28672) → display = ((28672-32768)>>1)+32768 = (-4096>>1)+32768 = -2048+32768 = 30720
        assert_eq!(decode_x(0x7000), 30720);
    }
}
