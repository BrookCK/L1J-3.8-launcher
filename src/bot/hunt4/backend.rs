use std::time::Instant;

use windows::Win32::Foundation::HANDLE;

use crate::bot::action::walk::WalkDriver;
use crate::bot::decide::hunt::{AttackSequenceStep, HuntConfig, HuntOutcome};
use crate::bot::hunt4::actions::{dispatch, DispatchIntent, DispatchState};
use crate::bot::hunt4::context::{
    AttackProgressWatch, HuntContext, PureTickInputs, ATTACK_NO_PROGRESS_DURATION,
};
use crate::bot::hunt4::memory::{MemoryUpdate, TacticalMemory};
use crate::bot::hunt4::model::Snapshot;
use crate::bot::hunt4::observe::label_dispatch_intent;
use crate::bot::hunt4::state::{EngageIntent, HuntState};
use crate::bot::hunt4::step::LastOutcome;
use crate::bot::perception::position::PlayerPosition;
use crate::bot::scroll_match::teleport_scroll_item_matches;

use super::intent::DispatchEvaluation;
use super::plan::{self, RuntimePlanEvaluation};
use super::policy::PolicyTelemetry;
use super::tick;
use super::world::WorldFrame;

pub struct BackendPlanEvaluationInput<'a> {
    pub h: HANDLE,
    pub world: &'a WorldFrame,
    pub cfg: &'a HuntConfig,
    pub ctx: &'a mut HuntContext,
}

pub struct BackendTickInputsInput<'a, 'ctx> {
    pub h: HANDLE,
    pub world: &'a WorldFrame,
    pub planning: &'a RuntimePlanEvaluation,
    pub master_enabled: bool,
    pub ctx: &'ctx HuntContext,
    pub cfg: &'a HuntConfig,
}

pub struct BackendDispatchEvaluationInput<'a> {
    pub planning: &'a RuntimePlanEvaluation,
    pub snapshot: &'a Snapshot,
    pub dispatch_state: &'a DispatchState,
    pub backend_intent: &'a DispatchIntent,
    pub policy_telemetry: &'a mut PolicyTelemetry,
    pub takeover_enabled: bool,
}

pub struct BackendDispatchInput<'a> {
    pub h: HANDLE,
    pub intent: &'a DispatchIntent,
    pub player: Option<PlayerPosition>,
    pub walk_driver: WalkDriver,
    pub dispatch_state: &'a mut DispatchState,
    pub player_weight: u8,
}

pub fn resolve_skill_gate_ms_for(h: HANDLE, skill_name: &str, ctx: &mut HuntContext) -> u64 {
    use crate::bot::hunt4::skill_cd::{lookup_gate_ms, FALLBACK_GATE_MS};

    let skill_name = skill_name.trim();
    if skill_name.is_empty() {
        return 0;
    }
    if let Some(ms) = ctx.cached_skill_gate_ms.get(skill_name) {
        return *ms;
    }
    let gate = lookup_gate_ms(h, skill_name).unwrap_or(FALLBACK_GATE_MS);
    ctx.cached_skill_gate_ms
        .insert(skill_name.to_string(), gate);
    gate
}

pub fn resolve_skill_range_for(h: HANDLE, skill_name: &str, ctx: &mut HuntContext) -> u32 {
    use crate::aux::spell_book::SpellBook;

    let skill_name = skill_name.trim();
    if skill_name.is_empty() {
        return 0;
    }
    if let Some(range) = ctx.cached_skill_range.get(skill_name) {
        return *range;
    }
    let range = SpellBook::build(h)
        .ok()
        .and_then(|book| book.range_of(skill_name))
        .unwrap_or(0);
    ctx.cached_skill_range.insert(skill_name.to_string(), range);
    range
}

pub fn evaluate_plan_for_backend(input: BackendPlanEvaluationInput<'_>) -> RuntimePlanEvaluation {
    let map = if input.world.in_game {
        current_map(input.world.map_id)
    } else {
        None
    };
    let grid = map
        .as_deref()
        .map(|map| crate::bot::decide::pathfind::MapWalkable { map });
    let player_tile = input.world.player_tile().unwrap_or((0, 0));
    let selected_attack_step = input
        .ctx
        .attack_sequence
        .current_step(input.cfg, input.world.now);
    let selected_skill_name = selected_attack_step
        .as_ref()
        .and_then(AttackSequenceStep::skill_for_cd);
    let selected_skill_range =
        selected_skill_name.map(|name| resolve_skill_range_for(input.h, name, input.ctx));
    let selected_skill_cd_ms =
        selected_skill_name.map(|name| resolve_skill_gate_ms_for(input.h, name, input.ctx));

    plan::evaluate_runtime_plan(plan::RuntimePlanEvaluationInput {
        world: input.world,
        player_tile,
        ranged_weapon: crate::aux::weapon::is_ranged_weapon_equipped(input.h),
        grid: grid.as_ref(),
        map_profile: map.as_deref().map(|map| &map.profile),
        memory: &input.ctx.memory,
        previous_hp: input.ctx.prev_hp,
        current_hp: input.world.cur_hp(),
        max_hp: input.world.cur_max_hp(),
        damage_spike_hp_percent: input.cfg.damage_spike_hp_percent,
        server_recent_attackers: crate::aux::server_packet_hook::recent_attackers(input.world.now),
        selected_attack_step: selected_attack_step.as_ref(),
        selected_skill_range,
        selected_skill_cd_ms,
        last_skill_cast: input.ctx.memory.last_skill_cast,
        post_skill_basic_pending_target: input.ctx.memory.post_skill_basic_pending_target,
    })
}

pub fn evaluate_dispatch_for_backend(
    input: BackendDispatchEvaluationInput<'_>,
) -> DispatchEvaluation {
    super::intent::evaluate_dispatch_choice(
        &input.planning.plan_frame,
        input.snapshot,
        input.dispatch_state,
        input.backend_intent,
        input.policy_telemetry,
        input.takeover_enabled,
    )
}

