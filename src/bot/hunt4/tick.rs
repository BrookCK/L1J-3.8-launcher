use crate::bot::hunt4::actions::{intent_for, DispatchIntent};
use crate::bot::hunt4::context::{HuntContext, PureTickInputs};
use crate::bot::hunt4::observe::{label_action, record_transition};
use crate::bot::hunt4::step::{step, Action, TickInput};

pub fn pure_tick(ctx: &mut HuntContext, inputs: PureTickInputs<'_>) -> DispatchIntent {
    let input = TickInput {
        snapshot: inputs.snapshot,
        player_pos: inputs.player_pos,
        player_alive: inputs.player_alive,
        master_enabled: inputs.master_enabled,
        in_game: inputs.in_game,
        now: inputs.now,
        state_since: inputs.state_since,
        cfg: inputs.cfg,
        memory: &ctx.memory,
        candidates: inputs.candidates,
        explore_suggestion: inputs.explore,
        last_outcome: ctx.last_outcome.clone(),
        teleport_scroll_available: inputs.teleport_scroll_available,
        damage_spike_detected: inputs.damage_spike_detected,
        critical_hp_detected: inputs.critical_hp_detected,
        skill_cd_ms: inputs.skill_cd_ms,
        attack_step: inputs.attack_step,
        locked_target_removed_or_dead: inputs.locked_target_removed_or_dead,
        locked_target_recently_damaged: inputs.locked_target_recently_damaged,
    };
    let output = step(&ctx.state, input);

    ctx.memory.apply(output.memory_updates, inputs.now);

    if let Some(next) = output.next_state {
        ctx.state = next;
        ctx.since = inputs.now;
    }
    if let Some(transition) = output.transition {
        record_transition(&transition);
        ctx.last_transition = Some(transition);
    }

    let action = output.actions.into_iter().next().unwrap_or(Action::Wait);
    ctx.last_action = Some(label_action(&action));

    intent_for(&action, inputs.snapshot, &ctx.dispatch_state)
}
