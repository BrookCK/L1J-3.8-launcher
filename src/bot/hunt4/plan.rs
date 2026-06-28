use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::bot::decide::hunt::{
    AttackSequenceStep, AttackStepKind, MELEE_RANGE_TILES, RANGED_RANGE_TILES,
};
use crate::bot::decide::pathfind::Walkable;
use crate::bot::hunt4::memory::TacticalMemory;
use crate::bot::hunt4::model::{Snapshot, LOCAL_CLEAR_RADIUS_TILES};
use crate::bot::hunt4::planner;
use crate::bot::hunt4::route::{self, RoutePlan};
use crate::bot::hunt4::step::{AttackStepInput, ExploreSuggestion};
use crate::minimap::nav_profile::NavProfile;

use super::candidate::{self, CandidateFrame};
use super::score::{self, TargetScoreSummary};
use super::world::WorldFrame;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFrame {
    pub now: Instant,
    pub in_game: bool,
    pub map_id: Option<u32>,
    pub player_tile: Option<(i32, i32)>,
    pub attack_range: u32,
    pub selected_attack_step: Option<AttackSequenceStep>,
    pub selected_skill_range: Option<u32>,
    pub selected_skill_cd_ms: Option<u64>,
    pub selected_skill_ready: Option<bool>,
    pub post_skill_basic_pending_target: Option<u32>,
    pub recent_attacker_count: usize,
    pub candidate_count: usize,
    pub reachable_count: usize,
    pub attacker_count: usize,
    pub ranked_targets: Vec<TargetScoreSummary>,
    pub route_plan: Option<RoutePlan>,
    pub selected_target_id: Option<u32>,
    pub selected_target_reason: Option<String>,
    pub non_viable_target_count: usize,
    pub last_target_reject_reason: Option<String>,
    pub abandoned_target_reason: Option<String>,
    pub selected_route_target_id: Option<u32>,
    pub selected_route_next_tile: Option<(i32, i32)>,
    pub selected_route_reason: Option<String>,
    pub movement_next_tile: Option<(i32, i32)>,
    pub movement_reason: Option<String>,
    pub teleport_should_use: bool,
    pub teleport_reason: Option<String>,
    pub final_action_kind: String,
    pub invariant_violations: Vec<String>,
    pub explore_goal: Option<(i32, i32)>,
    pub explore_next_tile: Option<(i32, i32)>,
    pub explore_steps: Option<usize>,
}

pub struct PlanFrameInput<'a> {
    pub world: &'a WorldFrame,
    pub candidate_frame: &'a CandidateFrame,
    pub explore: Option<&'a ExploreSuggestion>,
    pub selected_attack_step: Option<&'a AttackSequenceStep>,
    pub selected_skill_range: Option<u32>,
    pub selected_skill_cd_ms: Option<u64>,
    pub selected_skill_ready: Option<bool>,
    pub post_skill_basic_pending_target: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePlanEvaluation {
    pub candidate_frame: CandidateFrame,
    pub explore: Option<ExploreSuggestion>,
    pub plan_frame: PlanFrame,
    pub backend_skill_cd_ms: u64,
    pub backend_attack_step: AttackStepInput,
    pub damage_spike_detected: bool,
    pub critical_hp_detected: bool,
}

pub struct RuntimePlanEvaluationInput<'a, G: Walkable> {
    pub world: &'a WorldFrame,
    pub player_tile: (i32, i32),
    pub ranged_weapon: bool,
    pub grid: Option<&'a G>,
    pub map_profile: Option<&'a NavProfile>,
    pub memory: &'a TacticalMemory,
    pub previous_hp: Option<u32>,
    pub current_hp: Option<u32>,
    pub max_hp: u32,
    pub damage_spike_hp_percent: u8,
    pub server_recent_attackers: HashSet<u32>,
    pub selected_attack_step: Option<&'a AttackSequenceStep>,
    pub selected_skill_range: Option<u32>,
    pub selected_skill_cd_ms: Option<u64>,
    pub last_skill_cast: Option<Instant>,
    pub post_skill_basic_pending_target: Option<u32>,
}

impl PlanFrame {
    pub fn top_target(&self) -> Option<&TargetScoreSummary> {
        self.ranked_targets.first()
    }
}

pub fn damage_spike_absolute_threshold(percent: u8, max_hp: u32) -> Option<u32> {
    if percent == 0 || max_hp == 0 {
        return None;
    }
    let raw = (max_hp as u64 * percent as u64) / 100;
    Some((raw as u32).max(1))
}

