//! 全局時鐘幀選擇。純函數：給「現在毫秒 + 設定」算出該畫第幾幀或休息。
//! 同 gfxid 全畫面自動同步（純為 now 的函數，無 per-widget 狀態）。
//!
//! 註：這是 launcher 側的參考實作 + 測試守門。實際執行時這個數學由 codecave
//! shellcode 在 game 內用 GetTickCount 算，兩邊公式必須一致；shellcode 以本函數為規格。
//! 因此 runtime 不直接呼叫本模組（僅單元測試引用），allow(dead_code)。
#![allow(dead_code)]

use launcher::dynamic_icon_format::AnimEntry;

/// 該 tick 要畫什麼。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 畫動畫第 idx 幀（對應 entry.frames[idx]）。
    Frame(usize),
    /// 休息期：畫原生靜態 TBT。
    Rest,
}

/// 給「絕對毫秒時鐘 now_ms」與設定，回該畫的 Phase。
/// 公式：t = now_ms % cycle；t < anim_dur → Frame(t / speed)；否則 Rest。
pub fn phase_at(e: &AnimEntry, now_ms: u32) -> Phase {
    let speed = e.speed_ms.max(1) as u32;
    let anim_dur = e.frames.len() as u32 * e.speed_ms as u32;
    let cycle = anim_dur + e.interval_ms;
    if cycle == 0 {
        return Phase::Rest;
    }
    let t = now_ms % cycle;
    if t < anim_dur {
        Phase::Frame((t / speed) as usize)
    } else {
        Phase::Rest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> AnimEntry {
        // 3 幀 × 100ms = 300ms 動畫；休息 200ms；cycle = 500ms
        AnimEntry {
            tbt: 1,
            speed_ms: 100,
            interval_ms: 200,
            frames: vec![10, 11, 12],
        }
    }

    #[test]
    fn frame_progression_within_anim() {
        let e = entry();
        assert_eq!(phase_at(&e, 0), Phase::Frame(0));
        assert_eq!(phase_at(&e, 99), Phase::Frame(0));
        assert_eq!(phase_at(&e, 100), Phase::Frame(1));
        assert_eq!(phase_at(&e, 250), Phase::Frame(2));
    }

    #[test]
    fn rest_after_anim() {
        let e = entry();
        assert_eq!(phase_at(&e, 300), Phase::Rest);
        assert_eq!(phase_at(&e, 499), Phase::Rest);
    }

    #[test]
    fn wraps_each_cycle() {
        let e = entry();
        // t=500 → 回到 cycle 起點 = Frame(0)
        assert_eq!(phase_at(&e, 500), Phase::Frame(0));
        assert_eq!(phase_at(&e, 800), Phase::Rest);
    }
}
