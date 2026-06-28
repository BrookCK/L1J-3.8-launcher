use crate::bot::hunt4::actions::DispatchIntent;

use super::plan::PlanFrame;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShadowDecision {
    Wait,
    Attack {
        target_id: u32,
    },
    Approach {
        target_id: u32,
        next_tile: Option<(i32, i32)>,
    },
    Explore {
        goal: (i32, i32),
        next_tile: Option<(i32, i32)>,
    },
    UseTeleportScroll {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendIntentSummary {
    Wait,
    Attack { target_id: u32 },
    Walk { tile: (i32, i32) },
    UseScroll,
    AttackLookupFailed { target_id: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyComparison {
    pub shadow: ShadowDecision,
    pub backend: BackendIntentSummary,
    pub aligned: bool,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyTelemetry {
    pub total: u64,
    pub aligned: u64,
    pub mismatched: u64,
    pub aligned_streak: u64,
    pub last_mismatch: Option<PolicyComparison>,
}

impl PolicyTelemetry {
    pub fn record(&mut self, comparison: PolicyComparison) {
        self.total += 1;
        if comparison.aligned {
            self.aligned += 1;
            self.aligned_streak += 1;
        } else {
            self.mismatched += 1;
            self.aligned_streak = 0;
            self.last_mismatch = Some(comparison);
        }
    }
}

pub fn decide(plan: &PlanFrame) -> ShadowDecision {
    if !plan.in_game {
        return ShadowDecision::Wait;
    }
    if !plan.invariant_violations.is_empty() {
        return ShadowDecision::Wait;
    }

    if let Some(target) = plan.top_target() {
        if target.reachable && target.in_attack_range {
            return ShadowDecision::Attack {
                target_id: target.target_id,
            };
        }
        if target.reachable {
            return ShadowDecision::Approach {
                target_id: target.target_id,
                next_tile: approach_next_tile(plan, target.target_id, target.approach_next_tile),
            };
        }
    }

    if let Some(goal) = plan.explore_goal {
        return ShadowDecision::Explore {
            goal,
            next_tile: plan.explore_next_tile,
        };
    }

    if plan.teleport_should_use {
        return ShadowDecision::UseTeleportScroll {
            reason: plan
                .teleport_reason
                .clone()
                .unwrap_or_else(|| "teleport".to_string()),
        };
    }

    ShadowDecision::Wait
}

fn approach_next_tile(
    plan: &PlanFrame,
    target_id: u32,
    fallback: Option<(i32, i32)>,
) -> Option<(i32, i32)> {
    if plan.selected_route_target_id == Some(target_id) {
        return plan.selected_route_next_tile.or(fallback);
    }
    fallback
}

pub fn compare_to_backend(plan: &PlanFrame, intent: &DispatchIntent) -> PolicyComparison {
    let shadow = decide(plan);
    let backend = summarize_backend(intent);
    let (aligned, reason) = alignment(&shadow, &backend);

    PolicyComparison {
        shadow,
        backend,
        aligned,
        reason,
    }
}

fn summarize_backend(intent: &DispatchIntent) -> BackendIntentSummary {
    match intent {
        DispatchIntent::Noop => BackendIntentSummary::Wait,
        DispatchIntent::Walk { target_x, target_y } => BackendIntentSummary::Walk {
            tile: (*target_x, *target_y),
        },
        DispatchIntent::BootstrapAttack { target_id, .. }
        | DispatchIntent::CastSkill { target_id, .. } => BackendIntentSummary::Attack {
            target_id: *target_id,
        },
        DispatchIntent::UseScroll { .. } => BackendIntentSummary::UseScroll,
        DispatchIntent::AttackLookupFailed { target_id } => {
            BackendIntentSummary::AttackLookupFailed {
                target_id: *target_id,
            }
        }
    }
}

fn alignment(shadow: &ShadowDecision, backend: &BackendIntentSummary) -> (bool, &'static str) {
    match (shadow, backend) {
        (ShadowDecision::Wait, BackendIntentSummary::Wait) => (true, "both wait"),
        (
            ShadowDecision::Attack {
                target_id: shadow_id,
            },
            BackendIntentSummary::Attack {
                target_id: backend_id,
            },
        ) if shadow_id == backend_id => (true, "same attack target"),
        (ShadowDecision::Attack { .. }, BackendIntentSummary::Attack { .. }) => {
            (false, "different attack target")
        }
        (
            ShadowDecision::Approach {
                next_tile: Some(next_tile),
                ..
            },
            BackendIntentSummary::Walk { tile },
        ) if next_tile == tile => (true, "same approach tile"),
        (
            ShadowDecision::Approach {
                next_tile: None, ..
            },
            BackendIntentSummary::Walk { .. },
        ) => (false, "missing approach tile"),
        (ShadowDecision::Approach { .. }, BackendIntentSummary::Walk { .. }) => {
            (false, "different approach tile")
        }
        (
            ShadowDecision::Explore {
                next_tile: Some(next_tile),
                ..
            },
            BackendIntentSummary::Walk { tile },
        ) if next_tile == tile => (true, "same explore tile"),
        (
            ShadowDecision::Explore {
                next_tile: None, ..
            },
            BackendIntentSummary::Walk { .. },
        ) => (false, "missing explore tile"),
        (ShadowDecision::Explore { .. }, BackendIntentSummary::Walk { .. }) => {
            (false, "different explore tile")
        }
        (ShadowDecision::UseTeleportScroll { .. }, BackendIntentSummary::UseScroll) => {
            (true, "both use teleport scroll")
        }
        _ => (false, "different intent kind"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crate::bot::hunt4::actions::DispatchIntent;
    use crate::bot::hunt4::plan::PlanFrame;
    use crate::bot::hunt4::score::TargetScoreSummary;

    fn target(
        target_id: u32,
        in_attack_range: bool,
        reachable: bool,
        is_attacker: bool,
        approach_steps: Option<usize>,
    ) -> TargetScoreSummary {
        TargetScoreSummary {
            rank: 1,
            target_id,
            entity_ptr: target_id + 0x1000,
            name: format!("mob_{target_id:X}"),
            tile: (100, 100),
            distance: 1,
            in_attack_range,
            reachable,
            is_attacker,
            approach_steps,
            approach_next_tile: approach_steps.map(|_| (101, 100)),
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
            player_tile: Some((99, 99)),
            attack_range: 1,
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
            attacker_count: ranked_targets
                .iter()
                .filter(|target| target.is_attacker)
                .count(),
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

    #[test]
    fn decide_attacks_top_target_when_in_range() {
        let plan = plan(vec![target(0x0100_0001, true, true, false, Some(0))]);

        let decision = super::decide(&plan);

        assert_eq!(
            decision,
            super::ShadowDecision::Attack {
                target_id: 0x0100_0001
            }
        );
    }

    #[test]
    fn decide_approaches_reachable_top_target_when_out_of_range() {
        let plan = plan(vec![target(0x0100_0002, false, true, true, Some(3))]);

        let decision = super::decide(&plan);

        assert_eq!(
            decision,
            super::ShadowDecision::Approach {
                target_id: 0x0100_0002,
                next_tile: Some((101, 100)),
            }
        );
    }

    #[test]
    fn decide_reports_teleport_when_no_target_or_explore_action_exists() {
        let mut plan = plan(Vec::new());
        plan.teleport_should_use = true;
        plan.teleport_reason = Some("empty_area".to_string());

        let decision = super::decide(&plan);

        assert_eq!(
            decision,
            super::ShadowDecision::UseTeleportScroll {
                reason: "empty_area".to_string(),
            }
        );
    }

    #[test]
    fn compare_to_backend_records_matching_attack_target() {
        let plan = plan(vec![target(0x0100_0001, true, true, false, Some(0))]);

        let comparison = super::compare_to_backend(
            &plan,
            &DispatchIntent::BootstrapAttack {
                target_id: 0x0100_0001,
                entity_ptr: 0x0100_1001,
                target_raw_x: 0,
                target_y: 0,
                fresh_target: true,
            },
        );

        assert_eq!(
            comparison.shadow,
            super::ShadowDecision::Attack {
                target_id: 0x0100_0001
            }
        );
        assert_eq!(
            comparison.backend,
            super::BackendIntentSummary::Attack {
                target_id: 0x0100_0001
            }
        );
        assert!(comparison.aligned);
    }

    #[test]
    fn compare_to_backend_aligns_teleport_shadow_with_backend_scroll() {
        let mut plan = plan(Vec::new());
        plan.teleport_should_use = true;
        plan.teleport_reason = Some("no_actionable_target".to_string());

        let comparison = super::compare_to_backend(
            &plan,
            &DispatchIntent::UseScroll {
                name_keyword: "teleport".to_string(),
            },
        );

        assert_eq!(
            comparison.shadow,
            super::ShadowDecision::UseTeleportScroll {
                reason: "no_actionable_target".to_string(),
            }
        );
        assert_eq!(comparison.backend, super::BackendIntentSummary::UseScroll);
        assert!(comparison.aligned);
        assert_eq!(comparison.reason, "both use teleport scroll");
    }

    #[test]
    fn compare_to_backend_requires_same_approach_walk_tile() {
        let mut plan = plan(vec![target(0x0100_0002, false, true, false, Some(2))]);
        plan.ranked_targets[0].approach_next_tile = Some((101, 100));

        let comparison = super::compare_to_backend(
            &plan,
            &DispatchIntent::Walk {
                target_x: 102,
                target_y: 100,
            },
        );

        assert_eq!(
            comparison.shadow,
            super::ShadowDecision::Approach {
                target_id: 0x0100_0002,
                next_tile: Some((101, 100)),
            }
        );
        assert_eq!(
            comparison.backend,
            super::BackendIntentSummary::Walk { tile: (102, 100) }
        );
        assert!(!comparison.aligned);
        assert_eq!(comparison.reason, "different approach tile");
    }

    #[test]
    fn telemetry_counts_aligned_and_mismatched_comparisons() {
        let mut telemetry = super::PolicyTelemetry::default();

        telemetry.record(super::PolicyComparison {
            shadow: super::ShadowDecision::Attack {
                target_id: 0x0100_0001,
            },
            backend: super::BackendIntentSummary::Attack {
                target_id: 0x0100_0001,
            },
            aligned: true,
            reason: "same attack target",
        });
        telemetry.record(super::PolicyComparison {
            shadow: super::ShadowDecision::Attack {
                target_id: 0x0100_0001,
            },
            backend: super::BackendIntentSummary::UseScroll,
            aligned: false,
            reason: "different intent kind",
        });

        assert_eq!(telemetry.total, 2);
        assert_eq!(telemetry.aligned, 1);
        assert_eq!(telemetry.mismatched, 1);
        assert!(telemetry
            .last_mismatch
            .as_ref()
            .is_some_and(|comparison| !comparison.aligned));
    }

    #[test]
    fn telemetry_tracks_current_aligned_streak() {
        let mut telemetry = super::PolicyTelemetry::default();
        let aligned = super::PolicyComparison {
            shadow: super::ShadowDecision::Wait,
            backend: super::BackendIntentSummary::Wait,
            aligned: true,
            reason: "both wait",
        };
        let mismatched = super::PolicyComparison {
            shadow: super::ShadowDecision::Wait,
            backend: super::BackendIntentSummary::UseScroll,
            aligned: false,
            reason: "different intent kind",
        };

        telemetry.record(aligned.clone());
        telemetry.record(aligned);
        assert_eq!(telemetry.aligned_streak, 2);

        telemetry.record(mismatched);
        assert_eq!(telemetry.aligned_streak, 0);
    }
}