pub fn detect_damage_spike(prev_hp: Option<u32>, cur_hp: u32, hp_threshold: u32) -> bool {
    let Some(prev) = prev_hp else {
        return false;
    };
    prev.saturating_sub(cur_hp) >= hp_threshold
}

pub fn hp_drop_detected(prev_hp: Option<u32>, cur_hp: Option<u32>) -> bool {
    matches!((prev_hp, cur_hp), (Some(prev), Some(cur)) if cur > 0 && cur < prev)
}

pub const CRITICAL_HP_PERCENT: u32 = 15;

pub fn detect_critical_hp(cur_hp: u32, max_hp: u32) -> bool {
    if max_hp == 0 || cur_hp == 0 {
        return false;
    }
    let percent = (cur_hp as u64 * 100) / max_hp as u64;
    percent <= CRITICAL_HP_PERCENT as u64
}

pub fn attack_range_for_weapon(ranged_weapon: bool) -> u32 {
    if ranged_weapon {
        RANGED_RANGE_TILES
    } else {
        MELEE_RANGE_TILES
    }
}

pub fn attack_range_for_step(
    ranged_weapon: bool,
    step: Option<&AttackSequenceStep>,
    skill_range: Option<u32>,
) -> u32 {
    let basic = attack_range_for_weapon(ranged_weapon);
    let Some(step) = step else {
        return basic;
    };
    match step.normalized().kind {
        AttackStepKind::Skill => skill_range.filter(|range| *range > 0).unwrap_or(basic),
        AttackStepKind::Basic => basic,
    }
}

pub fn pressure_attackers_for_hp_drop(
    snapshot: &Snapshot,
    player: (i32, i32),
    mut recent_attackers: HashSet<u32>,
    hp_drop: bool,
) -> HashSet<u32> {
    if !hp_drop {
        return recent_attackers;
    }

    let local: Vec<_> = snapshot
        .valid_targets()
        .filter(|entity| entity.distance_from(player) <= LOCAL_CLEAR_RADIUS_TILES)
        .collect();

    if local.is_empty() {
        if let Some(nearest) = snapshot
            .valid_targets()
            .min_by_key(|entity| (entity.distance_from(player), entity.target_id))
        {
            recent_attackers.insert(nearest.target_id);
        }
    } else {
        for entity in local {
            recent_attackers.insert(entity.target_id);
        }
    }

    recent_attackers
}

pub fn evaluate_runtime_plan<G: Walkable>(
    input: RuntimePlanEvaluationInput<'_, G>,
) -> RuntimePlanEvaluation {
    let hp_drop_detected = hp_drop_detected(input.previous_hp, input.current_hp);
    let damage_spike_detected =
        damage_spike_absolute_threshold(input.damage_spike_hp_percent, input.max_hp)
            .map(|threshold| {
                detect_damage_spike(input.previous_hp, input.current_hp.unwrap_or(0), threshold)
            })
            .unwrap_or(false);
    let critical_hp_detected = detect_critical_hp(input.current_hp.unwrap_or(0), input.max_hp);
    let attack_range = attack_range_for_step(
        input.ranged_weapon,
        input.selected_attack_step,
        input.selected_skill_range,
    );
    let recent_attackers = pressure_attackers_for_hp_drop(
        &input.world.snapshot,
        input.player_tile,
        input.server_recent_attackers,
        hp_drop_detected,
    );
    let candidate_frame = candidate::build_candidate_frame(candidate::CandidateFrameInput {
        snapshot: &input.world.snapshot,
        player_tile: input.player_tile,
        attack_range,
        grid: input.grid,
        memory: input.memory,
        recent_attackers: &recent_attackers,
        now: input.world.now,
    });
    let explore = planner::build_explore_suggestion_with_profile(
        input.player_tile,
        input.world.map_id,
        input.grid,
        input.map_profile,
        input.memory,
        input.world.now,
    );
    let selected_skill_ready = selected_skill_ready(
        input.selected_attack_step,
        input.selected_skill_cd_ms,
        input.last_skill_cast,
        input.world.now,
    );
    let plan_frame = build_plan_frame(PlanFrameInput {
        world: input.world,
        candidate_frame: &candidate_frame,
        explore: explore.as_ref(),
        selected_attack_step: input.selected_attack_step,
        selected_skill_range: input.selected_skill_range,
        selected_skill_cd_ms: input.selected_skill_cd_ms,
        selected_skill_ready,
        post_skill_basic_pending_target: input.post_skill_basic_pending_target,
    });
    let backend_skill_cd_ms = plan_frame.selected_skill_cd_ms.unwrap_or(0);
    let backend_attack_step = plan_frame
        .selected_attack_step
        .clone()
        .map(AttackStepInput::Step)
        .unwrap_or(AttackStepInput::Waiting);

    RuntimePlanEvaluation {
        candidate_frame,
        explore,
        plan_frame,
        backend_skill_cd_ms,
        backend_attack_step,
        damage_spike_detected,
        critical_hp_detected,
    }
}

