//! 走路動作層 — 2026-05-13 第三輪 pivot:CreateRemoteThread + shellcode → PostMessage 模擬左鍵。
//! 2026-05-13 第五輪改進:click-release 三連發 → **按住左鍵 hold-and-move**,連續走路。
//!
//! ## 為什麼從 shellcode 改成 PostMessage
//!
//! 前兩輪嘗試 CreateRemoteThread + shellcode 走遊戲內部 walk 函式都失敗:
//! - **Option 1** `execute_action(0x248)`:過了 `0x4D3380(0x14)` self gate,但 state-update
//!   分支只 enqueue dispatcher `0x5B0350` → recursive 動畫迴圈,永遠不會 call 到
//!   `0x5A51A0`,玩家位置永遠不動。
//! - **Option 2** 直接 `call 0x5A51A0`(+ setter sprite_id / input flag / target tile):
//!   只 turn 不 walk — 內部 `[0x8DBC68 + action*4]` table 或 entity+0x14 同步 gate 還沒解開。
//!
//! 模擬左鍵交給 game 自己的 click handler `0x4F6CB0` 接管:它從 WM_LBUTTONDOWN lParam
//! 讀螢幕座標 → decode 成 tile → setter 全部 obfuscated globals → call `0x5A51A0`,
//! 100% 跑完整流程不需我們解內部 state 編碼。
//!
//! ## 為什麼 click-release → hold-and-move
//!
//! click-release 三連發每 tick 重 anchor 目標:`走幾格 → 停 → 走幾格 → 停`,
//! 視覺上抖動。 hold-and-move 按住不放,game 一路把玩家推往 cursor 位置,
//! cursor 持續更新方向 → 連續、平順、跟玩家手動按住滑鼠操作完全一致。
//!
//! `walk_step_toward_entity` 每 tick 呼叫一次,內部走 [`walk_hold`];
//! 切到「不需走路」分支前必須呼叫 [`walk_release`](由 hunt tick 負責)。
//!
//! ## 8-direction heading 表(對齊 server `l1j` `C_MoveChar.HEADING_TABLE_X/Y`)
//!
//! ```text
//! heading  delta(dx, dy)   羅盤
//! 0        ( 0, -1)        N
//! 1        ( 1, -1)        NE
//! 2        ( 1,  0)        E
//! 3        ( 1,  1)        SE
//! 4        ( 0,  1)        S
//! 5        (-1,  1)        SW
//! 6        (-1,  0)        W
//! 7        (-1, -1)        NW
//! ```

use anyhow::{bail, Result};
use once_cell::sync::Lazy;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HANDLE;

use crate::aux::address::G_PLAYER_PTR;
#[cfg(test)]
use crate::aux::input_sim::walk_hold;
use crate::aux::input_sim::{
    click_hold_at_player_anchor_pixel, walk_release as input_walk_release,
};
use crate::bot::perception::position::{decode_x, PlayerPosition};
use crate::logger::log_line;
use crate::memory::{read_bytes, read_u32};

const WALK_ACTOR_PTR: u32 = 0x00C2_B268;
const WALK_TARGET_ENTITY_PTR: u32 = 0x00C2_B264;
const WALK_PREPARED_TARGET_X: u32 = 0x009A_9C6C;
const WALK_PREPARED_TARGET_Y: u32 = 0x009A_9C70;
const WALK_MOUSE_X: u32 = 0x009A_9C74;
const WALK_MOUSE_Y: u32 = 0x009A_9C78;
const WALK_TARGET_VALID: u32 = 0x009A_93BF;
const WALK_TARGET_MODE: u32 = 0x009A_92FC;
const WALK_CONTINUE_FLAG: u32 = 0x00AC_24D4;
const WALK_FLAG_24D: u32 = 0x00C2_B24D;
const WALK_FLAG_24E: u32 = 0x00C2_B24E;
const WALK_FLAG_24F: u32 = 0x00C2_B24F;
const WALK_FLAG_272: u32 = 0x00C2_B272;
const WALK_FLAG_273: u32 = 0x00C2_B273;
const WALK_OBF_STATE_A: u32 = 0x00BD_A784;
const WALK_OBF_STATE_B: u32 = 0x00BD_A778;
const WALK_STATE_LOG_INTERVAL: Duration = Duration::from_millis(350);

