//! Tactical memory — 戰術記憶,獨立於 `HuntState` 之外。
//!
//! 跟舊 `HuntRuntimeState` 把 `failed_targets` / `runtime_blocks` / `last_walk` 等等
//! 全部塞在一起不同,這裡只放「跨 tick 的衰減資料」(有 TTL、可被 `step()` 純讀)。
//!
//! `step()` 不直接寫 memory;它在 `TickOutput.memory_updates` 裡描述要怎麼改,
//! runtime 在 dispatch action 後才 apply。

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// 失敗目標 TTL — 進 failed_targets 後降權的時間
pub const FAILED_TARGET_TTL: Duration = Duration::from_secs(8);
pub const REPEATED_FAILED_TARGET_TTL: Duration = Duration::from_secs(16);

/// 障礙物 TTL — 走路失敗的 tile 多久重新嘗試
pub const OBSTACLE_TTL: Duration = Duration::from_secs(6);

/// Short memory for exploration directions that just failed near the current position.
pub const FAILED_EXPLORE_DIRECTION_TTL: Duration = Duration::from_secs(8);
pub const FAILED_EXPLORE_ORIGIN_RADIUS_TILES: u32 = 3;

pub const PORTAL_AVOID_TTL: Duration = Duration::from_secs(10 * 60);

/// 位置歷史保留長度 — stall 偵測用
/// Number of recent positions retained for stall detection.
pub const POSITION_HISTORY_CAP: usize = 8;

/// 戰術記憶 — 跨 tick 衰減資料的容器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExploreDirectionKey {
    pub origin: (i32, i32),
    pub direction: (i8, i8),
}

impl ExploreDirectionKey {
    pub fn from_goal(origin: (i32, i32), goal: (i32, i32)) -> Option<Self> {
        let direction = explore_direction(origin, goal)?;
        Some(Self { origin, direction })
    }
}

/// Runtime tactical memory shared by the V4 planner and stepper.
#[derive(Debug, Clone, Default)]
pub struct TacticalMemory {
    /// 走路失敗 → tile 暫時不可走
    /// Temporary movement blockers observed while walking.
    pub obstacles: HashMap<(i32, i32), Instant>,
    pub portal_avoid_tiles: HashMap<(i32, i32), Instant>,
    /// Exploration directions that recently failed by stalling near an origin tile.
    pub failed_explore_directions: HashMap<ExploreDirectionKey, Instant>,

    /// 攻擊/路徑失敗 → 目標 8s 內降權
    /// Temporarily skipped targets with expiry and cause.
    pub failed_targets: HashMap<u32, FailureRecord>,

    /// stall 偵測用 — 最近 N 個位置 sample
    pub recent_positions: VecDeque<(i32, i32, Instant)>,
    /// Per-map visited tile memory used by dungeon exploration scoring.
    pub visited_tiles: HashMap<(u32, i32, i32), VisitRecord>,

