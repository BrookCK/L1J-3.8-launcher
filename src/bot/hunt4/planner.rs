//! Planner ?????game snapshot + map + memory?蕃??step() ?閬?蝝???
//! (`TargetCandidate` list 頝?`ExploreSuggestion`)??
//!
//! 頝?step.rs 銝璅?蝝撘?銝?撖?game memory / 銝??primitive??runtime ??
//! tick ??澆 planner,蝯?擗菟?step??
//!
//! 閮剛???:**candidate.reachable_path ??planner 蝞?A* 敺?甈?蝯?**??step
//! 摰?訾縑??隢?銝???頝臬????迨 planner 敹?隤祕:銝?停??None,??
//! ?箔???摰? Some(蝛?霈?step 隤文?箝歇?唳????

use std::collections::HashSet;
use std::time::Instant;

use crate::bot::decide::hunt::MELEE_RANGE_TILES;
use crate::bot::decide::pathfind::{plan_to_any, Walkable};
use crate::bot::hunt4::model::Snapshot;
use crate::minimap::nav_profile::NavProfile;

use super::memory::{FailureCause, TacticalMemory};
use super::step::{ExploreSuggestion, TargetCandidate};
use super::targeting;

const WALL_ADJACENT_STEP_PENALTY: u32 = 18;
const WALL_DIAGONAL_STEP_PENALTY: u32 = 4;
const WALL_CORNER_STEP_PENALTY: u32 = 24;
const WALL_TUNNEL_STEP_PENALTY: u32 = 72;
const EXPLORE_VISITED_TILE_PENALTY: u32 = 160;
const EXPLORE_BACKTRACK_TILE_PENALTY: u32 = 480;
const PROFILE_EXPLORE_GOAL_LIMIT: usize = 64;

/// 敺?snapshot + map + memory + recent_attackers 蝞??憟賜??銵??andidate list??
///
/// 閬?:
/// - ?蕪??`memory.failed_targets` 鋆⊿?瘝???
/// - distance = Chebyshev(player, target.tile)
/// - in_attack_range = distance ??attack_range 銝??/?? edge clear
/// - is_attacker = entity.target_id ??`recent_attackers`(?餈?10s server 撱???
///   摰韏瑞? Attack)
/// - reachable_path:
///   * in_attack_range ??`Some(vec![])`(撌脣?餅?雿?
///   * ?血? + map ????A* `plan_to_any(player, attack_tiles(target))`嚗?銝 ??`None`
///   * ?血? + map ????`Some(vec![attack_tile])`(??map 璅?韏啣?餅?雿?
///     銝粥?唳芰?祈澈 tile)
/// - 銵???gate:
///   * 銝?祉璅???`reachable_path.is_some()` ???candidate list??
///   * ???唬???/擃?/??LOS ?璅? snapshot 雿??餈??芥???銝?
///     ?脣撠???韏啗楝瘙箇???
///   * ?臭?靘??胯票頨思?甇??餅????格?,?迂靽?????
/// - target order: adjacent melee attackers, in-range reachable targets, local reachable targets,
///   then far reachable targets; unreachable non-attackers stay last.
pub fn build_candidates<G: Walkable>(
    snapshot: &Snapshot,
    player: (i32, i32),
    attack_range: u32,
    grid: Option<&G>,
    memory: &TacticalMemory,
    recent_attackers: &HashSet<u32>,
    now: Instant,
) -> Vec<TargetCandidate> {
    let occupied_tiles = movement_blocking_entity_tiles(snapshot, player);
    let mut candidates: Vec<TargetCandidate> = snapshot
        .valid_targets()
        .filter(|entity| crate::bot::hunt4::targeting::candidate_allowed(entity))
        .filter(|entity| should_consider_target(memory, entity.target_id, recent_attackers, now))
        .filter_map(|entity| {
            let dx = (entity.tile.0 - player.0).unsigned_abs();
            let dy = (entity.tile.1 - player.1).unsigned_abs();
            let distance = dx.max(dy);
            let in_range = distance <= attack_range;
            let (in_range, reachable_path) = compute_reachable_path(
                player,
                entity.tile,
                in_range,
                attack_range,
                grid,
                memory,
                &occupied_tiles,
                now,
            );
            let candidate = TargetCandidate {
                target_id: entity.target_id,
                entity_ptr: entity.entity_ptr,
                name: entity.name.clone(),
                tile: entity.tile,
                distance,
                in_attack_range: in_range,
                reachable_path,
                is_attacker: recent_attackers.contains(&entity.target_id),
            };
            actionable_candidate(candidate)
        })
        .collect();

    targeting::sort_candidates(&mut candidates);
    candidates
}

fn actionable_candidate(candidate: TargetCandidate) -> Option<TargetCandidate> {
    if candidate.reachable_path.is_some() || counterattackable_adjacent(&candidate) {
        Some(candidate)
    } else {
        None
    }
}

fn counterattackable_adjacent(candidate: &TargetCandidate) -> bool {
    candidate.is_attacker && candidate.entity_ptr != 0 && candidate.distance <= MELEE_RANGE_TILES
}

fn should_consider_target(
    memory: &TacticalMemory,
    target_id: u32,
    recent_attackers: &HashSet<u32>,
    now: Instant,
) -> bool {
    match memory
        .failed_targets
        .get(&target_id)
        .filter(|record| record.until > now)
        .map(|record| record.cause)
    {
        // `AttackRejected` also covers backend attack lookup failures. Retrying those
        // immediately can lock the bot into TargetAcquired -> AttackLookupFailed ->
        // RecoveringAttackFailed -> TargetAcquired on the same id forever, even when
        // the target is present in recent_attackers. Respect the short blacklist.
        Some(FailureCause::AttackRejected) => false,
        // A target that was path-unreachable may move into melee / become the actual
        // attacker. Keep the previous counterattack escape hatch for that cause only.
        Some(FailureCause::Unreachable) => recent_attackers.contains(&target_id),
        None => true,
    }
}

fn movement_blocking_entity_tiles(snapshot: &Snapshot, player: (i32, i32)) -> HashSet<(i32, i32)> {
    snapshot
        .entities
        .iter()
        .filter(|entity| entity.blocks_movement())
        .map(|entity| entity.tile)
        .filter(|tile| *tile != player)
        .collect()
}

fn compute_reachable_path<G: Walkable>(
    player: (i32, i32),
    target: (i32, i32),
    in_range: bool,
    attack_range: u32,
    grid: Option<&G>,
    memory: &TacticalMemory,
    occupied_tiles: &HashSet<(i32, i32)>,
    now: Instant,
) -> (bool, Option<Vec<(i32, i32)>>) {
    let Some(grid) = grid else {
        return if in_range {
            (true, Some(Vec::new()))
        } else {
            (
                false,
                Some(vec![optimistic_attack_tile(player, target, attack_range)]),
            )
        };
    };
    let grid = ObstacleOverlay {
        inner: grid,
        memory,
        occupied_tiles,
        now,
        start: player,
        explore_map_id: None,
        previous_position: None,
    };
    if in_range && attack_line_clear(player, target, &grid) {
        return (true, Some(Vec::new()));
    }
    if in_range && player.0.abs_diff(target.0).max(player.1.abs_diff(target.1)) <= MELEE_RANGE_TILES
    {
        return (false, None);
    }

    let goals = attack_tiles(target, attack_range, &grid);
    (false, plan_to_any(player, &goals, &grid))
}

fn optimistic_attack_tile(player: (i32, i32), target: (i32, i32), attack_range: u32) -> (i32, i32) {
    if attack_range == 0 {
        return target;
    }
    let range = attack_range as i32;
    let step_x = (target.0 - player.0).signum();
    let step_y = (target.1 - player.1).signum();
    let tile = (target.0 - step_x * range, target.1 - step_y * range);
    if tile == target {
        player
    } else {
        tile
    }
}

