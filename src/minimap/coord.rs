//! 座標轉換 — world tile ↔ block index ↔ minimap screen pixel。
//!
//! `world_tile_x` = `player.x`(已 decode 過的 display X,例 33071)。
//! `world_tile_y` = `player.y as i32`(memory 直接讀,無 decode)。
//!
//! ## block ↔ world tile 對映(2026-05-14 RE)
//!
//! 檔名 hex u32 拆 hi16/lo16 各減 0x8000 → block 座標。 玩家 world tile X(來自
//! `/loc` 解碼後值)也是以 0x8000 為原點:
//!
//! - player.x = 33071 → block_x = (33071 - 0x8000) / 64 = 4
//! - player.y = 32500 → block_y = (32500 - 0x8000) / 64 = -5
//!
//! 所以 tile→block 要先扣 0x8000 再除 64,才能對上檔名解碼出來的 block 座標。

pub const TILES_PER_BLOCK: i32 = 64;
/// world tile 座標與 block 座標都以 0x8000 為原點。 詳見模組層註解。
pub const BLOCK_ORIGIN: i32 = 0x8000;

/// 視窗 viewport — 跟著 follow target 走。
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    /// 視窗中央對應的 world tile
    pub center_tile_x: i32,
    pub center_tile_y: i32,
    pub screen_w: i32,
    pub screen_h: i32,
    /// 每個 tile 在螢幕上佔幾 px(zoom 倍率)
    pub zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            center_tile_x: 0,
            center_tile_y: 0,
            screen_w: 400,
            screen_h: 400,
            // 10x — 跟遊戲畫面 viewport 對齊(±20 tile,牆 / 地形 cell 10 像素清晰可見)。
            // 滑鼠滾輪可在 [0.5, 16] 範圍微調。
            zoom: 10.0,
        }
    }
}

impl Viewport {
    /// world tile → minimap screen pixel,**套 45° 旋轉跟遊戲 iso view 方位對齊**:
    ///
    /// - +X (game east) → 螢幕右上(畫面 x 增、y 減)
    /// - +Y (game south) → 螢幕右下(畫面 x 增、y 增)
    ///
    /// 走南方時遊戲畫面是斜下右,minimap 也跟著斜下右,玩家方位感一致。
    ///
    /// 用純 45° 旋轉(不做 2:1 squish),保留視野等邊正方形 → 上下左右 tile
    /// 數一致。 想要 Lineage 標準 2:1 iso 看起來「壓扁」效果可未來再加。
    pub fn tile_to_screen(&self, tile_x: i32, tile_y: i32) -> (i32, i32) {
        let dx = (tile_x - self.center_tile_x) as f32;
        let dy = (tile_y - self.center_tile_y) as f32;
        let k = self.zoom * std::f32::consts::FRAC_1_SQRT_2;
        let sdx = (dx + dy) * k;
        let sdy = (dy - dx) * k;
        (
            (self.screen_w / 2) + sdx as i32,
            (self.screen_h / 2) + sdy as i32,
        )
    }

    /// `tile_to_screen` 的逆變換 — 右鍵 drag 平移、screen → tile 點擊判定用。
    #[cfg(test)]
    pub fn screen_to_tile(&self, sx: i32, sy: i32) -> (i32, i32) {
        let sdx = (sx - self.screen_w / 2) as f32;
        let sdy = (sy - self.screen_h / 2) as f32;
        let k = self.zoom * std::f32::consts::FRAC_1_SQRT_2;
        // 解 sdx = (dx + dy) * k, sdy = (dy - dx) * k
        let dx = (sdx - sdy) / (2.0 * k);
        let dy = (sdx + sdy) / (2.0 * k);
        (
            self.center_tile_x + dx as i32,
            self.center_tile_y + dy as i32,
        )
    }

    /// 螢幕像素 delta → tile delta — `screen_to_tile` 的「方向版」,
    /// 右鍵 drag 拖曳平移用。 不加 viewport center。
    pub fn screen_delta_to_tile(&self, px_dx: i32, px_dy: i32) -> (i32, i32) {
        let k = self.zoom * std::f32::consts::FRAC_1_SQRT_2;
        let fx = px_dx as f32;
        let fy = px_dy as f32;
        let tx = (fx - fy) / (2.0 * k);
        let ty = (fx + fy) / (2.0 * k);
        (tx as i32, ty as i32)
    }
}

/// world tile → block index(以 0x8000 為原點, div_euclid 處理負 block)
pub fn tile_to_block(tile_x: i32, tile_y: i32) -> (i32, i32) {
    (
        (tile_x - BLOCK_ORIGIN).div_euclid(TILES_PER_BLOCK),
        (tile_y - BLOCK_ORIGIN).div_euclid(TILES_PER_BLOCK),
    )
}

