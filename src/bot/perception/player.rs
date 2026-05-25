//! 玩家狀態觀察 — 包裝 `aux::player_state::read_player_state`。
//!
//! Phase 1 用途:
//! - **alive 偵測**:hp > 0 → 活著;hp == 0 且 max_hp > 0 → 死亡(觸發 Stopped)
//! - **HP/MP 百分比**:給 UI 顯示 + 後續 phase 的觸發閾值(回家 when HP < X%)
//!
//! ## 死亡判定的細節
//!
//! `max_hp == 0` 代表玩家尚未進場(state != 3),這時 hp 也是 0。 純看 hp 會誤判為
//! 「永遠死亡」,所以判定要兩個條件都成立:`max_hp > 0 && hp == 0`。

use anyhow::Result;
use windows::Win32::Foundation::HANDLE;

use crate::aux::player_state::{read_player_state, PlayerState};

/// 玩家觀察快照 — 拿到後可在 tick 內任意使用,不再碰遊戲記憶體。
#[derive(Debug, Clone)]
pub struct PlayerView {
    pub raw: PlayerState,
}

impl PlayerView {
    /// 從遊戲讀一次最新狀態。 失敗(讀記憶體 fail)→ 回 default(全 0)
    /// 而非 propagate,因為 bot tick 不應該因為一次讀失敗就停手。
    pub fn read(h: HANDLE) -> Result<Self> {
        let raw = read_player_state(h)?;
        Ok(Self { raw })
    }

    /// 玩家還活著?— 必須 `max_hp > 0`(已進場)且 `hp > 0`
    pub fn alive(&self) -> bool {
        self.raw.max_hp > 0 && self.raw.hp > 0
    }

    /// 玩家是否已死亡(進場了但 hp == 0)
    #[cfg(test)]
    pub fn dead(&self) -> bool {
        self.raw.max_hp > 0 && self.raw.hp == 0
    }

    /// HP 百分比(0-100)。 `max_hp == 0` 時回 0 而非 NaN
    #[cfg(test)]
    pub fn hp_pct(&self) -> u8 {
        if self.raw.max_hp == 0 {
            return 0;
        }
        let pct = (self.raw.hp as u64 * 100) / self.raw.max_hp as u64;
        pct.min(100) as u8
    }

    /// MP 百分比(0-100)
    #[cfg(test)]
    pub fn mp_pct(&self) -> u8 {
        if self.raw.max_mp == 0 {
            return 0;
        }
        let pct = (self.raw.mp as u64 * 100) / self.raw.max_mp as u64;
        pct.min(100) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_with(hp: u32, max_hp: u32, mp: u32, max_mp: u32) -> PlayerView {
        PlayerView {
            raw: PlayerState {
                hp,
                max_hp,
                mp,
                max_mp,
                food: 0,
                weight: 0,
                map_id: 0,
            },
        }
    }

    #[test]
    fn alive_requires_max_hp_and_hp() {
        assert!(view_with(100, 1000, 0, 0).alive());
        // max_hp == 0(未進場)→ 不算活
        assert!(!view_with(100, 0, 0, 0).alive());
        // hp == 0(已死亡)→ 不算活
        assert!(!view_with(0, 1000, 0, 0).alive());
    }

    #[test]
    fn dead_only_when_in_game_and_zero_hp() {
        assert!(view_with(0, 1000, 0, 0).dead());
        // 未進場(max_hp=0)不算死亡,只是未初始化
        assert!(!view_with(0, 0, 0, 0).dead());
        // 還在 hunting 不算死
        assert!(!view_with(100, 1000, 0, 0).dead());
    }

    #[test]
    fn hp_pct_clamped_and_safe() {
        assert_eq!(view_with(500, 1000, 0, 0).hp_pct(), 50);
        assert_eq!(view_with(0, 1000, 0, 0).hp_pct(), 0);
        assert_eq!(view_with(1000, 1000, 0, 0).hp_pct(), 100);
        // max_hp == 0 → 不 panic
        assert_eq!(view_with(0, 0, 0, 0).hp_pct(), 0);
    }

    #[test]
    fn mp_pct_clamped_and_safe() {
        assert_eq!(view_with(0, 0, 500, 1000).mp_pct(), 50);
        assert_eq!(view_with(0, 0, 0, 0).mp_pct(), 0);
    }
}