fn attack_tiles<G: Walkable>(
    target: (i32, i32),
    attack_range: u32,
    grid: &ObstacleOverlay<'_, G>,
) -> Vec<(i32, i32)> {
    let range = attack_range as i32;
    let mut out = Vec::new();
    for y in target.1 - range..=target.1 + range {
        for x in target.0 - range..=target.0 + range {
            let tile = (x, y);
            if tile != target
                && x.abs_diff(target.0).max(y.abs_diff(target.1)) <= attack_range
                && grid.is_walkable(x, y)
                && attack_line_clear(tile, target, grid)
            {
                out.push(tile);
            }
        }
    }
    out
}

fn attack_line_clear<G: Walkable>(
    from: (i32, i32),
    target: (i32, i32),
    grid: &ObstacleOverlay<'_, G>,
) -> bool {
    if grid.blocks_sight_for_attack(target.0, target.1, target) {
        return false;
    }

    let dx = target.0 - from.0;
    let dy = target.1 - from.1;
    let steps = dx.abs().max(dy.abs());
    if steps <= 1 {
        return grid.can_step_for_attack(from, target, target);
    }

    let mut prev = from;
    for step in 1..steps {
        let x = from.0 + round_div_nearest(dx * step, steps);
        let y = from.1 + round_div_nearest(dy * step, steps);
        let cur = (x, y);
        if cur != prev && !grid.can_step_for_attack(prev, cur, target) {
            return false;
        }
        if cur != from && grid.blocks_sight_for_attack(cur.0, cur.1, target) {
            return false;
        }
        prev = cur;
    }

    grid.can_step_for_attack(prev, target, target)
}

fn round_div_nearest(n: i32, d: i32) -> i32 {
    debug_assert!(d > 0);
    if n >= 0 {
        (n + d / 2) / d
    } else {
        -((-n + d / 2) / d)
    }
}

struct ObstacleOverlay<'a, G> {
    inner: &'a G,
    memory: &'a TacticalMemory,
    occupied_tiles: &'a HashSet<(i32, i32)>,
    now: Instant,
    start: (i32, i32),
    explore_map_id: Option<u32>,
    previous_position: Option<(i32, i32)>,
}

impl<G: Walkable> ObstacleOverlay<'_, G> {
    fn is_runtime_blocked(&self, tile: (i32, i32)) -> bool {
        tile != self.start
            && (self.memory.is_obstacle(tile, self.now) || self.occupied_tiles.contains(&tile))
    }

    fn is_entity_clearance_blocker(&self, tile: (i32, i32)) -> bool {
        tile != self.start && self.occupied_tiles.contains(&tile)
    }

    fn is_solid_clearance_blocker(&self, tile: (i32, i32)) -> bool {
        tile != self.start
            && (self.memory.is_obstacle(tile, self.now)
                || !self.inner.is_walkable(tile.0, tile.1)
                || self.inner.blocks_sight(tile.0, tile.1))
    }

    fn side_has_entity_clearance_blocker(&self, tiles: [(i32, i32); 2]) -> bool {
        tiles
            .into_iter()
            .any(|tile| self.is_entity_clearance_blocker(tile))
    }

    fn side_has_solid_clearance_blocker(&self, tiles: [(i32, i32); 2]) -> bool {
        tiles
            .into_iter()
            .any(|tile| self.is_solid_clearance_blocker(tile))
    }

    fn cardinal_step_has_entity_wall_squeeze(&self, from: (i32, i32), to: (i32, i32)) -> bool {
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        if dx.abs() + dy.abs() != 1 {
            return false;
        }

        let lateral = (-dy, dx);
        let side_a = [
            (from.0 + lateral.0, from.1 + lateral.1),
            (to.0 + lateral.0, to.1 + lateral.1),
        ];
        let side_b = [
            (from.0 - lateral.0, from.1 - lateral.1),
            (to.0 - lateral.0, to.1 - lateral.1),
        ];

        (self.side_has_entity_clearance_blocker(side_a)
            && self.side_has_solid_clearance_blocker(side_b))
            || (self.side_has_entity_clearance_blocker(side_b)
                && self.side_has_solid_clearance_blocker(side_a))
    }

    fn cardinal_step_has_diagonal_wall_corner_squeeze(
        &self,
        from: (i32, i32),
        to: (i32, i32),
    ) -> bool {
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        if dx.abs() + dy.abs() != 1 {
            return false;
        }

        let lateral = (-dy, dx);
        let from_side_a = (from.0 + lateral.0, from.1 + lateral.1);
        let to_side_a = (to.0 + lateral.0, to.1 + lateral.1);
        let from_side_b = (from.0 - lateral.0, from.1 - lateral.1);
        let to_side_b = (to.0 - lateral.0, to.1 - lateral.1);

        let side_a_continuous = self.is_solid_clearance_blocker(from_side_a)
            && self.is_solid_clearance_blocker(to_side_a);
        let side_b_continuous = self.is_solid_clearance_blocker(from_side_b)
            && self.is_solid_clearance_blocker(to_side_b);
        if side_a_continuous && side_b_continuous {
            return false;
        }

        (self.is_solid_clearance_blocker(from_side_a) && self.is_solid_clearance_blocker(to_side_b))
            || (self.is_solid_clearance_blocker(from_side_b)
                && self.is_solid_clearance_blocker(to_side_a))
    }

    fn step_has_entity_wall_squeeze(&self, from: (i32, i32), to: (i32, i32)) -> bool {
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        if dx == 0 || dy == 0 {
            return self.cardinal_step_has_entity_wall_squeeze(from, to);
        }

        let side_x = (from.0 + dx, from.1);
        let side_y = (from.0, from.1 + dy);
        self.cardinal_step_has_entity_wall_squeeze(from, side_x)
            || self.cardinal_step_has_entity_wall_squeeze(side_x, to)
            || self.cardinal_step_has_entity_wall_squeeze(from, side_y)
            || self.cardinal_step_has_entity_wall_squeeze(side_y, to)
    }

    fn step_has_diagonal_wall_corner_squeeze(&self, from: (i32, i32), to: (i32, i32)) -> bool {
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        if dx == 0 || dy == 0 {
            return self.cardinal_step_has_diagonal_wall_corner_squeeze(from, to);
        }

        let side_x = (from.0 + dx, from.1);
        let side_y = (from.0, from.1 + dy);
        self.cardinal_step_has_diagonal_wall_corner_squeeze(from, side_x)
            || self.cardinal_step_has_diagonal_wall_corner_squeeze(side_x, to)
            || self.cardinal_step_has_diagonal_wall_corner_squeeze(from, side_y)
            || self.cardinal_step_has_diagonal_wall_corner_squeeze(side_y, to)
    }

    fn wall_clearance_penalty(&self, tile: (i32, i32)) -> u32 {
        if tile == self.start {
            return 0;
        }

        let n = self.is_solid_clearance_blocker((tile.0, tile.1 - 1));
        let e = self.is_solid_clearance_blocker((tile.0 + 1, tile.1));
        let s = self.is_solid_clearance_blocker((tile.0, tile.1 + 1));
        let w = self.is_solid_clearance_blocker((tile.0 - 1, tile.1));
        let ne = self.is_solid_clearance_blocker((tile.0 + 1, tile.1 - 1));
        let se = self.is_solid_clearance_blocker((tile.0 + 1, tile.1 + 1));
        let sw = self.is_solid_clearance_blocker((tile.0 - 1, tile.1 + 1));
        let nw = self.is_solid_clearance_blocker((tile.0 - 1, tile.1 - 1));

        let cardinal_count = [n, e, s, w].into_iter().filter(|solid| *solid).count() as u32;
        let diagonal_count = [ne, se, sw, nw].into_iter().filter(|solid| *solid).count() as u32;
        let corner_count = [(n, e), (e, s), (s, w), (w, n)]
            .into_iter()
            .filter(|(a, b)| *a && *b)
            .count() as u32;

        let mut penalty = cardinal_count * WALL_ADJACENT_STEP_PENALTY
            + diagonal_count * WALL_DIAGONAL_STEP_PENALTY
            + corner_count * WALL_CORNER_STEP_PENALTY;
        if (n && s) || (e && w) {
            penalty += WALL_TUNNEL_STEP_PENALTY;
        }
        penalty
    }

    fn explore_memory_penalty(&self, tile: (i32, i32)) -> u32 {
        let visit_penalty = self
            .explore_map_id
            .map(|map_id| {
                u32::from(self.memory.visit_count(map_id, tile).min(20))
                    * EXPLORE_VISITED_TILE_PENALTY
            })
            .unwrap_or(0);
        let backtrack_penalty = if self.previous_position == Some(tile) {
            EXPLORE_BACKTRACK_TILE_PENALTY
        } else {
            0
        };
        visit_penalty + backtrack_penalty
    }

    fn is_walkable_for_attack_target(&self, x: i32, y: i32, target: (i32, i32)) -> bool {
        let tile = (x, y);
        (tile == self.start || tile == target || !self.is_runtime_blocked(tile))
            && self.inner.is_walkable(x, y)
    }

    fn blocks_sight_for_attack(&self, x: i32, y: i32, target: (i32, i32)) -> bool {
        let tile = (x, y);
        self.inner.blocks_sight(x, y) || (tile != target && self.is_runtime_blocked(tile))
    }

    fn can_step_for_attack(&self, from: (i32, i32), to: (i32, i32), target: (i32, i32)) -> bool {
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        if dx == 0 && dy == 0 {
            return self.is_walkable_for_attack_target(from.0, from.1, target);
        }
        if dx.abs() > 1 || dy.abs() > 1 || !self.is_walkable_for_attack_target(to.0, to.1, target) {
            return false;
        }
        if !self.inner.can_step(from, to) {
            return false;
        }
        if self.step_has_entity_wall_squeeze(from, to)
            || self.step_has_diagonal_wall_corner_squeeze(from, to)
        {
            return false;
        }
        if dx != 0 && dy != 0 {
            let side_x = (from.0 + dx, from.1);
            let side_y = (from.0, from.1 + dy);
            return self.is_walkable(side_x.0, side_x.1) && self.is_walkable(side_y.0, side_y.1);
        }
        true
    }
}