static LAST_WALK_STATE_LOG: Lazy<Mutex<Option<Instant>>> = Lazy::new(|| Mutex::new(None));

/// 走路目標(顯示座標,跟 /loc 一致)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkDriver {
    PostMessage,
}

impl Default for WalkDriver {
    fn default() -> Self {
        Self::PostMessage
    }
}

impl Serialize for WalkDriver {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            Self::PostMessage => "post_message",
        })
    }
}

impl<'de> Deserialize<'de> for WalkDriver {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "post_message" => Ok(Self::PostMessage),
            // Retired experiment names are accepted only so old .bot.json files
            // still load. They no longer have enum variants or executable code.
            "client_click"
            | "memory_click"
            | "internal_walk"
            | "remote_internal_walk"
            | "move_packet" => Ok(Self::PostMessage),
            other => Err(de::Error::unknown_variant(other, &["post_message"])),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub struct WalkTarget {
    pub x: i32,
    pub y: i32,
}

/// 走一步往目標座標 — 計算 heading + PostMessage 模擬左鍵點地。
///
/// 失敗情境:
/// - 玩家已在目標(dx=0, dy=0)→ `bail!`(caller 應檢查距離,別呼叫無意義 step)
/// - heading 計算錯誤(理論上不會發生,有 unit test 守)
/// - PostMessage 出錯(視窗關閉 / handle 失效)
#[cfg(test)]
pub fn walk_step_toward(player: PlayerPosition, target: WalkTarget) -> Result<u8> {
    let dx = target.x - player.x;
    let dy = target.y - player.y;
    let heading = match heading_from_delta(dx, dy) {
        Some(h) => h,
        None => bail!("已在目標位置 ({}, {}),不需 walk", target.x, target.y),
    };
    walk_hold(heading)?;
    Ok(heading)
}

/// 釋放走路按住狀態 — 進入 cast / NoTarget / NotConfigured 等不需走路的分支前呼叫。
///
/// 重複呼叫安全(內部追蹤 held 狀態,沒按住就 no-op)。
pub fn walk_release() -> Result<()> {
    input_walk_release()
}

/// 走一步往指定 entity(raw_x + y)的方向。
///
/// 內部把 entity raw_x decode 成 display 後呼叫 [`walk_step_toward`]。
#[cfg(test)]
pub fn walk_step_toward_entity(
    player: PlayerPosition,
    entity_raw_x: u32,
    entity_y: u32,
) -> Result<u8> {
    walk_step_toward(
        player,
        WalkTarget {
            x: decode_x(entity_raw_x),
            y: entity_y as i32,
        },
    )
}

/// **引擎委派走路**(2026-05-14 Plan A)— 不算 heading,直接把 cursor 點到怪的
/// 螢幕像素位置,讓**遊戲引擎自己的尋路**(`0x4F6CB2` click handler →
/// `0x591000` movement manager)接管整段路徑。
///
/// ## 座標系是 Cartesian 不是 iso
///
/// 雖然 Lineage 3.8 視覺上是 iso 菱形 tile,但**遊戲 click handler 用 Cartesian
/// 解 cursor**(從現有 `walk_hold` HEADING_PIXEL_OFFSETS 反推):
///
/// | heading | world Δ | cursor offset | 推回 |
/// |---|---|---|---|
/// | 1 (NE) | dx=+1, dy=-1 | (+70, -70) | 純 Cartesian (+1, -1) × ~70 |
/// | 5 (SW) | dx=-1, dy=+1 | (-70, +70) | 純 Cartesian (-1, +1) × ~70 |
///
/// 加上 walk_handler_chain.md 實測「raw_x 0x8266→0x8264, y 0x826F→0x8271 = SW×2 格,
/// 跟 heading 5 cursor 對得上」確認:**`dx_pixel = dx_tile × 32`, `dy_pixel = dy_tile × 32`**。
///
/// 第一輪用 iso `(dx-dy)*16, (dx+dy)*8` 結果 bot 走錯方向(log 上鎖定 8 格 1 秒後變 11 格),
/// 那是因為「視覺 iso 投影 ≠ click 解碼座標系」。
///
/// ## 為什麼這比 walk_hold(heading) 強
///
/// `walk_hold(heading)` cursor 擺到 client 中心 + 固定 100px(8 方向),engine 只看
/// 到「user 想去 3 tile 外的空地」→ greedy 1 tile/tick → 撞牆停。
///
/// 這個函式把 cursor 擺到**怪的真實 tile 位置**(Cartesian 換算後),engine 看
/// 「user 想到 (X, Y) tile」→ 啟動自己的尋路(`0x591000` polymorphic dispatch 確認
/// engine 有 path queue)→ 自動繞牆 + per-step send 0x1D packet。
///
/// 超出 client viewport(怪在畫面外)時 `click_hold_at_pixel` 內部 clamp 到視窗邊界 —
/// engine 看 cursor 在邊緣方向,仍朝怪的方向走一格;下 tick 進入畫面後 cursor 就會點準。
#[cfg(test)]
pub fn walk_toward_entity_pixel(
    player: PlayerPosition,
    entity_raw_x: u32,
    entity_y: u32,
) -> Result<()> {
    let target_x = decode_x(entity_raw_x);
    let target_y = entity_y as i32;
    walk_toward_tile(player, target_x, target_y)
}

/// **引擎委派走路(A* waypoint 版)** — cursor 點到指定 tile(顯示座標)。
///
/// 跟 [`walk_toward_entity_pixel`] 的差別:這個吃**已 decode 的顯示座標**,通常是 A*
/// 路徑上的下個 waypoint。 hunt::try_walk 拿 A* 算好的 path 取下 N 個 waypoint 呼這個,
/// cursor 就點到「避開酒桶/牆角的中繼點」而不是怪的最終位置 — 引擎接 click handler
/// 解 cursor 位置 → 開始走 → 撞牆停的死循環解決。
///
/// 公式跟 entity_pixel 同:`PX_PER_TILE = 32`,Cartesian (cursor 偏移 dx/dy 直接對映
/// world tile dx/dy)。
pub fn walk_toward_tile(player: PlayerPosition, target_x: i32, target_y: i32) -> Result<()> {
    let dx_tile = target_x - player.x;
    let dy_tile = target_y - player.y;
    if dx_tile == 0 && dy_tile == 0 {
        bail!("已在目標 tile,不需走");
    }
    let (dx_pixel, dy_pixel) =
        crate::bot::action::screen_target::walk_drag_offset(player, target_x, target_y);
    log_line!(
        "[bot/coord] walk target=({}, {}) player=({}, {}) tile_delta=({}, {}) px_delta=({}, {})",
        target_x,
        target_y,
        player.x,
        player.y,
        dx_tile,
        dy_tile,
        dx_pixel,
        dy_pixel,
    );
    click_hold_at_player_anchor_pixel(dx_pixel, dy_pixel)
}

fn should_log_walk_state() -> bool {
    let now = Instant::now();
    let Ok(mut last) = LAST_WALK_STATE_LOG.lock() else {
        return true;
    };
    if let Some(previous) = *last {
        if now.duration_since(previous) < WALK_STATE_LOG_INTERVAL {
            return false;
        }
    }
    *last = Some(now);
    true
}

fn read_u32_opt(h: HANDLE, addr: u32) -> Option<u32> {
    read_u32(h, addr).ok()
}

fn read_u8_opt(h: HANDLE, addr: u32) -> Option<u8> {
    read_bytes(h, addr, 1)
        .ok()
        .and_then(|bytes| bytes.first().copied())
}

fn opt_hex32(value: Option<u32>) -> String {
    value
        .map(|value| format!("0x{value:08X}"))
        .unwrap_or_else(|| "?".to_string())
}

fn opt_hex8(value: Option<u8>) -> String {
    value
        .map(|value| format!("0x{value:02X}"))
        .unwrap_or_else(|| "?".to_string())
}

fn opt_dec32(value: Option<u32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".to_string())
}

fn opt_decoded_x(value: Option<u32>) -> String {
    value
        .map(|value| decode_x(value).to_string())
        .unwrap_or_else(|| "?".to_string())
}

fn log_walk_state(h: HANDLE, phase: &str, player: PlayerPosition, target_x: i32, target_y: i32) {
    let actor_ptr = read_u32_opt(h, WALK_ACTOR_PTR).filter(|value| *value != 0);
    let bot_player_ptr = read_u32_opt(h, G_PLAYER_PTR);
    let target_entity_ptr = read_u32_opt(h, WALK_TARGET_ENTITY_PTR);
    let prepared_x = read_u32_opt(h, WALK_PREPARED_TARGET_X);
    let prepared_y = read_u32_opt(h, WALK_PREPARED_TARGET_Y);
    let mouse_x = read_u32_opt(h, WALK_MOUSE_X);
    let mouse_y = read_u32_opt(h, WALK_MOUSE_Y);
    let target_valid = read_u8_opt(h, WALK_TARGET_VALID);
    let target_mode8 = read_u8_opt(h, WALK_TARGET_MODE);
    let target_mode32 = read_u32_opt(h, WALK_TARGET_MODE);
    let continue_flag = read_u8_opt(h, WALK_CONTINUE_FLAG);
    let flag_24d = read_u8_opt(h, WALK_FLAG_24D);
    let flag_24e = read_u8_opt(h, WALK_FLAG_24E);
    let flag_24f = read_u8_opt(h, WALK_FLAG_24F);
    let flag_272 = read_u8_opt(h, WALK_FLAG_272);
    let flag_273 = read_u8_opt(h, WALK_FLAG_273);
    let obf_a = read_u32_opt(h, WALK_OBF_STATE_A);
    let obf_b = read_u32_opt(h, WALK_OBF_STATE_B);

    let actor_action = actor_ptr.and_then(|ptr| read_u8_opt(h, ptr.wrapping_add(0x14)));
    let actor_dir = actor_ptr.and_then(|ptr| read_u8_opt(h, ptr.wrapping_add(0x15)));
    let actor_frame = actor_ptr.and_then(|ptr| read_u8_opt(h, ptr.wrapping_add(0x17)));
    let actor_raw_x = actor_ptr.and_then(|ptr| read_u32_opt(h, ptr.wrapping_add(0x34)));
    let actor_y = actor_ptr.and_then(|ptr| read_u32_opt(h, ptr.wrapping_add(0x38)));
    let actor_busy = actor_ptr.and_then(|ptr| read_u32_opt(h, ptr.wrapping_add(0x7C)));
    let actor_134 = actor_ptr.and_then(|ptr| read_u8_opt(h, ptr.wrapping_add(0x134)));

    log_line!(
        "[bot/walk_state] phase={} target=({}, {}) player=({}, {}) actor_ptr@00C2B268={} bot_player_ptr@00C2D2B8={} target_entity@00C2B264={} prepared@009A9C6C=({}, {}) mouse@009A9C74=({}, {}) valid@009A93BF={} mode@009A92FC={}/{} cont@00AC24D4={} flags@00C2B24D/B24E/B24F/B272/B273={}/{}/{}/{}/{} obf@00BDA784/778={}/{} actor(+14/+15/+17/+134)={}/{}/{}/{} actor_pos(raw_x/x/y)={}/{}/{} actor_busy@+7C={}",
        phase,
        target_x,
        target_y,
        player.x,
        player.y,
        opt_hex32(actor_ptr),
        opt_hex32(bot_player_ptr),
        opt_hex32(target_entity_ptr),
        opt_decoded_x(prepared_x),
        opt_dec32(prepared_y),
        opt_dec32(mouse_x),
        opt_dec32(mouse_y),
        opt_hex8(target_valid),
        opt_hex8(target_mode8),
        opt_hex32(target_mode32),
        opt_hex8(continue_flag),
        opt_hex8(flag_24d),
        opt_hex8(flag_24e),
        opt_hex8(flag_24f),
        opt_hex8(flag_272),
        opt_hex8(flag_273),
        opt_hex32(obf_a),
        opt_hex32(obf_b),
        opt_hex8(actor_action),
        opt_hex8(actor_dir),
        opt_hex8(actor_frame),
        opt_hex8(actor_134),
        opt_hex32(actor_raw_x),
        opt_decoded_x(actor_raw_x),
        opt_dec32(actor_y),
        opt_hex32(actor_busy),
    );
}

pub fn walk_toward_tile_with_driver(
    h: HANDLE,
    driver: WalkDriver,
    player: PlayerPosition,
    target_x: i32,
    target_y: i32,
) -> Result<()> {
    let driver_label = match driver {
        WalkDriver::PostMessage => "post_message",
    };
    log_line!(
        "[bot/walk] driver={} target=({}, {}) player=({}, {})",
        driver_label,
        target_x,
        target_y,
        player.x,
        player.y
    );
    let log_state = should_log_walk_state();
    if log_state {
        log_walk_state(h, "before", player, target_x, target_y);
    }
    let result = match driver {
        WalkDriver::PostMessage => walk_toward_tile(player, target_x, target_y),
    };
    if log_state {
        log_walk_state(h, "after", player, target_x, target_y);
    }
    result
}

/// 把 (dx, dy) 量化成 8-direction heading 0..7。 雙零(已在目標)回 None。
///
/// 對齊 server `l1j.../C_MoveChar.HEADING_TABLE_X/Y`:每個 heading 對應一個
/// (dx_sign, dy_sign) 組合,玩家送 packet 表示「我朝這方向走 1 格」。
pub fn heading_from_delta(dx: i32, dy: i32) -> Option<u8> {
    match (dx.signum(), dy.signum()) {
        (0, -1) => Some(0),
        (1, -1) => Some(1),
        (1, 0) => Some(2),
        (1, 1) => Some(3),
        (0, 1) => Some(4),
        (-1, 1) => Some(5),
        (-1, 0) => Some(6),
        (-1, -1) => Some(7),
        (0, 0) => None,
        _ => unreachable!("signum 只回 -1/0/1"),
    }
}

/// 計算兩個座標之間的曼哈頓距離(快速近似,夠用做「在不在範圍內」判斷)
#[cfg(test)]
pub fn manhattan_distance(a: PlayerPosition, b_x: i32, b_y: i32) -> u32 {
    a.x.abs_diff(b_x) + a.y.abs_diff(b_y)
}

/// Chebyshev distance — 8-direction 步距(對角線算 1 格),對齊遊戲移動 / 攻擊範圍邏輯。
#[cfg(test)]
pub fn chebyshev_distance(a: PlayerPosition, b_x: i32, b_y: i32) -> u32 {
    a.x.abs_diff(b_x).max(a.y.abs_diff(b_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manhattan_zero_at_same_point() {
        let pos = PlayerPosition { x: 33071, y: 33408 };
        assert_eq!(manhattan_distance(pos, 33071, 33408), 0);
    }

    #[test]
    fn manhattan_basic() {
        let pos = PlayerPosition { x: 100, y: 100 };
        assert_eq!(manhattan_distance(pos, 105, 100), 5);
        assert_eq!(manhattan_distance(pos, 100, 110), 10);
        assert_eq!(manhattan_distance(pos, 95, 95), 10);
    }

    #[test]
    fn chebyshev_diagonal_step_counts_one() {
        let pos = PlayerPosition { x: 100, y: 100 };
        assert_eq!(chebyshev_distance(pos, 110, 110), 10);
        assert_eq!(chebyshev_distance(pos, 105, 100), 5);
        assert_eq!(chebyshev_distance(pos, 100, 100), 0);
    }

    #[test]
    fn heading_all_8_directions() {
        assert_eq!(heading_from_delta(0, -1), Some(0)); // N
        assert_eq!(heading_from_delta(5, -3), Some(1)); // NE(任何正/負組合)
        assert_eq!(heading_from_delta(7, 0), Some(2)); // E
        assert_eq!(heading_from_delta(3, 8), Some(3)); // SE
        assert_eq!(heading_from_delta(0, 4), Some(4)); // S
        assert_eq!(heading_from_delta(-2, 9), Some(5)); // SW
        assert_eq!(heading_from_delta(-6, 0), Some(6)); // W
        assert_eq!(heading_from_delta(-1, -1), Some(7)); // NW
    }

    #[test]
    fn heading_zero_zero_returns_none() {
        assert_eq!(heading_from_delta(0, 0), None);
    }

    #[test]
    fn heading_quantizes_by_sign_not_magnitude() {
        // 不論 dx/dy 大小,只看正負號 — 因為一格只能往 8 個 unit 方向走
        assert_eq!(heading_from_delta(100, -100), Some(1));
        assert_eq!(heading_from_delta(1, -1), Some(1));
    }

    /// Cartesian 投影單元算 — 對齊 `aux::input_sim::HEADING_PIXEL_OFFSETS`
    /// (1 tile ≈ 32px),反推從 `walk_hold` 8 方向偏移 (±70, 0, ±70) ≈ 2.x tile。
    #[test]
    fn walk_driver_default_is_postmessage() {
        assert_eq!(WalkDriver::default(), WalkDriver::PostMessage);
    }

    #[test]
    fn retired_walk_driver_strings_load_as_postmessage_aliases() {
        for value in [
            "\"client_click\"",
            "\"memory_click\"",
            "\"internal_walk\"",
            "\"remote_internal_walk\"",
            "\"move_packet\"",
        ] {
            let parsed: WalkDriver = serde_json::from_str(value).expect("legacy alias loads");
            assert_eq!(parsed, WalkDriver::PostMessage);
        }
    }

    #[test]
    fn walk_dispatch_does_not_resize_lineage_window_each_step() {
        let source = include_str!("walk.rs");
        let production_source = source
            .split("#[cfg(test)]")
            .next()
            .expect("walk.rs production section");
        let guard_calls = production_source
            .matches("crate::aux::window_guard::guard_lineage_window_size();")
            .count();
        assert_eq!(
            guard_calls, 0,
            "walk dispatch must not resize the Lineage window on every step"
        );
    }

    #[test]
    fn walk_driver_does_not_queue_synthetic_window_clicks() {
        let source = include_str!("walk.rs");
        let production_source = source
            .split("#[cfg(test)]")
            .next()
            .expect("walk.rs production section");
        assert!(
            !production_source.contains("request_walk_tile_client_click"),
            "live walk dispatch must not enter a synthetic WM_LBUTTONDOWN/UP queue"
        );
    }

    #[test]
    fn cartesian_projection_8_directions_pixel_offsets() {
        const PX: i32 = 32;
        let cases: &[((i32, i32), (i32, i32))] = &[
            ((0, -1), (0, -PX)),    // N → 純上
            ((1, -1), (PX, -PX)),   // NE → 右上
            ((1, 0), (PX, 0)),      // E → 純右
            ((1, 1), (PX, PX)),     // SE → 右下
            ((0, 1), (0, PX)),      // S → 純下
            ((-1, 1), (-PX, PX)),   // SW → 左下
            ((-1, 0), (-PX, 0)),    // W → 純左
            ((-1, -1), (-PX, -PX)), // NW → 左上
        ];
        for &((dx, dy), (expected_dx, expected_dy)) in cases {
            let actual_dx = dx * PX;
            let actual_dy = dy * PX;
            assert_eq!(
                (actual_dx, actual_dy),
                (expected_dx, expected_dy),
                "cartesian projection wrong for tile ({dx}, {dy})"
            );
        }
    }

    #[test]
    fn cartesian_projection_scales_linearly_with_distance() {
        const PX: i32 = 32;
        // 5 tile NE → (5*32, -5*32) = (160, -160)
        assert_eq!((5 * PX, -5 * PX), (160, -160));
        // 10 tile E → (10*32, 0) = (320, 0)
        assert_eq!((10 * PX, 0 * PX), (320, 0));
        // 3 tile SW → (-3*32, 3*32) = (-96, 96)
        assert_eq!((-3 * PX, 3 * PX), (-96, 96));
    }
}
