use std::time::{Duration, Instant};

use crate::bot::decide::hunt::{AttackSequenceStep, AttackStepKind, HuntConfig, MELEE_RANGE_TILES};
use crate::bot::hunt4::memory::{
    ExploreDirectionKey, FailureCause, FailureRecord, MemoryUpdate, TacticalMemory,
    FAILED_TARGET_TTL,
};
use crate::bot::hunt4::model::{EntityView, Snapshot};
use crate::bot::hunt4::route;
use crate::bot::hunt4::state::{
    DisabledReason, EngageIntent, HuntState, RecoveryCause, StopReason, TargetLock,
};

pub const REPEAT_NORMAL_ATTACK_COOLDOWN: Duration = Duration::from_millis(800);
pub const TELEPORT_SCROLL_COOLDOWN: Duration = Duration::from_secs(3);
pub const ESCAPING_WAIT_DURATION: Duration = Duration::from_secs(2);
pub const KILL_CONFIRM_DURATION: Duration = Duration::from_millis(1500);
pub const RECOVERY_WALK_STUCK_DURATION: Duration = Duration::from_millis(800);
pub const RECOVERY_ATTACK_FAILED_DURATION: Duration = Duration::from_secs(2);
pub const RECOVERY_DAMAGE_SPIKE_DURATION: Duration = Duration::from_secs(2);
pub const RECOVERY_NO_REACHABLE_DURATION: Duration = Duration::from_secs(2);
pub const RECOVERY_CRITICAL_HP_DURATION: Duration = Duration::from_secs(2);
pub const STALL_WINDOW_TICKS: usize = 8;
pub const INACTIVITY_RESET_DURATION: Duration = Duration::from_secs(20);
pub const EMPTY_EXPLORE_WALK_BUDGET: u16 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetCandidate {
    pub target_id: u32,
    pub entity_ptr: u32,
    pub name: String,
    pub tile: (i32, i32),
    pub distance: u32,
    pub in_attack_range: bool,
    pub reachable_path: Option<Vec<(i32, i32)>>,
    pub is_attacker: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExploreSuggestion {
    pub goal: (i32, i32),
    pub path: Vec<(i32, i32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttackStepInput {
    #[cfg(test)]
    Legacy,
    Waiting,
    Step(AttackSequenceStep),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Wait,
    WalkTo {
        tile: (i32, i32),
    },
    AttackTarget {
        target_id: u32,
        entity_ptr: u32,
        skill: Option<String>,
    },
    UseTeleportScroll {
        name_keyword: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LastOutcome {
    None,
    WalkOk,
    WalkFailed { attempted_tile: (i32, i32) },
    AttackOk { target_id: u32 },
    AttackFailed { target_id: u32 },
    AttackNoProgress { target_id: u32 },
    ScrollOk,
    ScrollFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateLabel {
    DisabledMasterOff,
    DisabledNotInGame,
    Idle,
    EngagingApproach,
    EngagingAttack,
    EngagingKillConfirm,
    Exploring,
    RecoveringWalkStuck,
    RecoveringAttackFailed,
    RecoveringDamageSpike,
    RecoveringNoReachableTarget,
    RecoveringCriticalHp,
    Escaping,
    StoppedDied,
    #[cfg(test)]
    StoppedDisconnected,
    #[cfg(test)]
    StoppedManual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionCause {
    MasterOff,
    NotInGame,
    PlayerDied,
    Idle,
    TargetAcquired,
    StartApproach,
    StartAttack,
    TargetDead,
    StartExploration,
    AllTargetsUnreachable,
    WalkStuck,
    AttackFailed,
    DamageSpike,
    CriticalHp,
    UseTeleportScroll,
    RecoveryElapsed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: StateLabel,
    pub to: StateLabel,
    pub cause: TransitionCause,
}

#[derive(Debug, Clone)]
pub struct TickInput<'a> {
    pub snapshot: &'a Snapshot,
    pub player_pos: Option<(i32, i32)>,
    pub player_alive: bool,
    pub master_enabled: bool,
    pub in_game: bool,
    pub now: Instant,
    pub state_since: Instant,
    pub cfg: &'a HuntConfig,
    pub memory: &'a TacticalMemory,
    pub candidates: &'a [TargetCandidate],
    pub explore_suggestion: Option<&'a ExploreSuggestion>,
    pub last_outcome: LastOutcome,
    pub teleport_scroll_available: bool,
    pub damage_spike_detected: bool,
    pub critical_hp_detected: bool,
    pub skill_cd_ms: u64,
    pub attack_step: AttackStepInput,
    pub locked_target_removed_or_dead: bool,
    pub locked_target_recently_damaged: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TickOutput {
    pub actions: Vec<Action>,
    pub next_state: Option<HuntState>,
    pub transition: Option<Transition>,
    pub memory_updates: MemoryUpdate,
}

pub fn step(state: &HuntState, input: TickInput<'_>) -> TickOutput {
    let from = state_label(state);
    if !input.master_enabled {
        return transition_to(
            from,
            HuntState::Disabled {
                reason: DisabledReason::MasterOff,
            },
            TransitionCause::MasterOff,
        );
    }
    if !input.in_game {
        return transition_to(
            from,
            HuntState::Disabled {
                reason: DisabledReason::NotInGame,
            },
            TransitionCause::NotInGame,
        );
    }
    if !input.player_alive {
        return transition_to(
            from,
            HuntState::Stopped {
                reason: StopReason::Died,
            },
            TransitionCause::PlayerDied,
        );
    }
    if input.critical_hp_detected {
        return recover(
            from,
            RecoveryCause::CriticalHp,
            input.now,
            RECOVERY_CRITICAL_HP_DURATION,
        );
    }
    if input.damage_spike_detected {
        return recover(
            from,
            RecoveryCause::DamageSpike,
            input.now,
            RECOVERY_DAMAGE_SPIKE_DURATION,
        );
    }
    if let Some(output) = inactivity_watchdog_escape(from, &input) {
        return output;
    }

    match state {
        HuntState::Disabled { .. } | HuntState::Stopped { .. } => {
            transition_to(from, HuntState::Idle, TransitionCause::Idle)
        }
        HuntState::Recovering { until, cause } => {
            if input.now < *until {
                if let Some(output) = recovery_interrupt(from, *cause, &input) {
                    return output;
                }
                TickOutput {
                    actions: vec![Action::Wait],
                    ..TickOutput::default()
                }
            } else {
                if matches!(cause, RecoveryCause::NoReachableTarget) {
                    if let Some(scroll) = force_teleport_action(&input) {
                        return escape_with_scroll(from, &input, scroll);
                    }
                }
                transition_to(from, HuntState::Idle, TransitionCause::RecoveryElapsed)
            }
        }
        HuntState::Escaping { wait_until, .. } => {
            if input.now < *wait_until {
                TickOutput {
                    actions: vec![Action::Wait],
                    ..TickOutput::default()
                }
            } else {
                transition_to(from, HuntState::Idle, TransitionCause::RecoveryElapsed)
            }
        }
        HuntState::Idle => step_idle(from, &input),
        HuntState::Exploring { goal, path } => step_exploring(from, *goal, path, &input),
        HuntState::Engaging { lock, intent, path } => {
            step_engaging(from, lock, *intent, path, &input)
        }
    }
}

fn step_idle(from: StateLabel, input: &TickInput<'_>) -> TickOutput {
    if let Some(candidate) = best_candidate(input) {
        return engage_candidate(from, candidate, input.now, TransitionCause::TargetAcquired);
    }
    no_target_action(from, input)
}

fn recovery_interrupt(
    from: StateLabel,
    cause: RecoveryCause,
    input: &TickInput<'_>,
) -> Option<TickOutput> {
    if !matches!(
        cause,
        RecoveryCause::WalkStuck | RecoveryCause::AttackFailed | RecoveryCause::NoReachableTarget
    ) {
        return None;
    }
    best_candidate(input).map(|candidate| {
        engage_candidate(from, candidate, input.now, TransitionCause::TargetAcquired)
    })
}

fn step_exploring(
    from: StateLabel,
    goal: (i32, i32),
    path: &[(i32, i32)],
    input: &TickInput<'_>,
) -> TickOutput {
    if let Some(candidate) = best_candidate(input) {
        return engage_candidate(from, candidate, input.now, TransitionCause::TargetAcquired);
    }
    if let LastOutcome::WalkFailed { attempted_tile } = input.last_outcome {
        return recover_exploration_stuck(from, goal, attempted_tile, input);
    }
    if let Some(output) = continue_exploration_path(from, goal, path, input) {
        return output;
    }
    no_target_action(from, input)
}

fn step_engaging(
    from: StateLabel,
    lock: &TargetLock,
    intent: EngageIntent,
    path: &Option<Vec<(i32, i32)>>,
    input: &TickInput<'_>,
) -> TickOutput {
    if attack_outcome_matches_target(&input.last_outcome, lock.target_id)
        && input.locked_target_recently_damaged
    {
        if let Some(candidate) = input
            .candidates
            .iter()
            .find(|candidate| candidate.target_id == lock.target_id)
        {
            return attack_candidate(from, lock, candidate, input);
        }
        if let Some(entity) = locked_live_entity(input.snapshot, lock.target_id) {
            return attack_visible_lock(from, lock, entity, input);
        }
    }
    if attack_outcome_matches_target(&input.last_outcome, lock.target_id) {
        if let Some(candidate) = best_candidate_excluding(input, lock.target_id) {
            let mut output =
                engage_candidate(from, candidate, input.now, TransitionCause::TargetAcquired);
            mark_target_attack_rejected(&mut output.memory_updates, lock.target_id, input.now);
            if input.memory.post_skill_basic_pending_target == Some(lock.target_id)
                && output.memory_updates.set_post_skill_basic_pending.is_none()
            {
                output.memory_updates.clear_post_skill_basic_pending = true;
            }
            return output;
        }
        let mut output = recover(
            from,
            RecoveryCause::AttackFailed,
            input.now,
            RECOVERY_ATTACK_FAILED_DURATION,
        );
        mark_target_attack_rejected(&mut output.memory_updates, lock.target_id, input.now);
        return output;
    }
    if let LastOutcome::WalkFailed { attempted_tile } = input.last_outcome {
        let mut output = recover(
            from,
            RecoveryCause::WalkStuck,
            input.now,
            RECOVERY_WALK_STUCK_DURATION,
        );
        output
            .memory_updates
            .add_obstacles
            .push((attempted_tile, input.now));
        mark_target_unreachable(&mut output.memory_updates, lock.target_id, input.now);
        output.memory_updates.clear_recent_positions = true;
        return output;
    }

    if input.locked_target_removed_or_dead {
        if input.now.saturating_duration_since(input.state_since) < KILL_CONFIRM_DURATION {
            return TickOutput {
                actions: vec![Action::Wait],
                ..TickOutput::default()
            };
        }
        return transition_to(from, HuntState::Idle, TransitionCause::TargetDead);
    }

    if let Some(candidate) = input
        .candidates
        .iter()
        .find(|candidate| candidate.target_id == lock.target_id)
    {
        if candidate_can_attack_now(candidate) {
            return attack_candidate(from, lock, candidate, input);
        }
        if intent == EngageIntent::Attack && input.locked_target_recently_damaged {
            return attack_candidate(from, lock, candidate, input);
        }
        let approach_path = route::stable_approach_path(
            input.player_pos,
            input.memory,
            input.now,
            path.as_deref(),
            candidate.reachable_path.clone().unwrap_or_default(),
        );
        let next_tile = route::first_unreached_path_tile(input.player_pos, &approach_path);
        if let Some(tile) = next_tile {
            if let Some(output) = stalled_approach_recovery(from, tile, lock.target_id, input) {
                return output;
            }
            return TickOutput {
                actions: vec![Action::WalkTo { tile }],
                next_state: Some(HuntState::Engaging {
                    lock: refresh_lock(lock, candidate, input.now),
                    intent: EngageIntent::Approach,
                    path: Some(approach_path),
                }),
                transition: (from != StateLabel::EngagingApproach).then_some(Transition {
                    from,
                    to: StateLabel::EngagingApproach,
                    cause: TransitionCause::StartApproach,
                }),
                memory_updates: MemoryUpdate {
                    set_last_walk: Some(input.now),
                    ..MemoryUpdate::default()
                },
            };
        }

        let mut output = recover(
            from,
            RecoveryCause::NoReachableTarget,
            input.now,
            RECOVERY_NO_REACHABLE_DURATION,
        );
        mark_target_unreachable(&mut output.memory_updates, lock.target_id, input.now);
        output.memory_updates.clear_recent_positions = true;
        return output;
    }

    if intent == EngageIntent::Attack {
        if let Some(entity) = locked_live_entity(input.snapshot, lock.target_id) {
            return attack_visible_lock(from, lock, entity, input);
        }
    }

    let Some(candidate) = best_candidate(input) else {
        if intent == EngageIntent::KillConfirm
            && input.now.saturating_duration_since(input.state_since) < KILL_CONFIRM_DURATION
        {
            return TickOutput {
                actions: vec![Action::Wait],
                ..TickOutput::default()
            };
        }
        return no_target_action(from, input);
    };

    engage_candidate(from, candidate, input.now, TransitionCause::TargetAcquired)
}

fn continue_exploration_path(
    from: StateLabel,
    goal: (i32, i32),
    path: &[(i32, i32)],
    input: &TickInput<'_>,
) -> Option<TickOutput> {
    let remaining = route::consume_reached_path(input.player_pos, path);
    let tile = remaining.first().copied()?;
    if input.memory.is_obstacle(tile, input.now) {
        return Some(recover_exploration_stuck(from, goal, tile, input));
    }
    if input
        .memory
        .is_stalled_since(STALL_WINDOW_TICKS, input.memory.last_walk)
        && input.player_pos != Some(tile)
    {
        return Some(recover_exploration_stuck(from, goal, tile, input));
    }

    Some(TickOutput {
        actions: vec![Action::WalkTo { tile }],
        next_state: Some(HuntState::Exploring {
            goal,
            path: remaining,
        }),
        transition: None,
        memory_updates: MemoryUpdate {
            set_last_walk: Some(input.now),
            set_no_actionable_target_since: Some(input.now),
            increment_empty_explore_walks: true,
            ..MemoryUpdate::default()
        },
    })
}

fn recover_exploration_stuck(
    from: StateLabel,
    goal: (i32, i32),
    blocked_tile: (i32, i32),
    input: &TickInput<'_>,
) -> TickOutput {
    let mut output = recover(
        from,
        RecoveryCause::WalkStuck,
        input.now,
        RECOVERY_WALK_STUCK_DURATION,
    );
    output
        .memory_updates
        .add_obstacles
        .push((blocked_tile, input.now));
    if let Some(player) = input.player_pos {
        if let Some(key) = ExploreDirectionKey::from_goal(player, goal) {
            output
                .memory_updates
                .add_failed_explore_directions
                .push((key, input.now));
        }
    }
    output.memory_updates.set_no_actionable_target_since = Some(input.now);
    output.memory_updates.clear_recent_positions = true;
    output
}

fn stalled_approach_recovery(
    from: StateLabel,
    tile: (i32, i32),
    target_id: u32,
    input: &TickInput<'_>,
) -> Option<TickOutput> {
    if from != StateLabel::EngagingApproach {
        return None;
    }
    if !matches!(input.last_outcome, LastOutcome::WalkOk) {
        return None;
    }
    if !input
        .memory
        .is_stalled_since(STALL_WINDOW_TICKS, input.memory.last_walk)
        || input.player_pos == Some(tile)
    {
        return None;
    }

    let mut output = recover(
        from,
        RecoveryCause::WalkStuck,
        input.now,
        RECOVERY_WALK_STUCK_DURATION,
    );
    output.memory_updates.add_obstacles.push((tile, input.now));
    mark_target_unreachable(&mut output.memory_updates, target_id, input.now);
    output.memory_updates.clear_recent_positions = true;
    Some(output)
}

fn mark_target_unreachable(update: &mut MemoryUpdate, target_id: u32, now: Instant) {
    update.add_failed_targets.push((
        target_id,
        FailureRecord {
            cause: FailureCause::Unreachable,
            until: now + FAILED_TARGET_TTL,
        },
    ));
}

fn mark_target_attack_rejected(update: &mut MemoryUpdate, target_id: u32, now: Instant) {
    update.add_failed_targets.push((
        target_id,
        FailureRecord {
            cause: FailureCause::AttackRejected,
            until: now + FAILED_TARGET_TTL,
        },
    ));
}

fn locked_live_entity(snapshot: &Snapshot, target_id: u32) -> Option<&EntityView> {
    snapshot
        .entities
        .iter()
        .find(|entity| entity.target_id == target_id && entity.is_live_attackable())
}

fn attack_visible_lock(
    from: StateLabel,
    lock: &TargetLock,
    entity: &EntityView,
    input: &TickInput<'_>,
) -> TickOutput {
    if matches!(input.attack_step, AttackStepInput::Waiting) && !normal_attack_ready(input) {
        return TickOutput {
            actions: vec![Action::Wait],
            next_state: Some(HuntState::Engaging {
                lock: refresh_lock_from_entity(lock, entity, input.now),
                intent: EngageIntent::Attack,
                path: None,
            }),
            transition: None,
            memory_updates: MemoryUpdate {
                clear_no_actionable_target_since: true,
                ..MemoryUpdate::default()
            },
        };
    }

    let skill = selected_attack_skill(input, entity.target_id, true);

    TickOutput {
        actions: vec![Action::AttackTarget {
            target_id: entity.target_id,
            entity_ptr: entity.entity_ptr,
            skill: skill.clone(),
        }],
        next_state: Some(HuntState::Engaging {
            lock: refresh_lock_from_entity(lock, entity, input.now),
            intent: EngageIntent::Attack,
            path: None,
        }),
        transition: Some(Transition {
            from,
            to: StateLabel::EngagingAttack,
            cause: TransitionCause::StartAttack,
        }),
        memory_updates: MemoryUpdate {
            set_last_attack: Some(input.now),
            set_last_skill_cast: skill.as_ref().map(|_| input.now),
            set_post_skill_basic_pending: skill.as_ref().map(|_| entity.target_id),
            clear_post_skill_basic_pending: skill.is_none()
                && input.memory.post_skill_basic_pending_target == Some(entity.target_id),
            clear_no_actionable_target_since: true,
            ..MemoryUpdate::default()
        },
    }
}

fn refresh_lock_from_entity(lock: &TargetLock, entity: &EntityView, now: Instant) -> TargetLock {
    TargetLock {
        target_id: entity.target_id,
        entity_ptr: entity.entity_ptr,
        name: entity.name.clone(),
        acquired_at: lock.acquired_at,
        last_seen: now,
        bootstrapped: lock.bootstrapped,
    }
}

fn no_target_action(from: StateLabel, input: &TickInput<'_>) -> TickOutput {
    if let Some(scroll) = empty_explore_budget_action(input) {
        return escape_with_scroll(from, input, scroll);
    }
    if let Some(scroll) = teleport_action(input) {
        return escape_with_scroll(from, input, scroll);
    }
    if from == StateLabel::Exploring {
        if let Some(explore) = stalled_exploration(input) {
            let tile = explore.path.first().copied().unwrap_or(explore.goal);
            return recover_exploration_stuck(from, explore.goal, tile, input);
        }
    }
    if let Some(explore) = input.explore_suggestion {
        if let Some(tile) = explore.path.first().copied() {
            return TickOutput {
                actions: vec![Action::WalkTo { tile }],
                next_state: Some(HuntState::Exploring {
                    goal: explore.goal,
                    path: explore.path.clone(),
                }),
                transition: Some(Transition {
                    from,
                    to: StateLabel::Exploring,
                    cause: TransitionCause::StartExploration,
                }),
                memory_updates: MemoryUpdate {
                    set_last_walk: Some(input.now),
                    set_no_actionable_target_since: Some(input.now),
                    increment_empty_explore_walks: true,
                    ..MemoryUpdate::default()
                },
            };
        }
    }
    TickOutput {
        actions: vec![Action::Wait],
        memory_updates: MemoryUpdate {
            set_no_actionable_target_since: Some(input.now),
            ..MemoryUpdate::default()
        },
        ..TickOutput::default()
    }
}

fn stalled_exploration<'a>(input: &TickInput<'a>) -> Option<&'a ExploreSuggestion> {
    if !input
        .memory
        .is_stalled_since(STALL_WINDOW_TICKS, input.memory.last_walk)
    {
        return None;
    }
    let explore = input.explore_suggestion?;
    let tile = explore.path.first().copied()?;
    (input.player_pos != Some(tile)).then_some(explore)
}

fn engage_candidate(
    from: StateLabel,
    candidate: &TargetCandidate,
    now: Instant,
    cause: TransitionCause,
) -> TickOutput {
    let intent = if candidate_can_attack_now(candidate) {
        EngageIntent::Attack
    } else {
        EngageIntent::Approach
    };
    let mut output = TickOutput {
        next_state: Some(HuntState::Engaging {
            lock: new_lock(candidate, now),
            intent,
            path: candidate.reachable_path.clone(),
        }),
        transition: Some(Transition {
            from,
            to: if intent == EngageIntent::Attack {
                StateLabel::EngagingAttack
            } else {
                StateLabel::EngagingApproach
            },
            cause,
        }),
        memory_updates: MemoryUpdate {
            clear_no_actionable_target_since: true,
            clear_empty_explore_walks: true,
            ..MemoryUpdate::default()
        },
        ..TickOutput::default()
    };
    if candidate_can_attack_now(candidate) {
        output.actions.push(Action::AttackTarget {
            target_id: candidate.target_id,
            entity_ptr: candidate.entity_ptr,
            skill: None,
        });
        output.memory_updates.set_last_attack = Some(now);
    } else if let Some(tile) = candidate
        .reachable_path
        .as_ref()
        .and_then(|path| path.first().copied())
    {
        output.actions.push(Action::WalkTo { tile });
        output.memory_updates.set_last_walk = Some(now);
    } else {
        output.actions.push(Action::Wait);
    }
    output
}

fn attack_candidate(
    from: StateLabel,
    lock: &TargetLock,
    candidate: &TargetCandidate,
    input: &TickInput<'_>,
) -> TickOutput {
    if matches!(input.attack_step, AttackStepInput::Waiting) && !normal_attack_ready(input) {
        return TickOutput {
            actions: vec![Action::Wait],
            next_state: Some(HuntState::Engaging {
                lock: refresh_lock(lock, candidate, input.now),
                intent: EngageIntent::Attack,
                path: candidate.reachable_path.clone(),
            }),
            transition: None,
            memory_updates: MemoryUpdate {
                clear_no_actionable_target_since: true,
                ..MemoryUpdate::default()
            },
        };
    }

    let skill = selected_attack_skill(input, candidate.target_id, candidate.in_attack_range);
    TickOutput {
        actions: vec![Action::AttackTarget {
            target_id: candidate.target_id,
            entity_ptr: candidate.entity_ptr,
            skill: skill.clone(),
        }],
        next_state: Some(HuntState::Engaging {
            lock: refresh_lock(lock, candidate, input.now),
            intent: EngageIntent::Attack,
            path: candidate.reachable_path.clone(),
        }),
        transition: Some(Transition {
            from,
            to: StateLabel::EngagingAttack,
            cause: TransitionCause::StartAttack,
        }),
        memory_updates: MemoryUpdate {
            set_last_attack: Some(input.now),
            set_last_skill_cast: skill.as_ref().map(|_| input.now),
            set_post_skill_basic_pending: skill.as_ref().map(|_| candidate.target_id),
            clear_post_skill_basic_pending: skill.is_none()
                && input.memory.post_skill_basic_pending_target == Some(candidate.target_id),
            clear_no_actionable_target_since: true,
            ..MemoryUpdate::default()
        },
    }
}

fn selected_step(input: &TickInput<'_>) -> Option<AttackSequenceStep> {
    match &input.attack_step {
        AttackStepInput::Step(step) => Some(step.clone()),
        #[cfg(test)]
        AttackStepInput::Legacy => None,
        AttackStepInput::Waiting => None,
    }
}

fn selected_attack_skill(
    input: &TickInput<'_>,
    target_id: u32,
    skill_allowed: bool,
) -> Option<String> {
    if !skill_allowed || input.memory.post_skill_basic_pending_target == Some(target_id) {
        return None;
    }
    if input.skill_cd_ms > 0
        && input.memory.last_skill_cast.is_some_and(|last| {
            input.now.saturating_duration_since(last) < Duration::from_millis(input.skill_cd_ms)
        })
    {
        return None;
    }
    selected_step(input).and_then(|step| {
        (step.kind == AttackStepKind::Skill)
            .then(|| step.skill_name.trim().to_string())
            .filter(|name| !name.is_empty())
    })
}

fn normal_attack_ready(input: &TickInput<'_>) -> bool {
    input.memory.last_attack.is_none_or(|last| {
        input.now.saturating_duration_since(last) >= REPEAT_NORMAL_ATTACK_COOLDOWN
    })
}

fn attack_outcome_matches_target(outcome: &LastOutcome, target_id: u32) -> bool {
    matches!(
        outcome,
        LastOutcome::AttackFailed { target_id: seen }
            | LastOutcome::AttackNoProgress { target_id: seen }
            if *seen == target_id
    )
}

fn best_candidate<'a>(input: &'a TickInput<'_>) -> Option<&'a TargetCandidate> {
    input
        .candidates
        .iter()
        .find(|candidate| candidate_can_attack_now(candidate))
        .or_else(|| {
            input
                .candidates
                .iter()
                .find(|candidate| candidate.reachable_path.is_some())
        })
}

fn best_candidate_excluding<'a>(
    input: &'a TickInput<'_>,
    excluded_target_id: u32,
) -> Option<&'a TargetCandidate> {
    input
        .candidates
        .iter()
        .filter(|candidate| candidate.target_id != excluded_target_id)
        .find(|candidate| candidate_can_attack_now(candidate))
        .or_else(|| {
            input
                .candidates
                .iter()
                .filter(|candidate| candidate.target_id != excluded_target_id)
                .find(|candidate| candidate.reachable_path.is_some())
        })
}

fn candidate_can_attack_now(candidate: &TargetCandidate) -> bool {
    candidate.in_attack_range
        || (candidate.is_attacker
            && candidate.entity_ptr != 0
            && candidate.distance <= MELEE_RANGE_TILES)
}

fn teleport_action(input: &TickInput<'_>) -> Option<Action> {
    let name = input.cfg.teleport_scroll_name.trim();
    if !input.teleport_scroll_available || name.is_empty() {
        return None;
    }
    if input
        .memory
        .last_teleport
        .is_some_and(|last| input.now.saturating_duration_since(last) < TELEPORT_SCROLL_COOLDOWN)
    {
        return None;
    }
    if input.candidates.iter().any(|candidate| {
        candidate.reachable_path.is_some()
            || (candidate.is_attacker && candidate.entity_ptr != 0 && candidate.distance <= 1)
    }) {
        return None;
    }
    if input.explore_suggestion.is_some() && input.memory.last_walk.is_none() {
        return None;
    }
    if input.cfg.idle_teleport_secs > 0 {
        let ready_since = input
            .memory
            .no_actionable_target_since
            .unwrap_or(input.state_since);
        if input.now.saturating_duration_since(ready_since)
            < Duration::from_secs(input.cfg.idle_teleport_secs)
        {
            return None;
        }
    }
    Some(Action::UseTeleportScroll {
        name_keyword: name.to_string(),
    })
}

fn empty_explore_budget_action(input: &TickInput<'_>) -> Option<Action> {
    if input.memory.empty_explore_walks < EMPTY_EXPLORE_WALK_BUDGET {
        return None;
    }
    if input.candidates.iter().any(|candidate| {
        candidate.reachable_path.is_some()
            || (candidate.is_attacker && candidate.entity_ptr != 0 && candidate.distance <= 1)
    }) {
        return None;
    }
    force_teleport_action(input)
}

fn inactivity_watchdog_escape(from: StateLabel, input: &TickInput<'_>) -> Option<TickOutput> {
    if !inactivity_watchdog_state_allowed(from) {
        return None;
    }
    let last_activity = latest_activity_at(input.memory)?;
    if input.now.saturating_duration_since(last_activity) < INACTIVITY_RESET_DURATION {
        return None;
    }
    let scroll = force_teleport_action(input)?;
    Some(escape_with_scroll(from, input, scroll))
}

fn inactivity_watchdog_state_allowed(from: StateLabel) -> bool {
    matches!(
        from,
        StateLabel::EngagingApproach
            | StateLabel::EngagingAttack
            | StateLabel::EngagingKillConfirm
            | StateLabel::Exploring
            | StateLabel::RecoveringWalkStuck
            | StateLabel::RecoveringAttackFailed
            | StateLabel::RecoveringNoReachableTarget
    )
}

fn latest_activity_at(memory: &TacticalMemory) -> Option<Instant> {
    [
        memory.last_position_change,
        memory.last_attack,
        memory.last_skill_cast,
        memory.last_teleport,
    ]
    .into_iter()
    .flatten()
    .max()
}

fn force_teleport_action(input: &TickInput<'_>) -> Option<Action> {
    let name = input.cfg.teleport_scroll_name.trim();
    if name.is_empty() {
        return None;
    }
    if input
        .memory
        .last_teleport
        .is_some_and(|last| input.now.saturating_duration_since(last) < TELEPORT_SCROLL_COOLDOWN)
    {
        return None;
    }
    Some(Action::UseTeleportScroll {
        name_keyword: name.to_string(),
    })
}

fn escape_with_scroll(from: StateLabel, input: &TickInput<'_>, scroll: Action) -> TickOutput {
    let wait_until = input.now + ESCAPING_WAIT_DURATION;
    TickOutput {
        actions: vec![scroll],
        next_state: Some(HuntState::Escaping {
            scroll_used_at: input.now,
            wait_until,
            origin_pos: input.player_pos,
        }),
        transition: Some(Transition {
            from,
            to: StateLabel::Escaping,
            cause: TransitionCause::UseTeleportScroll,
        }),
        memory_updates: MemoryUpdate {
            set_last_teleport: Some(input.now),
            clear_no_actionable_target_since: true,
            clear_empty_explore_walks: true,
            ..MemoryUpdate::default()
        },
    }
}

fn recover(from: StateLabel, cause: RecoveryCause, now: Instant, duration: Duration) -> TickOutput {
    let to = match cause {
        RecoveryCause::WalkStuck => StateLabel::RecoveringWalkStuck,
        RecoveryCause::AttackFailed => StateLabel::RecoveringAttackFailed,
        RecoveryCause::DamageSpike => StateLabel::RecoveringDamageSpike,
        RecoveryCause::NoReachableTarget => StateLabel::RecoveringNoReachableTarget,
        RecoveryCause::CriticalHp => StateLabel::RecoveringCriticalHp,
    };
    let transition_cause = match cause {
        RecoveryCause::WalkStuck => TransitionCause::WalkStuck,
        RecoveryCause::AttackFailed => TransitionCause::AttackFailed,
        RecoveryCause::DamageSpike => TransitionCause::DamageSpike,
        RecoveryCause::NoReachableTarget => TransitionCause::AllTargetsUnreachable,
        RecoveryCause::CriticalHp => TransitionCause::CriticalHp,
    };
    TickOutput {
        actions: vec![Action::Wait],
        next_state: Some(HuntState::Recovering {
            cause,
            until: now + duration,
        }),
        transition: Some(Transition {
            from,
            to,
            cause: transition_cause,
        }),
        ..TickOutput::default()
    }
}

fn transition_to(from: StateLabel, state: HuntState, cause: TransitionCause) -> TickOutput {
    let to = state_label(&state);
    TickOutput {
        actions: vec![Action::Wait],
        next_state: Some(state),
        transition: Some(Transition { from, to, cause }),
        ..TickOutput::default()
    }
}

fn new_lock(candidate: &TargetCandidate, now: Instant) -> TargetLock {
    TargetLock {
        target_id: candidate.target_id,
        entity_ptr: candidate.entity_ptr,
        name: candidate.name.clone(),
        acquired_at: now,
        last_seen: now,
        bootstrapped: false,
    }
}

fn refresh_lock(lock: &TargetLock, candidate: &TargetCandidate, now: Instant) -> TargetLock {
    TargetLock {
        target_id: candidate.target_id,
        entity_ptr: candidate.entity_ptr,
        name: candidate.name.clone(),
        acquired_at: lock.acquired_at,
        last_seen: now,
        bootstrapped: lock.bootstrapped,
    }
}

fn state_label(state: &HuntState) -> StateLabel {
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::bot::decide::hunt::{AttackSequenceStep, HuntConfig};
    use crate::bot::hunt4::memory::{
        ExploreDirectionKey, FailureCause, TacticalMemory, FAILED_TARGET_TTL,
    };
    use crate::bot::hunt4::model::{EntityView, Snapshot};
    use crate::bot::hunt4::state::{EngageIntent, HuntState, RecoveryCause, TargetLock};
    use crate::bot::perception::classifier::EntityClass;

    use super::{
        step, Action, ExploreSuggestion, LastOutcome, StateLabel, TargetCandidate, TickInput,
        TransitionCause,
    };

    fn live_entity(target_id: u32, entity_ptr: u32, name: &str) -> EntityView {
        EntityView {
            target_id,
            entity_ptr,
            name: name.to_string(),
            sprite_id: 1134,
            action_state: 0,
            tile: (100, 100),
            raw_x: 100,
            y: 100,
            class: EntityClass::AttackableMonster,
            visible_confidence: 100,
            hostile_confidence: 100,
        }
    }

    fn target_candidate(target_id: u32, entity_ptr: u32) -> TargetCandidate {
        TargetCandidate {
            target_id,
            entity_ptr,
            name: format!("mob_{target_id:X}"),
            tile: (100, 100),
            distance: 1,
            in_attack_range: true,
            reachable_path: Some(Vec::new()),
            is_attacker: false,
        }
    }

    #[test]
    fn idle_counterattacks_adjacent_attacker_even_when_path_is_blocked() {
        let now = Instant::now();
        let target_id = 0x0BEB_C001;
        let entity_ptr = 0x1A00_C001;
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_entity(target_id, entity_ptr, "attacker")],
        };
        let mut candidate = target_candidate(target_id, entity_ptr);
        candidate.in_attack_range = false;
        candidate.reachable_path = None;
        candidate.is_attacker = true;
        candidate.distance = 1;
        let candidates = vec![candidate];

        let output = step(
            &HuntState::Idle,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(1),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::None,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::AttackTarget {
                target_id,
                entity_ptr,
                skill: None,
            }]
        );
        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::EngagingAttack, TransitionCause::TargetAcquired))
        );
    }

    #[test]
    fn approach_counterattacks_adjacent_attacker_without_casting_blocked_skill() {
        let now = Instant::now();
        let target_id = 0x0BEB_C002;
        let entity_ptr = 0x1A00_C002;
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_entity(target_id, entity_ptr, "attacker")],
        };
        let mut candidate = target_candidate(target_id, entity_ptr);
        candidate.in_attack_range = false;
        candidate.reachable_path = None;
        candidate.is_attacker = true;
        candidate.distance = 1;
        let candidates = vec![candidate];
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr,
                name: "attacker".to_string(),
                acquired_at: now - Duration::from_secs(1),
                last_seen: now - Duration::from_millis(200),
                bootstrapped: false,
            },
            intent: EngageIntent::Approach,
            path: None,
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(1),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::None,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Step(AttackSequenceStep::skill(
                    "triple_arrow".to_string(),
                    0,
                )),
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::AttackTarget {
                target_id,
                entity_ptr,
                skill: None,
            }]
        );
        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::EngagingAttack, TransitionCause::StartAttack))
        );
    }

    #[test]
    fn attacking_visible_locked_target_does_not_switch_to_other_candidate_when_lock_missing_from_candidates(
    ) {
        let now = Instant::now();
        let locked_id = 0x0BEB_FEE5;
        let other_id = 0x0BEB_FF0F;
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                live_entity(locked_id, 0x1AB5_9D30, "locked"),
                live_entity(other_id, 0x1AB5_A5C8, "other"),
            ],
        };
        let candidates = vec![target_candidate(other_id, 0x1AB5_A5C8)];
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id: locked_id,
                entity_ptr: 0x1AB5_9D30,
                name: "locked".to_string(),
                acquired_at: now - Duration::from_secs(2),
                last_seen: now - Duration::from_millis(200),
                bootstrapped: false,
            },
            intent: EngageIntent::Attack,
            path: None,
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(2),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::None,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::AttackTarget {
                target_id: locked_id,
                entity_ptr: 0x1AB5_9D30,
                skill: None,
            }]
        );
        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::EngagingAttack, TransitionCause::StartAttack))
        );
        match output.next_state {
            Some(HuntState::Engaging { lock, intent, .. }) => {
                assert_eq!(lock.target_id, locked_id);
                assert_eq!(lock.last_seen, now);
                assert_eq!(intent, EngageIntent::Attack);
            }
            other => panic!("expected locked attack state, got {other:?}"),
        }
    }

    #[test]
    fn attacking_recently_damaged_lock_continues_attack_when_candidate_temporarily_not_in_range() {
        let now = Instant::now();
        let locked_id = 0x0BEB_D43F;
        let other_id = 0x0BEB_D443;
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                live_entity(locked_id, 0x19CF_0ED0, "locked"),
                live_entity(other_id, 0x19FA_5E00, "other"),
            ],
        };
        let mut locked_candidate = target_candidate(locked_id, 0x19CF_0ED0);
        locked_candidate.in_attack_range = false;
        locked_candidate.reachable_path = None;
        let candidates = vec![locked_candidate, target_candidate(other_id, 0x19FA_5E00)];
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id: locked_id,
                entity_ptr: 0x19CF_0ED0,
                name: "locked".to_string(),
                acquired_at: now - Duration::from_secs(4),
                last_seen: now - Duration::from_millis(200),
                bootstrapped: false,
            },
            intent: EngageIntent::Attack,
            path: None,
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(4),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::None,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: true,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::AttackTarget {
                target_id: locked_id,
                entity_ptr: 0x19CF_0ED0,
                skill: None,
            }]
        );
        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::EngagingAttack, TransitionCause::StartAttack))
        );
        match output.next_state {
            Some(HuntState::Engaging { lock, intent, .. }) => {
                assert_eq!(lock.target_id, locked_id);
                assert_eq!(intent, EngageIntent::Attack);
            }
            other => panic!("expected locked attack state, got {other:?}"),
        }
    }

    #[test]
    fn attack_no_progress_immediately_switches_to_other_candidate() {
        let now = Instant::now();
        let locked_id = 0x0BEB_A001;
        let other_id = 0x0BEB_A002;
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                live_entity(locked_id, 0x1AB5_A001, "bad-lock"),
                live_entity(other_id, 0x1AB5_A002, "replacement"),
            ],
        };
        let mut locked_candidate = target_candidate(locked_id, 0x1AB5_A001);
        locked_candidate.in_attack_range = true;
        let replacement = target_candidate(other_id, 0x1AB5_A002);
        let candidates = vec![locked_candidate, replacement];
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id: locked_id,
                entity_ptr: 0x1AB5_A001,
                name: "bad-lock".to_string(),
                acquired_at: now - Duration::from_secs(12),
                last_seen: now,
                bootstrapped: false,
            },
            intent: EngageIntent::Attack,
            path: None,
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(12),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::AttackNoProgress {
                    target_id: locked_id,
                },
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::AttackTarget {
                target_id: other_id,
                entity_ptr: 0x1AB5_A002,
                skill: None,
            }]
        );
        match output.next_state {
            Some(HuntState::Engaging { lock, intent, .. }) => {
                assert_eq!(lock.target_id, other_id);
                assert_eq!(intent, EngageIntent::Attack);
            }
            other => panic!("expected immediate replacement engagement, got {other:?}"),
        }
        let (_, record) = output
            .memory_updates
            .add_failed_targets
            .iter()
            .find(|(id, _)| *id == locked_id)
            .expect("bad lock should be suppressed before retargeting");
        assert_eq!(record.cause, FailureCause::AttackRejected);
        assert_eq!(record.until.duration_since(now), FAILED_TARGET_TTL);
    }

    #[test]
    fn engaging_ignores_stale_attack_failure_for_previous_target() {
        let now = Instant::now();
        let target_id = 0x0BEB_1101;
        let stale_target = 0x0BEB_1100;
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let candidate = target_candidate(target_id, 0x2222_1101);
        let candidates = vec![candidate.clone()];
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_entity(target_id, candidate.entity_ptr, "current")],
        };
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr: candidate.entity_ptr,
                name: "current".to_string(),
                acquired_at: now - Duration::from_secs(1),
                last_seen: now,
                bootstrapped: false,
            },
            intent: EngageIntent::Attack,
            path: None,
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(1),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::AttackFailed {
                    target_id: stale_target,
                },
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::AttackTarget {
                target_id,
                entity_ptr: candidate.entity_ptr,
                skill: None,
            }]
        );
        assert!(output.memory_updates.add_failed_targets.is_empty());
    }

    #[test]
    fn attack_candidate_uses_basic_when_selected_skill_cd_not_ready() {
        let now = Instant::now();
        let target_id = 0x0BEB_1201;
        let cfg = HuntConfig::default();
        let mut memory = TacticalMemory::default();
        memory.last_skill_cast = Some(now - Duration::from_millis(500));
        let candidate = target_candidate(target_id, 0x2222_1201);
        let candidates = vec![candidate.clone()];
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_entity(target_id, candidate.entity_ptr, "cooldown")],
        };
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr: candidate.entity_ptr,
                name: "cooldown".to_string(),
                acquired_at: now - Duration::from_secs(1),
                last_seen: now,
                bootstrapped: false,
            },
            intent: EngageIntent::Attack,
            path: None,
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(1),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::None,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 2_000,
                attack_step: super::AttackStepInput::Step(AttackSequenceStep::skill(
                    "Frozen Cloud".to_string(),
                    0,
                )),
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::AttackTarget {
                target_id,
                entity_ptr: candidate.entity_ptr,
                skill: None,
            }]
        );
        assert_eq!(output.memory_updates.set_last_skill_cast, None);
    }

    #[test]
    fn attack_sequence_waiting_falls_back_to_basic_after_repeat_cooldown() {
        let now = Instant::now();
        let target_id = 0x0BEB_1202;
        let cfg = HuntConfig::default();
        let mut memory = TacticalMemory::default();
        memory.last_attack = Some(now - super::REPEAT_NORMAL_ATTACK_COOLDOWN);
        memory.post_skill_basic_pending_target = Some(target_id);
        let candidate = target_candidate(target_id, 0x2222_1202);
        let candidates = vec![candidate.clone()];
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_entity(target_id, candidate.entity_ptr, "waiting")],
        };
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr: candidate.entity_ptr,
                name: "waiting".to_string(),
                acquired_at: now - Duration::from_secs(1),
                last_seen: now,
                bootstrapped: false,
            },
            intent: EngageIntent::Attack,
            path: None,
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(1),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::None,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Waiting,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::AttackTarget {
                target_id,
                entity_ptr: candidate.entity_ptr,
                skill: None,
            }]
        );
        assert_eq!(output.memory_updates.set_last_attack, Some(now));
        assert!(output.memory_updates.clear_post_skill_basic_pending);
    }

    #[test]
    fn recovering_attack_failed_interrupts_for_available_candidate() {
        let now = Instant::now();
        let target_id = 0x0BEB_1301;
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let candidate = target_candidate(target_id, 0x2222_1301);
        let candidates = vec![candidate.clone()];
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_entity(target_id, candidate.entity_ptr, "replacement")],
        };
        let state = HuntState::Recovering {
            cause: RecoveryCause::AttackFailed,
            until: now + Duration::from_secs(1),
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(1),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::None,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::AttackTarget {
                target_id,
                entity_ptr: candidate.entity_ptr,
                skill: None,
            }]
        );
        assert_eq!(
            output.transition.as_ref().map(|t| (t.from, t.to, t.cause)),
            Some((
                StateLabel::RecoveringAttackFailed,
                StateLabel::EngagingAttack,
                TransitionCause::TargetAcquired
            ))
        );
    }

    #[test]
    fn empty_explore_budget_uses_scroll_before_idle_timer() {
        let now = Instant::now();
        let cfg = HuntConfig {
            teleport_scroll_name: "teleport".to_string(),
            idle_teleport_secs: 30,
            ..HuntConfig::default()
        };
        let mut memory = TacticalMemory::default();
        memory.last_walk = Some(now - Duration::from_millis(200));
        memory.no_actionable_target_since = Some(now - Duration::from_secs(1));
        memory.empty_explore_walks = super::EMPTY_EXPLORE_WALK_BUDGET;
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let explore = ExploreSuggestion {
            goal: (120, 100),
            path: vec![(101, 100)],
        };
        let state = HuntState::Exploring {
            goal: (110, 100),
            path: Vec::new(),
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(1),
                cfg: &cfg,
                memory: &memory,
                candidates: &[],
                explore_suggestion: Some(&explore),
                last_outcome: LastOutcome::None,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::UseTeleportScroll {
                name_keyword: "teleport".to_string()
            }]
        );
        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::Escaping, TransitionCause::UseTeleportScroll))
        );
        assert!(output.memory_updates.clear_empty_explore_walks);
    }

    #[test]
    fn exploration_walk_increments_empty_explore_budget() {
        let now = Instant::now();
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let explore = ExploreSuggestion {
            goal: (120, 100),
            path: vec![(101, 100), (102, 100)],
        };

        let output = step(
            &HuntState::Idle,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now,
                cfg: &cfg,
                memory: &memory,
                candidates: &[],
                explore_suggestion: Some(&explore),
                last_outcome: LastOutcome::None,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(output.actions, vec![Action::WalkTo { tile: (101, 100) }]);
        assert!(output.memory_updates.increment_empty_explore_walks);
    }

    #[test]
    fn target_acquired_clears_empty_explore_budget() {
        let now = Instant::now();
        let target_id = 0x0BEB_1401;
        let cfg = HuntConfig::default();
        let mut memory = TacticalMemory::default();
        memory.empty_explore_walks = 4;
        let candidate = target_candidate(target_id, 0x2222_1401);
        let candidates = vec![candidate.clone()];
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_entity(target_id, candidate.entity_ptr, "target")],
        };

        let output = step(
            &HuntState::Idle,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now,
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::None,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::AttackTarget {
                target_id,
                entity_ptr: candidate.entity_ptr,
                skill: None,
            }]
        );
        assert!(output.memory_updates.clear_empty_explore_walks);
    }

    #[test]
    fn attack_failed_keeps_recently_damaged_lock_instead_of_switching() {
        let now = Instant::now();
        let locked_id = 0x0BEB_1501;
        let other_id = 0x0BEB_1502;
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let locked = target_candidate(locked_id, 0x2222_1501);
        let other = target_candidate(other_id, 0x2222_1502);
        let candidates = vec![locked.clone(), other.clone()];
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                live_entity(locked_id, locked.entity_ptr, "damaged-lock"),
                live_entity(other_id, other.entity_ptr, "other"),
            ],
        };
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id: locked_id,
                entity_ptr: locked.entity_ptr,
                name: "damaged-lock".to_string(),
                acquired_at: now - Duration::from_secs(2),
                last_seen: now,
                bootstrapped: false,
            },
            intent: EngageIntent::Attack,
            path: None,
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(2),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::AttackFailed {
                    target_id: locked_id,
                },
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: true,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::AttackTarget {
                target_id: locked_id,
                entity_ptr: locked.entity_ptr,
                skill: None,
            }]
        );
        assert!(output.memory_updates.add_failed_targets.is_empty());
    }

    #[test]
    fn exploring_recovers_when_same_walk_tile_does_not_move_player() {
        let now = Instant::now();
        let cfg = HuntConfig::default();
        let mut memory = TacticalMemory::default();
        for i in 0..super::STALL_WINDOW_TICKS {
            memory.recent_positions.push_back((
                100,
                100,
                now - Duration::from_millis((super::STALL_WINDOW_TICKS - i) as u64 * 200),
            ));
        }
        memory.last_walk = Some(now - Duration::from_secs(2));
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let explore = ExploreSuggestion {
            goal: (105, 100),
            path: vec![(101, 100), (102, 100)],
        };
        let state = HuntState::Exploring {
            goal: explore.goal,
            path: explore.path.clone(),
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(3),
                cfg: &cfg,
                memory: &memory,
                candidates: &[],
                explore_suggestion: Some(&explore),
                last_outcome: LastOutcome::WalkOk,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(output.actions, vec![Action::Wait]);
        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::RecoveringWalkStuck, TransitionCause::WalkStuck))
        );
        assert_eq!(output.memory_updates.add_obstacles, vec![((101, 100), now)]);
        assert_eq!(
            output.memory_updates.add_failed_explore_directions,
            vec![(
                ExploreDirectionKey::from_goal((100, 100), explore.goal).unwrap(),
                now
            )]
        );
        assert!(
            output.memory_updates.clear_recent_positions,
            "confirmed exploration recovery must drop stale stall samples before the next route"
        );
    }

    #[test]
    fn exploring_walk_failed_records_failed_direction_before_retrying() {
        let now = Instant::now();
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let explore = ExploreSuggestion {
            goal: (105, 100),
            path: vec![(101, 100), (102, 100)],
        };
        let state = HuntState::Exploring {
            goal: explore.goal,
            path: explore.path.clone(),
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(1),
                cfg: &cfg,
                memory: &memory,
                candidates: &[],
                explore_suggestion: Some(&explore),
                last_outcome: LastOutcome::WalkFailed {
                    attempted_tile: (101, 100),
                },
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(output.actions, vec![Action::Wait]);
        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::RecoveringWalkStuck, TransitionCause::WalkStuck))
        );
        assert_eq!(output.memory_updates.add_obstacles, vec![((101, 100), now)]);
        assert_eq!(
            output.memory_updates.add_failed_explore_directions,
            vec![(
                ExploreDirectionKey::from_goal((100, 100), explore.goal).unwrap(),
                now
            )]
        );
        assert!(output.memory_updates.clear_recent_positions);
    }

    #[test]
    fn exploring_does_not_recover_from_startup_stall_samples_before_first_walk() {
        let now = Instant::now();
        let cfg = HuntConfig::default();
        let mut memory = TacticalMemory::default();
        for i in 0..super::STALL_WINDOW_TICKS {
            memory.recent_positions.push_back((
                100,
                100,
                now - Duration::from_millis((super::STALL_WINDOW_TICKS - i) as u64 * 200),
            ));
        }
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let explore = ExploreSuggestion {
            goal: (105, 100),
            path: vec![(101, 100), (102, 100)],
        };
        let state = HuntState::Exploring {
            goal: explore.goal,
            path: explore.path.clone(),
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(3),
                cfg: &cfg,
                memory: &memory,
                candidates: &[],
                explore_suggestion: Some(&explore),
                last_outcome: LastOutcome::WalkOk,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(output.actions, vec![Action::WalkTo { tile: (101, 100) }]);
        assert_eq!(output.transition, None);
    }

    #[test]
    fn exploring_continues_stored_path_instead_of_restarting_from_fresh_suggestion() {
        let now = Instant::now();
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let fresh_explore = ExploreSuggestion {
            goal: (95, 100),
            path: vec![(99, 100), (98, 100)],
        };
        let state = HuntState::Exploring {
            goal: (105, 100),
            path: vec![(101, 100), (102, 100), (103, 100)],
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(1),
                cfg: &cfg,
                memory: &memory,
                candidates: &[],
                explore_suggestion: Some(&fresh_explore),
                last_outcome: LastOutcome::WalkOk,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(output.actions, vec![Action::WalkTo { tile: (101, 100) }]);
        assert_eq!(output.transition, None);
        match output.next_state {
            Some(HuntState::Exploring { goal, path }) => {
                assert_eq!(goal, (105, 100));
                assert_eq!(path, vec![(101, 100), (102, 100), (103, 100)]);
            }
            other => panic!("expected continued exploration state, got {other:?}"),
        }
    }

    #[test]
    fn engaging_approach_continues_stored_path_instead_of_replanning_backwards() {
        let now = Instant::now();
        let target_id = 0x0BEB_CAFE;
        let entity_ptr = 0x1234_0000;
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let mut candidate = target_candidate(target_id, entity_ptr);
        candidate.tile = (104, 100);
        candidate.distance = 4;
        candidate.in_attack_range = false;
        candidate.reachable_path = Some(vec![(99, 100), (98, 100)]);
        let candidates = vec![candidate];
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr,
                name: "locked".to_string(),
                acquired_at: now - Duration::from_secs(1),
                last_seen: now - Duration::from_millis(200),
                bootstrapped: false,
            },
            intent: EngageIntent::Approach,
            path: Some(vec![(101, 100), (102, 100), (103, 100)]),
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(1),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::WalkOk,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(output.actions, vec![Action::WalkTo { tile: (101, 100) }]);
        assert_eq!(output.transition, None);
        match output.next_state {
            Some(HuntState::Engaging { intent, path, .. }) => {
                assert_eq!(intent, EngageIntent::Approach);
                assert_eq!(path, Some(vec![(101, 100), (102, 100), (103, 100)]));
            }
            other => panic!("expected continued approach state, got {other:?}"),
        }
    }

    #[test]
    fn engaging_approach_recovers_when_next_tile_does_not_move_player() {
        let now = Instant::now();
        let target_id = 0x0BEB_FB2F;
        let entity_ptr = 0x1981_0000;
        let cfg = HuntConfig::default();
        let mut memory = TacticalMemory::default();
        for i in 0..super::STALL_WINDOW_TICKS {
            memory.recent_positions.push_back((
                100,
                100,
                now - Duration::from_millis((super::STALL_WINDOW_TICKS - i) as u64 * 200),
            ));
        }
        memory.last_walk = Some(now - Duration::from_secs(2));
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let mut candidate = target_candidate(target_id, entity_ptr);
        candidate.distance = 2;
        candidate.in_attack_range = false;
        candidate.reachable_path = Some(vec![(100, 101), (100, 102)]);
        let candidates = vec![candidate];
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr,
                name: "locked".to_string(),
                acquired_at: now - Duration::from_secs(3),
                last_seen: now - Duration::from_millis(200),
                bootstrapped: false,
            },
            intent: EngageIntent::Approach,
            path: Some(vec![(100, 101), (100, 102)]),
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(3),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::WalkOk,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(output.actions, vec![Action::Wait]);
        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::RecoveringWalkStuck, TransitionCause::WalkStuck))
        );
        assert_eq!(output.memory_updates.add_obstacles, vec![((100, 101), now)]);
        let (_, record) = output
            .memory_updates
            .add_failed_targets
            .iter()
            .find(|(id, _)| *id == target_id)
            .expect("stalled approach should temporarily skip the wall-side target");
        assert_eq!(record.cause, FailureCause::Unreachable);
        assert_eq!(record.until.duration_since(now), FAILED_TARGET_TTL);
        assert!(
            output.memory_updates.clear_recent_positions,
            "confirmed approach recovery must drop stale stall samples before the next target"
        );
    }

    #[test]
    fn idle_with_expired_no_target_timer_explores_before_first_teleport() {
        let now = Instant::now();
        let cfg = HuntConfig {
            teleport_scroll_name: "teleport".to_string(),
            idle_teleport_secs: 1,
            ..HuntConfig::default()
        };
        let mut memory = TacticalMemory::default();
        memory.no_actionable_target_since = Some(now - Duration::from_secs(5));
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let explore = ExploreSuggestion {
            goal: (105, 100),
            path: vec![(101, 100), (102, 100)],
        };

        let output = step(
            &HuntState::Idle,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(5),
                cfg: &cfg,
                memory: &memory,
                candidates: &[],
                explore_suggestion: Some(&explore),
                last_outcome: LastOutcome::None,
                teleport_scroll_available: true,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(output.actions, vec![Action::WalkTo { tile: (101, 100) }]);
        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::Exploring, TransitionCause::StartExploration))
        );
    }

    #[test]
    fn idle_with_expired_no_target_timer_teleports_after_exploration_has_started() {
        let now = Instant::now();
        let cfg = HuntConfig {
            teleport_scroll_name: "teleport".to_string(),
            idle_teleport_secs: 1,
            ..HuntConfig::default()
        };
        let mut memory = TacticalMemory::default();
        memory.no_actionable_target_since = Some(now - Duration::from_secs(5));
        memory.last_walk = Some(now - Duration::from_secs(3));
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let explore = ExploreSuggestion {
            goal: (105, 100),
            path: vec![(101, 100), (102, 100)],
        };

        let output = step(
            &HuntState::Idle,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(5),
                cfg: &cfg,
                memory: &memory,
                candidates: &[],
                explore_suggestion: Some(&explore),
                last_outcome: LastOutcome::None,
                teleport_scroll_available: true,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::UseTeleportScroll {
                name_keyword: "teleport".to_string()
            }]
        );
        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::Escaping, TransitionCause::UseTeleportScroll))
        );
    }

    #[test]
    fn recovering_no_reachable_uses_configured_scroll_without_inventory_prescan() {
        let now = Instant::now();
        let cfg = HuntConfig {
            teleport_scroll_name: "teleport".to_string(),
            idle_teleport_secs: 1,
            ..HuntConfig::default()
        };
        let mut memory = TacticalMemory::default();
        memory.no_actionable_target_since = Some(now - Duration::from_secs(5));
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };

        let output = step(
            &HuntState::Recovering {
                cause: RecoveryCause::NoReachableTarget,
                until: now - Duration::from_millis(1),
            },
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(3),
                cfg: &cfg,
                memory: &memory,
                candidates: &[],
                explore_suggestion: None,
                last_outcome: LastOutcome::None,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::UseTeleportScroll {
                name_keyword: "teleport".to_string()
            }]
        );
        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::Escaping, TransitionCause::UseTeleportScroll))
        );
        assert_eq!(output.memory_updates.set_last_teleport, Some(now));
        match output.next_state {
            Some(HuntState::Escaping { wait_until, .. }) => {
                assert_eq!(
                    wait_until.duration_since(now),
                    super::ESCAPING_WAIT_DURATION
                );
            }
            other => panic!("expected escaping state, got {other:?}"),
        }
    }

    #[test]
    fn engaging_inactivity_watchdog_teleports_when_no_recent_movement_or_attack() {
        let now = Instant::now();
        let cfg = HuntConfig {
            teleport_scroll_name: "teleport".to_string(),
            ..HuntConfig::default()
        };
        let mut memory = TacticalMemory::default();
        memory.last_position_change = Some(now - Duration::from_secs(25));
        let target_id = 0x0BEB_C900;
        let entity_ptr = 0x1A00_C900;
        let candidate = TargetCandidate {
            reachable_path: Some(vec![(101, 100)]),
            in_attack_range: false,
            distance: 4,
            ..target_candidate(target_id, entity_ptr)
        };
        let candidates = vec![candidate];
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_entity(target_id, entity_ptr, "target")],
        };
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr,
                name: "target".to_string(),
                acquired_at: now - Duration::from_secs(25),
                last_seen: now - Duration::from_secs(1),
                bootstrapped: false,
            },
            intent: EngageIntent::Approach,
            path: Some(vec![(101, 100)]),
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(25),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::None,
                teleport_scroll_available: true,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.actions,
            vec![Action::UseTeleportScroll {
                name_keyword: "teleport".to_string(),
            }]
        );
        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::Escaping, TransitionCause::UseTeleportScroll))
        );
        assert_eq!(output.memory_updates.set_last_teleport, Some(now));
    }

    #[test]
    fn engaging_inactivity_watchdog_keeps_recent_attack_active() {
        let now = Instant::now();
        let cfg = HuntConfig {
            teleport_scroll_name: "teleport".to_string(),
            ..HuntConfig::default()
        };
        let mut memory = TacticalMemory::default();
        memory.last_position_change = Some(now - Duration::from_secs(25));
        memory.last_attack = Some(now - Duration::from_secs(1));
        let target_id = 0x0BEB_C901;
        let entity_ptr = 0x1A00_C901;
        let candidate = TargetCandidate {
            reachable_path: Some(vec![(101, 100)]),
            in_attack_range: false,
            distance: 4,
            ..target_candidate(target_id, entity_ptr)
        };
        let candidates = vec![candidate];
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_entity(target_id, entity_ptr, "target")],
        };
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr,
                name: "target".to_string(),
                acquired_at: now - Duration::from_secs(25),
                last_seen: now - Duration::from_secs(1),
                bootstrapped: false,
            },
            intent: EngageIntent::Approach,
            path: Some(vec![(101, 100)]),
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(25),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::None,
                teleport_scroll_available: true,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(output.actions, vec![Action::WalkTo { tile: (101, 100) }]);
        assert_eq!(output.memory_updates.set_last_walk, Some(now));
    }

    #[test]
    fn engaging_approach_walk_failed_marks_target_unreachable() {
        let now = Instant::now();
        let target_id = 0x0BEB_F00D;
        let entity_ptr = 0x1981_0001;
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr,
                name: "locked".to_string(),
                acquired_at: now - Duration::from_secs(3),
                last_seen: now - Duration::from_millis(200),
                bootstrapped: false,
            },
            intent: EngageIntent::Approach,
            path: Some(vec![(100, 101), (100, 102)]),
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(3),
                cfg: &cfg,
                memory: &memory,
                candidates: &[],
                explore_suggestion: None,
                last_outcome: LastOutcome::WalkFailed {
                    attempted_tile: (100, 101),
                },
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((StateLabel::RecoveringWalkStuck, TransitionCause::WalkStuck))
        );
        assert_eq!(output.memory_updates.add_obstacles, vec![((100, 101), now)]);
        let (_, record) = output
            .memory_updates
            .add_failed_targets
            .iter()
            .find(|(id, _)| *id == target_id)
            .expect("failed approach walk should blacklist the target briefly");
        assert_eq!(record.cause, FailureCause::Unreachable);
        assert_eq!(record.until.duration_since(now), FAILED_TARGET_TTL);
        assert!(output.memory_updates.clear_recent_positions);
    }

    #[test]
    fn engaging_candidate_without_remaining_path_marks_target_unreachable() {
        let now = Instant::now();
        let target_id = 0x0BEB_F011;
        let entity_ptr = 0x1981_0002;
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: Vec::new(),
        };
        let mut candidate = target_candidate(target_id, entity_ptr);
        candidate.distance = 2;
        candidate.in_attack_range = false;
        candidate.reachable_path = Some(Vec::new());
        let candidates = vec![candidate];
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr,
                name: "locked".to_string(),
                acquired_at: now - Duration::from_secs(3),
                last_seen: now - Duration::from_millis(200),
                bootstrapped: false,
            },
            intent: EngageIntent::Approach,
            path: Some(Vec::new()),
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(3),
                cfg: &cfg,
                memory: &memory,
                candidates: &candidates,
                explore_suggestion: None,
                last_outcome: LastOutcome::WalkOk,
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Legacy,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((
                StateLabel::RecoveringNoReachableTarget,
                TransitionCause::AllTargetsUnreachable
            ))
        );
        let (_, record) = output
            .memory_updates
            .add_failed_targets
            .iter()
            .find(|(id, _)| *id == target_id)
            .expect("no remaining approach path should briefly skip the target");
        assert_eq!(record.cause, FailureCause::Unreachable);
        assert_eq!(record.until.duration_since(now), FAILED_TARGET_TTL);
        assert!(output.memory_updates.clear_recent_positions);
    }

    #[test]
    fn attack_failed_records_failed_target_beyond_recovery_window() {
        let now = Instant::now();
        let target_id = 0x0BEB_DEC1;
        let cfg = HuntConfig::default();
        let memory = TacticalMemory::default();
        let snapshot = Snapshot::default();
        let state = HuntState::Engaging {
            lock: TargetLock {
                target_id,
                entity_ptr: 0x2222_0000,
                name: "mob".to_string(),
                acquired_at: now - Duration::from_secs(1),
                last_seen: now,
                bootstrapped: false,
            },
            intent: EngageIntent::Attack,
            path: None,
        };

        let output = step(
            &state,
            TickInput {
                snapshot: &snapshot,
                player_pos: Some((100, 100)),
                player_alive: true,
                master_enabled: true,
                in_game: true,
                now,
                state_since: now - Duration::from_secs(1),
                cfg: &cfg,
                memory: &memory,
                candidates: &[],
                explore_suggestion: None,
                last_outcome: LastOutcome::AttackFailed { target_id },
                teleport_scroll_available: false,
                damage_spike_detected: false,
                critical_hp_detected: false,
                skill_cd_ms: 0,
                attack_step: super::AttackStepInput::Waiting,
                locked_target_removed_or_dead: false,
                locked_target_recently_damaged: false,
            },
        );

        assert_eq!(
            output.transition.as_ref().map(|t| (t.to, t.cause)),
            Some((
                StateLabel::RecoveringAttackFailed,
                TransitionCause::AttackFailed
            ))
        );
        match output.next_state {
            Some(HuntState::Recovering {
                cause: RecoveryCause::AttackFailed,
                until,
            }) => assert_eq!(
                until.duration_since(now),
                super::RECOVERY_ATTACK_FAILED_DURATION
            ),
            other => panic!("expected attack-failed recovery, got {other:?}"),
        }
        let (_, record) = output
            .memory_updates
            .add_failed_targets
            .iter()
            .find(|(id, _)| *id == target_id)
            .expect("attack failure should blacklist the rejected target");
        assert_eq!(record.cause, FailureCause::AttackRejected);
        assert_eq!(record.until.duration_since(now), FAILED_TARGET_TTL);
    }
}
