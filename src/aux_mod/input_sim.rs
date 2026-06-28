//! 遊戲輸入座標換算工具 — 點擊/拖曳的安全區計算。
//!
//! round-3 死碼清除(2026-06-20):原本的 PostMessage 按鍵 / hover-click 模擬
//! (`press_fkey` / `click_hold_at_pixel`)與其 helper(`find_game_window` /
//! `client_size_for_hwnd`)、heartbeat 狀態(`MOUSE_HELD` / `LAST_DOWN_AT`)
//! 全專案已無任何引用，保留作為輸入座標換算工具。
//! 保留的純算術 helper 仍被單元測試引用,故留下。

// 以下 helper 在 round-3 死碼清除後僅剩單元測試引用(production 呼叫端已移除),
// 為保留測試覆蓋率不刪除,統一標 #[allow(dead_code)] 避免 non-test build 觸發 dead_code 警告。

/// `Fn` (n=1..12) 對應 Win32 VK code:VK_F1=0x70 ... VK_F12=0x7B
#[allow(dead_code)]
fn fkey_vk(n: u8) -> Option<u32> {
    if (1..=12).contains(&n) {
        Some(0x6F + n as u32) // VK_F1=0x70 即 0x6F+1
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct GameplayInputArea {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[allow(dead_code)]
impl GameplayInputArea {
    pub(crate) fn clamp_point(self, x: i32, y: i32) -> (i32, i32) {
        (
            x.clamp(self.left, self.right),
            y.clamp(self.top, self.bottom),
        )
    }

    pub(crate) fn clamp_drag_start(
        self,
        x: i32,
        y: i32,
        release_dx: i32,
        release_dy: i32,
    ) -> (i32, i32) {
        let max_x = self.right.saturating_sub(release_dx.max(0));
        let min_x = self.left.saturating_sub(release_dx.min(0));
        let max_y = self.bottom.saturating_sub(release_dy.max(0));
        let min_y = self.top.saturating_sub(release_dy.min(0));

        (
            x.clamp(min_x.min(max_x), min_x.max(max_x)),
            y.clamp(min_y.min(max_y), min_y.max(max_y)),
        )
    }
}

#[allow(dead_code)]
pub(crate) fn gameplay_input_area(
    client_width: i32,
    client_height: i32,
) -> Option<GameplayInputArea> {
    if client_width <= 0 || client_height <= 0 {
        return None;
    }

    let left = (client_width / 100).max(4);
    let top = (client_height * 54 / 1000).max(24);
    let right = (client_width * 75 / 100).saturating_sub(1);
    let bottom = (client_height * 83 / 100).saturating_sub(1);

    if right <= left || bottom <= top {
        return Some(GameplayInputArea {
            left: 0,
            top: 0,
            right: client_width.saturating_sub(1),
            bottom: client_height.saturating_sub(1),
        });
    }

    Some(GameplayInputArea {
        left,
        top,
        right,
        bottom,
    })
}

#[allow(dead_code)]
pub(crate) fn gameplay_input_dimensions(client_width: i32, client_height: i32) -> (i32, i32) {
    (client_width, client_height)
}

#[allow(dead_code)]
pub(crate) fn gameplay_click_point_from_offset(
    client_width: i32,
    client_height: i32,
    dx_px: i32,
    dy_px: i32,
) -> Option<(i32, i32)> {
    let (client_width, client_height) = gameplay_input_dimensions(client_width, client_height);
    let area = gameplay_input_area(client_width, client_height)?;
    Some(area.clamp_point(client_width / 2 + dx_px, client_height / 2 + dy_px))
}

/// 玩家在 client 上的螢幕錨點 — 走路/攻擊 offset 的原點。
///
/// 2026-06-03 live 徑向校準(`tools/_live_radial_cal.py`):以 (寬/2, 高/2) 為圓心繞一圈
/// 點擊,引擎判定的 world 方向落成乾淨的 8 等分扇區 → 證明引擎把玩家**鎖在 client 正中央**
/// (非地圖邊緣時)。 舊值 (寬/2, 高·40%) 把錨點抬高 60px(800×600 = y 240 vs 300),
/// 上半部方向被高估、下半部被低估 → 走路偏向。 改回正中央。
#[allow(dead_code)]
pub(crate) fn gameplay_player_anchor(client_width: i32, client_height: i32) -> (i32, i32) {
    let (client_width, client_height) = gameplay_input_dimensions(client_width, client_height);
    (client_width / 2, client_height / 2)
}

#[allow(dead_code)]
pub(crate) fn gameplay_click_point_from_player_anchor_offset(
    client_width: i32,
    client_height: i32,
    dx_px: i32,
    dy_px: i32,
) -> Option<(i32, i32)> {
    let (client_width, client_height) = gameplay_input_dimensions(client_width, client_height);
    let area = gameplay_input_area(client_width, client_height)?;
    let (anchor_x, anchor_y) = gameplay_player_anchor(client_width, client_height);
    Some(area.clamp_point(anchor_x + dx_px, anchor_y + dy_px))
}

#[allow(dead_code)]
pub(crate) fn gameplay_drag_start_point_from_player_anchor_offset(
    client_width: i32,
    client_height: i32,
    dx_px: i32,
    dy_px: i32,
    release_dx: i32,
    release_dy: i32,
) -> Option<(i32, i32)> {
    let (client_width, client_height) = gameplay_input_dimensions(client_width, client_height);
    let area = gameplay_input_area(client_width, client_height)?;
    let (anchor_x, anchor_y) = gameplay_player_anchor(client_width, client_height);
    Some(area.clamp_drag_start(anchor_x + dx_px, anchor_y + dy_px, release_dx, release_dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fkey_vk_range() {
        assert_eq!(fkey_vk(1), Some(0x70));
        assert_eq!(fkey_vk(12), Some(0x7B));
        assert_eq!(fkey_vk(0), None);
        assert_eq!(fkey_vk(13), None);
    }

    #[test]
    fn bot_click_points_are_clamped_to_gameplay_area_not_full_client() {
        let area = gameplay_input_area(800, 600).expect("800x600 should have safe gameplay area");

        assert_eq!(area.clamp_point(400 + 10_000, 300), (599, 300));
        assert_eq!(area.clamp_point(400, 300 + 10_000), (400, 497));
        assert_eq!(area.clamp_point(400 - 10_000, 300 - 10_000), (8, 32));
    }

    #[test]
    fn bot_click_offsets_stay_relative_to_full_client_center() {
        assert_eq!(
            gameplay_click_point_from_offset(800, 600, 100, 0),
            Some((500, 300))
        );
        assert_eq!(
            gameplay_click_point_from_offset(800, 600, -100, 0),
            Some((300, 300))
        );
    }

    #[test]
    fn bot_click_offsets_follow_actual_client_size() {
        assert_eq!(
            gameplay_input_dimensions(1904, 1041),
            (1904, 1041),
            "input should follow the actual client size instead of forcing 800x600"
        );
        assert_eq!(
            gameplay_click_point_from_offset(1904, 1041, 10_000, 10_000),
            Some((1427, 863)),
            "input must stay inside the actual gameplay area"
        );
    }

    #[test]
    fn bot_drag_start_keeps_release_inside_gameplay_area() {
        let area = gameplay_input_area(800, 600).expect("800x600 should have safe gameplay area");

        assert_eq!(
            area.clamp_drag_start(790, 590, 120, 300),
            (479, 197),
            "drag start must be pulled back so release is still inside gameplay area"
        );
    }

    #[test]
    fn player_anchor_is_client_center() {
        // 2026-06-03 live 徑向校準:玩家鎖在 client 正中央(非地圖邊緣時)。
        assert_eq!(gameplay_player_anchor(800, 600), (400, 300));
        assert_eq!(gameplay_player_anchor(1904, 1041), (952, 520));
        assert_eq!(
            gameplay_click_point_from_player_anchor_offset(800, 600, 0, -32),
            Some((400, 268))
        );
    }

    #[test]
    fn player_anchor_drag_start_keeps_release_inside_gameplay_area() {
        assert_eq!(
            gameplay_drag_start_point_from_player_anchor_offset(800, 600, 10_000, 10_000, 120, 300),
            Some((479, 197))
        );
    }
}