impl<G: Walkable> Walkable for ObstacleOverlay<'_, G> {
    fn is_walkable(&self, x: i32, y: i32) -> bool {
        let tile = (x, y);
        (tile == self.start || !self.is_runtime_blocked(tile)) && self.inner.is_walkable(x, y)
    }

    fn blocks_sight(&self, x: i32, y: i32) -> bool {
        let tile = (x, y);
        self.is_runtime_blocked(tile) || self.inner.blocks_sight(x, y)
    }

    fn can_step(&self, from: (i32, i32), to: (i32, i32)) -> bool {
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        if dx == 0 && dy == 0 {
            return self.is_walkable(from.0, from.1);
        }
        if dx.abs() > 1 || dy.abs() > 1 || !self.is_walkable(to.0, to.1) {
            return false;
        }
        if !self.inner.can_step(from, to) {
            return false;
        }
        if self.step_has_entity_wall_squeeze(from, to)
            || self.step_has_diagonal_wall_corner_squeeze(from, to)
        {
            return false;
        }
        if dx != 0 && dy != 0 {
            let side_x = (from.0 + dx, from.1);
            let side_y = (from.0, from.1 + dy);
            return self.is_walkable(side_x.0, side_x.1) && self.is_walkable(side_y.0, side_y.1);
        }
        true
    }

    fn movement_penalty(&self, _from: (i32, i32), to: (i32, i32)) -> u32 {
        self.wall_clearance_penalty(to) + self.explore_memory_penalty(to)
    }
}

/// ?Ｙ揣?格?頝(tile)??viewport 蝝?30 ????20 ?潛策 A* ??蝯西雲??踹?蝺票?楠??
pub const EXPLORE_RANGE_TILES: i32 = 20;
const EXPLORE_FALLBACK_RADII: [i32; 8] = [EXPLORE_RANGE_TILES, 16, 12, 8, 5, 3, 2, 1];

/// 瘝芣???A* ??8 ???銝韏?20 ????蝯?V4 Exploring state ?曉?goal??
///
/// Phase 5(2026-05-18)??explorer:
/// - 8 cardinals(N/NE/E/SE/S/SW/W/NW)??銝??20 ?潮???goal tile
/// - ??`memory.recent_positions.len() % 8` ?嗉絲憪?offset ??頝?tick ?芰頛芣??孵?,
///   ?踹??偶??閰?N ????????? NE ??銋? ??靘??舐?車 corner case
/// - 蝚砌???A* ????餈? ??銝?閰行?頛?path ?釭,蝪∪撠望憟?
///
/// ??grid(map_id=0 / ?芾???????None??8 ???A* ?典仃???游?鋡怠)????None,
/// step ?撠梁雁??Idle 銝? ??瘥′?曆?摮??path 摰??
#[cfg(test)]
pub fn build_explore_suggestion<G: Walkable>(
    player: (i32, i32),
    grid: Option<&G>,
    memory: &TacticalMemory,
    now: Instant,
) -> Option<ExploreSuggestion> {
    build_explore_suggestion_for_map(player, None, grid, memory, now)
}

#[cfg(test)]
pub fn build_explore_suggestion_for_map<G: Walkable>(
    player: (i32, i32),
    map_id: Option<u32>,
    grid: Option<&G>,
    memory: &TacticalMemory,
    now: Instant,
) -> Option<ExploreSuggestion> {
    build_explore_suggestion_with_profile(player, map_id, grid, None, memory, now)
}

pub fn build_explore_suggestion_with_profile<G: Walkable>(
    player: (i32, i32),
    map_id: Option<u32>,
    grid: Option<&G>,
    profile: Option<&NavProfile>,
    memory: &TacticalMemory,
    now: Instant,
) -> Option<ExploreSuggestion> {
    let grid = grid?;
    let occupied_tiles = HashSet::new();
    let grid = ObstacleOverlay {
        inner: grid,
        memory,
        occupied_tiles: &occupied_tiles,
        now,
        start: player,
        explore_map_id: map_id,
        previous_position: last_distinct_recent_position(memory, player),
    };
    if let Some(suggestion) = profile_explore_suggestion(player, map_id, profile, &grid, memory) {
        return Some(suggestion);
    }
    let goals = explore_goal_directions(player);
    let offset = explore_direction_offset(memory, goals.len());
    if let Some(suggestion) =
        best_directional_explore_suggestion(player, &goals, offset, &grid, memory, map_id)
    {
        return Some(suggestion);
    }

    for radius in EXPLORE_FALLBACK_RADII {
        let goals: Vec<(i32, i32)> = explore_ring_goals(player, radius)
            .into_iter()
            .filter(|goal| !memory.is_explore_direction_failed(player, *goal, now))
            .collect();
        if goals.is_empty() {
            continue;
        }
        let Some(path) = plan_to_any(player, &goals, &grid).filter(|p| !p.is_empty()) else {
            continue;
        };
        let goal = path.last().copied().unwrap_or(player);
        return Some(ExploreSuggestion { goal, path });
    }
    None
}

