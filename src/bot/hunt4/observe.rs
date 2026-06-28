use std::time::Instant;

use crate::bot::hunt4::actions::DispatchIntent;
use crate::bot::hunt4::intent::DispatchChoice;
#[cfg(test)]
use crate::bot::hunt4::intent::ShadowDispatch;
use crate::bot::hunt4::policy::{PolicyComparison, PolicyTelemetry};
use crate::bot::hunt4::score::TargetScoreSummary;
use crate::bot::hunt4::state::{EngageIntent, HuntState};
use crate::bot::hunt4::step::{Action, LastOutcome, StateLabel, Transition};
use crate::log_line;

#[derive(Debug, Clone)]
pub struct StateReport {
    pub current_label: StateLabel,
    pub since: Instant,
    pub last_transition: Option<Transition>,
    pub last_action: Option<ActionLabel>,
    pub last_outcome: Option<LastOutcome>,
    pub last_position_change: Option<Instant>,
    pub last_target_progress: Option<Instant>,
    pub lock_summary: Option<LockSummary>,
    pub memory_summary: MemorySummary,
    pub target_summary: Option<TargetScoreSummary>,
    pub route_next_tile: Option<(i32, i32)>,
    pub route_reason: Option<String>,
    pub teleport_reason: Option<String>,
    #[cfg(test)]
    pub shadow_dispatch: Option<ShadowDispatch>,
    pub dispatch_choice: Option<DispatchChoice>,
    pub policy_comparison: Option<PolicyComparison>,
    pub policy_telemetry: PolicyTelemetry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionLabel {
    Wait,
    WalkTo { tile: (i32, i32) },
    Attack { target_id: u32, with_skill: bool },
    UseScroll,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockSummary {
    pub target_id: u32,
    pub name: String,
    pub intent: &'static str,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemorySummary {
    pub obstacle_count: usize,
    pub failed_target_count: usize,
}

pub fn record_transition(t: &Transition) {
    log_line!(
        "[bot/hunt4/state] {:?} -> {:?} cause={:?}",
        t.from,
        t.to,
        t.cause
    );
}

pub fn label_action(action: &Action) -> ActionLabel {
    match action {
        Action::Wait => ActionLabel::Wait,
        Action::WalkTo { tile } => ActionLabel::WalkTo { tile: *tile },
        Action::AttackTarget {
            target_id, skill, ..
        } => ActionLabel::Attack {
            target_id: *target_id,
            with_skill: skill.as_ref().is_some_and(|s| !s.is_empty()),
        },
        Action::UseTeleportScroll { .. } => ActionLabel::UseScroll,
    }
}

pub fn label_dispatch_intent(intent: &DispatchIntent) -> ActionLabel {
    match intent {
        DispatchIntent::Noop => ActionLabel::Wait,
        DispatchIntent::Walk { target_x, target_y } => ActionLabel::WalkTo {
            tile: (*target_x, *target_y),
        },
        DispatchIntent::BootstrapAttack { target_id, .. } => ActionLabel::Attack {
            target_id: *target_id,
            with_skill: false,
        },
        DispatchIntent::CastSkill { target_id, .. } => ActionLabel::Attack {
            target_id: *target_id,
            with_skill: true,
        },
        DispatchIntent::UseScroll { .. } => ActionLabel::UseScroll,
        DispatchIntent::AttackLookupFailed { target_id } => ActionLabel::Attack {
            target_id: *target_id,
            with_skill: false,
        },
    }
}

pub fn snapshot_lock(state: &HuntState) -> Option<LockSummary> {
    match state {
        HuntState::Engaging { lock, intent, .. } => Some(LockSummary {
            target_id: lock.target_id,
            name: lock.name.clone(),
            intent: match intent {
                EngageIntent::Approach => "Approach",
                EngageIntent::Attack => "Attack",
                EngageIntent::KillConfirm => "KillConfirm",
            },
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::hunt4::state::{EngageIntent, HuntState, StopReason, TargetLock};

    fn fake_lock(target_id: u32) -> TargetLock {
        TargetLock {
            target_id,
            entity_ptr: 0xDEAD_BEEF,
            name: format!("mob_{target_id:X}"),
            acquired_at: Instant::now(),
            last_seen: Instant::now(),
            bootstrapped: false,
        }
    }

    #[test]
    fn label_action_wait() {
        assert_eq!(label_action(&Action::Wait), ActionLabel::Wait);
    }

    #[test]
    fn label_action_walk_to_preserves_tile() {
        let action = Action::WalkTo { tile: (105, 99) };
        assert_eq!(
            label_action(&action),
            ActionLabel::WalkTo { tile: (105, 99) }
        );
    }

    #[test]
    fn label_action_attack_without_skill() {
        let action = Action::AttackTarget {
            target_id: 0xABCD,
            entity_ptr: 0xDEAD,
            skill: None,
        };
        assert_eq!(
            label_action(&action),
            ActionLabel::Attack {
                target_id: 0xABCD,
                with_skill: false,
            }
        );
    }

    #[test]
    fn label_action_attack_with_skill_marks_with_skill_true() {
        let action = Action::AttackTarget {
            target_id: 0xABCD,
            entity_ptr: 0xDEAD,
            skill: Some("skill".to_string()),
        };
        assert_eq!(
            label_action(&action),
            ActionLabel::Attack {
                target_id: 0xABCD,
                with_skill: true,
            }
        );
    }

    #[test]
    fn label_action_attack_with_empty_skill_marks_with_skill_false() {
        let action = Action::AttackTarget {
            target_id: 0xABCD,
            entity_ptr: 0xDEAD,
            skill: Some(String::new()),
        };
        assert_eq!(
            label_action(&action),
            ActionLabel::Attack {
                target_id: 0xABCD,
                with_skill: false,
            }
        );
    }

    #[test]
    fn label_action_use_scroll() {
        let action = Action::UseTeleportScroll {
            name_keyword: "teleport".to_string(),
        };
        assert_eq!(label_action(&action), ActionLabel::UseScroll);
    }

    #[test]
    fn label_dispatch_intent_reports_actual_dispatch_surface() {
        assert_eq!(
            label_dispatch_intent(&DispatchIntent::Walk {
                target_x: 77,
                target_y: 88,
            }),
            ActionLabel::WalkTo { tile: (77, 88) }
        );
        assert_eq!(
            label_dispatch_intent(&DispatchIntent::BootstrapAttack {
                target_id: 0xAB,
                entity_ptr: 0xCD,
                target_raw_x: 0,
                target_y: 0,
                fresh_target: true,
            }),
            ActionLabel::Attack {
                target_id: 0xAB,
                with_skill: false,
            }
        );
        assert_eq!(
            label_dispatch_intent(&DispatchIntent::CastSkill {
                skill_name: "skill".to_string(),
                target_id: 0xAC,
            }),
            ActionLabel::Attack {
                target_id: 0xAC,
                with_skill: true,
            }
        );
        assert_eq!(
            label_dispatch_intent(&DispatchIntent::AttackLookupFailed { target_id: 0xAD }),
            ActionLabel::Attack {
                target_id: 0xAD,
                with_skill: false,
            }
        );
    }

    #[test]
    fn snapshot_lock_returns_none_for_non_engaging() {
        assert!(snapshot_lock(&HuntState::Idle).is_none());
        assert!(snapshot_lock(&HuntState::Stopped {
            reason: StopReason::Manual
        })
        .is_none());
    }

    #[test]
    fn snapshot_lock_returns_summary_for_engaging() {
        let state = HuntState::Engaging {
            lock: fake_lock(0xABCD),
            intent: EngageIntent::Attack,
            path: None,
        };
        let summary = snapshot_lock(&state).expect("Engaging should produce lock summary");
        assert_eq!(summary.target_id, 0xABCD);
        assert_eq!(summary.intent, "Attack");
        assert_eq!(summary.name, "mob_ABCD");
    }

    #[test]
    fn snapshot_lock_intent_labels_match_variants() {
        for (intent, expected) in [
            (EngageIntent::Approach, "Approach"),
            (EngageIntent::Attack, "Attack"),
            (EngageIntent::KillConfirm, "KillConfirm"),
        ] {
            let state = HuntState::Engaging {
                lock: fake_lock(0x1234),
                intent,
                path: None,
            };
            let summary = snapshot_lock(&state).expect("engaging has lock");
            assert_eq!(summary.intent, expected, "intent {intent:?}");
        }
    }
}
