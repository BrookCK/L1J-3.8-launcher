use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HuntState {
    Disabled {
        reason: DisabledReason,
    },
    Idle,
    Engaging {
        lock: TargetLock,
        intent: EngageIntent,
        path: Option<Vec<(i32, i32)>>,
    },
    Exploring {
        goal: (i32, i32),
        path: Vec<(i32, i32)>,
    },
    Recovering {
        cause: RecoveryCause,
        until: Instant,
    },
    Escaping {
        scroll_used_at: Instant,
        wait_until: Instant,
        origin_pos: Option<(i32, i32)>,
    },
    Stopped {
        reason: StopReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledReason {
    MasterOff,
    NotInGame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngageIntent {
    Approach,
    Attack,
    KillConfirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryCause {
    WalkStuck,
    AttackFailed,
    DamageSpike,
    NoReachableTarget,
    CriticalHp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Died,
    #[cfg(test)]
    Disconnected,
    #[cfg(test)]
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetLock {
    pub target_id: u32,
    pub entity_ptr: u32,
    pub name: String,
    pub acquired_at: Instant,
    pub last_seen: Instant,
    pub bootstrapped: bool,
}