fn profile_explore_suggestion<G: Walkable>(
    player: (i32, i32),
    map_id: Option<u32>,
    profile: Option<&NavProfile>,
    grid: &ObstacleOverlay<'_, G>,
    memory: &TacticalMemory,
) -> Option<ExploreSuggestion> {
    let map_id = map_id?;
    let profile = profile?;
    let component_id = profile.component_id(player)?;
    let tiles = profile.component_tiles(component_id)?;
    let previous_position = last_distinct_recent_position(memory, player);
    let mut scored_goals = Vec::new();

    for &tile in tiles {
        if tile == player || !grid.is_walkable(tile.0, tile.1) {
            continue;
        }
        if memory.is_explore_direction_failed(player, tile, grid.now) {
            continue;
        }
        let distance = tile.0.abs_diff(player.0).max(tile.1.abs_diff(player.1));
        if distance == 0 {
            continue;
        }
        let visit_score = u64::from(memory.visit_count(map_id, tile)) * 1_000_000;
        let backtrack_score = if previous_position == Some(tile) {
            500_000
        } else {
            0
        };
        let search_radius_score = u64::from(distance.abs_diff(EXPLORE_RANGE_TILES as u32));
        let score = visit_score + backtrack_score + search_radius_score;
        scored_goals.push((score, tile));
    }

    scored_goals.sort_by_key(|&(score, (x, y))| (score, y, x));
    let mut start = 0;
    while start < scored_goals.len() {
        let score = scored_goals[start].0;
        let end = scored_goals[start..]
            .iter()
            .position(|&(next_score, _)| next_score != score)
            .map(|offset| start + offset)
            .unwrap_or(scored_goals.len());
        for chunk in scored_goals[start..end].chunks(PROFILE_EXPLORE_GOAL_LIMIT) {
            let goals: Vec<(i32, i32)> = chunk.iter().map(|&(_, tile)| tile).collect();
            let Some(path) = plan_to_any(player, &goals, grid).filter(|p| !p.is_empty()) else {
                continue;
            };
            let goal = path.last().copied().unwrap_or(player);
            return Some(ExploreSuggestion { goal, path });
        }
        start = end;
    }

    None
}

fn best_directional_explore_suggestion<G: Walkable>(
    player: (i32, i32),
    goals: &[(i32, i32)],
    offset: usize,
    grid: &ObstacleOverlay<'_, G>,
    memory: &TacticalMemory,
    map_id: Option<u32>,
) -> Option<ExploreSuggestion> {
    let mut best: Option<(u64, usize, ExploreSuggestion)> = None;
    for order in 0..goals.len() {
        let idx = (order + offset) % goals.len();
        let goal = goals[idx];
        if memory.is_explore_direction_failed(player, goal, grid.now) {
            continue;
        }
        let Some(path) = plan_to_any(player, &[goal], grid).filter(|p| !p.is_empty()) else {
            continue;
        };
        let score = explore_path_score(&path, memory, map_id);
        let suggestion = ExploreSuggestion { goal, path };
        if best
            .as_ref()
            .is_none_or(|(best_score, best_order, _)| (score, order) < (*best_score, *best_order))
        {
            best = Some((score, order, suggestion));
        }
    }
    best.map(|(_, _, suggestion)| suggestion)
}

fn explore_path_score(path: &[(i32, i32)], memory: &TacticalMemory, map_id: Option<u32>) -> u64 {
    let visit_score = map_id
        .map(|map_id| {
            path.iter()
                .map(|tile| u64::from(memory.visit_count(map_id, *tile)))
                .sum::<u64>()
        })
        .unwrap_or(0);
    visit_score * 10_000 + path.len() as u64
}

fn explore_direction_offset(memory: &TacticalMemory, direction_count: usize) -> usize {
    if direction_count == 0 {
        return 0;
    }
    if memory.recent_positions.len() < crate::bot::hunt4::memory::POSITION_HISTORY_CAP {
        return memory.recent_positions.len() % direction_count;
    }
    let seed = memory
        .recent_positions
        .iter()
        .fold(0usize, |acc, (x, y, _)| {
            acc.wrapping_mul(31)
                .wrapping_add((*x as usize).wrapping_mul(17))
                .wrapping_add((*y as usize).wrapping_mul(13))
        });
    seed % direction_count
}

fn last_distinct_recent_position(
    memory: &TacticalMemory,
    player: (i32, i32),
) -> Option<(i32, i32)> {
    memory
        .recent_positions
        .iter()
        .rev()
        .map(|(x, y, _)| (*x, *y))
        .find(|pos| *pos != player)
}

/// 8 ???蝞???`EXPLORE_RANGE_TILES` ???格? tile???? N ??NE ??E ??... ??NW??
pub fn explore_goal_directions(player: (i32, i32)) -> [(i32, i32); 8] {
    let d = EXPLORE_RANGE_TILES;
    [
        (player.0, player.1 - d),
        (player.0 + d, player.1 - d),
        (player.0 + d, player.1),
        (player.0 + d, player.1 + d),
        (player.0, player.1 + d),
        (player.0 - d, player.1 + d),
        (player.0 - d, player.1),
        (player.0 - d, player.1 - d),
    ]
}