pub fn build_tick_inputs<'a>(input: BackendTickInputsInput<'a, '_>) -> PureTickInputs<'a> {
    PureTickInputs {
        snapshot: &input.world.snapshot,
        candidates: input.planning.candidate_frame.candidates.as_slice(),
        explore: input.planning.explore.as_ref(),
        player_pos: input.world.player_tile(),
        player_alive: input.world.player_alive(),
        master_enabled: input.master_enabled,
        in_game: input.world.in_game,
        now: input.world.now,
        state_since: input.ctx.since,
        cfg: input.cfg,
        teleport_scroll_available: check_teleport_scroll_available(
            input.h,
            &input.cfg.teleport_scroll_name,
        ),
        damage_spike_detected: input.planning.damage_spike_detected,
        critical_hp_detected: input.planning.critical_hp_detected,
        locked_target_removed_or_dead: locked_target_removed_or_dead(input.ctx),
        locked_target_recently_damaged: locked_target_recently_damaged(input.ctx),
        skill_cd_ms: input.planning.backend_skill_cd_ms,
        attack_step: input.planning.backend_attack_step.clone(),
    }
}

pub fn run_pure_tick_for_backend(
    ctx: &mut HuntContext,
    inputs: PureTickInputs<'_>,
) -> DispatchIntent {
    tick::pure_tick(ctx, inputs)
}

pub fn record_plan_target_summary_after_evaluation(
    ctx: &mut HuntContext,
    planning: &RuntimePlanEvaluation,
) {
    ctx.last_target_summary = planning.plan_frame.top_target().cloned();
    ctx.last_route_next_tile = planning.plan_frame.selected_route_next_tile;
    ctx.last_route_reason = planning.plan_frame.selected_route_reason.clone();
    ctx.last_teleport_reason = planning.plan_frame.teleport_reason.clone();
}

pub fn record_dispatch_diagnostics_after_evaluation(
    ctx: &mut HuntContext,
    evaluation: DispatchEvaluation,
) -> DispatchIntent {
    let dispatch_intent = evaluation.dispatch_choice.intent.clone();
    ctx.last_shadow_dispatch = Some(evaluation.shadow_dispatch);
    ctx.last_dispatch_choice = Some(evaluation.dispatch_choice);
    ctx.last_policy_comparison = Some(evaluation.policy_comparison);
    dispatch_intent
}

pub fn record_actual_dispatch_action_after_choice(ctx: &mut HuntContext, intent: &DispatchIntent) {
    ctx.last_action = Some(label_dispatch_intent(intent));
}

pub fn dispatch_for_backend(input: BackendDispatchInput<'_>) -> LastOutcome {
    let outcome = dispatch(
        input.h,
        input.intent,
        input.player,
        input.walk_driver,
        input.dispatch_state,
    );
    log_teleport_scroll_attempt(input.intent, &outcome, input.player_weight);
    outcome
}

pub fn prepare_attack_watch_before_tick(ctx: &mut HuntContext, now: Instant) {
    let target_damaged_recently = locked_target_recently_damaged(ctx);
    prepare_attack_watch_before_tick_with_damage_observation(ctx, now, target_damaged_recently);
}

pub fn record_attack_bookkeeping_after_dispatch(
    ctx: &mut HuntContext,
    cfg: &HuntConfig,
    now: Instant,
    intent: &DispatchIntent,
    dispatch_outcome: &LastOutcome,
) {
    record_attack_sequence_after_dispatch(ctx, cfg, now, intent, dispatch_outcome);
    record_attack_watch_after_dispatch(ctx, dispatch_outcome, now);
}

fn log_teleport_scroll_attempt(intent: &DispatchIntent, outcome: &LastOutcome, player_weight: u8) {
    if let DispatchIntent::UseScroll { name_keyword } = intent {
        crate::log_line!(
            "[bot/hunt4/teleport] attempt keyword=\"{}\" outcome={:?} weight={}%",
            name_keyword,
            outcome,
            player_weight
        );
    }
}

pub fn record_dispatch_context_after_dispatch(
    ctx: &mut HuntContext,
    intent: &DispatchIntent,
    dispatch_outcome: &LastOutcome,
) {
    match (intent, dispatch_outcome) {
        (DispatchIntent::Walk { target_x, target_y }, LastOutcome::WalkOk) => {
            ctx.last_walk_tile = Some((*target_x, *target_y));
        }
        (DispatchIntent::Walk { .. }, LastOutcome::WalkFailed { .. }) => {
            ctx.last_walk_tile = None;
        }
        (DispatchIntent::UseScroll { .. }, LastOutcome::ScrollOk) => {
            ctx.last_walk_tile = None;
        }
        _ => {}
    }
    ctx.last_outcome = dispatch_outcome.clone();
}

pub fn publish_active_path(state: &HuntState) {
    use crate::bot::decide::pathfind;

    let path = match state {
        HuntState::Engaging {
            intent: EngageIntent::Approach,
            path: Some(p),
            ..
        } => p.clone(),
        HuntState::Exploring { path, .. } => path.clone(),
        _ => Vec::new(),
    };
    pathfind::set_bot_path(path);
}

pub fn advance_hp_baseline_after_plan(ctx: &mut HuntContext, cur_hp: Option<u32>) {
    if let Some(hp) = cur_hp {
        ctx.prev_hp = Some(hp);
    }
}

pub fn record_map_change_after_frame(ctx: &mut HuntContext, map_id: Option<u32>, now: Instant) {
    let portal_update =
        portal_avoid_update_for_map_change(ctx.last_map_id, map_id, ctx.last_walk_tile, now);
    if !portal_update.add_portal_avoid_tiles.is_empty() {
        crate::log_line!(
            "[bot/hunt4/portal] learned avoid tile(s) after map change prev={:?} cur={:?} tiles={:?}",
            ctx.last_map_id,
            map_id,
            portal_update.add_portal_avoid_tiles
        );
    }
    ctx.memory.apply(portal_update, now);
    ctx.last_map_id = map_id;
}

pub fn record_position_memory_after_frame(
    ctx: &mut HuntContext,
    player_pos: Option<(i32, i32)>,
    map_id: Option<u32>,
    now: Instant,
) {
    let Some(pos) = player_pos else {
        return;
    };
    let update = position_memory_update_for_map(&ctx.memory, pos, map_id, now);
    ctx.memory.apply(update, now);
}

pub fn position_memory_update(
    memory: &TacticalMemory,
    pos: (i32, i32),
    now: Instant,
) -> MemoryUpdate {
    let position_changed = memory
        .recent_positions
        .back()
        .is_none_or(|(x, y, _)| (*x, *y) != pos);

    MemoryUpdate {
        push_position: Some((pos, now)),
        set_last_position_change: position_changed.then_some(now),
        ..Default::default()
    }
}

