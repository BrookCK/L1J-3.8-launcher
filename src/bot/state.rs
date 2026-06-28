//! BotState — 內掛主狀態機。
//!
//! Phase 1 只 3 個狀態:
//! - `Idle` — 內掛已 install 但 master toggle 關閉或 game_state != 3
//! - `Hunting` — 進入遊戲 + master toggle 開啟,跑狩獵循環
//! - `Stopped` — 觸發停損(死亡/斷線/手動停)後永久停止,要 user 手動重啟
//!
//! Phase 2+ 會擴充 `Returning` / `Resupplying` / `TransportingBack` 子狀態。

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotState {
    Idle,
    Hunting,
    Stopped,
}

impl BotState {
    /// 合法轉移表 — state machine 的不變量。
    ///
    /// `Stopped → Idle`:**允許**,讓 user 重啟下一場遊戲時可以重新開始
    /// `Hunting → Idle`:**禁止**,Hunting 中斷只能透過 Stopped(避免半開半關狀態)
    pub fn can_transition_to(self, next: BotState) -> bool {
        use BotState::*;
        matches!(
            (self, next),
            (Idle, Hunting)
                | (Idle, Stopped)
                | (Hunting, Stopped)
                | (Stopped, Idle)
                | (Stopped, Stopped)
                | (Idle, Idle)
                | (Hunting, Hunting)
        )
    }
}

impl fmt::Display for BotState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BotState::Idle => write!(f, "Idle"),
            BotState::Hunting => write!(f, "Hunting"),
            BotState::Stopped => write!(f, "Stopped"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legal_transitions_allowed() {
        assert!(BotState::Idle.can_transition_to(BotState::Hunting));
        assert!(BotState::Idle.can_transition_to(BotState::Stopped));
        assert!(BotState::Hunting.can_transition_to(BotState::Stopped));
        assert!(BotState::Stopped.can_transition_to(BotState::Idle));
    }

    #[test]
    fn illegal_transitions_rejected() {
        // Hunting 不能直接回 Idle(必須先進 Stopped)
        assert!(!BotState::Hunting.can_transition_to(BotState::Idle));
    }

    #[test]
    fn self_loop_is_legal() {
        assert!(BotState::Idle.can_transition_to(BotState::Idle));
        assert!(BotState::Hunting.can_transition_to(BotState::Hunting));
        assert!(BotState::Stopped.can_transition_to(BotState::Stopped));
    }
}