/// world tile → block 內 local cell(0..63,以 0x8000 為原點)
pub fn tile_to_local(tile_x: i32, tile_y: i32) -> (usize, usize) {
    (
        (tile_x - BLOCK_ORIGIN).rem_euclid(TILES_PER_BLOCK) as usize,
        (tile_y - BLOCK_ORIGIN).rem_euclid(TILES_PER_BLOCK) as usize,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport_at(cx: i32, cy: i32, zoom: f32) -> Viewport {
        Viewport {
            center_tile_x: cx,
            center_tile_y: cy,
            screen_w: 400,
            screen_h: 400,
            zoom,
        }
    }

    #[test]
    fn tile_to_screen_center_unchanged() {
        let v = viewport_at(100, 200, 10.0);
        // viewport center 永遠落在螢幕正中
        assert_eq!(v.tile_to_screen(100, 200), (200, 200));
    }

    #[test]
    fn tile_to_screen_iso_east_goes_upper_right() {
        // +X (game east) → 螢幕右上
        let v = viewport_at(100, 200, 10.0);
        let (sx, sy) = v.tile_to_screen(101, 200);
        assert!(sx > 200, "+X should move right, sx={sx}");
        assert!(sy < 200, "+X should move up, sy={sy}");
    }

    #[test]
    fn tile_to_screen_iso_south_goes_lower_right() {
        // +Y (game south) → 螢幕右下
        let v = viewport_at(100, 200, 10.0);
        let (sx, sy) = v.tile_to_screen(100, 201);
        assert!(sx > 200, "+Y should move right, sx={sx}");
        assert!(sy > 200, "+Y should move down, sy={sy}");
    }

    #[test]
    fn tile_to_screen_iso_north_goes_upper_left() {
        // -Y (game north) → 螢幕左上
        let v = viewport_at(100, 200, 10.0);
        let (sx, sy) = v.tile_to_screen(100, 199);
        assert!(sx < 200, "-Y should move left, sx={sx}");
        assert!(sy < 200, "-Y should move up, sy={sy}");
    }

    #[test]
    fn screen_to_tile_round_trip() {
        let v = viewport_at(100, 200, 10.0);
        // 隨機選幾個點驗證 round-trip(整數量化下允許 ±1 tile 誤差)
        for (tx, ty) in [(110, 195), (90, 210), (105, 205), (95, 195)] {
            let (sx, sy) = v.tile_to_screen(tx, ty);
            let (rx, ry) = v.screen_to_tile(sx, sy);
            assert!((rx - tx).abs() <= 1, "X round-trip: {tx} → {rx}");
            assert!((ry - ty).abs() <= 1, "Y round-trip: {ty} → {ry}");
        }
    }

    #[test]
    fn screen_delta_to_tile_inverse_of_rotation() {
        let v = viewport_at(100, 200, 10.0);
        // 取中心 → +1 tile +X 的螢幕位移當 delta,反向算回來應該 ≈ (+1, 0)
        let (sx0, sy0) = v.tile_to_screen(100, 200);
        let (sx1, sy1) = v.tile_to_screen(101, 200);
        let (tdx, tdy) = v.screen_delta_to_tile(sx1 - sx0, sy1 - sy0);
        assert!((tdx - 1).abs() <= 1, "+X round-trip: tdx={tdx}");
        assert!(tdy.abs() <= 1, "+X round-trip: tdy={tdy}");

        // +1 tile +Y 也測一次
        let (sx2, sy2) = v.tile_to_screen(100, 201);
        let (tdx2, tdy2) = v.screen_delta_to_tile(sx2 - sx0, sy2 - sy0);
        assert!(tdx2.abs() <= 1, "+Y round-trip: tdx={tdx2}");
        assert!((tdy2 - 1).abs() <= 1, "+Y round-trip: tdy={tdy2}");
    }

    #[test]
    fn tile_to_block_at_origin() {
        // 0x8000 是 block 原點
        assert_eq!(tile_to_block(0x8000, 0x8000), (0, 0));
        assert_eq!(tile_to_block(0x8000 + 63, 0x8000 + 63), (0, 0));
        assert_eq!(tile_to_block(0x8000 + 64, 0x8000), (1, 0));
    }

    #[test]
    fn tile_to_block_player_loc_example() {
        // player.x=33071 → (33071-0x8000)/64 = 303/64 = 4
        assert_eq!(tile_to_block(33071, 32500), (4, -5));
    }

    #[test]
    fn tile_to_block_negative_offset() {
        // 0x7FFF = 0x8000 - 1 → block_x = -1
        assert_eq!(tile_to_block(0x8000 - 1, 0x8000 - 1), (-1, -1));
        assert_eq!(tile_to_block(0x8000 - 64, 0x8000), (-1, 0));
        assert_eq!(tile_to_block(0x8000 - 65, 0x8000), (-2, 0));
    }

    #[test]
    fn tile_to_local_within_block() {
        assert_eq!(tile_to_local(0x8000, 0x8000), (0, 0));
        assert_eq!(tile_to_local(0x8000 + 63, 0x8000 + 63), (63, 63));
        // 跨 block 邊界
        assert_eq!(tile_to_local(0x8000 + 64, 0x8000), (0, 0));
        // player.x=33071 → (33071-0x8000)%64 = 303%64 = 47
        // player.y=33000 → (33000-0x8000)%64 = 232%64 = 40
        assert_eq!(tile_to_local(33071, 33000), (47, 40));
    }
}