pub fn position_memory_update_for_map(
    memory: &TacticalMemory,
    pos: (i32, i32),
    map_id: Option<u32>,
    now: Instant,
) -> MemoryUpdate {
    let mut update = position_memory_update(memory, pos, now);
    if let Some(map_id) = map_id {
        update.record_visited_tile = Some((map_id, pos, now));
    }
    update
}

pub fn portal_avoid_update_for_map_change(
    previous_map: Option<u32>,
    current_map: Option<u32>,
    last_walk_tile: Option<(i32, i32)>,
    now: Instant,
) -> MemoryUpdate {
    let changed = matches!((previous_map, current_map), (Some(prev), Some(cur)) if prev != cur);
    if !changed {
        return MemoryUpdate::default();
    }
    let Some(tile) = last_walk_tile else {
        return MemoryUpdate::default();
    };
    MemoryUpdate {
        add_portal_avoid_tiles: vec![(tile, now)],
        ..Default::default()
    }
}

pub fn intent_to_outcome(
    intent: &DispatchIntent,
    snapshot: &Snapshot,
    player_pos: Option<PlayerPosition>,
    dispatch_outcome: &LastOutcome,
) -> HuntOutcome {
    match intent {
        DispatchIntent::Noop => HuntOutcome::NoTarget,
        DispatchIntent::Walk { target_x, target_y } => {
            let heading = player_pos
                .and_then(|p| {
                    use crate::bot::action::walk::heading_from_delta;
                    heading_from_delta(target_x - p.x, target_y - p.y)
                })
                .unwrap_or(0);
            let distance = player_pos
                .map(|p| {
                    (target_x - p.x)
                        .unsigned_abs()
                        .max((target_y - p.y).unsigned_abs())
                })
                .unwrap_or(0);
            HuntOutcome::Walked {
                target_id: 0,
                name: "hunt4 walk".to_string(),
                heading,
                distance_tiles: distance,
            }
        }
        DispatchIntent::BootstrapAttack { entity_ptr, .. } => {
            let (target_id, name) = snapshot
                .entities
                .iter()
                .find(|e| e.entity_ptr == *entity_ptr)
                .map(|e| (e.target_id, e.name.clone()))
                .unwrap_or((0, "hunt4 melee".to_string()));
            HuntOutcome::Cast {
                target_id,
                name,
                player_pos,
            }
        }
        DispatchIntent::CastSkill {
            skill_name,
            target_id,
        } => {
            let name = snapshot
                .entities
                .iter()
                .find(|e| e.target_id == *target_id)
                .map(|e| format!("{} ({})", skill_name, e.name))
                .unwrap_or_else(|| skill_name.clone());
            HuntOutcome::Cast {
                target_id: *target_id,
                name,
                player_pos,
            }
        }
        DispatchIntent::UseScroll { name_keyword } => match dispatch_outcome {
            LastOutcome::ScrollOk => HuntOutcome::Cooldown { remaining_ms: 0 },
            LastOutcome::ScrollFailed => {
                HuntOutcome::ActionFailed(format!("hunt4 use scroll failed: {name_keyword}"))
            }
            _ => HuntOutcome::ActionFailed(format!(
                "hunt4 use scroll returned unexpected outcome: {dispatch_outcome:?}"
            )),
        },
        DispatchIntent::AttackLookupFailed { target_id } => {
            HuntOutcome::ActionFailed(format!("hunt4 attack lookup failed target=0x{target_id:X}"))
        }
    }
}

fn current_map(map_id: Option<u32>) -> Option<std::sync::Arc<crate::minimap::map_loader::Map>> {
    let map_id = map_id?;
    crate::minimap::get_or_load_map(map_id).ok()
}

fn locked_target_removed_or_dead(ctx: &HuntContext) -> bool {
    current_lock_target_id(&ctx.state)
        .is_some_and(crate::aux::server_packet_hook::target_recently_removed_or_dead)
}

fn locked_target_recently_damaged(ctx: &HuntContext) -> bool {
    current_attacking_lock_target_id(&ctx.state)
        .is_some_and(crate::aux::server_packet_hook::target_recently_damaged)
}

fn check_teleport_scroll_available(h: HANDLE, keyword: &str) -> bool {
    let trimmed = keyword.trim();
    if trimmed.is_empty() {
        return false;
    }
    let Ok(items) = crate::aux::inventory::list_items(h) else {
        return false;
    };
    items
        .iter()
        .any(|it| teleport_scroll_item_matches(it, trimmed))
}

fn current_lock_target_id(state: &HuntState) -> Option<u32> {
    match state {
        HuntState::Engaging { lock, .. } => Some(lock.target_id),
        _ => None,
    }
}

fn current_attacking_lock_target_id(state: &HuntState) -> Option<u32> {
    match state {
        HuntState::Engaging {
            lock,
            intent: EngageIntent::Attack,
            ..
        } => Some(lock.target_id),
        _ => None,
    }
}

fn prepare_attack_watch_before_tick_with_damage_observation(
    ctx: &mut HuntContext,
    now: Instant,
    target_damaged_recently: bool,
) -> Option<u32> {
    let stale_target = update_attack_watch_before_tick(ctx, now, target_damaged_recently);
    if let Some(target_id) = stale_target {
        record_no_progress_target(ctx, target_id);
    }
    stale_target
}

fn update_attack_watch_before_tick(
    ctx: &mut HuntContext,
    now: Instant,
    target_damaged_recently: bool,
) -> Option<u32> {
    let Some(target_id) = current_attacking_lock_target_id(&ctx.state) else {
        ctx.attack_watch = AttackProgressWatch::default();
        return None;
    };
    if ctx.attack_watch.target_id != Some(target_id) {
        ctx.attack_watch = AttackProgressWatch {
            target_id: Some(target_id),
            no_progress_since: Some(now),
        };
        return None;
    }
    if target_damaged_recently {
        ctx.last_target_progress = Some(now);
        ctx.attack_watch.no_progress_since = Some(now);
        return None;
    }
    let Some(since) = ctx.attack_watch.no_progress_since else {
        return None;
    };
    if now.saturating_duration_since(since) >= ATTACK_NO_PROGRESS_DURATION {
        Some(target_id)
    } else {
        None
    }
}

fn record_no_progress_target(ctx: &mut HuntContext, target_id: u32) {
    crate::log_line!(
        "[bot/hunt4/target] no damage progress target=0x{:X} mark_attack_failed=true",
        target_id
    );
    ctx.last_outcome = LastOutcome::AttackNoProgress { target_id };
    ctx.attack_watch = AttackProgressWatch::default();
}

