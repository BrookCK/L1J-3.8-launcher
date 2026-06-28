use crate::bot::hunt4::actions::{intent_for, DispatchIntent, DispatchState};
use crate::bot::hunt4::model::Snapshot;
use crate::bot::hunt4::step::Action;

use super::plan::PlanFrame;
use super::policy::{self, PolicyComparison, PolicyTelemetry, ShadowDecision};

pub const DISPATCH_TAKEOVER_MIN_ALIGNED_STREAK: u64 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowDispatch {
    pub decision: ShadowDecision,
    pub action: Action,
    pub intent: DispatchIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchChoiceSource {
    Backend,
    Shadow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchChoice {
    pub source: DispatchChoiceSource,
    pub intent: DispatchIntent,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchEvaluation {
    pub shadow_dispatch: ShadowDispatch,
    pub policy_comparison: PolicyComparison,
    pub dispatch_choice: DispatchChoice,
}

pub fn build_shadow_dispatch(
    plan: &PlanFrame,
    snapshot: &Snapshot,
    state: &DispatchState,
) -> ShadowDispatch {
    let decision = policy::decide(plan);
    let action = action_for_decision(plan, &decision);
    let intent = intent_for(&action, snapshot, state);
    ShadowDispatch {
        decision,
        action,
        intent,
    }
}

pub fn evaluate_dispatch_choice(
    plan: &PlanFrame,
    snapshot: &Snapshot,
    state: &DispatchState,
    backend: &DispatchIntent,
    telemetry: &mut PolicyTelemetry,
    takeover_enabled: bool,
) -> DispatchEvaluation {
    let shadow_dispatch = build_shadow_dispatch(plan, snapshot, state);
    let policy_comparison = policy::compare_to_backend(plan, backend);
    telemetry.record(policy_comparison.clone());
    let dispatch_choice = choose_dispatch_intent(
        backend,
        &shadow_dispatch,
        &policy_comparison,
        telemetry.aligned_streak,
        takeover_enabled,
    );

    DispatchEvaluation {
        shadow_dispatch,
        policy_comparison,
        dispatch_choice,
    }
}

pub fn choose_dispatch_intent(
    backend: &DispatchIntent,
    shadow: &ShadowDispatch,
    comparison: &PolicyComparison,
    aligned_streak: u64,
    takeover_enabled: bool,
) -> DispatchChoice {
    if !takeover_enabled {
        return backend_choice(backend, "v4 takeover disabled");
    }
    if let Some(reason) = backend_owned_dispatch_reason(backend) {
        return backend_choice(backend, reason);
    }
    if !comparison.aligned {
        return backend_choice(backend, "policy mismatch");
    }
    if aligned_streak < DISPATCH_TAKEOVER_MIN_ALIGNED_STREAK {
        return backend_choice(backend, "alignment streak too short");
    }
    if &shadow.intent != backend {
        return backend_choice(backend, "shadow intent differs");
    }
    if !takeover_allowed(&shadow.intent) {
        return backend_choice(backend, "takeover action not allowed");
    }
    DispatchChoice {
        source: DispatchChoiceSource::Shadow,
        intent: shadow.intent.clone(),
        reason: "shadow intent exact match",
    }
}

fn takeover_allowed(intent: &DispatchIntent) -> bool {
    matches!(
        intent,
        DispatchIntent::Noop
            | DispatchIntent::Walk { .. }
            | DispatchIntent::BootstrapAttack { .. }
            | DispatchIntent::CastSkill { .. }
    )
}

fn backend_owned_dispatch_reason(intent: &DispatchIntent) -> Option<&'static str> {
    match intent {
        DispatchIntent::UseScroll { .. } => Some("backend-owned scroll"),
        DispatchIntent::AttackLookupFailed { .. } => Some("backend-owned attack lookup failure"),
        _ => None,
    }
}

fn backend_choice(intent: &DispatchIntent, reason: &'static str) -> DispatchChoice {
    DispatchChoice {
        source: DispatchChoiceSource::Backend,
        intent: intent.clone(),
        reason,
    }
}

fn action_for_decision(plan: &PlanFrame, decision: &ShadowDecision) -> Action {
    match decision {
        ShadowDecision::Wait => Action::Wait,
        ShadowDecision::Attack { target_id } => plan
            .ranked_targets
            .iter()
            .find(|target| target.target_id == *target_id)
            .map(|target| Action::AttackTarget {
                target_id: target.target_id,
                entity_ptr: target.entity_ptr,
                skill: selected_attack_skill(plan, target.target_id),
            })
            .unwrap_or(Action::Wait),
        ShadowDecision::Approach {
            next_tile: Some(tile),
            ..
        }
        | ShadowDecision::Explore {
            next_tile: Some(tile),
            ..
        } => Action::WalkTo { tile: *tile },
        ShadowDecision::Approach {
            next_tile: None, ..
        }
        | ShadowDecision::Explore {
            next_tile: None, ..
        }
        | ShadowDecision::UseTeleportScroll { .. } => Action::Wait,
    }
}

fn selected_attack_skill(plan: &PlanFrame, target_id: u32) -> Option<String> {
    if plan.post_skill_basic_pending_target == Some(target_id) {
        return None;
    }
    if plan.selected_skill_ready != Some(true) {
        return None;
    }
    plan.selected_attack_step
        .as_ref()
        .and_then(|step| step.skill_for_cd())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crate::bot::decide::hunt::AttackSequenceStep;
    use crate::bot::hunt4::actions::{DispatchIntent, DispatchState};
    use crate::bot::hunt4::model::{EntityView, Snapshot};
    use crate::bot::hunt4::plan::PlanFrame;
    use crate::bot::hunt4::policy::{
        BackendIntentSummary, PolicyComparison, PolicyTelemetry, ShadowDecision,
    };
    use crate::bot::hunt4::score::TargetScoreSummary;
    use crate::bot::hunt4::step::Action;
    use crate::bot::perception::classifier::EntityClass;

    fn target(
        target_id: u32,
        in_attack_range: bool,
        reachable: bool,
        next_tile: Option<(i32, i32)>,
    ) -> TargetScoreSummary {
        TargetScoreSummary {
            rank: 1,
            target_id,
            entity_ptr: 0xDEAD_0000 + target_id,
            name: format!("mob_{target_id:X}"),
            tile: (105, 100),
            distance: 5,
            in_attack_range,
            reachable,
            is_attacker: false,
            approach_steps: next_tile.map(|_| 2),
            approach_next_tile: next_tile,
            reason: "test".to_string(),
        }
    }

    fn plan(ranked_targets: Vec<TargetScoreSummary>) -> PlanFrame {
        let selected_target_id = ranked_targets.first().map(|target| target.target_id);
        let selected_target_reason = ranked_targets.first().map(|target| target.reason.clone());
        PlanFrame {
            now: Instant::now(),
            in_game: true,
            map_id: Some(4),
            player_tile: Some((100, 100)),
            attack_range: 5,
            selected_attack_step: None,
            selected_skill_range: None,
            selected_skill_cd_ms: None,
            selected_skill_ready: None,
            post_skill_basic_pending_target: None,
            recent_attacker_count: 0,
            candidate_count: ranked_targets.len(),
            reachable_count: ranked_targets
                .iter()
                .filter(|target| target.reachable)
                .count(),
            attacker_count: 0,
            ranked_targets,
            route_plan: None,
            selected_target_id,
            selected_target_reason,
            non_viable_target_count: 0,
            last_target_reject_reason: None,
            abandoned_target_reason: None,
            selected_route_target_id: None,
            selected_route_next_tile: None,
            selected_route_reason: None,
            movement_next_tile: None,
            movement_reason: None,
            teleport_should_use: false,
            teleport_reason: None,
            final_action_kind: "wait".to_string(),
            invariant_violations: Vec::new(),
            explore_goal: None,
            explore_next_tile: None,
            explore_steps: None,
        }
    }

    fn live_mob(target_id: u32) -> EntityView {
        EntityView {
            target_id,
            entity_ptr: 0xDEAD_0000 + target_id,
            name: format!("mob_{target_id:X}"),
            sprite_id: 1234,
            action_state: 2,
            tile: (105, 100),
            raw_x: 0x8800,
            y: 100,
            class: EntityClass::AttackableMonster,
            visible_confidence: 100,
            hostile_confidence: 100,
        }
    }

    #[test]
    fn build_shadow_dispatch_builds_basic_attack_intent_from_plan_and_snapshot() {
        let target_id = 0x0100_0001;
        let plan = plan(vec![target(target_id, true, true, None)]);
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(target_id)],
        };

        let shadow = super::build_shadow_dispatch(&plan, &snapshot, &DispatchState::default());

        assert_eq!(shadow.decision, ShadowDecision::Attack { target_id });
        assert_eq!(
            shadow.action,
            Action::AttackTarget {
                target_id,
                entity_ptr: 0xDEAD_0000 + target_id,
                skill: None,
            }
        );
        assert_eq!(
            shadow.intent,
            DispatchIntent::BootstrapAttack {
                target_id,
                entity_ptr: 0xDEAD_0000 + target_id,
                target_raw_x: 0x8800,
                target_y: 100,
                fresh_target: true,
            }
        );
    }

    #[test]
    fn build_shadow_dispatch_walks_first_approach_tile() {
        let plan = plan(vec![target(0x0100_0002, false, true, Some((101, 100)))]);
        let snapshot = Snapshot::default();

        let shadow = super::build_shadow_dispatch(&plan, &snapshot, &DispatchState::default());

        assert_eq!(
            shadow.decision,
            ShadowDecision::Approach {
                target_id: 0x0100_0002,
                next_tile: Some((101, 100)),
            }
        );
        assert_eq!(shadow.action, Action::WalkTo { tile: (101, 100) });
        assert_eq!(
            shadow.intent,
            DispatchIntent::Walk {
                target_x: 101,
                target_y: 100,
            }
        );
    }

    #[test]
    fn shadow_approach_action_uses_route_next_tile_exactly() {
        let target_id = 0x0100_0020;
        let mut plan = plan(vec![target(target_id, false, true, Some((101, 100)))]);
        plan.selected_route_target_id = Some(target_id);
        plan.selected_route_next_tile = Some((102, 100));
        plan.selected_route_reason = Some("target_approach".to_string());
        plan.movement_next_tile = Some((102, 100));
        plan.movement_reason = Some("target_approach".to_string());
        let snapshot = Snapshot::default();

        let shadow = super::build_shadow_dispatch(&plan, &snapshot, &DispatchState::default());

        assert_eq!(
            shadow.decision,
            ShadowDecision::Approach {
                target_id,
                next_tile: Some((102, 100)),
            }
        );
        assert_eq!(shadow.action, Action::WalkTo { tile: (102, 100) });
        assert_eq!(
            shadow.intent,
            DispatchIntent::Walk {
                target_x: 102,
                target_y: 100,
            }
        );
    }

    #[test]
    fn shadow_dispatch_waits_when_plan_has_invariant_violation() {
        let mut plan = plan(vec![target(0x0100_0021, false, true, Some((101, 100)))]);
        plan.invariant_violations
            .push("route target mismatch selected=0x01000021 route=0x01000022".to_string());
        let snapshot = Snapshot::default();

        let shadow = super::build_shadow_dispatch(&plan, &snapshot, &DispatchState::default());

        assert_eq!(shadow.decision, ShadowDecision::Wait);
        assert_eq!(shadow.action, Action::Wait);
        assert_eq!(shadow.intent, DispatchIntent::Noop);
    }

    #[test]
    fn build_shadow_dispatch_walks_first_explore_tile() {
        let mut plan = plan(Vec::new());
        plan.explore_goal = Some((120, 100));
        plan.explore_next_tile = Some((101, 100));
        plan.explore_steps = Some(4);
        let snapshot = Snapshot::default();

        let shadow = super::build_shadow_dispatch(&plan, &snapshot, &DispatchState::default());

        assert_eq!(
            shadow.decision,
            ShadowDecision::Explore {
                goal: (120, 100),
                next_tile: Some((101, 100)),
            }
        );
        assert_eq!(shadow.action, Action::WalkTo { tile: (101, 100) });
        assert_eq!(
            shadow.intent,
            DispatchIntent::Walk {
                target_x: 101,
                target_y: 100,
            }
        );
    }

    #[test]
    fn build_shadow_dispatch_reports_teleport_without_manufacturing_scroll_action() {
        let mut plan = plan(Vec::new());
        plan.teleport_should_use = true;
        plan.teleport_reason = Some("empty_area".to_string());
        let snapshot = Snapshot::default();

        let shadow = super::build_shadow_dispatch(&plan, &snapshot, &DispatchState::default());

        assert_eq!(
            shadow.decision,
            ShadowDecision::UseTeleportScroll {
                reason: "empty_area".to_string(),
            }
        );
        assert_eq!(shadow.action, Action::Wait);
        assert_eq!(shadow.intent, DispatchIntent::Noop);
    }

    #[test]
    fn build_shadow_dispatch_reports_lookup_failed_for_missing_attack_target() {
        let target_id = 0x0100_0003;
        let plan = plan(vec![target(target_id, true, true, None)]);
        let snapshot = Snapshot::default();

        let shadow = super::build_shadow_dispatch(&plan, &snapshot, &DispatchState::default());

        assert_eq!(
            shadow.intent,
            DispatchIntent::AttackLookupFailed { target_id }
        );
    }

    #[test]
    fn build_shadow_dispatch_builds_skill_cast_intent_when_selected_skill_ready() {
        let target_id = 0x0100_0013;
        let mut plan = plan(vec![target(target_id, true, true, None)]);
        plan.selected_attack_step = Some(AttackSequenceStep::skill("Frozen Cloud".to_string(), 0));
        plan.selected_skill_cd_ms = Some(2000);
        plan.selected_skill_ready = Some(true);
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(target_id)],
        };

        let shadow = super::build_shadow_dispatch(&plan, &snapshot, &DispatchState::default());

        assert_eq!(
            shadow.action,
            Action::AttackTarget {
                target_id,
                entity_ptr: 0xDEAD_0000 + target_id,
                skill: Some("Frozen Cloud".to_string()),
            }
        );
        assert_eq!(
            shadow.intent,
            DispatchIntent::CastSkill {
                skill_name: "Frozen Cloud".to_string(),
                target_id,
            }
        );
    }

    #[test]
    fn build_shadow_dispatch_uses_basic_attack_when_selected_skill_not_ready() {
        let target_id = 0x0100_0014;
        let mut plan = plan(vec![target(target_id, true, true, None)]);
        plan.selected_attack_step = Some(AttackSequenceStep::skill("Frozen Cloud".to_string(), 0));
        plan.selected_skill_cd_ms = Some(2000);
        plan.selected_skill_ready = Some(false);
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(target_id)],
        };

        let shadow = super::build_shadow_dispatch(&plan, &snapshot, &DispatchState::default());

        assert_eq!(
            shadow.action,
            Action::AttackTarget {
                target_id,
                entity_ptr: 0xDEAD_0000 + target_id,
                skill: None,
            }
        );
        assert!(matches!(
            shadow.intent,
            DispatchIntent::BootstrapAttack { target_id: seen, .. } if seen == target_id
        ));
    }

    #[test]
    fn build_shadow_dispatch_uses_post_skill_basic_before_ready_skill_recast() {
        let target_id = 0x0100_0015;
        let mut plan = plan(vec![target(target_id, true, true, None)]);
        plan.selected_attack_step = Some(AttackSequenceStep::skill("Frozen Cloud".to_string(), 0));
        plan.selected_skill_cd_ms = Some(2000);
        plan.selected_skill_ready = Some(true);
        plan.post_skill_basic_pending_target = Some(target_id);
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(target_id)],
        };

        let shadow = super::build_shadow_dispatch(&plan, &snapshot, &DispatchState::default());

        assert_eq!(
            shadow.action,
            Action::AttackTarget {
                target_id,
                entity_ptr: 0xDEAD_0000 + target_id,
                skill: None,
            }
        );
        assert!(matches!(
            shadow.intent,
            DispatchIntent::BootstrapAttack { target_id: seen, .. } if seen == target_id
        ));
    }

    #[test]
    fn evaluate_dispatch_choice_records_telemetry_and_returns_shadow_choice() {
        let target_id = 0x0100_0016;
        let plan = plan(vec![target(target_id, true, true, None)]);
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(target_id)],
        };
        let backend = DispatchIntent::BootstrapAttack {
            target_id,
            entity_ptr: 0xDEAD_0000 + target_id,
            target_raw_x: 0x8800,
            target_y: 100,
            fresh_target: true,
        };
        let mut telemetry = PolicyTelemetry {
            total: 2,
            aligned: 2,
            mismatched: 0,
            aligned_streak: 2,
            last_mismatch: None,
        };

        let evaluation = super::evaluate_dispatch_choice(
            &plan,
            &snapshot,
            &DispatchState::default(),
            &backend,
            &mut telemetry,
            true,
        );

        assert_eq!(telemetry.total, 3);
        assert_eq!(telemetry.aligned, 3);
        assert_eq!(telemetry.aligned_streak, 3);
        assert_eq!(
            evaluation.shadow_dispatch.decision,
            ShadowDecision::Attack { target_id }
        );
        assert!(evaluation.policy_comparison.aligned);
        assert_eq!(
            evaluation.dispatch_choice.source,
            super::DispatchChoiceSource::Shadow
        );
        assert_eq!(
            evaluation.dispatch_choice.reason,
            "shadow intent exact match"
        );
        assert_eq!(evaluation.dispatch_choice.intent, backend);
    }

    #[test]
    fn choose_dispatch_intent_keeps_backend_when_takeover_disabled() {
        let backend = DispatchIntent::Walk {
            target_x: 101,
            target_y: 100,
        };
        let shadow = super::ShadowDispatch {
            decision: ShadowDecision::Wait,
            action: Action::Wait,
            intent: DispatchIntent::Noop,
        };
        let comparison = PolicyComparison {
            shadow: ShadowDecision::Wait,
            backend: BackendIntentSummary::Wait,
            aligned: true,
            reason: "both wait",
        };

        let choice = super::choose_dispatch_intent(&backend, &shadow, &comparison, 0, false);

        assert_eq!(choice.source, super::DispatchChoiceSource::Backend);
        assert_eq!(choice.intent, backend);
        assert_eq!(choice.reason, "v4 takeover disabled");
    }

    #[test]
    fn choose_dispatch_intent_uses_shadow_when_enabled_aligned_and_exact_match() {
        let intent = DispatchIntent::Walk {
            target_x: 101,
            target_y: 100,
        };
        let shadow = super::ShadowDispatch {
            decision: ShadowDecision::Approach {
                target_id: 0x0100_0004,
                next_tile: Some((101, 100)),
            },
            action: Action::WalkTo { tile: (101, 100) },
            intent: intent.clone(),
        };
        let comparison = PolicyComparison {
            shadow: shadow.decision.clone(),
            backend: BackendIntentSummary::Walk { tile: (101, 100) },
            aligned: true,
            reason: "same approach tile",
        };

        let choice = super::choose_dispatch_intent(&intent, &shadow, &comparison, 3, true);

        assert_eq!(choice.source, super::DispatchChoiceSource::Shadow);
        assert_eq!(choice.intent, intent);
        assert_eq!(choice.reason, "shadow intent exact match");
    }

    #[test]
    fn choose_dispatch_intent_rejects_shadow_when_policy_mismatches() {
        let backend = DispatchIntent::Walk {
            target_x: 101,
            target_y: 100,
        };
        let shadow = super::ShadowDispatch {
            decision: ShadowDecision::Wait,
            action: Action::Wait,
            intent: DispatchIntent::Noop,
        };
        let comparison = PolicyComparison {
            shadow: ShadowDecision::Wait,
            backend: BackendIntentSummary::Walk { tile: (101, 100) },
            aligned: false,
            reason: "different intent kind",
        };

        let choice = super::choose_dispatch_intent(&backend, &shadow, &comparison, 3, true);

        assert_eq!(choice.source, super::DispatchChoiceSource::Backend);
        assert_eq!(choice.intent, backend);
        assert_eq!(choice.reason, "policy mismatch");
    }

    #[test]
    fn choose_dispatch_intent_rejects_shadow_when_intent_differs() {
        let backend = DispatchIntent::Walk {
            target_x: 101,
            target_y: 100,
        };
        let shadow = super::ShadowDispatch {
            decision: ShadowDecision::Approach {
                target_id: 0x0100_0005,
                next_tile: Some((101, 100)),
            },
            action: Action::WalkTo { tile: (101, 100) },
            intent: DispatchIntent::Walk {
                target_x: 102,
                target_y: 100,
            },
        };
        let comparison = PolicyComparison {
            shadow: shadow.decision.clone(),
            backend: BackendIntentSummary::Walk { tile: (101, 100) },
            aligned: true,
            reason: "same approach tile",
        };

        let choice = super::choose_dispatch_intent(&backend, &shadow, &comparison, 3, true);

        assert_eq!(choice.source, super::DispatchChoiceSource::Backend);
        assert_eq!(choice.intent, backend);
        assert_eq!(choice.reason, "shadow intent differs");
    }

    #[test]
    fn choose_dispatch_intent_requires_aligned_streak_before_shadow() {
        let intent = DispatchIntent::Walk {
            target_x: 101,
            target_y: 100,
        };
        let shadow = super::ShadowDispatch {
            decision: ShadowDecision::Approach {
                target_id: 0x0100_0006,
                next_tile: Some((101, 100)),
            },
            action: Action::WalkTo { tile: (101, 100) },
            intent: intent.clone(),
        };
        let comparison = PolicyComparison {
            shadow: shadow.decision.clone(),
            backend: BackendIntentSummary::Walk { tile: (101, 100) },
            aligned: true,
            reason: "same approach tile",
        };

        let choice = super::choose_dispatch_intent(&intent, &shadow, &comparison, 2, true);

        assert_eq!(choice.source, super::DispatchChoiceSource::Backend);
        assert_eq!(choice.intent, intent);
        assert_eq!(choice.reason, "alignment streak too short");
    }

    #[test]
    fn choose_dispatch_intent_uses_shadow_for_exact_basic_attack_after_streak() {
        let intent = DispatchIntent::BootstrapAttack {
            target_id: 0x0100_0007,
            entity_ptr: 0xDEAD_0007,
            target_raw_x: 0x8800,
            target_y: 100,
            fresh_target: true,
        };
        let shadow = super::ShadowDispatch {
            decision: ShadowDecision::Attack {
                target_id: 0x0100_0007,
            },
            action: Action::AttackTarget {
                target_id: 0x0100_0007,
                entity_ptr: 0xDEAD_0007,
                skill: None,
            },
            intent: intent.clone(),
        };
        let comparison = PolicyComparison {
            shadow: shadow.decision.clone(),
            backend: BackendIntentSummary::Attack {
                target_id: 0x0100_0007,
            },
            aligned: true,
            reason: "same attack target",
        };

        let choice = super::choose_dispatch_intent(&intent, &shadow, &comparison, 3, true);

        assert_eq!(choice.source, super::DispatchChoiceSource::Shadow);
        assert_eq!(choice.intent, intent);
        assert_eq!(choice.reason, "shadow intent exact match");
    }

    #[test]
    fn choose_dispatch_intent_uses_shadow_for_exact_wait_after_streak() {
        let intent = DispatchIntent::Noop;
        let shadow = super::ShadowDispatch {
            decision: ShadowDecision::Wait,
            action: Action::Wait,
            intent: intent.clone(),
        };
        let comparison = PolicyComparison {
            shadow: ShadowDecision::Wait,
            backend: BackendIntentSummary::Wait,
            aligned: true,
            reason: "both wait",
        };

        let choice = super::choose_dispatch_intent(&intent, &shadow, &comparison, 3, true);

        assert_eq!(choice.source, super::DispatchChoiceSource::Shadow);
        assert_eq!(choice.intent, intent);
        assert_eq!(choice.reason, "shadow intent exact match");
    }

    #[test]
    fn choose_dispatch_intent_uses_shadow_for_exact_skill_attack_after_streak() {
        let intent = DispatchIntent::CastSkill {
            skill_name: "Frozen Cloud".to_string(),
            target_id: 0x0100_0008,
        };
        let shadow = super::ShadowDispatch {
            decision: ShadowDecision::Attack {
                target_id: 0x0100_0008,
            },
            action: Action::AttackTarget {
                target_id: 0x0100_0008,
                entity_ptr: 0xDEAD_0008,
                skill: Some("Frozen Cloud".to_string()),
            },
            intent: intent.clone(),
        };
        let comparison = PolicyComparison {
            shadow: shadow.decision.clone(),
            backend: BackendIntentSummary::Attack {
                target_id: 0x0100_0008,
            },
            aligned: true,
            reason: "same attack target",
        };

        let choice = super::choose_dispatch_intent(&intent, &shadow, &comparison, 3, true);

        assert_eq!(choice.source, super::DispatchChoiceSource::Shadow);
        assert_eq!(choice.intent, intent);
        assert_eq!(choice.reason, "shadow intent exact match");
    }

    #[test]
    fn choose_dispatch_intent_keeps_scroll_backend_owned() {
        let intent = DispatchIntent::UseScroll {
            name_keyword: "teleport".to_string(),
        };
        let shadow = super::ShadowDispatch {
            decision: ShadowDecision::Wait,
            action: Action::UseTeleportScroll {
                name_keyword: "teleport".to_string(),
            },
            intent: intent.clone(),
        };
        let comparison = PolicyComparison {
            shadow: ShadowDecision::Wait,
            backend: BackendIntentSummary::UseScroll,
            aligned: true,
            reason: "forced exact scroll test",
        };

        let choice = super::choose_dispatch_intent(&intent, &shadow, &comparison, 3, true);

        assert_eq!(choice.source, super::DispatchChoiceSource::Backend);
        assert_eq!(choice.intent, intent);
        assert_eq!(choice.reason, "backend-owned scroll");
    }

    #[test]
    fn choose_dispatch_intent_keeps_attack_lookup_failure_backend_owned() {
        let intent = DispatchIntent::AttackLookupFailed {
            target_id: 0x0100_0009,
        };
        let shadow = super::ShadowDispatch {
            decision: ShadowDecision::Attack {
                target_id: 0x0100_0009,
            },
            action: Action::AttackTarget {
                target_id: 0x0100_0009,
                entity_ptr: 0xDEAD_0009,
                skill: None,
            },
            intent: intent.clone(),
        };
        let comparison = PolicyComparison {
            shadow: shadow.decision.clone(),
            backend: BackendIntentSummary::AttackLookupFailed {
                target_id: 0x0100_0009,
            },
            aligned: true,
            reason: "forced exact lookup-failure test",
        };

        let choice = super::choose_dispatch_intent(&intent, &shadow, &comparison, 3, true);

        assert_eq!(choice.source, super::DispatchChoiceSource::Backend);
        assert_eq!(choice.intent, intent);
        assert_eq!(choice.reason, "backend-owned attack lookup failure");
    }
}
