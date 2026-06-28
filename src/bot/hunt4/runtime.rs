//! Hunt4 runtime wrapper.
//!
//! V4 owns the world-frame planning, pure tick, dispatch selection, dispatch execution,
//! and outcome mapping used by the live bot path.
use std::time::Instant;

use windows::Win32::Foundation::HANDLE;

use super::{backend as hunt4_backend, world};
use crate::bot::decide::hunt::{HuntConfig, HuntOutcome};
use crate::bot::hunt4::context::HuntContext;

pub fn tick_io(
    h: HANDLE,
    cfg: &HuntConfig,
    ctx: &mut HuntContext,
    master_enabled: bool,
) -> HuntOutcome {
    let frame = world::read_frame(h, cfg, Instant::now());
    tick_io_with_world_frame(h, cfg, ctx, master_enabled, frame)
}

pub fn tick_io_with_world_frame(
    h: HANDLE,
    cfg: &HuntConfig,
    ctx: &mut HuntContext,
    master_enabled: bool,
    frame: world::WorldFrame,
) -> HuntOutcome {
    let now = frame.now;
    let map_id = frame.map_id;
    let cur_hp = frame.cur_hp();
    let player_pos_data = frame.player_pos_data;
    let player_pos = frame.player_tile();
    let player_weight = frame.weight_pct();
    let snapshot = &frame.snapshot;

    hunt4_backend::record_map_change_after_frame(ctx, map_id, now);
    hunt4_backend::record_position_memory_after_frame(ctx, player_pos, map_id, now);

    let planning_evaluation =
        hunt4_backend::evaluate_plan_for_backend(hunt4_backend::BackendPlanEvaluationInput {
            h,
            world: &frame,
            cfg,
            ctx,
        });
    hunt4_backend::record_plan_target_summary_after_evaluation(ctx, &planning_evaluation);
    hunt4_backend::advance_hp_baseline_after_plan(ctx, cur_hp);
    hunt4_backend::prepare_attack_watch_before_tick(ctx, now);

    let backend_inputs = hunt4_backend::build_tick_inputs(hunt4_backend::BackendTickInputsInput {
        h,
        world: &frame,
        planning: &planning_evaluation,
        master_enabled,
        ctx,
        cfg,
    });
    let backend_intent = hunt4_backend::run_pure_tick_for_backend(ctx, backend_inputs);
    let dispatch_evaluation = hunt4_backend::evaluate_dispatch_for_backend(
        hunt4_backend::BackendDispatchEvaluationInput {
            planning: &planning_evaluation,
            snapshot,
            dispatch_state: &ctx.dispatch_state,
            backend_intent: &backend_intent,
            policy_telemetry: &mut ctx.policy_telemetry,
            takeover_enabled: cfg.v4_dispatch_takeover,
        },
    );
    let dispatch_intent =
        hunt4_backend::record_dispatch_diagnostics_after_evaluation(ctx, dispatch_evaluation);
    hunt4_backend::record_actual_dispatch_action_after_choice(ctx, &dispatch_intent);

    let dispatch_outcome =
        hunt4_backend::dispatch_for_backend(hunt4_backend::BackendDispatchInput {
            h,
            intent: &dispatch_intent,
            player: player_pos_data,
            walk_driver: cfg.walk_driver,
            dispatch_state: &mut ctx.dispatch_state,
            player_weight,
        });
    hunt4_backend::record_dispatch_context_after_dispatch(ctx, &dispatch_intent, &dispatch_outcome);
    hunt4_backend::record_attack_bookkeeping_after_dispatch(
        ctx,
        cfg,
        now,
        &dispatch_intent,
        &dispatch_outcome,
    );
    hunt4_backend::publish_active_path(&ctx.state);

    hunt4_backend::intent_to_outcome(
        &dispatch_intent,
        snapshot,
        player_pos_data,
        &dispatch_outcome,
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_uses_v4_world_and_backend_pipeline() {
        let source = include_str!("runtime.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();

        assert!(production.contains("world::read_frame"));
        assert!(production.contains("hunt4_backend::evaluate_plan_for_backend"));
        assert!(production.contains("hunt4_backend::run_pure_tick_for_backend"));
        assert!(production.contains(
            "hunt4_backend::record_actual_dispatch_action_after_choice(ctx, &dispatch_intent)"
        ));
        assert!(production.contains("hunt4_backend::dispatch_for_backend"));
        assert!(!production.contains("hunt3"));
        assert!(!production.contains("hunt_v2"));
    }

    #[test]
    fn hunt4_step_owns_pure_tick_surface() {
        let tick = include_str!("tick.rs");
        let step = include_str!("step.rs");

        assert!(tick.contains("let input = TickInput"));
        assert!(tick.contains("ctx.memory.apply(output.memory_updates, inputs.now)"));
        assert!(step.contains("pub struct TickInput"));
        assert!(step.contains("pub enum Action"));
        assert!(step.contains("pub fn step"));
    }
}