fn record_attack_watch_after_dispatch(
    ctx: &mut HuntContext,
    dispatch_outcome: &LastOutcome,
    now: Instant,
) {
    match dispatch_outcome {
        LastOutcome::AttackOk { target_id } => {
            if ctx.attack_watch.target_id != Some(*target_id) {
                ctx.attack_watch = AttackProgressWatch {
                    target_id: Some(*target_id),
                    no_progress_since: Some(now),
                };
            } else if ctx.attack_watch.no_progress_since.is_none() {
                ctx.attack_watch.no_progress_since = Some(now);
            }
        }
        LastOutcome::AttackFailed { target_id } => {
            if ctx.attack_watch.target_id == Some(*target_id) {
                ctx.attack_watch = AttackProgressWatch::default();
            }
        }
        _ => {}
    }
}

fn record_attack_sequence_after_dispatch(
    ctx: &mut HuntContext,
    cfg: &HuntConfig,
    now: Instant,
    intent: &DispatchIntent,
    dispatch_outcome: &LastOutcome,
) {
    let attack_attempted = match (intent, dispatch_outcome) {
        (
            DispatchIntent::BootstrapAttack { target_id, .. }
            | DispatchIntent::CastSkill { target_id, .. },
            LastOutcome::AttackOk {
                target_id: outcome_id,
            }
            | LastOutcome::AttackFailed {
                target_id: outcome_id,
            },
        ) => target_id == outcome_id,
        _ => false,
    };
    if attack_attempted && ctx.attack_sequence.current_step(cfg, now).is_some() {
        ctx.attack_sequence.advance_after_attack(cfg, now);
    }
}

