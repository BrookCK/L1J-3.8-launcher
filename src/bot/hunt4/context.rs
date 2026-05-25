use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::bot::decide::hunt::{AttackSequenceStep, HuntConfig};
use crate::bot::hunt4::actions::DispatchState;
use crate::bot::hunt4::memory::TacticalMemory;
use crate::bot::hunt4::model::Snapshot;
use crate::bot::hunt4::observe::{snapshot_lock, ActionLabel, MemorySummary, StateReport};
use crate::bot::hunt4::state::{
    DisabledReason, EngageIntent, HuntState, RecoveryCause, StopReason,
};
use crate::bot::hunt4::step::{
    AttackStepInput, ExploreSuggestion, LastOutcome, StateLabel, TargetCandidate, Transition,
};

#[derive(Debug, Clone, Default)]
pub struct AttackSequenceRuntime {
    active_index: usize,
    cycle_started_at: Option<Instant>,
    next_ready_at: Option<Instant>,
}

impl AttackSequenceRuntime {
    pub fn current_step(&mut self, cfg: &HuntConfig, now: Instant) -> Option<AttackSequenceStep> {
        let steps = cfg.effective_attack_sequence();
        if steps.is_empty() {
            return None;
        }
        if self.active_index >= steps.len() {
            self.active_index = 0;
            self.cycle_started_at = Some(now);
            self.next_ready_at = None;
        }
        if self.cycle_started_at.is_none() {
            self.cycle_started_at = Some(now);
        }
        if self.next_ready_at.is_some_and(|ready_at| now < ready_at) {
            return None;
        }
        steps.get(self.active_index).cloned()
    }

    pub fn advance_after_attack(&mut self, cfg: &HuntConfig, now: Instant) {
        let steps = cfg.effective_attack_sequence();
        if steps.is_empty() {
            self.active_index = 0;
            self.cycle_started_at = None;
            self.next_ready_at = None;
            return;
        }

        if self.active_index >= steps.len() {
            self.active_index = 0;
        }
        let cycle_started_at = *self.cycle_started_at.get_or_insert(now);
        let step_ready_at = now + Duration::from_millis(steps[self.active_index].interval_ms);
        let next_index = self.active_index + 1;

        if next_index >= steps.len() {
            self.active_index = 0;
            let cycle_ready_at = if cfg.attack_sequence_cycle_ms == 0 {
                now
            } else {
                cycle_started_at + Duration::from_millis(cfg.attack_sequence_cycle_ms)
            };
            let ready_at = step_ready_at.max(cycle_ready_at);
            self.next_ready_at = Some(ready_at);
            self.cycle_started_at = Some(ready_at);
        } else {
            self.active_index = next_index;
            self.next_ready_at = Some(step_ready_at);
        }
    }
}

pub struct HuntContext {
    pub state: HuntState,
    pub memory: TacticalMemory,
    pub dispatch_state: DispatchState,
    pub attack_watch: AttackProgressWatch,
    pub last_outcome: LastOutcome,
    pub last_action: Option<ActionLabel>,
    pub last_target_progress: Option<Instant>,
    pub since: Instant,
    pub last_transition: Option<Transition>,
    pub prev_hp: Option<u32>,
    pub last_map_id: Option<u32>,
    pub last_walk_tile: Option<(i32, i32)>,
    pub cached_skill_gate_ms: HashMap<String, u64>,
    pub cached_skill_range: HashMap<String, u32>,
    pub attack_sequence: AttackSequenceRuntime,
    pub last_target_summary: Option<super::score::TargetScoreSummary>,
    pub last_route_next_tile: Option<(i32, i32)>,
    pub last_route_reason: Option<String>,
    pub last_teleport_reason: Option<String>,
    pub last_shadow_dispatch: Option<super::intent::ShadowDispatch>,
    pub last_dispatch_choice: Option<super::intent::DispatchChoice>,
    pub last_policy_comparison: Option<super::policy::PolicyComparison>,
    pub policy_telemetry: super::policy::PolicyTelemetry,
}

impl HuntContext {
    pub fn new(now: Instant) -> Self {
        Self {
            state: HuntState::Disabled {
                reason: DisabledReason::MasterOff,
            },
            memory: TacticalMemory::default(),
            dispatch_state: DispatchState::default(),
            attack_watch: AttackProgressWatch::default(),
            last_outcome: LastOutcome::None,
            last_action: None,
            last_target_progress: None,
            since: now,
            last_transition: None,
            prev_hp: None,
            last_map_id: None,
            last_walk_tile: None,
            cached_skill_gate_ms: HashMap::new(),
            cached_skill_range: HashMap::new(),
            attack_sequence: AttackSequenceRuntime::default(),
            last_target_summary: None,
            last_route_next_tile: None,
            last_route_reason: None,
            last_teleport_reason: None,
            last_shadow_dispatch: None,
            last_dispatch_choice: None,
            last_policy_comparison: None,
            policy_telemetry: super::policy::PolicyTelemetry::default(),
        }
    }

