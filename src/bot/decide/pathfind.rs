//! A* 8-direction pathfinder for bot tile-precise walking.
//!
//! ## 概觀
//!
//! Hunt / 自動走路需要避開牆找路徑。 純 heading-based walk(看 dx/dy 決定 8 方向)走遇到
//! 牆會卡住,A* 預先規劃一條 walkable tile 序列,bot 每 tick 對齊下一個 waypoint 的
//! heading 後送 walk_hold,game engine 自動走一格 → bot 偵測到 → 更新 heading。
//!
//! ## 演算法
//!
//! - 8-direction(N/NE/E/SE/S/SW/W/NW),對齊 `bot::action::walk::heading_from_delta`
//! - Heuristic: Chebyshev distance(對角 1 步,跟 8-direction step distance 一致)
//! - Step cost: 直走 10 / 對角 14(避免浮點,~sqrt(2)*10)
//! - **No corner squeeze**:對角線移動需要兩個正交鄰居也 walkable,
//!   避免穿越「兩面牆夾的縫」這種視覺上不合理的路徑
//! - [`MAX_ITERATIONS`] 上限:防止 unreachable goal 算到天荒地老
//!
//! ## 路徑顯示
//!
//! [`BOT_PATH`] 全域(Mutex<Vec<(i32, i32)>>)— bot 規劃完寫進去,minimap renderer 讀。
//! 空 vec = 沒在跑路徑模式(空閒 / 直線 heading walk)。

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Mutex;

use crate::minimap::map_loader::Map;

/// 對齊 `bot::action::walk::heading_from_delta` 的 8-direction step 表(heading 0..7)
const STEPS: [(i32, i32); 8] = [
    (0, -1),  // 0  N
    (1, -1),  // 1  NE
    (1, 0),   // 2  E
    (1, 1),   // 3  SE
    (0, 1),   // 4  S
    (-1, 1),  // 5  SW
    (-1, 0),  // 6  W
    (-1, -1), // 7  NW
];

/// A* 探索上限。 8000 ≈ 90 半徑可達 tile,對 hunt 視野走路綽綽有餘;
/// 過長的 unreachable goal 直接早退避免每 tick 卡死。
pub const MAX_ITERATIONS: usize = 8000;

/// 走路網格的查詢介面 — pathfind 只需要「這個 tile 能不能走」。
///
/// 實作者(`MapWalkable`)負責 tile → block + local cell 換算 + 拿 walkable bit。
/// out-of-bounds / 未載入 block 一律視為 blocked(保守:寧可繞路也不闖未知區)。
pub trait Walkable {
    fn is_walkable(&self, tile_x: i32, tile_y: i32) -> bool;

    fn blocks_sight(&self, tile_x: i32, tile_y: i32) -> bool {
        !self.is_walkable(tile_x, tile_y)
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
        if dx != 0 && dy != 0 {
            self.is_walkable(from.0 + dx, from.1) && self.is_walkable(from.0, from.1 + dy)
        } else {
            true
        }
    }

    fn movement_penalty(&self, _from: (i32, i32), _to: (i32, i32)) -> u32 {
        0
    }
}

/// `minimap::map_loader::Map` 對 [`Walkable`] 的 adapter。
///
/// `(tile_x, tile_y)` → 算 block 座標 + local cell,從 `map.blocks` 拿 walkable bit。
/// 沒對應 block 的 tile 回 false。
pub struct MapWalkable<'a> {
    pub map: &'a Map,
}

impl<'a> Walkable for MapWalkable<'a> {
    fn is_walkable(&self, tile_x: i32, tile_y: i32) -> bool {
        self.map.nav.is_walkable(tile_x, tile_y)
    }

    fn blocks_sight(&self, tile_x: i32, tile_y: i32) -> bool {
        self.map.nav.blocks_sight(tile_x, tile_y)
    }

    fn can_step(&self, from: (i32, i32), to: (i32, i32)) -> bool {
        self.map.nav.can_step(from, to)
    }
}

/// 全域 bot 規劃路徑 — pathfind 寫,minimap renderer 讀,
/// 空 vec = 目前沒走 A* path(空閒或退化到 heading walk)。
pub static BOT_PATH: Mutex<Vec<(i32, i32)>> = Mutex::new(Vec::new());

#[cfg(test)]
static BOT_PATH_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub fn with_bot_path_test_lock<T>(f: impl FnOnce() -> T) -> T {
    let _guard = BOT_PATH_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f()
}

/// 寫入新 path(整段覆蓋)。 mutex 取鎖失敗就放棄寫,不阻塞 bot tick。
pub fn set_bot_path(path: Vec<(i32, i32)>) {
    if let Ok(mut p) = BOT_PATH.lock() {
        *p = path;
    }
}

