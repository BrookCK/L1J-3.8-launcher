//! GDI BitBlt double-buffer minimap renderer。
//!
//! 每次 WM_PAINT:
//! 1. CreateCompatibleDC + bitmap 大小 = viewport.screen_w × screen_h
//! 2. 整片背景填可走色(walkable)
//! 3. 對 viewport 範圍內每個 tile 找對應 block,若 blocked 就 FillRect 牆色
//! 4. paint player + monster dots(Task C3)
//! 5. BitBlt to window DC + DeleteDC

use std::sync::Arc;

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen, CreateSolidBrush,
    DeleteDC, DeleteObject, EndPaint, FillRect, LineTo, MoveToEx, SelectObject, HGDIOBJ,
    PAINTSTRUCT, PS_SOLID, SRCCOPY,
};

use super::coord::{tile_to_block, tile_to_local, Viewport};
use super::map_loader::Map;
use super::snapshot::MapSnapshot;
use crate::bot::decide::pathfind;
use crate::bot::perception::position::PlayerPosition;

const COLOR_WALKABLE: u32 = rgb(60, 60, 60);
const COLOR_WALL: u32 = rgb(26, 26, 26);
const COLOR_PLAYER: u32 = rgb(80, 160, 255); // 玩家 = 藍點
const COLOR_MONSTER: u32 = rgb(220, 60, 60); // 怪 = 紅點
const COLOR_BOT_PATH: u32 = rgb(255, 220, 60); // bot A* path = 黃線

/// 遊戲畫面實際可見半徑 — 大致對齊 Lineage 3.8 client viewport,
/// 玩家為中心 ±12 tile 內的 entity 才在遊戲視窗內可見。 超出範圍的
/// entity 雖然 heap pool 內存在,但遊戲畫面看不到,minimap 也不畫紅點 —
/// 讓 minimap 跟 in-game 視覺對齊,entity 隨玩家移動逐步進入視野。
///
/// 牆 / 地形不受此限制,維持完整地圖預覽。
const VISIBLE_RADIUS_TILES: i32 = 12;

pub fn paint(hwnd: HWND, viewport: &Viewport, map: Option<&Arc<Map>>, snap: &MapSnapshot) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mem_dc = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, viewport.screen_w, viewport.screen_h);
        let old_bmp = SelectObject(mem_dc, bmp.into());

        // 1) 背景全填可走色
        let bg_brush = CreateSolidBrush(COLORREF(COLOR_WALKABLE));
        let full = RECT {
            left: 0,
            top: 0,
            right: viewport.screen_w,
            bottom: viewport.screen_h,
        };
        FillRect(mem_dc, &full, bg_brush);
        let _ = DeleteObject(bg_brush.into());

        // 2) 牆(blocked)— 只在 viewport 可見範圍掃 tile
        if let Some(map) = map {
            paint_walls(mem_dc, viewport, map);
        }

        // 3) bot 規劃路徑(黃線)— 在牆之上、entity 之下,避免被紅藍點遮住
        paint_bot_path(mem_dc, viewport, snap);

        // 4) entities — 玩家自己藍點 + 其他全部紅點(NPC / 怪 / 召喚物都同顏色,先 ship)
        paint_entities(mem_dc, viewport, snap);

        // 5) Blit + cleanup
        let _ = BitBlt(
            hdc,
            0,
            0,
            viewport.screen_w,
            viewport.screen_h,
            Some(mem_dc),
            0,
            0,
            SRCCOPY,
        );
        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem_dc);
        let _ = EndPaint(hwnd, &ps);
    }
}

fn paint_walls(dc: windows::Win32::Graphics::Gdi::HDC, viewport: &Viewport, map: &Map) {
    unsafe {
        let wall_brush = CreateSolidBrush(COLORREF(COLOR_WALL));
        let cell_px = viewport.zoom.max(1.0) as i32;
        // 45° 旋轉後可見區域在 tile 空間是菱形 — bounding square 邊長 = 對角線 / 0.707。
        // 對 W=H=400, zoom=10 算出來大概 ±29 tile,加 2 緩衝 cell 邊界。
        let radius_tiles = (viewport.screen_w.max(viewport.screen_h) as f32
            / 2.0
            / (viewport.zoom * std::f32::consts::FRAC_1_SQRT_2))
            .ceil() as i32
            + 2;
        let min_tile_x = viewport.center_tile_x - radius_tiles;
        let max_tile_x = viewport.center_tile_x + radius_tiles;
        let min_tile_y = viewport.center_tile_y - radius_tiles;
        let max_tile_y = viewport.center_tile_y + radius_tiles;

        for ty in min_tile_y..=max_tile_y {
            for tx in min_tile_x..=max_tile_x {
                let (bx, by) = tile_to_block(tx, ty);
                let Some(block) = map.blocks.get(&(bx, by)) else {
                    continue;
                };
                let (lx, ly) = tile_to_local(tx, ty);
                if block.walkable[ly][lx] {
                    continue;
                }
                let (sx, sy) = viewport.tile_to_screen(tx, ty);
                // 把 cell 中心對齊到 (sx, sy),旋轉後的 cell 不會偏一側
                let half = cell_px / 2;
                let r = RECT {
                    left: sx - half,
                    top: sy - half,
                    right: sx + cell_px - half,
                    bottom: sy + cell_px - half,
                };
                FillRect(dc, &r, wall_brush);
            }
        }
        let _ = DeleteObject(wall_brush.into());
    }
}