    pub fn report(&self) -> StateReport {
        StateReport {
            current_label: label_of_state(&self.state),
            since: self.since,
            last_transition: self.last_transition.clone(),
            last_action: self.last_action.clone(),
            last_outcome: Some(self.last_outcome.clone()),
            last_position_change: self.memory.last_position_change,
            last_target_progress: self.last_target_progress,
            lock_summary: snapshot_lock(&self.state),
            memory_summary: MemorySummary {
                obstacle_count: self.memory.obstacles.len() + self.memory.portal_avoid_tiles.len(),
                failed_target_count: self.memory.failed_targets.len(),
            },
            target_summary: self.last_target_summary.clone(),
            route_next_tile: self.last_route_next_tile,
            route_reason: self.last_route_reason.clone(),
            teleport_reason: self.last_teleport_reason.clone(),
            #[cfg(test)]
            shadow_dispatch: self.last_shadow_dispatch.clone(),
            dispatch_choice: self.last_dispatch_choice.clone(),
            policy_comparison: self.last_policy_comparison.clone(),
            policy_telemetry: self.policy_telemetry.clone(),
        }
    }
}

/// 撌脤?餅?雿??遙雿?damage packet ?捆敹???///
/// ?格??亙歇蝬???????/ 蝛箸除嚗hellcode 隞?賢??單???
/// Duration with no observed damage before the active attack is considered stalled.
/// Live packet damage parsing is still discovery-only, so this must allow
/// normal 3-4 second kills to finish before marking the lock as failed.
pub const ATTACK_NO_PROGRESS_DURATION: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default)]
pub struct AttackProgressWatch {
    pub target_id: Option<u32>,
    pub no_progress_since: Option<Instant>,
}

fn label_of_state(state: &HuntState) -> StateLabel {
    match state {
        HuntState::Disabled {
            reason: DisabledReason::MasterOff,
        } => StateLabel::DisabledMasterOff,
        HuntState::Disabled {
            reason: DisabledReason::NotInGame,
        } => StateLabel::DisabledNotInGame,
        HuntState::Idle => StateLabel::Idle,
        HuntState::Engaging {
            intent: EngageIntent::Approach,
            ..
        } => StateLabel::EngagingApproach,
        HuntState::Engaging {
            intent: EngageIntent::Attack,
            ..
        } => StateLabel::EngagingAttack,
        HuntState::Engaging {
            intent: EngageIntent::KillConfirm,
            ..
        } => StateLabel::EngagingKillConfirm,
        HuntState::Exploring { .. } => StateLabel::Exploring,
        HuntState::Recovering {
            cause: RecoveryCause::WalkStuck,
            ..
        } => StateLabel::RecoveringWalkStuck,
        HuntState::Recovering {
            cause: RecoveryCause::AttackFailed,
            ..
        } => StateLabel::RecoveringAttackFailed,
        HuntState::Recovering {
            cause: RecoveryCause::DamageSpike,
            ..
        } => StateLabel::RecoveringDamageSpike,
        HuntState::Recovering {
            cause: RecoveryCause::NoReachableTarget,
            ..
        } => StateLabel::RecoveringNoReachableTarget,
        HuntState::Recovering {
            cause: RecoveryCause::CriticalHp,
            ..
        } => StateLabel::RecoveringCriticalHp,
        HuntState::Escaping { .. } => StateLabel::Escaping,
        HuntState::Stopped {
            reason: StopReason::Died,
        } => StateLabel::StoppedDied,
        #[cfg(test)]
        HuntState::Stopped {
            reason: StopReason::Disconnected,
        } => StateLabel::StoppedDisconnected,
        #[cfg(test)]
        HuntState::Stopped {
            reason: StopReason::Manual,
        } => StateLabel::StoppedManual,
    }
}

pub struct PureTickInputs<'a> {
    pub snapshot: &'a Snapshot,
    pub candidates: &'a [TargetCandidate],
    pub explore: Option<&'a ExploreSuggestion>,
    pub player_pos: Option<(i32, i32)>,
    pub player_alive: bool,
    pub master_enabled: bool,
    pub in_game: bool,
    pub now: Instant,
    pub state_since: Instant,
    pub cfg: &'a HuntConfig,
    pub teleport_scroll_available: bool,
    pub damage_spike_detected: bool,
    pub critical_hp_detected: bool,
    pub locked_target_removed_or_dead: bool,
    pub locked_target_recently_damaged: bool,
    pub skill_cd_ms: u64,
    pub attack_step: AttackStepInput,
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn report_carries_observed_position_and_target_progress() {
        let now = Instant::now();
        let mut ctx = HuntContext::new(now);
        ctx.memory.last_position_change = Some(now - Duration::from_secs(2));
        ctx.last_target_progress = Some(now - Duration::from_millis(500));

        let report = ctx.report();

        assert_eq!(
            report.last_position_change,
            Some(now - Duration::from_secs(2))
        );
        assert_eq!(
            report.last_target_progress,
            Some(now - Duration::from_millis(500))
        );
    }
}