fn explore_ring_goals(player: (i32, i32), radius: i32) -> Vec<(i32, i32)> {
    let mut goals = Vec::with_capacity((radius as usize).saturating_mul(8));
    for dx in -radius..=radius {
        goals.push((player.0 + dx, player.1 - radius));
        goals.push((player.0 + dx, player.1 + radius));
    }
    for dy in -radius + 1..radius {
        goals.push((player.0 - radius, player.1 + dy));
        goals.push((player.0 + radius, player.1 + dy));
    }
    goals
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::hunt4::memory::{
        ExploreDirectionKey, FailureCause, FailureRecord, MemoryUpdate, FAILED_TARGET_TTL,
    };
    use crate::bot::hunt4::model::EntityView;
    use crate::bot::perception::classifier::EntityClass;
    use std::collections::HashSet;

    fn live_mob(target_id: u32, tile: (i32, i32)) -> EntityView {
        EntityView {
            target_id,
            entity_ptr: 0xDEAD_0000 + target_id,
            name: format!("mob_{target_id:X}"),
            sprite_id: 1234,
            action_state: 2,
            tile,
            raw_x: (tile.0 as u32) * 2 + 0x8000,
            y: tile.1 as u32,
            class: EntityClass::AttackableMonster,
            visible_confidence: 100,
            hostile_confidence: 100,
        }
    }

    fn non_target_entity(target_id: u32, tile: (i32, i32), class: EntityClass) -> EntityView {
        EntityView {
            target_id,
            entity_ptr: 0xBEEF_0000 + target_id,
            name: format!("entity_{target_id:X}"),
            sprite_id: 66,
            action_state: 0,
            tile,
            raw_x: (tile.0 as u32) * 2 + 0x8000,
            y: tile.1 as u32,
            class,
            visible_confidence: 100,
            hostile_confidence: 0,
        }
    }

    fn hidden_mob(target_id: u32, tile: (i32, i32), action_state: u8) -> EntityView {
        let mut entity = live_mob(target_id, tile);
        entity.action_state = action_state;
        entity.class = EntityClass::NonWorldMonsterState;
        entity.hostile_confidence = 0;
        entity
    }

    /// ??walkable ?葫閰衣雯??
    struct OpenGrid;
    impl Walkable for OpenGrid {
        fn is_walkable(&self, _x: i32, _y: i32) -> bool {
            true
        }
    }

    /// ??blocked ?葫閰衣雯????璅⊥?摰嗉◤?啜?
    struct WallGrid;
    impl Walkable for WallGrid {
        fn is_walkable(&self, x: i32, y: i32) -> bool {
            // ?芣? player ?芸楛 tile ?航粥,?嗡??冽?
            x == 100 && y == 100
        }
    }

    struct ShortCorridorGrid;
    impl Walkable for ShortCorridorGrid {
        fn is_walkable(&self, x: i32, y: i32) -> bool {
            y == 100 && (100..=105).contains(&x)
        }
    }

    struct VerticalWallGrid;
    impl Walkable for VerticalWallGrid {
        fn is_walkable(&self, x: i32, _y: i32) -> bool {
            x != 101
        }
    }

    struct EdgeBlockedGrid;
    impl Walkable for EdgeBlockedGrid {
        fn is_walkable(&self, _x: i32, _y: i32) -> bool {
            true
        }

        fn can_step(&self, from: (i32, i32), to: (i32, i32)) -> bool {
            let blocked_edge = (from == (100, 100) && to == (101, 100))
                || (from == (101, 100) && to == (100, 100));
            if blocked_edge {
                return false;
            }

            let dx = to.0 - from.0;
            let dy = to.1 - from.1;
            if dx == 0 && dy == 0 {
                return true;
            }
            if dx.abs() > 1 || dy.abs() > 1 {
                return false;
            }
            if dx != 0 && dy != 0 {
                return self.can_step(from, (from.0 + dx, from.1))
                    && self.can_step((from.0 + dx, from.1), to)
                    && self.can_step(from, (from.0, from.1 + dy))
                    && self.can_step((from.0, from.1 + dy), to);
            }
            true
        }
    }

    struct DiagonalCornerSqueezeGrid;
    impl Walkable for DiagonalCornerSqueezeGrid {
        fn is_walkable(&self, x: i32, y: i32) -> bool {
            !matches!((x, y), (100, 99) | (101, 101))
        }
    }

    struct WallHugDetourGrid;
    impl Walkable for WallHugDetourGrid {
        fn is_walkable(&self, x: i32, y: i32) -> bool {
            let in_bounds = (100..=110).contains(&x) && (100..=104).contains(&y);
            let wall_above_shortcut = y == 101 && (101..=109).contains(&x);
            in_bounds && !wall_above_shortcut
        }
    }

    #[test]
    fn build_candidates_skips_failed_targets() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (101, 100)), live_mob(0x2000, (102, 100))],
        };
        let now = Instant::now();
        let mut memory = TacticalMemory::default();
        memory.apply(
            MemoryUpdate {
                add_failed_targets: vec![(
                    0x1000,
                    FailureRecord {
                        cause: FailureCause::AttackRejected,
                        until: now + FAILED_TARGET_TTL,
                    },
                )],
                ..Default::default()
            },
            now,
        );
        let candidates =
            build_candidates::<OpenGrid>(&snap, (100, 100), 1, None, &memory, &HashSet::new(), now);
        assert_eq!(candidates.len(), 1, "failed target should be filtered");
        assert_eq!(candidates[0].target_id, 0x2000);
    }

    #[test]
    fn build_candidates_keeps_unreachable_failed_target_when_it_is_recent_attacker() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (101, 100)), live_mob(0x2000, (102, 100))],
        };
        let now = Instant::now();
        let mut memory = TacticalMemory::default();
        memory.apply(
            MemoryUpdate {
                add_failed_targets: vec![(
                    0x1000,
                    FailureRecord {
                        cause: FailureCause::Unreachable,
                        until: now + FAILED_TARGET_TTL,
                    },
                )],
                ..Default::default()
            },
            now,
        );
        let recent_attackers = HashSet::from([0x1000]);

        let candidates = build_candidates::<OpenGrid>(
            &snap,
            (100, 100),
            1,
            None,
            &memory,
            &recent_attackers,
            now,
        );

        assert!(
            candidates
                .iter()
                .any(|cand| cand.target_id == 0x1000 && cand.is_attacker),
            "recent attacker should still bypass an unreachable-only failure"
        );
    }

    #[test]
    fn build_candidates_skips_attack_rejected_target_even_when_it_is_recent_attacker() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (101, 100)), live_mob(0x2000, (102, 100))],
        };
        let now = Instant::now();
        let mut memory = TacticalMemory::default();
        memory.apply(
            MemoryUpdate {
                add_failed_targets: vec![(
                    0x1000,
                    FailureRecord {
                        cause: FailureCause::AttackRejected,
                        until: now + FAILED_TARGET_TTL,
                    },
                )],
                ..Default::default()
            },
            now,
        );
        let recent_attackers = HashSet::from([0x1000]);

        let candidates = build_candidates::<OpenGrid>(
            &snap,
            (100, 100),
            1,
            None,
            &memory,
            &recent_attackers,
            now,
        );

        assert!(
            candidates.iter().all(|cand| cand.target_id != 0x1000),
            "attack lookup / AttackRejected target must not be reacquired immediately"
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].target_id, 0x2000);
    }

    #[test]
    fn build_candidates_marks_in_attack_range_for_close_mob() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (101, 100))],
        };
        let memory = TacticalMemory::default();
        let cands = build_candidates::<OpenGrid>(
            &snap,
            (100, 100),
            1,
            None,
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        assert_eq!(cands.len(), 1);
        assert!(cands[0].in_attack_range);
        assert_eq!(cands[0].reachable_path, Some(Vec::new()));
        assert_eq!(cands[0].distance, 1);
    }

    #[test]
    fn build_candidates_keeps_adjacent_target_attackable_with_map() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (101, 100))],
        };
        let memory = TacticalMemory::default();
        let grid = OpenGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        assert!(cands[0].in_attack_range);
        assert_eq!(cands[0].reachable_path, Some(Vec::new()));
    }

    #[test]
    fn build_candidates_marks_out_of_range_for_far_mob() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (110, 100))],
        };
        let memory = TacticalMemory::default();
        let cands = build_candidates::<OpenGrid>(
            &snap,
            (100, 100),
            1,
            None,
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        assert!(!cands[0].in_attack_range);
        assert_eq!(cands[0].distance, 10);
    }

    #[test]
    fn build_candidates_uses_optimistic_attack_tile_when_no_map() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (110, 100))],
        };
        let memory = TacticalMemory::default();
        let cands = build_candidates::<OpenGrid>(
            &snap,
            (100, 100),
            1,
            None,
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        // ??map ??璅?韏啣?餅?雿?銝?韏啣?芰?祈澈 tile??
        assert_eq!(cands[0].reachable_path, Some(vec![(109, 100)]));
    }

    #[test]
    fn build_candidates_uses_diagonal_optimistic_attack_tile_when_no_map() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1001, (106, 104))],
        };
        let memory = TacticalMemory::default();
        let cands = build_candidates::<OpenGrid>(
            &snap,
            (100, 100),
            2,
            None,
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        assert_eq!(cands[0].reachable_path, Some(vec![(104, 102)]));
        assert_ne!(cands[0].reachable_path, Some(vec![(106, 104)]));
    }

    #[test]
    fn build_candidates_uses_astar_when_map_provided_and_open() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (103, 100))],
        };
        let memory = TacticalMemory::default();
        let grid = OpenGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        let path = cands[0].reachable_path.as_ref().expect("path exists");
        assert!(!path.is_empty(), "out of range ??A* gives non-empty path");
        // 蝯??臬?餅? target ??雿?銝?芰?祈澈??tile
        assert_eq!(path.last(), Some(&(102, 100)));
    }

    #[test]
    fn build_candidates_avoids_recent_obstacle_tiles() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (103, 100))],
        };
        let now = Instant::now();
        let mut memory = TacticalMemory::default();
        memory.apply(
            MemoryUpdate {
                add_obstacles: vec![((101, 100), now)],
                ..Default::default()
            },
            now,
        );
        let grid = OpenGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            now,
        );
        let path = cands[0].reachable_path.as_ref().expect("path exists");
        assert_ne!(
            path.first(),
            Some(&(101, 100)),
            "planner should not immediately re-use the tile that just failed"
        );
    }

    #[test]
    fn build_candidates_avoids_live_entity_tiles_when_approaching() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (103, 100)), live_mob(0x2000, (101, 100))],
        };
        let memory = TacticalMemory::default();
        let grid = OpenGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        let target = cands
            .iter()
            .find(|c| c.target_id == 0x1000)
            .expect("target should exist");
        let path = target.reachable_path.as_ref().expect("path exists");
        assert_ne!(
            path.first(),
            Some(&(101, 100)),
            "approach path should not step into a tile already occupied by another live entity"
        );
    }

    #[test]
    fn build_candidates_avoids_non_target_character_collision_tile() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                live_mob(0x1000, (103, 100)),
                non_target_entity(0x20, (101, 100), EntityClass::LocalOrInvalid),
            ],
        };
        let memory = TacticalMemory::default();
        let grid = OpenGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );

        assert_eq!(
            cands.iter().filter(|c| c.target_id == 0x20).count(),
            0,
            "non-target character should block movement but must not become a target candidate"
        );
        let target = cands
            .iter()
            .find(|c| c.target_id == 0x1000)
            .expect("target should exist");
        let path = target.reachable_path.as_ref().expect("path exists");
        assert_ne!(
            path.first(),
            Some(&(101, 100)),
            "approach path should not step into a non-target character collision tile"
        );
    }

    #[test]
    fn build_candidates_ignores_shadow_entity_for_collision() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                live_mob(0x1000, (103, 100)),
                non_target_entity(0x30, (101, 100), EntityClass::DecorationOrShadow),
            ],
        };
        let memory = TacticalMemory::default();
        let grid = OpenGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        let target = cands
            .iter()
            .find(|c| c.target_id == 0x1000)
            .expect("target should exist");
        let path = target.reachable_path.as_ref().expect("path exists");
        assert_eq!(
            path.first(),
            Some(&(101, 100)),
            "shadow/decoration entities should not force route detours"
        );
    }

    #[test]
    fn build_candidates_allows_open_route_next_to_entity_without_wall_squeeze() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (103, 100)), live_mob(0x2000, (101, 101))],
        };
        let memory = TacticalMemory::default();
        let grid = OpenGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        let target = cands
            .iter()
            .find(|c| c.target_id == 0x1000)
            .expect("target should exist");
        let path = target.reachable_path.as_ref().expect("path exists");
        assert_eq!(
            path.first(),
            Some(&(101, 100)),
            "open ground should still allow walking next to a live entity"
        );
    }

    #[test]
    fn build_candidates_avoids_wall_entity_squeeze_tile() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (103, 100)), live_mob(0x2000, (101, 101))],
        };
        let now = Instant::now();
        let mut memory = TacticalMemory::default();
        memory.apply(
            MemoryUpdate {
                add_obstacles: vec![((101, 99), now)],
                ..Default::default()
            },
            now,
        );
        let grid = OpenGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            now,
        );
        let target = cands
            .iter()
            .find(|c| c.target_id == 0x1000)
            .expect("target should exist");
        let path = target.reachable_path.as_ref().expect("path exists");
        assert!(
            !path.contains(&(101, 100)),
            "planner should avoid the tile squeezed between wall and live entity, path={path:?}"
        );
    }

    #[test]
    fn build_candidates_avoids_step_between_diagonal_wall_corners() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (104, 100))],
        };
        let memory = TacticalMemory::default();
        let grid = DiagonalCornerSqueezeGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        let path = cands[0].reachable_path.as_ref().expect("path exists");
        assert_ne!(
            path.first(),
            Some(&(101, 100)),
            "planner should not step through an edge squeezed by diagonal wall corners, path={path:?}"
        );
    }

    #[test]
    fn build_candidates_prefers_clearance_detour_over_wall_hugging_shortcut() {
        let snap = Snapshot {
            player: Some((100, 102)),
            entities: vec![live_mob(0x1000, (110, 102))],
        };
        let memory = TacticalMemory::default();
        let grid = WallHugDetourGrid;
        let cands = build_candidates(
            &snap,
            (100, 102),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );

        let path = cands[0].reachable_path.as_ref().expect("path exists");
        assert!(
            !path.contains(&(105, 102)),
            "planner should prefer the open detour instead of hugging the wall line, path={path:?}"
        );
    }

    #[test]
    fn build_candidates_filters_unreachable_when_walled_off() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (110, 110))],
        };
        let memory = TacticalMemory::default();
        let grid = WallGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        assert!(
            cands.is_empty(),
            "??/擃?/?∟楝敺璅??脣撠???candidate list"
        );
    }

    #[test]
    fn build_candidates_rejects_in_range_target_behind_wall() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (102, 100))],
        };
        let memory = TacticalMemory::default();
        let grid = VerticalWallGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            5,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        assert!(
            cands.is_empty(),
            "in-range target behind a wall must be filtered out before target selection"
        );
    }

    #[test]
    fn build_candidates_respects_inner_can_step_edges() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (103, 100))],
        };
        let memory = TacticalMemory::default();
        let grid = EdgeBlockedGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        let path = cands[0].reachable_path.as_ref().expect("path exists");
        assert_ne!(
            path.first(),
            Some(&(101, 100)),
            "navigation overlay must preserve map edge blocks instead of walking into a wall edge"
        );
    }

    #[test]
    fn build_candidates_rejects_adjacent_target_across_blocked_edge() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (101, 100))],
        };
        let memory = TacticalMemory::default();
        let grid = EdgeBlockedGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        assert!(
            cands.is_empty(),
            "adjacent target across a blocked edge must be filtered unless it is a real adjacent attacker"
        );
    }

    #[test]
    fn build_candidates_keeps_adjacent_attacker_across_blocked_edge_for_counterattack() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (101, 100))],
        };
        let memory = TacticalMemory::default();
        let grid = EdgeBlockedGrid;
        let recent_attackers = HashSet::from([0x1000]);
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &recent_attackers,
            Instant::now(),
        );
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].target_id, 0x1000);
        assert!(cands[0].is_attacker);
        assert!(
            cands[0].reachable_path.is_none(),
            "counterattack exception must not pretend the blocked edge is normally reachable"
        );
    }

    #[test]
    fn build_candidates_rejects_far_in_range_target_behind_wall_edge() {
        struct LongVerticalEdgeWallGrid;

        impl Walkable for LongVerticalEdgeWallGrid {
            fn is_walkable(&self, _x: i32, _y: i32) -> bool {
                true
            }

            fn can_step(&self, from: (i32, i32), to: (i32, i32)) -> bool {
                let crosses_wall = (from.0 == 119 && to.0 == 120) || (from.0 == 120 && to.0 == 119);
                if crosses_wall {
                    return false;
                }

                let dx = to.0 - from.0;
                let dy = to.1 - from.1;
                if dx == 0 && dy == 0 {
                    return true;
                }
                if dx.abs() > 1 || dy.abs() > 1 {
                    return false;
                }
                if dx != 0 && dy != 0 {
                    return self.can_step(from, (from.0 + dx, from.1))
                        && self.can_step((from.0 + dx, from.1), to)
                        && self.can_step(from, (from.0, from.1 + dy))
                        && self.can_step((from.0, from.1 + dy), to);
                }
                true
            }
        }

        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1000, (120, 100))],
        };
        let memory = TacticalMemory::default();
        let grid = LongVerticalEdgeWallGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            20,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );

        assert!(
            cands.is_empty(),
            "target 20 tiles away behind a wall edge must be filtered before target selection"
        );
    }

    #[test]
    fn build_candidates_rejects_non_world_monster_state_even_when_nearby() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![hidden_mob(0x1000, (101, 100), 0x0B)],
        };
        let memory = TacticalMemory::default();
        let grid = OpenGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );

        assert!(cands.is_empty());
    }

    #[test]
    fn build_candidates_sorts_in_range_first_then_reachable_then_distance() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                live_mob(0x1000, (105, 100)), // far reachable
                live_mob(0x2000, (101, 100)), // close in-range
                live_mob(0x3000, (103, 100)), // mid reachable
            ],
        };
        let memory = TacticalMemory::default();
        let grid = OpenGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            1,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        // ??:in-range(2000)??頛? reachable(3000)??頛? reachable(1000)
        assert_eq!(cands[0].target_id, 0x2000);
        assert_eq!(cands[1].target_id, 0x3000);
        assert_eq!(cands[2].target_id, 0x1000);
    }

    #[test]
    fn sort_candidates_prefers_local_reachable_target_before_shorter_far_route() {
        let mut cands = vec![
            TargetCandidate {
                target_id: 0x1000,
                entity_ptr: 0xDEAD_1000,
                name: "near_detour".to_string(),
                tile: (103, 100),
                distance: 3,
                in_attack_range: false,
                reachable_path: Some(vec![
                    (101, 100),
                    (101, 101),
                    (102, 101),
                    (103, 101),
                    (104, 101),
                ]),
                is_attacker: false,
            },
            TargetCandidate {
                target_id: 0x2000,
                entity_ptr: 0xDEAD_2000,
                name: "far_on_route".to_string(),
                tile: (105, 100),
                distance: 5,
                in_attack_range: false,
                reachable_path: Some(vec![(101, 100), (102, 100)]),
                is_attacker: false,
            },
        ];

        targeting::sort_candidates(&mut cands);

        assert_eq!(
            cands[0].target_id, 0x1000,
            "local radar candidates should be cleared before a farther route-efficient target"
        );
    }

    #[test]
    fn build_candidates_filters_unreachable_even_when_reachable_target_exists() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                live_mob(0x1000, (102, 100)), // ??銝??
                live_mob(0x2000, (99, 100)),  // ??舀??
            ],
        };
        let memory = TacticalMemory::default();
        let grid = VerticalWallGrid;
        let cands = build_candidates(
            &snap,
            (100, 100),
            5,
            Some(&grid),
            &memory,
            &HashSet::new(),
            Instant::now(),
        );
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].target_id, 0x2000);
        assert!(cands[0].reachable_path.is_some());
    }

    // ===== under_attack target weighting(Phase 5 3c)=====

    #[test]
    fn build_candidates_marks_is_attacker_when_id_in_recent_attackers_set() {
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![live_mob(0x1234, (101, 100)), live_mob(0x5678, (102, 100))],
        };
        let memory = TacticalMemory::default();
        let mut attackers = HashSet::new();
        attackers.insert(0x1234);
        let cands = build_candidates::<OpenGrid>(
            &snap,
            (100, 100),
            1,
            None,
            &memory,
            &attackers,
            Instant::now(),
        );
        let mob1 = cands.iter().find(|c| c.target_id == 0x1234).unwrap();
        let mob2 = cands.iter().find(|c| c.target_id == 0x5678).unwrap();
        assert!(
            mob1.is_attacker,
            "0x1234 ??attackers set ????is_attacker=true"
        );
        assert!(!mob2.is_attacker, "0x5678 銝 ??false");
    }

    #[test]
    fn build_candidates_prefers_near_in_range_target_over_far_attacker() {
        // ?拚??in_range + reachable;0x2000 ??attacker 雿??????
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                live_mob(0x1000, (101, 100)), // 1 tile ????attacker
                live_mob(0x2000, (108, 100)), // 8 tile ??attacker
            ],
        };
        let memory = TacticalMemory::default();
        let mut attackers = HashSet::new();
        attackers.insert(0x2000);
        // attack_range=10 ???拚??in_range
        let cands = build_candidates::<OpenGrid>(
            &snap,
            (100, 100),
            10,
            None,
            &memory,
            &attackers,
            Instant::now(),
        );
        assert_eq!(
            cands[0].target_id, 0x1000,
            "near in-range target should be cleared before a far attacker"
        );
        assert_eq!(cands[1].target_id, 0x2000);
    }

    #[test]
    fn build_candidates_prefers_in_range_target_over_far_reachable_attacker() {
        // 鋡急芣????? attacker,?喃蝙???行?銝?祆芸歇?冽???Ｗ??
        let snap = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                live_mob(0x1000, (101, 100)), // in_range, non-attacker
                live_mob(0x2000, (110, 100)), // out_of_range, attacker
            ],
        };
        let memory = TacticalMemory::default();
        let mut attackers = HashSet::new();
        attackers.insert(0x2000);
        let cands = build_candidates::<OpenGrid>(
            &snap,
            (100, 100),
            1,
            None,
            &memory,
            &attackers,
            Instant::now(),
        );
        assert_eq!(
            cands[0].target_id, 0x1000,
            "in-range target should be cleared before chasing a far reachable attacker"
        );
        assert_eq!(cands[1].target_id, 0x2000);
    }

    // ===== build_explore_suggestion(Phase 5 1B 撖虫?)=====

    #[test]
    fn build_explore_suggestion_returns_none_without_grid() {
        let memory = TacticalMemory::default();
        assert!(
            build_explore_suggestion::<OpenGrid>((100, 100), None, &memory, Instant::now())
                .is_none()
        );
    }

    #[test]
    fn build_explore_suggestion_returns_none_when_player_walled_off() {
        // WallGrid ?芣? (100, 100) ?航粥 ??8 ?孵? A* ?典仃??
        let memory = TacticalMemory::default();
        let grid = WallGrid;
        assert!(
            build_explore_suggestion((100, 100), Some(&grid), &memory, Instant::now()).is_none()
        );
    }

    #[test]
    fn build_explore_suggestion_returns_some_with_open_grid() {
        let memory = TacticalMemory::default();
        let grid = OpenGrid;
        let sug = build_explore_suggestion((100, 100), Some(&grid), &memory, Instant::now())
            .expect("open ??Some");
        // path ?征,銝?敺??潭 EXPLORE_RANGE_TILES ????8 cardinal goal
        assert!(!sug.path.is_empty(), "open grid ??non-empty path");
        let goals = explore_goal_directions((100, 100));
        assert!(goals.contains(&sug.goal), "goal 敹???8 cardinals 銋?");
        assert_eq!(sug.path.last(), Some(&sug.goal), "path 蝯? = goal");
    }

    #[test]
    fn build_explore_suggestion_falls_back_to_reachable_corridor_tile_when_far_goals_blocked() {
        let memory = TacticalMemory::default();
        let grid = ShortCorridorGrid;
        let sug = build_explore_suggestion((100, 100), Some(&grid), &memory, Instant::now())
            .expect("short dungeon corridor should still produce an exploration step");

        assert!(
            !sug.path.is_empty(),
            "fallback exploration must keep moving"
        );
        assert_eq!(sug.path.first(), Some(&(101, 100)));
        assert_eq!(sug.path.last(), Some(&(105, 100)));
        assert_eq!(sug.goal, (105, 100));
    }

    #[test]
    fn build_explore_suggestion_does_not_reset_direction_when_position_history_is_full() {
        let now = Instant::now();
        let mut memory = TacticalMemory::default();
        for i in 0..crate::bot::hunt4::memory::POSITION_HISTORY_CAP {
            memory.apply(
                MemoryUpdate {
                    push_position: Some(((100 + i as i32, 100), now)),
                    ..Default::default()
                },
                now,
            );
        }
        let grid = OpenGrid;
        let player = (120, 100);
        let sug = build_explore_suggestion(player, Some(&grid), &memory, now)
            .expect("open grid should always produce an exploration path");
        let goals = explore_goal_directions(player);

        assert_ne!(
            sug.goal, goals[0],
            "full recent-position history must not make exploration restart at the first direction"
        );
    }

    #[test]
    fn build_explore_suggestion_for_map_prefers_unvisited_direction_over_visited_path() {
        let now = Instant::now();
        let mut memory = TacticalMemory::default();
        let map_id = 63;
        for y in 80..100 {
            memory.apply(
                MemoryUpdate {
                    record_visited_tile: Some((map_id, (100, y), now)),
                    ..Default::default()
                },
                now,
            );
        }
        let grid = OpenGrid;
        let player = (100, 100);
        let goals = explore_goal_directions(player);
        let sug = build_explore_suggestion_for_map(player, Some(map_id), Some(&grid), &memory, now)
            .expect("open grid should always produce an exploration path");

        assert_ne!(
            sug.goal, goals[0],
            "map-scoped visited tiles should stop exploration from repeatedly choosing an already walked corridor"
        );
        assert_eq!(
            sug.goal, goals[1],
            "with the first direction visited and all other scores tied, the next direction should win"
        );
    }

    #[test]
    fn build_explore_suggestion_rotates_direction_via_memory_recent_positions_len() {
        // 蝚?0 ??recent_position(offset=0)??閰?N ?孵???
        let memory0 = TacticalMemory::default();
        let grid = OpenGrid;
        let sug0 =
            build_explore_suggestion((100, 100), Some(&grid), &memory0, Instant::now()).unwrap();
        let goals = explore_goal_directions((100, 100));
        assert_eq!(sug0.goal, goals[0], "offset 0 ??蝚?1 ???N)");

        // push 1 ??recent_position ??len=1 ??offset=1 ??閰?NE ?孵???
        let mut memory1 = TacticalMemory::default();
        memory1.apply(
            crate::bot::hunt4::memory::MemoryUpdate {
                push_position: Some(((50, 50), Instant::now())),
                ..Default::default()
            },
            Instant::now(),
        );
        let sug1 =
            build_explore_suggestion((100, 100), Some(&grid), &memory1, Instant::now()).unwrap();
        assert_eq!(sug1.goal, goals[1], "offset 1 ??蝚?2 ???NE)");
    }

    #[test]
    fn build_explore_suggestion_avoids_recent_obstacle_first_step() {
        let now = Instant::now();
        let mut memory = TacticalMemory::default();
        memory.apply(
            MemoryUpdate {
                add_obstacles: vec![((100, 99), now)],
                ..Default::default()
            },
            now,
        );
        let grid = OpenGrid;
        let sug = build_explore_suggestion((100, 100), Some(&grid), &memory, now)
            .expect("open grid can route around one obstacle");

        assert_ne!(
            sug.path.first(),
            Some(&(100, 99)),
            "exploration must not retry the same just-failed wall/turn tile"
        );
    }

    #[test]
    fn build_explore_suggestion_avoids_recent_failed_direction() {
        let now = Instant::now();
        let player = (100, 100);
        let goals = explore_goal_directions(player);
        let mut memory = TacticalMemory::default();
        memory.apply(
            MemoryUpdate {
                add_failed_explore_directions: vec![(
                    ExploreDirectionKey::from_goal(player, goals[0]).unwrap(),
                    now,
                )],
                ..Default::default()
            },
            now,
        );
        let grid = OpenGrid;
        let sug = build_explore_suggestion(player, Some(&grid), &memory, now)
            .expect("open grid can choose another direction");

        assert_ne!(
            sug.goal, goals[0],
            "exploration must not immediately retry a just-stalled direction"
        );
        assert_eq!(
            sug.goal, goals[1],
            "with north temporarily failed and all other scores tied, northeast should win"
        );
    }

    #[test]
    fn profile_explore_prefers_search_frontier_over_adjacent_tile() {
        use crate::bot::decide::pathfind::MapWalkable;
        use crate::minimap::coord::BLOCK_ORIGIN;
        use crate::minimap::map_loader::{Bounds, Map};
        use crate::minimap::nav_profile::NavProfile;
        use crate::minimap::s32_parser::Block;
        use std::collections::HashMap;

        let mut walkable = Box::new([[false; 64]; 64]);
        for x in 0..=30 {
            walkable[0][x] = true;
        }
        let blocks = HashMap::from([((0, 0), Block::from_walkable(walkable))]);
        let bounds = Bounds {
            min_block_x: 0,
            max_block_x: 0,
            min_block_y: 0,
            max_block_y: 0,
        };
        let nav = crate::minimap::nav_grid::NavGrid::from_blocks(&blocks);
        let profile = NavProfile::from_blocks(&nav, &blocks);
        let map = Map {
            map_id: 7,
            nav,
            profile,
            blocks,
            bounds,
        };
        let grid = MapWalkable { map: &map };
        let now = Instant::now();
        let player = (BLOCK_ORIGIN, BLOCK_ORIGIN);
        let memory = TacticalMemory::default();

        let sug = build_explore_suggestion_with_profile(
            player,
            Some(7),
            Some(&grid),
            Some(&map.profile),
            &memory,
            now,
        )
        .expect("profile corridor should produce exploration");

        assert!(
            sug.goal.0 - player.0 >= 18,
            "profile exploration should search outward instead of circling adjacent tiles; got {:?}",
            sug.goal
        );
        assert_eq!(sug.path.first(), Some(&(player.0 + 1, player.1)));
    }

    #[test]
    fn explore_goal_directions_layout_n_ne_e_se_s_sw_w_nw_at_20_tiles() {
        let goals = explore_goal_directions((100, 100));
        assert_eq!(goals[0], (100, 80), "N");
        assert_eq!(goals[1], (120, 80), "NE");
        assert_eq!(goals[2], (120, 100), "E");
        assert_eq!(goals[3], (120, 120), "SE");
        assert_eq!(goals[4], (100, 120), "S");
        assert_eq!(goals[5], (80, 120), "SW");
        assert_eq!(goals[6], (80, 100), "W");
        assert_eq!(goals[7], (80, 80), "NW");
    }

    #[test]
    fn profile_explore_stays_inside_current_connected_component() {
        use crate::bot::decide::pathfind::MapWalkable;
        use crate::minimap::coord::BLOCK_ORIGIN;
        use crate::minimap::map_loader::{Bounds, Map};
        use crate::minimap::nav_profile::NavProfile;
        use crate::minimap::s32_parser::Block;
        use std::collections::HashMap;

        let mut walkable = Box::new([[false; 64]; 64]);
        for x in 0..=3 {
            walkable[0][x] = true;
        }
        for x in 10..=13 {
            walkable[0][x] = true;
        }
        let blocks = HashMap::from([((0, 0), Block::from_walkable(walkable))]);
        let bounds = Bounds {
            min_block_x: 0,
            max_block_x: 0,
            min_block_y: 0,
            max_block_y: 0,
        };
        let nav = crate::minimap::nav_grid::NavGrid::from_blocks(&blocks);
        let profile = NavProfile::from_blocks(&nav, &blocks);
        let map = Map {
            map_id: 7,
            nav,
            profile,
            blocks,
            bounds,
        };
        let grid = MapWalkable { map: &map };
        let now = Instant::now();
        let map_id = 7;
        let player = (BLOCK_ORIGIN, BLOCK_ORIGIN);
        let mut memory = TacticalMemory::default();
        for x in 1..=3 {
            memory.apply(
                MemoryUpdate {
                    record_visited_tile: Some((map_id, (BLOCK_ORIGIN + x, BLOCK_ORIGIN), now)),
                    ..Default::default()
                },
                now,
            );
        }

        let sug = build_explore_suggestion_with_profile(
            player,
            Some(map_id),
            Some(&grid),
            Some(&map.profile),
            &memory,
            now,
        )
        .expect("current component still has reachable exploration tiles");

        assert!(
            map.profile.same_component(player, sug.goal),
            "profile exploration must not choose a disconnected map component"
        );
        assert_eq!(sug.path.last(), Some(&sug.goal));
    }
}