#[cfg(test)]
pub(crate) fn prepare_attack_watch_before_tick_with_damage_observation_for_test(
    ctx: &mut HuntContext,
    now: Instant,
    target_damaged_recently: bool,
) -> Option<u32> {
    prepare_attack_watch_before_tick_with_damage_observation(ctx, now, target_damaged_recently)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::bot::decide::hunt::{AttackSequenceStep, HuntConfig};
    use crate::bot::hunt4::candidate::CandidateFrame;
    use crate::bot::hunt4::intent::{
        DispatchChoice, DispatchChoiceSource, DispatchEvaluation, ShadowDispatch,
    };
    use crate::bot::hunt4::model::{EntityView, Snapshot};
    use crate::bot::hunt4::plan::{PlanFrame, RuntimePlanEvaluation};
    use crate::bot::hunt4::policy::{BackendIntentSummary, PolicyComparison, ShadowDecision};
    use crate::bot::hunt4::score::TargetScoreSummary;
    use crate::bot::hunt4::state::{EngageIntent, HuntState, TargetLock};
    use crate::bot::hunt4::step::{Action, AttackStepInput, ExploreSuggestion, TargetCandidate};
    use crate::bot::perception::classifier::EntityClass;
    use crate::bot::perception::position::PlayerPosition;

    use super::*;

    fn target_candidate(target_id: u32) -> TargetCandidate {
        TargetCandidate {
            target_id,
            entity_ptr: target_id + 0x1000,
            name: format!("mob_{target_id:X}"),
            tile: (101, 100),
            distance: 1,
            in_attack_range: true,
            reachable_path: Some(Vec::new()),
            is_attacker: false,
        }
    }

    fn target_summary(rank: usize, target_id: u32) -> TargetScoreSummary {
        TargetScoreSummary {
            rank,
            target_id,
            entity_ptr: target_id + 0x1000,
            name: format!("mob_{target_id:X}"),
            tile: (101, 100),
            distance: 1,
            in_attack_range: true,
            reachable: true,
            is_attacker: false,
            approach_steps: Some(0),
            approach_next_tile: None,
            reason: "reachable in_range distance=1".to_string(),
        }
    }

    fn plan_evaluation_with_ranked_targets(
        now: Instant,
        ranked_targets: Vec<TargetScoreSummary>,
    ) -> RuntimePlanEvaluation {
        let selected_target_id = ranked_targets.first().map(|target| target.target_id);
        let selected_target_reason = ranked_targets.first().map(|target| target.reason.clone());
        RuntimePlanEvaluation {
            candidate_frame: CandidateFrame {
                now,
                player_tile: (100, 100),
                attack_range: 1,
                recent_attacker_count: 0,
                candidates: Vec::new(),
                target_selection: Default::default(),
            },
            explore: None,
            plan_frame: PlanFrame {
                now,
                in_game: true,
                map_id: Some(4),
                player_tile: Some((100, 100)),
                attack_range: 1,
                selected_attack_step: None,
                selected_skill_range: None,
                selected_skill_cd_ms: None,
                selected_skill_ready: None,
                post_skill_basic_pending_target: None,
                recent_attacker_count: 0,
                candidate_count: ranked_targets.len(),
                reachable_count: ranked_targets.len(),
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
            },
            backend_skill_cd_ms: 0,
            backend_attack_step: AttackStepInput::Legacy,
            damage_spike_detected: false,
            critical_hp_detected: false,
        }
    }

    fn entity(target_id: u32, entity_ptr: u32, name: &str) -> EntityView {
        EntityView {
            target_id,
            entity_ptr,
            name: name.to_string(),
            sprite_id: 1,
            action_state: 0,
            tile: (101, 100),
            raw_x: 0,
            y: 0,
            class: EntityClass::AttackableMonster,
            visible_confidence: 100,
            hostile_confidence: 100,
        }
    }

    #[test]
    fn build_tick_inputs_maps_v4_evaluation_into_backend_shape() {
        let now = Instant::now();
        let state_since = now - Duration::from_secs(3);
        let cfg = HuntConfig::default();
        let attack_step = AttackSequenceStep::skill("Frozen Cloud".to_string(), 100);
        let world = WorldFrame {
            now,
            in_game: true,
            map_id: Some(4),
            player_view: None,
            player_pos_data: Some(PlayerPosition { x: 100, y: 100 }),
            snapshot: Snapshot {
                player: Some((100, 100)),
                entities: Vec::new(),
            },
        };
        let planning = RuntimePlanEvaluation {
            candidate_frame: CandidateFrame {
                now,
                player_tile: (100, 100),
                attack_range: 7,
                recent_attacker_count: 0,
                candidates: vec![target_candidate(0x1000)],
                target_selection: Default::default(),
            },
            explore: Some(ExploreSuggestion {
                goal: (120, 100),
                path: vec![(101, 100)],
            }),
            plan_frame: PlanFrame {
                now,
                in_game: true,
                map_id: Some(4),
                player_tile: Some((100, 100)),
                attack_range: 7,
                selected_attack_step: Some(attack_step.clone()),
                selected_skill_range: Some(7),
                selected_skill_cd_ms: Some(2000),
                selected_skill_ready: Some(true),
                post_skill_basic_pending_target: None,
                recent_attacker_count: 0,
                candidate_count: 1,
                reachable_count: 1,
                attacker_count: 0,
                ranked_targets: Vec::new(),
                route_plan: None,
                selected_target_id: None,
                selected_target_reason: None,
                non_viable_target_count: 0,
                last_target_reject_reason: None,
                abandoned_target_reason: None,
                selected_route_target_id: None,
                selected_route_next_tile: Some((101, 100)),
                selected_route_reason: Some("explore".to_string()),
                movement_next_tile: Some((101, 100)),
                movement_reason: Some("explore".to_string()),
                teleport_should_use: false,
                teleport_reason: None,
                final_action_kind: "wait".to_string(),
                invariant_violations: Vec::new(),
                explore_goal: Some((120, 100)),
                explore_next_tile: Some((101, 100)),
                explore_steps: Some(1),
            },
            backend_skill_cd_ms: 2000,
            backend_attack_step: AttackStepInput::Step(attack_step.clone()),
            damage_spike_detected: true,
            critical_hp_detected: false,
        };

        let ctx = HuntContext::new(state_since);
        let inputs = build_tick_inputs(BackendTickInputsInput {
            h: HANDLE(std::ptr::null_mut()),
            world: &world,
            planning: &planning,
            master_enabled: true,
            ctx: &ctx,
            cfg: &cfg,
        });

        assert_eq!(inputs.snapshot.player, Some((100, 100)));
        assert_eq!(inputs.candidates[0].target_id, 0x1000);
        assert_eq!(inputs.explore.unwrap().goal, (120, 100));
        assert_eq!(inputs.player_pos, Some((100, 100)));
        assert!(!inputs.player_alive);
        assert!(inputs.master_enabled);
        assert!(inputs.in_game);
        assert_eq!(inputs.now, now);
        assert_eq!(inputs.state_since, state_since);
        assert!(!inputs.teleport_scroll_available);
        assert!(inputs.damage_spike_detected);
        assert!(!inputs.critical_hp_detected);
        assert!(!inputs.locked_target_removed_or_dead);
        assert_eq!(inputs.skill_cd_ms, 2000);
        assert_eq!(inputs.attack_step, AttackStepInput::Step(attack_step));
    }

    #[test]
    fn record_plan_target_summary_after_evaluation_updates_and_clears_top_target() {
        let now = Instant::now();
        let mut ctx = HuntContext::new(now);

        let planning = plan_evaluation_with_ranked_targets(
            now,
            vec![target_summary(1, 0x1000), target_summary(2, 0x2000)],
        );
        record_plan_target_summary_after_evaluation(&mut ctx, &planning);
        assert_eq!(
            ctx.last_target_summary
                .as_ref()
                .map(|summary| summary.target_id),
            Some(0x1000)
        );

        let empty_planning = plan_evaluation_with_ranked_targets(now, Vec::new());
        record_plan_target_summary_after_evaluation(&mut ctx, &empty_planning);
        assert!(ctx.last_target_summary.is_none());
    }

    #[test]
    fn record_plan_target_summary_after_evaluation_updates_route_and_teleport_reasons() {
        let now = Instant::now();
        let mut ctx = HuntContext::new(now);
        let mut planning = plan_evaluation_with_ranked_targets(now, Vec::new());
        planning.plan_frame.selected_route_next_tile = Some((101, 100));
        planning.plan_frame.selected_route_reason = Some("attack: target=0x1000".to_string());
        planning.plan_frame.teleport_reason = Some("visible_no_actionable".to_string());

        record_plan_target_summary_after_evaluation(&mut ctx, &planning);

        assert_eq!(ctx.last_route_next_tile, Some((101, 100)));
        assert_eq!(
            ctx.last_route_reason.as_deref(),
            Some("attack: target=0x1000")
        );
        assert_eq!(
            ctx.last_teleport_reason.as_deref(),
            Some("visible_no_actionable")
        );
    }

    #[test]
    fn evaluate_dispatch_for_backend_uses_v4_intent_contract_and_records_telemetry() {
        let now = Instant::now();
        let planning = plan_evaluation_with_ranked_targets(now, Vec::new());
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let dispatch_state = DispatchState::default();
        let mut policy_telemetry = PolicyTelemetry::default();

        let evaluation = evaluate_dispatch_for_backend(BackendDispatchEvaluationInput {
            planning: &planning,
            snapshot: &snapshot,
            dispatch_state: &dispatch_state,
            backend_intent: &DispatchIntent::Noop,
            policy_telemetry: &mut policy_telemetry,
            takeover_enabled: false,
        });

        assert_eq!(policy_telemetry.total, 1);
        assert!(evaluation.policy_comparison.aligned);
        assert_eq!(evaluation.dispatch_choice.intent, DispatchIntent::Noop);
    }

    #[test]
    fn evaluate_dispatch_for_backend_aligns_teleport_shadow_uut_keeps_backend_scroll() {
        let now = Instant::now();
        let mut planning = plan_evaluation_with_ranked_targets(now, Vec::new());
        planning.plan_frame.teleport_should_use = true;
        planning.plan_frame.teleport_reason = Some("empty_area".to_string());
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let dispatch_state = DispatchState::default();
        let backend = DispatchIntent::UseScroll {
            name_keyword: "teleport".to_string(),
        };
        let mut policy_telemetry = PolicyTelemetry::default();

        let evaluation = evaluate_dispatch_for_backend(BackendDispatchEvaluationInput {
            planning: &planning,
            snapshot: &snapshot,
            dispatch_state: &dispatch_state,
            backend_intent: &backend,
            policy_telemetry: &mut policy_telemetry,
            takeover_enabled: true,
        });

        assert_eq!(policy_telemetry.total, 1);
        assert_eq!(policy_telemetry.aligned, 1);
        assert_eq!(
            evaluation.shadow_dispatch.decision,
            ShadowDecision::UseTeleportScroll {
                reason: "empty_area".to_string(),
            }
        );
        assert_eq!(evaluation.shadow_dispatch.action, Action::Wait);
        assert_eq!(evaluation.shadow_dispatch.intent, DispatchIntent::Noop);
        assert!(evaluation.policy_comparison.aligned);
        assert_eq!(
            evaluation.policy_comparison.reason,
            "both use teleport scroll"
        );
        assert_eq!(
            evaluation.dispatch_choice.source,
            DispatchChoiceSource::Backend
        );
        assert_eq!(evaluation.dispatch_choice.intent, backend);
        assert_eq!(evaluation.dispatch_choice.reason, "backend-owned scroll");
    }

    #[test]
    fn record_dispatch_diagnostics_after_evaluation_records_fields_and_returns_intent() {
        let now = Instant::now();
        let mut ctx = HuntContext::new(now);
        let evaluation = DispatchEvaluation {
            shadow_dispatch: ShadowDispatch {
                decision: ShadowDecision::Wait,
                action: Action::Wait,
                intent: DispatchIntent::Noop,
            },
            policy_comparison: PolicyComparison {
                shadow: ShadowDecision::Wait,
                backend: BackendIntentSummary::Walk { tile: (101, 102) },
                aligned: false,
                reason: "policy mismatch",
            },
            dispatch_choice: DispatchChoice {
                source: DispatchChoiceSource::Backend,
                intent: DispatchIntent::Walk {
                    target_x: 101,
                    target_y: 102,
                },
                reason: "v4 takeover disauled",
            },
        };

        let dispatch_intent = record_dispatch_diagnostics_after_evaluation(&mut ctx, evaluation);

        assert_eq!(
            dispatch_intent,
            DispatchIntent::Walk {
                target_x: 101,
                target_y: 102,
            }
        );
        assert_eq!(
            ctx.last_shadow_dispatch
                .as_ref()
                .map(|dispatch| dispatch.intent.clone()),
            Some(DispatchIntent::Noop)
        );
        assert_eq!(
            ctx.last_dispatch_choice
                .as_ref()
                .map(|choice| choice.intent.clone()),
            Some(dispatch_intent)
        );
        assert_eq!(
            ctx.last_policy_comparison
                .as_ref()
                .map(|comparison| comparison.reason),
            Some("policy mismatch")
        );
    }

    #[test]
    fn record_actual_dispatch_action_after_choice_updates_reported_action() {
        let now = Instant::now();
        let mut ctx = HuntContext::new(now);

        record_actual_dispatch_action_after_choice(
            &mut ctx,
            &DispatchIntent::Walk {
                target_x: 7,
                target_y: 8,
            },
        );
        assert_eq!(
            ctx.last_action,
            Some(crate::bot::hunt4::observe::ActionLabel::WalkTo { tile: (7, 8) })
        );

        record_actual_dispatch_action_after_choice(
            &mut ctx,
            &DispatchIntent::AttackLookupFailed { target_id: 0x123 },
        );
        assert_eq!(
            ctx.last_action,
            Some(crate::bot::hunt4::observe::ActionLabel::Attack {
                target_id: 0x123,
                with_skill: false,
            })
        );
    }

    #[test]
    fn record_attack_bookkeeping_after_dispatch_seeds_watch_and_advances_sequence() {
        let now = Instant::now();
        let cfg = HuntConfig {
            attack_sequence: vec![
                AttackSequenceStep::basic(1000),
                AttackSequenceStep::skill("Fire".to_string(), 0),
            ],
            ..HuntConfig::default()
        };
        let mut ctx = HuntContext::new(now);

        assert_eq!(
            ctx.attack_sequence.current_step(&cfg, now),
            Some(AttackSequenceStep::basic(1000))
        );

        record_attack_bookkeeping_after_dispatch(
            &mut ctx,
            &cfg,
            now,
            &DispatchIntent::BootstrapAttack {
                target_id: 0x1234,
                entity_ptr: 0x2234,
                target_raw_x: 0,
                target_y: 0,
                fresh_target: true,
            },
            &LastOutcome::AttackOk { target_id: 0x1234 },
        );

        assert_eq!(ctx.attack_watch.target_id, Some(0x1234));
        assert_eq!(ctx.attack_watch.no_progress_since, Some(now));
        assert_eq!(
            ctx.attack_sequence
                .current_step(&cfg, now + std::time::Duration::from_millis(999)),
            None
        );
        assert_eq!(
            ctx.attack_sequence
                .current_step(&cfg, now + std::time::Duration::from_millis(1000)),
            Some(AttackSequenceStep::skill("Fire".to_string(), 0))
        );
    }

    #[test]
    fn attack_watch_starts_when_attack_state_has_not_dispatched_yet() {
        let now = Instant::now();
        let target_id = 0x1234;
        let mut ctx = HuntContext::new(now);
        ctx.state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr: 0x2234,
                name: "stalled".to_string(),
                acquired_at: now,
                last_seen: now,
                bootstrapped: false,
            },
            intent: EngageIntent::Attack,
            path: None,
        };

        let first =
            prepare_attack_watch_before_tick_with_damage_observation_for_test(&mut ctx, now, false);

        assert_eq!(first, None);
        assert_eq!(ctx.attack_watch.target_id, Some(target_id));
        assert_eq!(ctx.attack_watch.no_progress_since, Some(now));

        let stale = prepare_attack_watch_before_tick_with_damage_observation_for_test(
            &mut ctx,
            now + ATTACK_NO_PROGRESS_DURATION,
            false,
        );

        assert_eq!(stale, Some(target_id));
        assert_eq!(
            ctx.last_outcome,
            LastOutcome::AttackNoProgress { target_id }
        );
    }

    #[test]
    fn attack_watch_does_not_fail_target_at_three_seconds_without_packet_damage() {
        let now = Instant::now();
        let target_id = 0x1234;
        let mut ctx = HuntContext::new(now);
        ctx.state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr: 0x2234,
                name: "slow_kill".to_string(),
                acquired_at: now,
                last_seen: now,
                bootstrapped: false,
            },
            intent: EngageIntent::Attack,
            path: None,
        };

        prepare_attack_watch_before_tick_with_damage_observation_for_test(&mut ctx, now, false);
        let stale = prepare_attack_watch_before_tick_with_damage_observation_for_test(
            &mut ctx,
            now + Duration::from_secs(3),
            false,
        );

        assert_eq!(stale, None);
        assert_eq!(ctx.attack_watch.target_id, Some(target_id));
        assert_eq!(ctx.last_outcome, LastOutcome::None);
    }

    #[test]
    fn attack_watch_records_packet_damage_as_observed_target_progress() {
        let now = Instant::now();
        let target_id = 0x1234;
        let mut ctx = HuntContext::new(now);
        ctx.state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr: 0x2234,
                name: "damaged".to_string(),
                acquired_at: now,
                last_seen: now,
                bootstrapped: false,
            },
            intent: EngageIntent::Attack,
            path: None,
        };

        prepare_attack_watch_before_tick_with_damage_observation_for_test(&mut ctx, now, false);
        let damaged_at = now + Duration::from_secs(1);
        let stale = prepare_attack_watch_before_tick_with_damage_observation_for_test(
            &mut ctx, damaged_at, true,
        );

        assert_eq!(stale, None);
        assert_eq!(ctx.last_target_progress, Some(damaged_at));
        assert_eq!(ctx.attack_watch.no_progress_since, Some(damaged_at));
    }

    #[test]
    fn attack_sequence_advances_past_skill_attempt_even_when_dispatch_reports_failed() {
        let now = Instant::now();
        let cfg = HuntConfig {
            attack_sequence: vec![
                AttackSequenceStep::basic(0),
                AttackSequenceStep::basic(0),
                AttackSequenceStep::skill("skill-a".to_string(), 0),
            ],
            ..HuntConfig::default()
        };
        let mut ctx = HuntContext::new(now);

        assert_eq!(
            ctx.attack_sequence.current_step(&cfg, now),
            Some(AttackSequenceStep::basic(0))
        );
        record_attack_bookkeeping_after_dispatch(
            &mut ctx,
            &cfg,
            now,
            &DispatchIntent::BootstrapAttack {
                target_id: 0x1234,
                entity_ptr: 0x2234,
                target_raw_x: 0,
                target_y: 0,
                fresh_target: true,
            },
            &LastOutcome::AttackOk { target_id: 0x1234 },
        );

        assert_eq!(
            ctx.attack_sequence.current_step(&cfg, now),
            Some(AttackSequenceStep::basic(0))
        );
        record_attack_bookkeeping_after_dispatch(
            &mut ctx,
            &cfg,
            now,
            &DispatchIntent::BootstrapAttack {
                target_id: 0x1234,
                entity_ptr: 0x2234,
                target_raw_x: 0,
                target_y: 0,
                fresh_target: false,
            },
            &LastOutcome::AttackOk { target_id: 0x1234 },
        );

        assert_eq!(
            ctx.attack_sequence.current_step(&cfg, now),
            Some(AttackSequenceStep::skill("skill-a".to_string(), 0))
        );
        record_attack_bookkeeping_after_dispatch(
            &mut ctx,
            &cfg,
            now,
            &DispatchIntent::CastSkill {
                skill_name: "skill-a".to_string(),
                target_id: 0x1234,
            },
            &LastOutcome::AttackFailed { target_id: 0x1234 },
        );

        assert_eq!(
            ctx.attack_sequence.current_step(&cfg, now),
            Some(AttackSequenceStep::basic(0)),
            "a skill dispatch attempt must close the configured round so the next attack starts from rank 1 instead of repeating the skill"
        );
    }

    #[test]
    fn attack_sequence_waiting_basic_filler_does_not_extend_next_ready_time() {
        let now = Instant::now();
        let cfg = HuntConfig {
            attack_sequence: vec![AttackSequenceStep::skill("Fire".to_string(), 2_000)],
            ..HuntConfig::default()
        };
        let mut ctx = HuntContext::new(now);

        assert_eq!(
            ctx.attack_sequence.current_step(&cfg, now),
            Some(AttackSequenceStep::skill("Fire".to_string(), 2_000))
        );
        record_attack_bookkeeping_after_dispatch(
            &mut ctx,
            &cfg,
            now,
            &DispatchIntent::CastSkill {
                skill_name: "Fire".to_string(),
                target_id: 0x1234,
            },
            &LastOutcome::AttackOk { target_id: 0x1234 },
        );

        let filler_time = now + Duration::from_millis(800);
        assert_eq!(ctx.attack_sequence.current_step(&cfg, filler_time), None);
        record_attack_bookkeeping_after_dispatch(
            &mut ctx,
            &cfg,
            filler_time,
            &DispatchIntent::BootstrapAttack {
                target_id: 0x1234,
                entity_ptr: 0x2234,
                target_raw_x: 0,
                target_y: 0,
                fresh_target: false,
            },
            &LastOutcome::AttackOk { target_id: 0x1234 },
        );

        assert_eq!(
            ctx.attack_sequence
                .current_step(&cfg, now + Duration::from_millis(1_999)),
            None
        );
        assert_eq!(
            ctx.attack_sequence
                .current_step(&cfg, now + Duration::from_millis(2_000)),
            Some(AttackSequenceStep::skill("Fire".to_string(), 2_000))
        );
    }

    #[test]
    fn record_dispatch_context_after_dispatch_updates_last_outcome_and_walk_tile() {
        let now = Instant::now();
        let mut ctx = HuntContext::new(now);

        record_dispatch_context_after_dispatch(
            &mut ctx,
            &DispatchIntent::Walk {
                target_x: 101,
                target_y: 102,
            },
            &LastOutcome::WalkOk,
        );
        assert_eq!(ctx.last_outcome, LastOutcome::WalkOk);
        assert_eq!(ctx.last_walk_tile, Some((101, 102)));

        record_dispatch_context_after_dispatch(
            &mut ctx,
            &DispatchIntent::Walk {
                target_x: 103,
                target_y: 104,
            },
            &LastOutcome::WalkFailed {
                attempted_tile: (103, 104),
            },
        );
        assert_eq!(
            ctx.last_outcome,
            LastOutcome::WalkFailed {
                attempted_tile: (103, 104),
            }
        );
        assert_eq!(ctx.last_walk_tile, None);

        ctx.last_walk_tile = Some((105, 106));
        record_dispatch_context_after_dispatch(
            &mut ctx,
            &DispatchIntent::UseScroll {
                name_keyword: "scroll".to_string(),
            },
            &LastOutcome::ScrollOk,
        );
        assert_eq!(ctx.last_outcome, LastOutcome::ScrollOk);
        assert_eq!(ctx.last_walk_tile, None);
    }

    #[test]
    fn advance_hp_baseline_after_plan_updates_only_when_hp_is_available() {
        let now = Instant::now();
        let mut ctx = HuntContext::new(now);

        advance_hp_baseline_after_plan(&mut ctx, Some(777));
        assert_eq!(ctx.prev_hp, Some(777));

        advance_hp_baseline_after_plan(&mut ctx, None);
        assert_eq!(ctx.prev_hp, Some(777));
    }

    #[test]
    fn record_map_change_after_frame_updates_map_and_records_portal_tile() {
        let now = Instant::now();
        let mut ctx = HuntContext::new(now);
        ctx.last_map_id = Some(10);
        ctx.last_walk_tile = Some((321, 456));

        record_map_change_after_frame(&mut ctx, Some(11), now);

        assert_eq!(ctx.last_map_id, Some(11));
        assert!(ctx.memory.is_obstacle((321, 456), now));

        ctx.last_walk_tile = Some((111, 222));
        record_map_change_after_frame(&mut ctx, Some(11), now);

        assert_eq!(ctx.last_map_id, Some(11));
        assert!(!ctx.memory.is_obstacle((111, 222), now));
    }

    #[test]
    fn record_position_memory_after_frame_pushes_position_and_map_visit() {
        let now = Instant::now();
        let mut ctx = HuntContext::new(now);

        record_position_memory_after_frame(&mut ctx, Some((100, 200)), Some(63), now);

        assert_eq!(ctx.memory.recent_positions.len(), 1);
        assert_eq!(
            ctx.memory
                .recent_positions
                .back()
                .map(|(x, y, at)| (*x, *y, *at)),
            Some((100, 200, now))
        );
        assert_eq!(ctx.memory.last_position_change, Some(now));
        assert_eq!(ctx.memory.visit_count(63, (100, 200)), 1);

        let unchanged_at = now + std::time::Duration::from_secs(1);
        record_position_memory_after_frame(&mut ctx, Some((100, 200)), None, unchanged_at);

        assert_eq!(ctx.memory.recent_positions.len(), 2);
        assert_eq!(ctx.memory.last_position_change, Some(now));
        assert_eq!(ctx.memory.visit_count(63, (100, 200)), 1);

        record_position_memory_after_frame(
            &mut ctx,
            None,
            Some(63),
            now + std::time::Duration::from_secs(2),
        );

        assert_eq!(ctx.memory.recent_positions.len(), 2);
        assert_eq!(ctx.memory.visit_count(63, (100, 200)), 1);
    }

    #[test]
    fn publish_active_path_writes_walk_paths_and_clears_idle() {
        use crate::bot::decide::pathfind;
        use crate::bot::hunt4::state::TargetLock;

        pathfind::with_bot_path_test_lock(|| {
            let now = Instant::now();
            let approach_path = vec![(10, 10), (11, 10)];
            let lock = TargetLock {
                target_id: 0x1,
                entity_ptr: 0xDEAD,
                name: "mob".to_string(),
                acquired_at: now,
                last_seen: now,
                bootstrapped: false,
            };
            publish_active_path(&HuntState::Engaging {
                lock,
                intent: EngageIntent::Approach,
                path: Some(approach_path.clone()),
            });
            assert_eq!(pathfind::read_bot_path(), approach_path);

            let explore_path = vec![(20, 20), (21, 20)];
            publish_active_path(&HuntState::Exploring {
                goal: (25, 20),
                path: explore_path.clone(),
            });
            assert_eq!(pathfind::read_bot_path(), explore_path);

            publish_active_path(&HuntState::Idle);
            assert!(pathfind::read_bot_path().is_empty());
        });
    }

    #[test]
    fn intent_to_outcome_maps_walk_attack_skill_scroll_and_lookup_results() {
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                entity(0x11, 0xAA11, "target_a"),
                entity(0x22, 0xAA22, "target_u"),
            ],
        };
        let player_pos = Some(PlayerPosition { x: 100, y: 100 });

        assert!(matches!(
            intent_to_outcome(
                &DispatchIntent::Noop,
                &snapshot,
                player_pos,
                &LastOutcome::None
            ),
            HuntOutcome::NoTarget
        ));

        match intent_to_outcome(
            &DispatchIntent::Walk {
                target_x: 101,
                target_y: 102,
            },
            &snapshot,
            player_pos,
            &LastOutcome::WalkOk,
        ) {
            HuntOutcome::Walked {
                target_id,
                name,
                distance_tiles,
                ..
            } => {
                assert_eq!(target_id, 0);
                assert_eq!(name, "hunt4 walk");
                assert_eq!(distance_tiles, 2);
            }
            other => panic!("expected walked outcome, got {other:?}"),
        }

        match intent_to_outcome(
            &DispatchIntent::BootstrapAttack {
                target_id: 0x11,
                entity_ptr: 0xAA11,
                target_raw_x: 0,
                target_y: 0,
                fresh_target: true,
            },
            &snapshot,
            player_pos,
            &LastOutcome::AttackOk { target_id: 0x11 },
        ) {
            HuntOutcome::Cast {
                target_id, name, ..
            } => {
                assert_eq!(target_id, 0x11);
                assert_eq!(name, "target_a");
            }
            other => panic!("expected cast outcome, got {other:?}"),
        }

        match intent_to_outcome(
            &DispatchIntent::CastSkill {
                skill_name: "Fire".to_string(),
                target_id: 0x22,
            },
            &snapshot,
            player_pos,
            &LastOutcome::AttackOk { target_id: 0x22 },
        ) {
            HuntOutcome::Cast {
                target_id, name, ..
            } => {
                assert_eq!(target_id, 0x22);
                assert_eq!(name, "Fire (target_u)");
            }
            other => panic!("expected skill cast outcome, got {other:?}"),
        }

        assert!(matches!(
            intent_to_outcome(
                &DispatchIntent::UseScroll {
                    name_keyword: "scroll".to_string(),
                },
                &snapshot,
                player_pos,
                &LastOutcome::ScrollOk,
            ),
            HuntOutcome::Cooldown { remaining_ms: 0 }
        ));

        match intent_to_outcome(
            &DispatchIntent::AttackLookupFailed { target_id: 0xCC },
            &snapshot,
            player_pos,
            &LastOutcome::None,
        ) {
            HuntOutcome::ActionFailed(msg) => assert!(msg.contains("target=0xCC")),
            other => panic!("expected lookup ActionFailed, got {other:?}"),
        }
    }
}