    /// 各種 last_*  timestamp(idle teleport 觸發 / CD 計算用)
    pub last_walk: Option<Instant>,
    pub last_attack: Option<Instant>,
    pub last_skill_cast: Option<Instant>,
    /// Target that must receive one basic attack before the next skill cast.
    pub post_skill_basic_pending_target: Option<u32>,
    pub last_position_change: Option<Instant>,
    pub last_teleport: Option<Instant>,
    /// First tick at which planner had no actionable target.  Unlike `state_since`, this survives
    /// Idle <-> Exploring churn so empty/blocked areas can still use the idle-teleport timer.
    pub no_actionable_target_since: Option<Instant>,
    /// Consecutive exploration walk requests made without seeing an actionable target.
    pub empty_explore_walks: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisitRecord {
    pub count: u16,
    pub last_seen: Instant,
}

/// 失敗目標紀錄 — 進 failed_targets 帶的 metadata
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureRecord {
    pub cause: FailureCause,
    pub until: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCause {
    /// A* 算不出路徑或路徑全 blocked
    Unreachable,
    /// bootstrap_click_attack / cast packet 失敗
    AttackRejected,
}

impl TacticalMemory {
    /// 玩家最近 `window` 個 position sample 都同一格 — watchdog 用來偵測「我在走路 state
    /// 但實際沒動」。 200ms tick × window=8(`POSITION_HISTORY_CAP`)→ 1.6s stall。
    ///
    /// 規則:
    /// - `recent_positions.len() < window` → 還沒累積夠 sample,回 false(不誤判)
    /// - `window < 2` → 1 個 sample 不能算 stall,回 false
    /// - 否則取最後 `window` 個 sample,全部 (x, y) 相同 → true
    #[cfg(test)]
    pub fn is_stalled(&self, window: usize) -> bool {
        if window < 2 || self.recent_positions.len() < window {
            return false;
        }
        let tail = self.recent_positions.iter().rev().take(window);
        let mut anchor: Option<(i32, i32)> = None;
        for &(x, y, _) in tail {
            match anchor {
                None => anchor = Some((x, y)),
                Some((ax, ay)) if ax == x && ay == y => continue,
                _ => return false,
            }
        }
        true
    }

    pub fn is_stalled_since(&self, window: usize, since: Option<Instant>) -> bool {
        let Some(since) = since else {
            return false;
        };
        if window < 2 {
            return false;
        }
        let samples: Vec<_> = self
            .recent_positions
            .iter()
            .rev()
            .filter(|(_, _, at)| *at >= since)
            .take(window)
            .collect();
        if samples.len() < window {
            return false;
        }
        let Some(first) = samples.first() else {
            return false;
        };
        let (anchor_x, anchor_y, _) = **first;
        samples
            .iter()
            .all(|&&(x, y, _)| x == anchor_x && y == anchor_y)
    }
}

/// `step()` 輸出給 memory 的 delta — runtime 在 dispatch action 後 apply。
///
/// 用 add/clear 而非「直接給新 memory」是為了避免 step() 需要 clone 整個 memory
/// (每 tick 都做的話有 GC 壓力)。
/// Delta emitted by step() and applied after each tick.
#[derive(Debug, Clone, Default)]
pub struct MemoryUpdate {
    /// 要加入 obstacles 的 tile + 觸發時間
    pub add_obstacles: Vec<((i32, i32), Instant)>,
    pub add_portal_avoid_tiles: Vec<((i32, i32), Instant)>,
    pub add_failed_explore_directions: Vec<(ExploreDirectionKey, Instant)>,
    /// 要加入 failed_targets 的目標
    /// Failed target inserts.
    pub add_failed_targets: Vec<(u32, FailureRecord)>,
    /// 要記錄的玩家位置 sample
    pub push_position: Option<((i32, i32), Instant)>,
    /// Drop stale position samples after a confirmed walk-stuck recovery.
    pub clear_recent_positions: bool,
    /// Record a map-scoped tile visit for exploration scoring.
    pub record_visited_tile: Option<(u32, (i32, i32), Instant)>,
    /// 設定 last_walk(成功走路後)
    pub set_last_walk: Option<Instant>,
    /// 設定 last_attack(成功攻擊後)
    pub set_last_attack: Option<Instant>,
    /// 設定 last_skill_cast(成功放技能後)
    pub set_last_skill_cast: Option<Instant>,
    /// Mark target for one required basic attack after a skill request.
    pub set_post_skill_basic_pending: Option<u32>,
    /// Clear the required post-skill basic attack marker.
    pub clear_post_skill_basic_pending: bool,
    /// 設定 last_position_change(玩家位置改變後)
    pub set_last_position_change: Option<Instant>,
    /// 設定 last_teleport(用過卷後)
    pub set_last_teleport: Option<Instant>,
    /// Start/keep the no-actionable-target timer.
    pub set_no_actionable_target_since: Option<Instant>,
    /// Clear the no-actionable-target timer once a real/actionable target is selected.
    pub clear_no_actionable_target_since: bool,
    /// Count one exploration walk made while no target is actionable.
    pub increment_empty_explore_walks: bool,
    /// Reset empty exploration budget after target acquisition or teleport.
    pub clear_empty_explore_walks: bool,
}

impl TacticalMemory {
    /// 套用 `step()` 給的 delta,並順便清掉過期的 obstacles / failed_targets。
    pub fn apply(&mut self, update: MemoryUpdate, now: Instant) {
        for (tile, at) in update.add_obstacles {
            self.obstacles.insert(tile, at);
        }
        for (tile, at) in update.add_portal_avoid_tiles {
            self.portal_avoid_tiles.insert(tile, at);
        }
        for (key, at) in update.add_failed_explore_directions {
            self.failed_explore_directions.insert(key, at);
        }
        for (id, mut rec) in update.add_failed_targets {
            if self
                .failed_targets
                .get(&id)
                .is_some_and(|old| old.until > now)
            {
                rec.until = rec.until.max(now + REPEATED_FAILED_TARGET_TTL);
            }
            self.failed_targets.insert(id, rec);
        }
        if let Some((pos, at)) = update.push_position {
            self.recent_positions.push_back((pos.0, pos.1, at));
            while self.recent_positions.len() > POSITION_HISTORY_CAP {
                self.recent_positions.pop_front();
            }
        }
        if update.clear_recent_positions {
            self.recent_positions.clear();
        }
        if let Some((map_id, pos, at)) = update.record_visited_tile {
            self.visited_tiles
                .entry((map_id, pos.0, pos.1))
                .and_modify(|record| {
                    record.count = record.count.saturating_add(1);
                    record.last_seen = at;
                })
                .or_insert(VisitRecord {
                    count: 1,
                    last_seen: at,
                });
        }
        if let Some(t) = update.set_last_walk {
            self.last_walk = Some(t);
        }
        if let Some(t) = update.set_last_attack {
            self.last_attack = Some(t);
        }
        if let Some(t) = update.set_last_skill_cast {
            self.last_skill_cast = Some(t);
        }
        if let Some(target_id) = update.set_post_skill_basic_pending {
            self.post_skill_basic_pending_target = Some(target_id);
        }
        if update.clear_post_skill_basic_pending {
            self.post_skill_basic_pending_target = None;
        }
        if let Some(t) = update.set_last_position_change {
            self.last_position_change = Some(t);
        }
        if let Some(t) = update.set_last_teleport {
            self.last_teleport = Some(t);
        }
        if update.clear_no_actionable_target_since {
            self.no_actionable_target_since = None;
        }
        if let Some(t) = update.set_no_actionable_target_since {
            self.no_actionable_target_since.get_or_insert(t);
        }
        if update.clear_empty_explore_walks {
            self.empty_explore_walks = 0;
        }
        if update.increment_empty_explore_walks {
            self.empty_explore_walks = self.empty_explore_walks.saturating_add(1);
        }

        self.obstacles
            .retain(|_, at| now.duration_since(*at) < OBSTACLE_TTL);
        self.portal_avoid_tiles
            .retain(|_, at| now.duration_since(*at) < PORTAL_AVOID_TTL);
        self.failed_explore_directions
            .retain(|_, at| now.duration_since(*at) < FAILED_EXPLORE_DIRECTION_TTL);
        self.failed_targets.retain(|_, rec| rec.until > now);
    }

    #[cfg(test)]
    pub fn is_target_failed(&self, target_id: u32, now: Instant) -> bool {
        self.failed_targets
            .get(&target_id)
            .map(|rec| rec.until > now)
            .unwrap_or(false)
    }

    pub fn is_obstacle(&self, tile: (i32, i32), now: Instant) -> bool {
        self.obstacles
            .get(&tile)
            .map(|at| now.duration_since(*at) < OBSTACLE_TTL)
            .unwrap_or(false)
            || self
                .portal_avoid_tiles
                .get(&tile)
                .map(|at| now.duration_since(*at) < PORTAL_AVOID_TTL)
                .unwrap_or(false)
    }

    pub fn visit_count(&self, map_id: u32, tile: (i32, i32)) -> u16 {
        self.visited_tiles
            .get(&(map_id, tile.0, tile.1))
            .map(|record| record.count)
            .unwrap_or(0)
    }

    pub fn is_explore_direction_failed(
        &self,
        origin: (i32, i32),
        goal: (i32, i32),
        now: Instant,
    ) -> bool {
        let Some(direction) = explore_direction(origin, goal) else {
            return false;
        };
        self.failed_explore_directions.iter().any(|(key, at)| {
            key.direction == direction
                && now.duration_since(*at) < FAILED_EXPLORE_DIRECTION_TTL
                && key
                    .origin
                    .0
                    .abs_diff(origin.0)
                    .max(key.origin.1.abs_diff(origin.1))
                    <= FAILED_EXPLORE_ORIGIN_RADIUS_TILES
        })
    }
}

fn explore_direction(origin: (i32, i32), goal: (i32, i32)) -> Option<(i8, i8)> {
    let dx = (goal.0 - origin.0).signum() as i8;
    let dy = (goal.1 - origin.1).signum() as i8;
    (dx != 0 || dy != 0).then_some((dx, dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_inserts_obstacle() {
        let mut memory = TacticalMemory::default();
        let now = Instant::now();
        let update = MemoryUpdate {
            add_obstacles: vec![((10, 20), now)],
            ..Default::default()
        };
        memory.apply(update, now);
        assert!(memory.is_obstacle((10, 20), now));
    }

    #[test]
    fn apply_inserts_failed_target() {
        let mut memory = TacticalMemory::default();
        let now = Instant::now();
        let until = now + FAILED_TARGET_TTL;
        let update = MemoryUpdate {
            add_failed_targets: vec![(
                0x1234,
                FailureRecord {
                    cause: FailureCause::AttackRejected,
                    until,
                },
            )],
            ..Default::default()
        };
        memory.apply(update, now);
        assert!(memory.is_target_failed(0x1234, now));
    }

    #[test]
    fn apply_records_failed_explore_direction_near_origin() {
        let mut memory = TacticalMemory::default();
        let now = Instant::now();
        let key = ExploreDirectionKey::from_goal((100, 100), (100, 80)).unwrap();
        memory.apply(
            MemoryUpdate {
                add_failed_explore_directions: vec![(key, now)],
                ..Default::default()
            },
            now,
        );

        assert!(memory.is_explore_direction_failed((100, 100), (100, 80), now));
        assert!(memory.is_explore_direction_failed((101, 100), (101, 80), now));
        assert!(!memory.is_explore_direction_failed((100, 100), (120, 80), now));
    }

    #[test]
    fn apply_tracks_and_clears_empty_explore_walk_budget() {
        let mut memory = TacticalMemory::default();
        let now = Instant::now();

        memory.apply(
            MemoryUpdate {
                increment_empty_explore_walks: true,
                ..Default::default()
            },
            now,
        );
        memory.apply(
            MemoryUpdate {
                increment_empty_explore_walks: true,
                ..Default::default()
            },
            now + Duration::from_millis(200),
        );

        assert_eq!(memory.empty_explore_walks, 2);

        memory.apply(
            MemoryUpdate {
                clear_empty_explore_walks: true,
                ..Default::default()
            },
            now + Duration::from_millis(400),
        );

        assert_eq!(memory.empty_explore_walks, 0);
    }

    #[test]
    fn stalled_since_ignores_position_samples_before_last_walk() {
        let mut memory = TacticalMemory::default();
        let now = Instant::now();
        let last_walk = now - Duration::from_millis(200);
        for i in 0..POSITION_HISTORY_CAP {
            memory.recent_positions.push_back((
                100,
                100,
                now - Duration::from_millis(2000 + i as u64 * 100),
            ));
        }
        memory
            .recent_positions
            .push_back((100, 100, now - Duration::from_millis(100)));

        assert!(memory.is_stalled(POSITION_HISTORY_CAP));
        assert!(
            !memory.is_stalled_since(POSITION_HISTORY_CAP, Some(last_walk)),
            "startup idle samples must not make the first walk look stuck"
        );
    }

    #[test]
    fn stalled_since_detects_full_stall_window_after_last_walk() {
        let mut memory = TacticalMemory::default();
        let now = Instant::now();
        let last_walk = now - Duration::from_secs(2);
        for i in 0..POSITION_HISTORY_CAP {
            memory.recent_positions.push_back((
                100,
                100,
                last_walk + Duration::from_millis(i as u64 * 200),
            ));
        }

        assert!(memory.is_stalled_since(POSITION_HISTORY_CAP, Some(last_walk)));
    }

    #[test]
    fn apply_records_visited_tiles_per_map() {
        let mut memory = TacticalMemory::default();
        let now = Instant::now();
        memory.apply(
            MemoryUpdate {
                record_visited_tile: Some((63, (32686, 32832), now)),
                ..Default::default()
            },
            now,
        );
        memory.apply(
            MemoryUpdate {
                record_visited_tile: Some((63, (32686, 32832), now + Duration::from_millis(1))),
                ..Default::default()
            },
            now + Duration::from_millis(1),
        );
        memory.apply(
            MemoryUpdate {
                record_visited_tile: Some((248, (32686, 32832), now + Duration::from_millis(2))),
                ..Default::default()
            },
            now + Duration::from_millis(2),
        );

        assert_eq!(memory.visit_count(63, (32686, 32832)), 2);
        assert_eq!(memory.visit_count(248, (32686, 32832)), 1);
    }

    #[test]
    fn apply_clears_recent_positions_when_requested() {
        let mut memory = TacticalMemory::default();
        let now = Instant::now();
        for i in 0..POSITION_HISTORY_CAP {
            memory.apply(
                MemoryUpdate {
                    push_position: Some(((100, 100), now + Duration::from_millis(i as u64))),
                    ..Default::default()
                },
                now + Duration::from_millis(i as u64),
            );
        }
        assert!(memory.is_stalled(POSITION_HISTORY_CAP));

        memory.apply(
            MemoryUpdate {
                clear_recent_positions: true,
                ..Default::default()
            },
            now + Duration::from_secs(1),
        );

        assert!(memory.recent_positions.is_empty());
        assert!(!memory.is_stalled(POSITION_HISTORY_CAP));
    }

    #[test]
    fn is_target_failed_returns_false_after_until() {
        let mut memory = TacticalMemory::default();
        let now = Instant::now();
        let until = now + Duration::from_millis(500);
        memory.apply(
            MemoryUpdate {
                add_failed_targets: vec![(
                    0x5678,
                    FailureRecord {
                        cause: FailureCause::Unreachable,
                        until,
                    },
                )],
                ..Default::default()
            },
            now,
        );
        assert!(memory.is_target_failed(0x5678, now));
        let later = now + Duration::from_secs(1);
        assert!(!memory.is_target_failed(0x5678, later));
    }

    #[test]
    fn apply_expires_old_obstacles_on_subsequent_apply() {
        let mut memory = TacticalMemory::default();
        let t0 = Instant::now();
        memory.apply(
            MemoryUpdate {
                add_obstacles: vec![((1, 1), t0)],
                ..Default::default()
            },
            t0,
        );
        assert!(memory.is_obstacle((1, 1), t0));
        // 10s later — apply 空 update,過期的應被清除
        let t1 = t0 + Duration::from_secs(10);
        memory.apply(MemoryUpdate::default(), t1);
        assert!(!memory.is_obstacle((1, 1), t1));
        assert!(
            memory.obstacles.is_empty(),
            "expired obstacle should be removed"
        );
    }

    #[test]
    fn portal_avoid_tiles_are_treated_as_long_lived_obstacles() {
        let mut memory = TacticalMemory::default();
        let t0 = Instant::now();
        memory.apply(
            MemoryUpdate {
                add_portal_avoid_tiles: vec![((10, 20), t0)],
                ..Default::default()
            },
            t0,
        );

        let after_regular_obstacle_ttl = t0 + OBSTACLE_TTL + Duration::from_secs(1);
        assert!(
            memory.is_obstacle((10, 20), after_regular_obstacle_ttl),
            "learned portal tiles should outlive regular walk-failure obstacles"
        );

        let after_portal_ttl = t0 + PORTAL_AVOID_TTL + Duration::from_secs(1);
        memory.apply(MemoryUpdate::default(), after_portal_ttl);
        assert!(!memory.is_obstacle((10, 20), after_portal_ttl));
        assert!(memory.portal_avoid_tiles.is_empty());
    }

    #[test]
    fn apply_expires_old_failed_targets_on_subsequent_apply() {
        let mut memory = TacticalMemory::default();
        let t0 = Instant::now();
        memory.apply(
            MemoryUpdate {
                add_failed_targets: vec![(
                    0xABCD,
                    FailureRecord {
                        cause: FailureCause::AttackRejected,
                        until: t0 + Duration::from_millis(100),
                    },
                )],
                ..Default::default()
            },
            t0,
        );
        let t1 = t0 + Duration::from_secs(1);
        memory.apply(MemoryUpdate::default(), t1);
        assert!(memory.failed_targets.is_empty());
    }

    #[test]
    fn repeated_failed_target_extends_suppression_window() {
        let mut memory = TacticalMemory::default();
        let t0 = Instant::now();
        memory.apply(
            MemoryUpdate {
                add_failed_targets: vec![(
                    0xABCD,
                    FailureRecord {
                        cause: FailureCause::AttackRejected,
                        until: t0 + FAILED_TARGET_TTL,
                    },
                )],
                ..Default::default()
            },
            t0,
        );

        let t1 = t0 + Duration::from_secs(1);
        memory.apply(
            MemoryUpdate {
                add_failed_targets: vec![(
                    0xABCD,
                    FailureRecord {
                        cause: FailureCause::AttackRejected,
                        until: t1 + FAILED_TARGET_TTL,
                    },
                )],
                ..Default::default()
            },
            t1,
        );

        let record = memory.failed_targets.get(&0xABCD).unwrap();
        assert_eq!(record.cause, FailureCause::AttackRejected);
        assert_eq!(record.until.duration_since(t1), FAILED_TARGET_TTL * 2);
    }

    #[test]
    fn apply_expires_old_failed_explore_directions_on_subsequent_apply() {
        let mut memory = TacticalMemory::default();
        let t0 = Instant::now();
        let key = ExploreDirectionKey::from_goal((100, 100), (100, 80)).unwrap();
        memory.apply(
            MemoryUpdate {
                add_failed_explore_directions: vec![(key, t0)],
                ..Default::default()
            },
            t0,
        );
        let later = t0 + FAILED_EXPLORE_DIRECTION_TTL + Duration::from_secs(1);
        memory.apply(MemoryUpdate::default(), later);

        assert!(memory.failed_explore_directions.is_empty());
        assert!(!memory.is_explore_direction_failed((100, 100), (100, 80), later));
    }

    #[test]
    fn position_history_caps_at_history_cap() {
        let mut memory = TacticalMemory::default();
        let t0 = Instant::now();
        for i in 0..(POSITION_HISTORY_CAP as i32 + 3) {
            memory.apply(
                MemoryUpdate {
                    push_position: Some(((i, i), t0 + Duration::from_millis(i as u64 * 100))),
                    ..Default::default()
                },
                t0 + Duration::from_millis(i as u64 * 100),
            );
        }
        assert_eq!(memory.recent_positions.len(), POSITION_HISTORY_CAP);
        // 最舊的被擠掉了
        let (oldest_x, _, _) = memory.recent_positions.front().unwrap();
        assert!(*oldest_x >= 3, "earliest entries should be evicted");
    }

    // ===== is_stalled(watchdog stall 偵測)=====

    #[test]
    fn is_stalled_returns_false_when_recent_positions_empty() {
        let memory = TacticalMemory::default();
        assert!(!memory.is_stalled(8));
    }

    #[test]
    fn is_stalled_returns_false_when_window_below_two() {
        let mut memory = TacticalMemory::default();
        memory.apply(
            MemoryUpdate {
                push_position: Some(((10, 10), Instant::now())),
                ..Default::default()
            },
            Instant::now(),
        );
        assert!(!memory.is_stalled(0));
        assert!(!memory.is_stalled(1));
    }

    #[test]
    fn is_stalled_returns_false_when_samples_below_window() {
        let mut memory = TacticalMemory::default();
        let t0 = Instant::now();
        for i in 0..3 {
            memory.apply(
                MemoryUpdate {
                    push_position: Some(((10, 10), t0 + Duration::from_millis(i * 100))),
                    ..Default::default()
                },
                t0 + Duration::from_millis(i * 100),
            );
        }
        // 3 個 sample 都同位置,但 window=5 → 不夠樣本 → false
        assert!(!memory.is_stalled(5));
    }

    #[test]
    fn is_stalled_returns_true_when_all_last_window_samples_same_tile() {
        let mut memory = TacticalMemory::default();
        let t0 = Instant::now();
        for i in 0..8 {
            memory.apply(
                MemoryUpdate {
                    push_position: Some(((42, 17), t0 + Duration::from_millis(i * 100))),
                    ..Default::default()
                },
                t0 + Duration::from_millis(i * 100),
            );
        }
        assert!(memory.is_stalled(8));
        assert!(memory.is_stalled(2));
    }

    #[test]
    fn is_stalled_returns_false_when_any_recent_sample_moved() {
        let mut memory = TacticalMemory::default();
        let t0 = Instant::now();
        for i in 0..7 {
            memory.apply(
                MemoryUpdate {
                    push_position: Some(((10, 10), t0 + Duration::from_millis(i * 100))),
                    ..Default::default()
                },
                t0 + Duration::from_millis(i * 100),
            );
        }
        // 第 8 格動了一下
        memory.apply(
            MemoryUpdate {
                push_position: Some(((11, 10), t0 + Duration::from_millis(700))),
                ..Default::default()
            },
            t0 + Duration::from_millis(700),
        );
        // 最後 8 個 sample 不全同 → 不算 stall
        assert!(!memory.is_stalled(8));
    }

    #[test]
    fn apply_sets_last_walk_attack_skill_timestamps() {
        let mut memory = TacticalMemory::default();
        let t0 = Instant::now();
        memory.apply(
            MemoryUpdate {
                set_last_walk: Some(t0),
                set_last_attack: Some(t0),
                set_last_skill_cast: Some(t0),
                set_last_position_change: Some(t0),
                set_last_teleport: Some(t0),
                ..Default::default()
            },
            t0,
        );
        assert_eq!(memory.last_walk, Some(t0));
        assert_eq!(memory.last_attack, Some(t0));
        assert_eq!(memory.last_skill_cast, Some(t0));
        assert_eq!(memory.last_position_change, Some(t0));
        assert_eq!(memory.last_teleport, Some(t0));
    }

    #[test]
    fn apply_sets_and_clears_post_skill_basic_pending_target() {
        let mut memory = TacticalMemory::default();
        let t0 = Instant::now();
        memory.apply(
            MemoryUpdate {
                set_post_skill_basic_pending: Some(0x30008),
                ..Default::default()
            },
            t0,
        );
        assert_eq!(memory.post_skill_basic_pending_target, Some(0x30008));

        memory.apply(
            MemoryUpdate {
                clear_post_skill_basic_pending: true,
                ..Default::default()
            },
            t0,
        );
        assert_eq!(memory.post_skill_basic_pending_target, None);
    }
}
