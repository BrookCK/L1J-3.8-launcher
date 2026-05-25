use std::time::Instant;

use crate::bot::hunt4::memory::TacticalMemory;
use crate::bot::hunt4::step::{ExploreSuggestion, TargetCandidate};
use crate::bot::hunt4::targeting::TargetSelectionFrame;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteKind {
    Attack,
    Explore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteStatus {
    AlreadyInRange,
    Reachable,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePlan {
    pub kind: RouteKind,
    pub target_id: Option<u32>,
    pub target_tile: Option<(i32, i32)>,
    pub attack_tile: Option<(i32, i32)>,
    pub goal: Option<(i32, i32)>,
    pub path: Vec<(i32, i32)>,
    pub next_tile: Option<(i32, i32)>,
    pub status: RouteStatus,
    pub reason: String,
}

pub fn route_for_selection(
    selection: &TargetSelectionFrame,
    candidates: &[TargetCandidate],
) -> Option<RoutePlan> {
    let selected = selection.selected.as_ref()?;
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.target_id == selected.target_id)?;
    Some(route_for_candidate(candidate))
}

pub fn route_for_candidate(candidate: &TargetCandidate) -> RoutePlan {
    let path = candidate.reachable_path.clone().unwrap_or_default();
    let next_tile = path.first().copied();
    let status = if candidate.in_attack_range {
        RouteStatus::AlreadyInRange
    } else if candidate.reachable_path.is_some() {
        RouteStatus::Reachable
    } else {
        RouteStatus::Unreachable
    };
    let attack_tile = if matches!(status, RouteStatus::Reachable) {
        path.last().copied()
    } else {
        None
    };
    let reason = match status {
        RouteStatus::AlreadyInRange => {
            format!("attack_already_in_range target={:08X}", candidate.target_id)
        }
        RouteStatus::Reachable => {
            format!("attack_reachable target={:08X}", candidate.target_id)
        }
        RouteStatus::Unreachable => {
            format!("attack_unreachable target={:08X}", candidate.target_id)
        }
    };

    RoutePlan {
        kind: RouteKind::Attack,
        target_id: Some(candidate.target_id),
        target_tile: Some(candidate.tile),
        attack_tile,
        goal: attack_tile,
        path,
        next_tile,
        status,
        reason,
    }
}

pub fn route_for_explore(explore: &ExploreSuggestion) -> RoutePlan {
    RoutePlan {
        kind: RouteKind::Explore,
        target_id: None,
        target_tile: None,
        attack_tile: None,
        goal: Some(explore.goal),
        path: explore.path.clone(),
        next_tile: explore.path.first().copied(),
        status: RouteStatus::Reachable,
        reason: format!(
            "explore_reachable goal=({}, {})",
            explore.goal.0, explore.goal.1
        ),
    }
}

pub fn route_for_plan(
    selection: &TargetSelectionFrame,
    candidates: &[TargetCandidate],
    explore: Option<&ExploreSuggestion>,
) -> Option<RoutePlan> {
    route_for_selection(selection, candidates).or_else(|| explore.map(route_for_explore))
}

pub fn consume_reached_path(
    player_pos: Option<(i32, i32)>,
    path: &[(i32, i32)],
) -> Vec<(i32, i32)> {
    let Some(player) = player_pos else {
        return path.to_vec();
    };
    if let Some(idx) = path.iter().position(|tile| *tile == player) {
        return path[idx + 1..].to_vec();
    }
    if let Some(idx) = path
        .iter()
        .enumerate()
        .rev()
        .find(|(_, tile)| player.0.abs_diff(tile.0).max(player.1.abs_diff(tile.1)) <= 1)
        .map(|(idx, _)| idx)
    {
        return path[idx..].to_vec();
    }
    path.to_vec()
}

pub fn first_unreached_path_tile(
    player_pos: Option<(i32, i32)>,
    path: &[(i32, i32)],
) -> Option<(i32, i32)> {
    consume_reached_path(player_pos, path).first().copied()
}

pub fn stable_approach_path(
    player_pos: Option<(i32, i32)>,
    memory: &TacticalMemory,
    now: Instant,
    current_path: Option<&[(i32, i32)]>,
    fresh_path: Vec<(i32, i32)>,
) -> Vec<(i32, i32)> {
    let Some(current_path) = current_path else {
        return fresh_path;
    };
    if current_path.is_empty() {
        return fresh_path;
    }
    let Some(player) = player_pos else {
        return current_path.to_vec();
    };

    if let Some(next) = first_unreached_path_tile(player_pos, current_path) {
        if memory.is_obstacle(next, now) {
            return fresh_path;
        }
    }

    let progressed_path = consume_reached_path(player_pos, current_path);
    if progressed_path.len() < current_path.len() {
        if !progressed_path.is_empty() {
            return progressed_path;
        }
        return fresh_path;
    }

    if !current_path.iter().any(|tile| *tile == player) {
        if let Some(current_first) = current_path.first().copied() {
            let current_adjacent = player
                .0
                .abs_diff(current_first.0)
                .max(player.1.abs_diff(current_first.1))
                <= 1;
            if current_adjacent {
                return current_path.to_vec();
            }
        }
        return fresh_path;
    }

    if !progressed_path.is_empty() {
        return progressed_path;
    }
    fresh_path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        target_id: u32,
        in_attack_range: bool,
        reachable_path: Option<Vec<(i32, i32)>>,
    ) -> TargetCandidate {
        TargetCandidate {
            target_id,
            entity_ptr: target_id + 0x1000,
            name: format!("mob_{target_id:X}"),
            tile: (104, 100),
            distance: 4,
            in_attack_range,
            reachable_path,
            is_attacker: false,
        }
    }

    #[test]
    fn route_for_candidate_records_reachable_attack_route() {
        let route = route_for_candidate(&candidate(
            0x0100_0010,
            false,
            Some(vec![(101, 100), (102, 100)]),
        ));

        assert_eq!(route.kind, RouteKind::Attack);
        assert_eq!(route.status, RouteStatus::Reachable);
        assert_eq!(route.target_id, Some(0x0100_0010));
        assert_eq!(route.target_tile, Some((104, 100)));
        assert_eq!(route.attack_tile, Some((102, 100)));
        assert_eq!(route.next_tile, Some((101, 100)));
    }

    #[test]
    fn consume_reached_path_skips_tiles_already_reached_by_player() {
        let path = vec![(101, 100), (102, 100), (103, 100)];

        assert_eq!(
            consume_reached_path(Some((102, 100)), &path),
            vec![(103, 100)]
        );
        assert_eq!(
            first_unreached_path_tile(Some((102, 100)), &path),
            Some((103, 100))
        );
    }
}