/// 清空 path — 沒目標 / 走完 / 重規劃前用。
#[cfg(test)]
pub fn clear_bot_path() {
    set_bot_path(Vec::new());
}

/// minimap renderer 用 — 拿 path 的 snapshot copy,不要長時間持鎖。
pub fn read_bot_path() -> Vec<(i32, i32)> {
    BOT_PATH.lock().map(|p| p.clone()).unwrap_or_default()
}

/// 快速判斷 start → goal 是否可達。 內部跟 [`plan`] 同一條 A* 路徑,只是丟掉 path。
/// 跟 plan 一樣受 [`MAX_ITERATIONS`] 上限,unreachable 也會在 8000 iter 內結束。
///
/// 注意:**這跟「minimap 有沒有載入」是不同概念**。 caller 要先確認 map_cache 有
/// 這張 map 才呼叫;沒載入時應 fallback 為「視同可達」(無法判斷,不要過濾掉好 target)。
#[inline]
#[cfg(test)]
pub fn is_reachable<W: Walkable>(start: (i32, i32), goal: (i32, i32), grid: &W) -> bool {
    plan(start, goal, grid).is_some()
}

/// 算出 A* 真實路徑長度(8-direction steps,對角算 1 格)— picker 排序用。
///
/// 用「真實路徑長度」而非 Chebyshev 直線:玩家被牆包圍時,牆對面的怪 Chebyshev=11
/// 但真實 A* 要繞 30 格;按直線距離排會選那隻不該選的近怪 → 2 秒走不到 blacklist。
///
/// 不可達 → `None`(caller 自行 fallback,例如「視同最遠」)。
#[cfg(test)]
pub fn path_steps<W: Walkable>(start: (i32, i32), goal: (i32, i32), grid: &W) -> Option<u32> {
    plan(start, goal, grid).map(|path| path.len() as u32)
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Node {
    /// f = g + heuristic,BinaryHeap 排序 key
    f: u32,
    /// 從 start 到此 node 已走 cost
    g: u32,
    pos: (i32, i32),
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap 預設 max-heap,我們要 min-heap on f → 反轉
        other.f.cmp(&self.f).then_with(|| other.g.cmp(&self.g))
    }
}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Chebyshev distance — 8-direction min steps(對角線算 1 步,跟遊戲 movement 規則一致)
fn chebyshev(a: (i32, i32), b: (i32, i32)) -> u32 {
    a.0.abs_diff(b.0).max(a.1.abs_diff(b.1))
}

/// Step cost — 直走 10、對角 14(~sqrt(2)*10,避免浮點)
fn step_cost(dx: i32, dy: i32) -> u32 {
    if dx != 0 && dy != 0 {
        14
    } else {
        10
    }
}

/// 規劃 `start` → `goal` 的 walkable tile 序列。
///
/// 回傳:
/// - `Some(vec![])` — 已在 goal,不需走
/// - `Some(path)` — path **不含 start**,從第一個 waypoint 起到 goal(含)
/// - `None` — goal 不可走 / 不可達 / 超過 [`MAX_ITERATIONS`]
///
/// 對角線需要兩個正交鄰居都 walkable(no corner squeeze)。 例如 (0,0)→(1,1) 需要
/// (1,0) 跟 (0,1) 都 walkable,避免穿越牆角這種視覺不合理的捷徑。
#[cfg(test)]
pub fn plan<W: Walkable>(start: (i32, i32), goal: (i32, i32), grid: &W) -> Option<Vec<(i32, i32)>> {
    plan_to_any(start, &[goal], grid)
}