pub fn build_plan_frame(input: PlanFrameInput<'_>) -> PlanFrame {
    let ranked_targets = score::summarize_ranked_candidates(&input.candidate_frame.candidates);
    let reachable_count = ranked_targets
        .iter()
        .filter(|target| target.reachable)
        .count();
    let attacker_count = ranked_targets
        .iter()
        .filter(|target| target.is_attacker)
        .count();
    let route_plan = route::route_for_plan(
        &input.candidate_frame.target_selection,
        &input.candidate_frame.candidates,
        input.explore,
    );
    let selected_target = input.candidate_frame.target_selection.selected.as_ref();
    let selected_target_id = selected_target.map(|target| target.target_id);
    let selected_target_reason = selected_target.map(|target| target.reason.clone());
    let selected_route_target_id = route_plan.as_ref().and_then(|route| route.target_id);
    let selected_route_next_tile = route_plan.as_ref().and_then(|route| route.next_tile);
    let selected_route_reason = route_plan.as_ref().map(route_summary_reason);
    let movement_next_tile = selected_route_next_tile;
    let movement_reason = movement_next_tile.and(selected_route_reason.clone());

    PlanFrame {
        now: input.world.now,
        in_game: input.world.in_game,
        map_id: input.world.map_id,
        player_tile: input.world.player_tile(),
        attack_range: input.candidate_frame.attack_range,
        selected_attack_step: input.selected_attack_step.cloned(),
        selected_skill_range: input.selected_skill_range,
        selected_skill_cd_ms: input.selected_skill_cd_ms,
        selected_skill_ready: input.selected_skill_ready,
        post_skill_basic_pending_target: input.post_skill_basic_pending_target,
        recent_attacker_count: input.candidate_frame.recent_attacker_count,
        candidate_count: input.candidate_frame.candidates.len(),
        reachable_count,
        attacker_count,
        ranked_targets,
        route_plan,
        selected_target_id,
        selected_target_reason,
        non_viable_target_count: input.candidate_frame.target_selection.rejected.len(),
        last_target_reject_reason: input
            .candidate_frame
            .target_selection
            .rejected
            .first()
            .map(|reject| reject.reason.clone()),
        abandoned_target_reason: None,
        selected_route_target_id,
        selected_route_next_tile,
        selected_route_reason,
        movement_next_tile,
        movement_reason,
        teleport_should_use: false,
        teleport_reason: None,
        final_action_kind: "wait".to_string(),
        invariant_violations: Vec::new(),
        explore_goal: input.explore.map(|explore| explore.goal),
        explore_next_tile: input
            .explore
            .and_then(|explore| explore.path.first().copied()),
        explore_steps: input.explore.map(|explore| explore.path.len()),
    }
}

fn route_summary_reason(route: &RoutePlan) -> String {
    match route.kind {
        route::RouteKind::Attack => "target_approach",
        route::RouteKind::Explore => "explore",
    }
    .to_string()
}