/// 畫玩家(藍點 5×5)+ 其他 entity(紅點 3×3)。
///
/// entity 過濾規則:
/// 1. coord 在 sane range(L3 地圖約 16000-50000,排除未初始化 slot)
/// 2. 貼身或重疊玩家的怪仍要畫出來；戰鬥中的怪物座標常會與玩家同格。
/// 3. **超出 `VISIBLE_RADIUS_TILES` 範圍跳過** — 跟遊戲畫面對齊,
///    在遊戲視窗看不到的 entity 也不顯示紅點(隨玩家移動才進視野)
fn paint_entities(dc: windows::Win32::Graphics::Gdi::HDC, viewport: &Viewport, snap: &MapSnapshot) {
    unsafe {
        // 玩家自己 — 藍 5x5
        if let Some(p) = &snap.player {
            let brush = CreateSolidBrush(COLORREF(COLOR_PLAYER));
            let (sx, sy) = viewport.tile_to_screen(p.x, p.y as i32);
            let r = RECT {
                left: sx - 2,
                top: sy - 2,
                right: sx + 3,
                bottom: sy + 3,
            };
            FillRect(dc, &r, brush);
            let _ = DeleteObject(brush.into());
        }
        // 其他 entity — 紅 3x3
        let monster_brush = CreateSolidBrush(COLORREF(COLOR_MONSTER));
        for m in &snap.entities {
            if !is_valid_entity_coord(m.raw_x, m.y) {
                continue;
            }
            let display_x = crate::bot::perception::position::decode_x(m.raw_x);
            let entity_y = m.y as i32;
            if !entity_is_visible_near_player(display_x, entity_y, snap.player.as_ref()) {
                continue;
            }
            let (sx, sy) = viewport.tile_to_screen(display_x, entity_y);
            let r = RECT {
                left: sx - 1,
                top: sy - 1,
                right: sx + 2,
                bottom: sy + 2,
            };
            FillRect(dc, &r, monster_brush);
        }
        let _ = DeleteObject(monster_brush.into());
    }
}

/// 畫 bot A* 規劃路徑 — 從 `pathfind::BOT_PATH` 拿 tile 序列,Polyline 連起來。
///
/// path 為空就 no-op,bot 不在跑路徑模式時 minimap 上看不到黃線。 路徑會跟著 viewport
/// 平移 / zoom 自動更新(每 100ms tick 重畫整片時都會讀新 path)。
fn paint_bot_path(dc: windows::Win32::Graphics::Gdi::HDC, viewport: &Viewport, snap: &MapSnapshot) {
    let path = path_with_player_start(snap.player.as_ref(), pathfind::read_bot_path());
    if path.len() < 2 {
        // 0 個或 1 個 waypoint 沒線可畫(同一點不形成線段)
        return;
    }
    unsafe {
        // 2px 寬黃線,solid。 i32 寬度(GDI 預設 stock pen 不支援 anti-alias,先這樣)
        let pen = CreatePen(PS_SOLID, 2, COLORREF(COLOR_BOT_PATH));
        let old_pen = SelectObject(dc, pen.into());

        let (sx0, sy0) = viewport.tile_to_screen(path[0].0, path[0].1);
        let _ = MoveToEx(dc, sx0, sy0, None);
        for &(tx, ty) in &path[1..] {
            let (sx, sy) = viewport.tile_to_screen(tx, ty);
            let _ = LineTo(dc, sx, sy);
        }

        SelectObject(dc, old_pen);
        let _ = DeleteObject(pen.into());
    }
}

/// entity coord sanity check — 未初始化的 heap slot 通常是 0 / 0xFFFF_FFFF / 極小值。
/// L3 地圖實際座標約 16000-50000 範圍。
fn is_valid_entity_coord(raw_x: u32, y: u32) -> bool {
    const MIN: u32 = 8_000;
    const MAX: u32 = 60_000;
    raw_x >= MIN && raw_x <= MAX && y >= MIN && y <= MAX
}

fn path_with_player_start(
    player: Option<&PlayerPosition>,
    mut path: Vec<(i32, i32)>,
) -> Vec<(i32, i32)> {
    let Some(player) = player else {
        return path;
    };
    let start = (player.x, player.y);
    if path.first().copied() != Some(start) {
        path.insert(0, start);
    }
    path
}

fn entity_is_visible_near_player(
    display_x: i32,
    entity_y: i32,
    player: Option<&crate::bot::perception::position::PlayerPosition>,
) -> bool {
    let Some(p) = player else {
        return true;
    };
    let dx = (display_x - p.x).abs();
    let dy = (entity_y - p.y).abs();
    dx <= VISIBLE_RADIUS_TILES && dy <= VISIBLE_RADIUS_TILES
}

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::perception::position::PlayerPosition;

    #[test]
    fn nearby_monsters_are_visible_even_when_overlapping_player_tile() {
        let player = PlayerPosition { x: 32726, y: 32781 };

        assert!(
            entity_is_visible_near_player(32726, 32781, Some(&player)),
            "a monster sharing the player tile must still be drawn; combat sprites can overlap the player"
        );
        assert!(
            entity_is_visible_near_player(32727, 32782, Some(&player)),
            "adjacent monsters must not be hidden as self/avatar"
        );
        assert!(
            !entity_is_visible_near_player(32726 + VISIBLE_RADIUS_TILES + 1, 32781, Some(&player)),
            "far entities remain outside minimap live-view radius"
        );
    }

    #[test]
    fn bot_path_display_starts_from_player_tile() {
        let player = PlayerPosition { x: 10, y: 20 };
        let planned = vec![(11, 20), (12, 20)];

        let display = path_with_player_start(Some(&player), planned);

        assert_eq!(display, vec![(10, 20), (11, 20), (12, 20)]);
    }

    #[test]
    fn bot_path_display_draws_single_waypoint_segment() {
        let player = PlayerPosition { x: 10, y: 20 };
        let planned = vec![(11, 20)];

        let display = path_with_player_start(Some(&player), planned);

        assert_eq!(display, vec![(10, 20), (11, 20)]);
    }
}