/// 規劃 `start` → 任一可接受終點的 walkable tile 序列。
///
/// 用於 bot 走到「可攻擊位置」而不是硬走到怪物所在 tile。 例如怪物格本身被
/// Layer3 標 blocked,但旁邊 1 格可站立且已在近戰射程內,這時應走到旁邊而不是
/// 判定整隻怪 unreachable 後退化成直接點怪。
pub fn plan_to_any<W: Walkable>(
    start: (i32, i32),
    goals: &[(i32, i32)],
    grid: &W,
) -> Option<Vec<(i32, i32)>> {
    if goals.contains(&start) {
        return Some(Vec::new());
    }

    let goals: Vec<(i32, i32)> = goals
        .iter()
        .copied()
        .filter(|&(x, y)| grid.is_walkable(x, y))
        .collect();
    if goals.is_empty() {
        return None;
    }
    let goal_set: HashSet<(i32, i32)> = goals.iter().copied().collect();
    let heuristic = |pos: (i32, i32)| -> u32 {
        goals.iter().map(|&g| chebyshev(pos, g)).min().unwrap_or(0) * 10
    };

    let mut open = BinaryHeap::new();
    let mut g_score: HashMap<(i32, i32), u32> = HashMap::new();
    let mut came_from: HashMap<(i32, i32), (i32, i32)> = HashMap::new();

    g_score.insert(start, 0);
    open.push(Node {
        f: heuristic(start),
        g: 0,
        pos: start,
    });

    let mut iter = 0usize;
    while let Some(cur) = open.pop() {
        iter += 1;
        if iter > MAX_ITERATIONS {
            return None;
        }

        if goal_set.contains(&cur.pos) {
            // 重建 path:從命中的 goal 反追 came_from,直到追到 start(不含 start)
            let mut path = vec![cur.pos];
            let mut p = cur.pos;
            while let Some(&prev) = came_from.get(&p) {
                if prev == start {
                    break;
                }
                path.push(prev);
                p = prev;
            }
            path.reverse();
            return Some(path);
        }

        // 過時 node — 已被更便宜的路徑覆蓋
        if cur.g > *g_score.get(&cur.pos).unwrap_or(&u32::MAX) {
            continue;
        }

        for &(dx, dy) in &STEPS {
            let nx = cur.pos.0 + dx;
            let ny = cur.pos.1 + dy;
            if !grid.can_step(cur.pos, (nx, ny)) {
                continue;
            }
            let tentative_g = cur.g + step_cost(dx, dy) + grid.movement_penalty(cur.pos, (nx, ny));
            if tentative_g < *g_score.get(&(nx, ny)).unwrap_or(&u32::MAX) {
                g_score.insert((nx, ny), tentative_g);
                came_from.insert((nx, ny), cur.pos);
                open.push(Node {
                    f: tentative_g + heuristic((nx, ny)),
                    g: tentative_g,
                    pos: (nx, ny),
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 測試用簡易 grid — `cells[y][x] == true` 表示 walkable。 原點 (0,0) 在左上。
    struct GridArr {
        cells: Vec<Vec<bool>>,
    }
    impl Walkable for GridArr {
        fn is_walkable(&self, x: i32, y: i32) -> bool {
            if y < 0 || x < 0 {
                return false;
            }
            let y = y as usize;
            let x = x as usize;
            self.cells
                .get(y)
                .and_then(|r| r.get(x))
                .copied()
                .unwrap_or(false)
        }
    }
    fn open_grid(rows: usize, cols: usize) -> GridArr {
        GridArr {
            cells: vec![vec![true; cols]; rows],
        }
    }

    #[test]
    fn chebyshev_basics() {
        assert_eq!(chebyshev((0, 0), (0, 0)), 0);
        assert_eq!(chebyshev((0, 0), (3, 0)), 3);
        assert_eq!(chebyshev((0, 0), (0, 5)), 5);
        // 對角線 max(dx, dy)
        assert_eq!(chebyshev((0, 0), (3, 5)), 5);
        assert_eq!(chebyshev((0, 0), (7, 3)), 7);
    }

    #[test]
    fn step_cost_orthogonal_vs_diagonal() {
        assert_eq!(step_cost(1, 0), 10);
        assert_eq!(step_cost(0, -1), 10);
        assert_eq!(step_cost(1, 1), 14);
        assert_eq!(step_cost(-1, 1), 14);
    }

    #[test]
    fn plan_same_start_goal_returns_empty_path() {
        let g = open_grid(5, 5);
        let p = plan((2, 2), (2, 2), &g).unwrap();
        assert!(p.is_empty(), "已在 goal 應回空 path,實得 {p:?}");
    }

    #[test]
    fn plan_straight_east() {
        let g = open_grid(5, 5);
        let p = plan((0, 0), (3, 0), &g).unwrap();
        assert_eq!(p, vec![(1, 0), (2, 0), (3, 0)]);
    }

    #[test]
    fn plan_straight_south() {
        let g = open_grid(5, 5);
        let p = plan((0, 0), (0, 3), &g).unwrap();
        assert_eq!(p, vec![(0, 1), (0, 2), (0, 3)]);
    }

    #[test]
    fn plan_diagonal_uses_three_diag_steps() {
        // 5×5 全開,(0,0) → (3,3) Chebyshev = 3 → A* 應該找 3 步對角線
        let g = open_grid(5, 5);
        let p = plan((0, 0), (3, 3), &g).unwrap();
        assert_eq!(p.len(), 3, "對角 3 步,實得 {p:?}");
        assert_eq!(p.last(), Some(&(3, 3)));
        for w in p.windows(2) {
            let (dx, dy) = (w[1].0 - w[0].0, w[1].1 - w[0].1);
            assert!(
                dx.abs() == 1 && dy.abs() == 1,
                "全空地 (0,0)→(3,3) 每步應對角,得 {dx},{dy}"
            );
        }
    }

    #[test]
    fn plan_returns_none_when_goal_is_wall() {
        let mut g = open_grid(3, 3);
        g.cells[1][1] = false;
        assert!(plan((0, 0), (1, 1), &g).is_none());
    }

    #[test]
    fn plan_routes_around_wall() {
        // 3×3 中心 (1,1) 牆,goal (2,2) 對角過去
        let mut g = open_grid(3, 3);
        g.cells[1][1] = false;
        let p = plan((0, 0), (2, 2), &g).unwrap();
        assert_eq!(p.last(), Some(&(2, 2)));
        assert!(!p.contains(&(1, 1)), "path 不應穿過牆 (1,1)");
    }

    #[test]
    fn plan_corner_squeeze_blocked() {
        // (0,0) 想對角到 (1,1),但 (1,0) 跟 (0,1) 都是牆 → 對角穿越被禁
        // (0,0) 唯一鄰居全擋 → 起點孤立 → None
        let mut g = open_grid(5, 5);
        g.cells[0][1] = false; // (1, 0) wall
        g.cells[1][0] = false; // (0, 1) wall
        let p = plan((0, 0), (1, 1), &g);
        assert!(p.is_none(), "對角 squeeze 兩 orthogonal 全擋應 unreachable");
    }

    #[test]
    fn plan_single_corner_block_still_allows_diagonal() {
        // (1,0) 牆但 (0,1) 可走 — 對角仍被擋(我們要兩個都 walkable),
        // 但可以走 (0,0)→(0,1)→(1,1) 兩步 orthogonal 繞過去
        let mut g = open_grid(3, 3);
        g.cells[0][1] = false; // (1, 0) wall
        let p = plan((0, 0), (1, 1), &g).unwrap();
        assert_eq!(p.last(), Some(&(1, 1)));
        assert!(p.len() >= 2, "繞過去至少 2 步,實得 {p:?}");
        assert!(!p.contains(&(1, 0)), "path 不應穿過牆");
    }

    #[test]
    fn plan_unreachable_isolated_start_returns_none() {
        // (0,0) 周圍全牆,孤立無解
        let mut g = open_grid(3, 3);
        g.cells[0][1] = false; // (1,0)
        g.cells[1][0] = false; // (0,1)
        g.cells[1][1] = false; // (1,1) 順便擋對角後備
        assert!(plan((0, 0), (2, 2), &g).is_none());
    }

    #[test]
    fn plan_path_only_contains_walkable_tiles() {
        // 隨機牆 + 規劃一條 → path 上每個 tile 都應 walkable
        let mut g = open_grid(8, 8);
        for &(x, y) in &[(2, 1), (3, 1), (4, 1), (5, 1), (5, 2), (5, 3)] {
            g.cells[y][x] = false;
        }
        let p = plan((0, 0), (7, 7), &g).unwrap();
        for &t in &p {
            assert!(g.is_walkable(t.0, t.1), "path 含不可走 tile {:?}", t);
        }
        assert_eq!(p.last(), Some(&(7, 7)));
    }

    #[test]
    fn plan_to_any_reaches_attack_tile_when_monster_tile_is_blocked() {
        let mut g = open_grid(5, 5);
        g.cells[2][2] = false; // 怪物 / 障礙物本身不可站
        let goals = vec![(2, 2), (2, 1), (1, 2), (3, 2), (2, 3)];

        let p = plan_to_any((0, 2), &goals, &g).expect("應能走到怪旁邊可攻擊格");

        assert_ne!(p.last(), Some(&(2, 2)), "不應把不可走的怪物格當終點");
        assert!(
            matches!(
                p.last(),
                Some(&(1, 2)) | Some(&(2, 1)) | Some(&(2, 3)) | Some(&(3, 2))
            ),
            "終點應是射程內可站立 tile,實得 {p:?}"
        );
    }

    #[test]
    fn bot_path_global_round_trip() {
        with_bot_path_test_lock(|| {
            // BOT_PATH 是 process 全域,測試前先清,確保 deterministic
            clear_bot_path();
            assert!(read_bot_path().is_empty());
            let p = vec![(1, 1), (2, 2), (3, 3)];
            set_bot_path(p.clone());
            assert_eq!(read_bot_path(), p);
            clear_bot_path();
            assert!(read_bot_path().is_empty());
        });
    }
}