pub fn selected_skill_ready(
    selected_attack_step: Option<&AttackSequenceStep>,
    selected_skill_cd_ms: Option<u64>,
    last_skill_cast: Option<Instant>,
    now: Instant,
) -> Option<bool> {
    selected_attack_step.and_then(AttackSequenceStep::skill_for_cd)?;
    let cd_ms = selected_skill_cd_ms?;
    if cd_ms == 0 {
        return Some(true);
    }
    Some(
        last_skill_cast
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_millis(cd_ms)),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use crate::bot::decide::hunt::{AttackSequenceStep, AttackStepKind};
    use crate::bot::decide::pathfind::Walkable;
    use crate::bot::hunt4::candidate::CandidateFrame;
    use crate::bot::hunt4::memory::TacticalMemory;
    use crate::bot::hunt4::model::{EntityView, Snapshot};
    use crate::bot::hunt4::step::{ExploreSuggestion, TargetCandidate};
    use crate::bot::hunt4::world::WorldFrame;
    use crate::bot::perception::classifier::EntityClass;
    use crate::bot::perception::position::PlayerPosition;

    struct OpenGrid;

    impl Walkable for OpenGrid {
        fn is_walkable(&self, _tile_x: i32, _tile_y: i32) -> bool {
            true
        }
    }

    fn candidate(
        target_id: u32,
        distance: u32,
        in_attack_range: bool,
        reachable_path: Option<Vec<(i32, i32)>>,
        is_attacker: bool,
    ) -> TargetCandidate {
        TargetCandidate {
            target_id,
            entity_ptr: target_id + 0x1000,
            name: format!("mob_{target_id:X}"),
            tile: (target_id as i32, target_id as i32 + 1),
            distance,
            in_attack_range,
            reachable_path,
            is_attacker,
        }
    }

    fn world(now: Instant, in_game: bool) -> WorldFrame {
        WorldFrame {
            now,
            in_game,
            map_id: in_game.then_some(4),
            player_view: None,
            player_pos_data: in_game.then_some(PlayerPosition { x: 33000, y: 33000 }),
            snapshot: Snapshot::default(),
        }
    }

    fn candidate_frame(
        now: Instant,
        player_tile: (i32, i32),
        attack_range: u32,
        recent_attacker_count: usize,
        mut candidates: Vec<TargetCandidate>,
    ) -> CandidateFrame {
        crate::bot::hunt4::targeting::sort_candidates(&mut candidates);
        CandidateFrame {
            now,
            player_tile,
            attack_range,
            recent_attacker_count,
            candidates,
            target_selection: Default::default(),
        }
    }

    fn live_mob(target_id: u32, tile: (i32, i32)) -> EntityView {
        EntityView {
            target_id,
            entity_ptr: target_id + 0x1000,
            name: format!("mob_{target_id:X}"),
            sprite_id: 2489,
            action_state: 0,
            tile,
            raw_x: tile.0 as u32,
            y: tile.1 as u32,
            class: EntityClass::AttackableMonster,
            visible_confidence: 100,
            hostile_confidence: 100,
        }
    }

    fn hidden_mob(target_id: u32, tile: (i32, i32)) -> EntityView {
        EntityView {
            target_id,
            entity_ptr: target_id + 0x1000,
            name: format!("hidden_{target_id:X}"),
            sprite_id: 2489,
            action_state: 0x0D,
            tile,
            raw_x: tile.0 as u32,
            y: tile.1 as u32,
            class: EntityClass::NonWorldMonsterState,
            visible_confidence: 100,
            hostile_confidence: 0,
        }
    }

    #[test]
    fn build_plan_frame_records_ranked_targets_and_explore_summary() {
        let now = Instant::now();
        let frame = world(now, true);
        let candidates = [
            candidate(0x0100_0001, 2, true, Some(Vec::new()), false),
            candidate(
                0x0100_0002,
                7,
                false,
                Some(vec![(33001, 33000), (33002, 33000)]),
                true,
            ),
            candidate(0x0100_0003, 1, false, None, false),
        ];
        let explore = ExploreSuggestion {
            goal: (33010, 33000),
            path: vec![(33001, 33000), (33002, 33000)],
        };
        let candidate_frame = candidate_frame(now, (33000, 33000), 5, 1, candidates.to_vec());

        let plan = super::build_plan_frame(super::PlanFrameInput {
            world: &frame,
            candidate_frame: &candidate_frame,
            explore: Some(&explore),
            selected_attack_step: None,
            selected_skill_range: None,
            selected_skill_cd_ms: None,
            selected_skill_ready: None,
            post_skill_basic_pending_target: None,
        });

        assert_eq!(plan.now, now);
        assert!(plan.in_game);
        assert_eq!(plan.map_id, Some(4));
        assert_eq!(plan.player_tile, Some((33000, 33000)));
        assert_eq!(plan.attack_range, 5);
        assert_eq!(plan.recent_attacker_count, 1);
        assert_eq!(plan.candidate_count, 3);
        assert_eq!(plan.reachable_count, 2);
        assert_eq!(plan.attacker_count, 1);
        assert_eq!(plan.explore_goal, Some((33010, 33000)));
        assert_eq!(plan.explore_next_tile, Some((33001, 33000)));
        assert_eq!(plan.explore_steps, Some(2));
        assert_eq!(plan.ranked_targets[0].target_id, 0x0100_0001);
        assert_eq!(plan.top_target().unwrap().target_id, 0x0100_0001);
    }

    #[test]
    fn build_plan_frame_records_attack_route_for_selected_target() {
        let now = Instant::now();
        let frame = world(now, true);
        let candidates = vec![candidate(
            0x0100_0002,
            7,
            false,
            Some(vec![(33001, 33000), (33002, 33000)]),
            true,
        )];
        let mut candidate_frame = candidate_frame(now, (33000, 33000), 5, 1, candidates);
        candidate_frame.target_selection = crate::bot::hunt4::targeting::select_target(
            &candidate_frame.candidates,
            &Snapshot::default(),
        );

        let plan = super::build_plan_frame(super::PlanFrameInput {
            world: &frame,
            candidate_frame: &candidate_frame,
            explore: None,
            selected_attack_step: None,
            selected_skill_range: None,
            selected_skill_cd_ms: None,
            selected_skill_ready: None,
            post_skill_basic_pending_target: None,
        });

        let route = plan.route_plan.expect("selected target route");
        assert_eq!(route.kind, crate::bot::hunt4::route::RouteKind::Attack);
        assert_eq!(
            route.status,
            crate::bot::hunt4::route::RouteStatus::Reachable
        );
        assert_eq!(route.target_id, Some(0x0100_0002));
        assert_eq!(route.next_tile, Some((33001, 33000)));
        assert_eq!(route.path, vec![(33001, 33000), (33002, 33000)]);
    }

    #[test]
    fn build_plan_frame_records_target_route_movement_and_default_action_summary() {
        let now = Instant::now();
        let frame = world(now, true);
        let candidates = vec![candidate(
            0x0100_0002,
            7,
            false,
            Some(vec![(33001, 33000), (33002, 33000)]),
            true,
        )];
        let snapshot = Snapshot {
            player: Some((33000, 33000)),
            entities: vec![hidden_mob(0x0100_0099, (33003, 33000))],
        };
        let mut candidate_frame = candidate_frame(now, (33000, 33000), 5, 1, candidates);
        candidate_frame.target_selection =
            crate::bot::hunt4::targeting::select_target(&candidate_frame.candidates, &snapshot);

        let plan = super::build_plan_frame(super::PlanFrameInput {
            world: &frame,
            candidate_frame: &candidate_frame,
            explore: None,
            selected_attack_step: None,
            selected_skill_range: None,
            selected_skill_cd_ms: None,
            selected_skill_ready: None,
            post_skill_basic_pending_target: None,
        });

        assert_eq!(plan.selected_target_id, Some(0x0100_0002));
        assert_eq!(
            plan.selected_target_reason.as_deref(),
            Some("reachable approach attacker distance=7")
        );
        assert_eq!(plan.non_viable_target_count, 1);
        assert_eq!(
            plan.last_target_reject_reason.as_deref(),
            Some("HiddenOrBurrowed action_state=0x0D")
        );
        assert_eq!(plan.abandoned_target_reason, None);
        assert_eq!(plan.selected_route_target_id, Some(0x0100_0002));
        assert_eq!(plan.selected_route_next_tile, Some((33001, 33000)));
        assert_eq!(
            plan.selected_route_reason.as_deref(),
            Some("target_approach")
        );
        assert_eq!(plan.movement_next_tile, Some((33001, 33000)));
        assert_eq!(plan.movement_reason.as_deref(), Some("target_approach"));
        assert!(!plan.teleport_should_use);
        assert_eq!(plan.teleport_reason, None);
        assert_eq!(plan.final_action_kind, "wait");
        assert!(plan.invariant_violations.is_empty());
    }

    #[test]
    fn build_plan_frame_records_explore_route_movement_summary_without_target() {
        let now = Instant::now();
        let frame = world(now, true);
        let explore = ExploreSuggestion {
            goal: (33010, 33000),
            path: vec![(33001, 33000), (33002, 33000)],
        };
        let candidate_frame = candidate_frame(now, (33000, 33000), 5, 0, Vec::new());

        let plan = super::build_plan_frame(super::PlanFrameInput {
            world: &frame,
            candidate_frame: &candidate_frame,
            explore: Some(&explore),
            selected_attack_step: None,
            selected_skill_range: None,
            selected_skill_cd_ms: None,
            selected_skill_ready: None,
            post_skill_basic_pending_target: None,
        });

        assert_eq!(plan.selected_target_id, None);
        assert_eq!(plan.selected_target_reason, None);
        assert_eq!(plan.selected_route_target_id, None);
        assert_eq!(plan.selected_route_next_tile, Some((33001, 33000)));
        assert_eq!(plan.selected_route_reason.as_deref(), Some("explore"));
        assert_eq!(plan.movement_next_tile, Some((33001, 33000)));
        assert_eq!(plan.movement_reason.as_deref(), Some("explore"));
        assert_eq!(plan.final_action_kind, "wait");
        assert!(plan.invariant_violations.is_empty());
    }

    #[test]
    fn evaluate_runtime_plan_builds_candidate_and_plan_frames() {
        let now = Instant::now();
        let target_id = 0x0100_0100;
        let mut frame = world(now, true);
        frame.snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(target_id, (101, 100))],
        };
        let grid = OpenGrid;
        let memory = TacticalMemory::default();

        let evaluation = super::evaluate_runtime_plan(super::RuntimePlanEvaluationInput {
            world: &frame,
            player_tile: (100, 100),
            ranged_weapon: false,
            grid: Some(&grid),
            map_profile: None,
            memory: &memory,
            previous_hp: None,
            current_hp: None,
            max_hp: 0,
            damage_spike_hp_percent: 0,
            server_recent_attackers: HashSet::from([target_id]),
            selected_attack_step: None,
            selected_skill_range: None,
            selected_skill_cd_ms: None,
            last_skill_cast: None,
            post_skill_basic_pending_target: None,
        });

        assert_eq!(evaluation.candidate_frame.now, now);
        assert_eq!(evaluation.candidate_frame.recent_attacker_count, 1);
        assert_eq!(evaluation.plan_frame.candidate_count, 1);
        assert_eq!(evaluation.plan_frame.attacker_count, 1);
        assert_eq!(
            evaluation.plan_frame.top_target().unwrap().target_id,
            target_id
        );
        let explore = evaluation.explore.as_ref().expect("explore suggestion");
        assert_eq!(evaluation.plan_frame.explore_goal, Some(explore.goal));
        assert_eq!(
            evaluation.plan_frame.explore_next_tile,
            explore.path.first().copied()
        );
    }

    #[test]
    fn evaluate_runtime_plan_computes_selected_skill_ready_from_last_cast() {
        let now = Instant::now();
        let frame = world(now, true);
        let grid = OpenGrid;
        let memory = TacticalMemory::default();
        let attack_step = AttackSequenceStep::skill("Frozen Cloud".to_string(), 100);

        let evaluation = super::evaluate_runtime_plan(super::RuntimePlanEvaluationInput {
            world: &frame,
            player_tile: (100, 100),
            ranged_weapon: false,
            grid: Some(&grid),
            map_profile: None,
            memory: &memory,
            previous_hp: None,
            current_hp: None,
            max_hp: 0,
            damage_spike_hp_percent: 0,
            server_recent_attackers: HashSet::new(),
            selected_attack_step: Some(&attack_step),
            selected_skill_range: Some(7),
            selected_skill_cd_ms: Some(2000),
            last_skill_cast: Some(now - Duration::from_millis(500)),
            post_skill_basic_pending_target: None,
        });

        assert_eq!(evaluation.plan_frame.selected_skill_ready, Some(false));
        assert_eq!(evaluation.plan_frame.selected_skill_cd_ms, Some(2000));
        assert_eq!(evaluation.plan_frame.attack_range, 7);
    }

    #[test]
    fn evaluate_runtime_plan_returns_backend_step_inputs_from_plan_frame() {
        let now = Instant::now();
        let frame = world(now, true);
        let grid = OpenGrid;
        let memory = TacticalMemory::default();
        let attack_step = AttackSequenceStep::skill("Frozen Cloud".to_string(), 100);

        let evaluation = super::evaluate_runtime_plan(super::RuntimePlanEvaluationInput {
            world: &frame,
            player_tile: (100, 100),
            ranged_weapon: false,
            grid: Some(&grid),
            map_profile: None,
            memory: &memory,
            previous_hp: None,
            current_hp: None,
            max_hp: 0,
            damage_spike_hp_percent: 0,
            server_recent_attackers: HashSet::new(),
            selected_attack_step: Some(&attack_step),
            selected_skill_range: Some(7),
            selected_skill_cd_ms: Some(2000),
            last_skill_cast: None,
            post_skill_basic_pending_target: None,
        });

        assert_eq!(evaluation.backend_skill_cd_ms, 2000);
        assert_eq!(
            evaluation.backend_attack_step,
            crate::bot::hunt4::step::AttackStepInput::Step(attack_step)
        );
    }

    #[test]
    fn evaluate_runtime_plan_computes_attack_range_from_weapon_and_step() {
        let now = Instant::now();
        let mut frame = world(now, true);
        frame.snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x0100_0200, (108, 100))],
        };
        let grid = OpenGrid;
        let memory = TacticalMemory::default();

        let ranged_basic = super::evaluate_runtime_plan(super::RuntimePlanEvaluationInput {
            world: &frame,
            player_tile: (100, 100),
            ranged_weapon: true,
            grid: Some(&grid),
            map_profile: None,
            memory: &memory,
            previous_hp: None,
            current_hp: None,
            max_hp: 0,
            damage_spike_hp_percent: 0,
            server_recent_attackers: HashSet::new(),
            selected_attack_step: None,
            selected_skill_range: None,
            selected_skill_cd_ms: None,
            last_skill_cast: None,
            post_skill_basic_pending_target: None,
        });
        assert_eq!(
            ranged_basic.plan_frame.attack_range,
            crate::bot::decide::hunt::RANGED_RANGE_TILES
        );

        let skill_step = AttackSequenceStep::skill("Frozen Cloud".to_string(), 100);
        let skill = super::evaluate_runtime_plan(super::RuntimePlanEvaluationInput {
            world: &frame,
            player_tile: (100, 100),
            ranged_weapon: false,
            grid: Some(&grid),
            map_profile: None,
            memory: &memory,
            previous_hp: None,
            current_hp: None,
            max_hp: 0,
            damage_spike_hp_percent: 0,
            server_recent_attackers: HashSet::new(),
            selected_attack_step: Some(&skill_step),
            selected_skill_range: Some(7),
            selected_skill_cd_ms: Some(2000),
            last_skill_cast: None,
            post_skill_basic_pending_target: None,
        });
        assert_eq!(skill.plan_frame.attack_range, 7);
    }

    #[test]
    fn evaluate_runtime_plan_expands_recent_attackers_from_hp_drop() {
        let now = Instant::now();
        let mut frame = world(now, true);
        let local_target_id = 0x0100_0300;
        let far_target_id = 0x0100_0301;
        frame.snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                live_mob(local_target_id, (103, 100)),
                live_mob(far_target_id, (120, 100)),
            ],
        };
        let grid = OpenGrid;
        let memory = TacticalMemory::default();

        let evaluation = super::evaluate_runtime_plan(super::RuntimePlanEvaluationInput {
            world: &frame,
            player_tile: (100, 100),
            ranged_weapon: false,
            grid: Some(&grid),
            map_profile: None,
            memory: &memory,
            previous_hp: Some(100),
            current_hp: Some(90),
            max_hp: 100,
            damage_spike_hp_percent: 0,
            server_recent_attackers: HashSet::new(),
            selected_attack_step: None,
            selected_skill_range: None,
            selected_skill_cd_ms: None,
            last_skill_cast: None,
            post_skill_basic_pending_target: None,
        });

        assert_eq!(evaluation.plan_frame.recent_attacker_count, 1);
        assert!(evaluation
            .candidate_frame
            .candidates
            .iter()
            .any(|candidate| candidate.target_id == local_target_id && candidate.is_attacker));
        assert!(evaluation
            .candidate_frame
            .candidates
            .iter()
            .any(|candidate| candidate.target_id == far_target_id && !candidate.is_attacker));
    }

    #[test]
    fn evaluate_runtime_plan_computes_survival_pressure_facts_from_hp() {
        let now = Instant::now();
        let mut frame = world(now, true);
        let target_id = 0x0100_0400;
        frame.snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(target_id, (102, 100))],
        };
        let grid = OpenGrid;
        let memory = TacticalMemory::default();

        let evaluation = super::evaluate_runtime_plan(super::RuntimePlanEvaluationInput {
            world: &frame,
            player_tile: (100, 100),
            ranged_weapon: false,
            grid: Some(&grid),
            map_profile: None,
            memory: &memory,
            previous_hp: Some(1000),
            current_hp: Some(100),
            max_hp: 1000,
            damage_spike_hp_percent: 10,
            server_recent_attackers: HashSet::new(),
            selected_attack_step: None,
            selected_skill_range: None,
            selected_skill_cd_ms: None,
            last_skill_cast: None,
            post_skill_basic_pending_target: None,
        });

        assert!(evaluation.damage_spike_detected);
        assert!(evaluation.critical_hp_detected);
        assert_eq!(evaluation.plan_frame.recent_attacker_count, 1);
        assert!(evaluation
            .candidate_frame
            .candidates
            .iter()
            .any(|candidate| candidate.target_id == target_id && candidate.is_attacker));
    }

    #[test]
    fn build_plan_frame_keeps_empty_target_summary_without_candidates() {
        let frame = world(Instant::now(), false);
        let candidate_frame = candidate_frame(frame.now, (0, 0), 1, 0, Vec::new());

        let plan = super::build_plan_frame(super::PlanFrameInput {
            world: &frame,
            candidate_frame: &candidate_frame,
            explore: None,
            selected_attack_step: None,
            selected_skill_range: None,
            selected_skill_cd_ms: None,
            selected_skill_ready: None,
            post_skill_basic_pending_target: None,
        });

        assert!(!plan.in_game);
        assert_eq!(plan.map_id, None);
        assert_eq!(plan.player_tile, None);
        assert_eq!(plan.attack_range, 1);
        assert_eq!(plan.recent_attacker_count, 0);
        assert_eq!(plan.candidate_count, 0);
        assert_eq!(plan.reachable_count, 0);
        assert_eq!(plan.attacker_count, 0);
        assert!(plan.ranked_targets.is_empty());
        assert_eq!(plan.explore_next_tile, None);
        assert!(plan.top_target().is_none());
    }

    #[test]
    fn build_plan_frame_records_selected_basic_attack_step() {
        let frame = world(Instant::now(), true);
        let candidate_frame = candidate_frame(frame.now, (33000, 33000), 1, 0, Vec::new());
        let attack_step = AttackSequenceStep::basic(250);

        let plan = super::build_plan_frame(super::PlanFrameInput {
            world: &frame,
            candidate_frame: &candidate_frame,
            explore: None,
            selected_attack_step: Some(&attack_step),
            selected_skill_range: None,
            selected_skill_cd_ms: None,
            selected_skill_ready: None,
            post_skill_basic_pending_target: None,
        });

        assert_eq!(plan.selected_attack_step, Some(attack_step));
        assert_eq!(plan.selected_skill_range, None);
    }

    #[test]
    fn build_plan_frame_records_selected_skill_attack_step() {
        let frame = world(Instant::now(), true);
        let candidate_frame = candidate_frame(frame.now, (33000, 33000), 7, 0, Vec::new());
        let attack_step = AttackSequenceStep::skill("triple_arrow".to_string(), 100);

        let plan = super::build_plan_frame(super::PlanFrameInput {
            world: &frame,
            candidate_frame: &candidate_frame,
            explore: None,
            selected_attack_step: Some(&attack_step),
            selected_skill_range: Some(7),
            selected_skill_cd_ms: Some(0),
            selected_skill_ready: Some(true),
            post_skill_basic_pending_target: None,
        });

        let recorded = plan
            .selected_attack_step
            .as_ref()
            .expect("selected attack step");
        assert_eq!(recorded.kind, AttackStepKind::Skill);
        assert_eq!(recorded.skill_name, "triple_arrow");
        assert_eq!(recorded.interval_ms, 100);
        assert_eq!(plan.selected_skill_range, Some(7));
    }

    #[test]
    fn build_plan_frame_records_skill_gate_and_pending_basic_diagnostics() {
        let frame = world(Instant::now(), true);
        let candidate_frame = candidate_frame(frame.now, (33000, 33000), 7, 0, Vec::new());
        let attack_step = AttackSequenceStep::skill("triple_arrow".to_string(), 100);

        let plan = super::build_plan_frame(super::PlanFrameInput {
            world: &frame,
            candidate_frame: &candidate_frame,
            explore: None,
            selected_attack_step: Some(&attack_step),
            selected_skill_range: Some(7),
            selected_skill_cd_ms: Some(2000),
            selected_skill_ready: Some(false),
            post_skill_basic_pending_target: Some(0x30008),
        });

        assert_eq!(plan.selected_skill_cd_ms, Some(2000));
        assert_eq!(plan.selected_skill_ready, Some(false));
        assert_eq!(plan.post_skill_basic_pending_target, Some(0x30008));
    }

    #[test]
    fn selected_skill_ready_ignores_basic_steps() {
        let now = Instant::now();
        let attack_step = AttackSequenceStep::basic(0);

        assert_eq!(
            super::selected_skill_ready(Some(&attack_step), Some(2000), Some(now), now),
            None
        );
    }

    #[test]
    fn selected_skill_ready_tracks_last_cast_gate() {
        let now = Instant::now();
        let attack_step = AttackSequenceStep::skill("triple_arrow".to_string(), 0);

        assert_eq!(
            super::selected_skill_ready(
                Some(&attack_step),
                Some(2000),
                Some(now - Duration::from_millis(500)),
                now,
            ),
            Some(false)
        );
        assert_eq!(
            super::selected_skill_ready(
                Some(&attack_step),
                Some(2000),
                Some(now - Duration::from_millis(2000)),
                now,
            ),
            Some(true)
        );
        assert_eq!(
            super::selected_skill_ready(Some(&attack_step), Some(2000), None, now),
            Some(true)
        );
        assert_eq!(
            super::selected_skill_ready(Some(&attack_step), Some(0), Some(now), now),
            Some(true)
        );
    }
}
