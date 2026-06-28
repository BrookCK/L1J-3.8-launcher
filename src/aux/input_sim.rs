//! 模擬按鍵到遊戲視窗 — `/KEY=Fn` 與 `/DKEY=Fn` 共用入口。
//!
//! 兩種傳遞策略:
//! 1. **PostMessage**(目前實作):送 WM_KEYDOWN/WM_KEYUP 到遊戲視窗 message queue,
//!    遊戲在自己的 message pump 內 dispatch — 不需要 launcher 是 foreground。
//!    缺點:DirectInput / 低層 hook 抓不到(但天堂 client 用 WM_KEYDOWN 流不影響)。
//! 2. **SendInput**(備案,未啟用):全域 input event,需要 launcher 視窗 foreground。
//!
//! `delayed=true` (`/DKEY`) 在 down 與 up 之間插 ~80ms,模擬玩家「按住」效果,
//! 用於需要長按才生效的指令(例如某些連發魔法)。
//!
//! 視窗標題用 `find_game_window()` 找,跟 launcher 主程式同樣方式。

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(test)]
use anyhow::bail;
use anyhow::{anyhow, Result};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, PostMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};
#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::{WM_KEYDOWN, WM_KEYUP};

/// 模擬左鍵當前是否「按住中」 — `walk_hold` / `walk_release` 共享的狀態。
/// 同一時刻只會有 bot tick thread 操作走路,所以用 single global AtomicBool 即可。
static MOUSE_HELD: AtomicBool = AtomicBool::new(false);

/// 上一個 LBUTTONDOWN 的方向(0..7),`u8::MAX` = 還沒按過。 換方向時要重送 LBUTTONDOWN
/// 才能讓 game 重新 anchor 點擊目標。
static LAST_HEADING: AtomicU8 = AtomicU8::new(u8::MAX);

/// 上一次發 LBUTTONDOWN 的時間 — heartbeat 用,持續超過此 interval 重送一次。
///
/// **為什麼需要 heartbeat:**第一次 PostMessage LBUTTONDOWN 若在 game 還沒 focus 或
/// message queue 滿時被丟掉,`MOUSE_HELD=true` 會把所有後續 LBUTTONDOWN 鎖住 →
/// 玩家「永遠在送 MOUSEMOVE 但 client 不認為左鍵按住中」→ 永遠不會走路。
/// 每 ~1s 重送一次保證即使某次 PostMessage 掉訊也能在 1s 內恢復。
static LAST_DOWN_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// LBUTTONDOWN heartbeat 間隔。 太短會讓 game 每 tick re-anchor 路徑導致抖動,
/// 太長則首次掉訊後恢復太慢。 1000ms 是平衡值(玩家視覺看不出 stutter,1 秒內必復原)。
const LBUTTONDOWN_HEARTBEAT_MS: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GameplayInputArea {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl GameplayInputArea {
    pub(crate) fn clamp_point(self, x: i32, y: i32) -> (i32, i32) {
        (
            x.clamp(self.left, self.right),
            y.clamp(self.top, self.bottom),
        )
    }

    #[cfg(test)]
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

pub(crate) fn gameplay_input_dimensions(client_width: i32, client_height: i32) -> (i32, i32) {
    (client_width, client_height)
}

#[cfg(test)]
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

pub(crate) fn gameplay_player_anchor(client_width: i32, client_height: i32) -> (i32, i32) {
    let (client_width, client_height) = gameplay_input_dimensions(client_width, client_height);
    (client_width / 2, client_height * 40 / 100)
}

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

#[cfg(test)]
pub(crate) fn gameplay_drag_start_point_from_offset(
    client_width: i32,
    client_height: i32,
    dx_px: i32,
    dy_px: i32,
    release_dx: i32,
    release_dy: i32,
) -> Option<(i32, i32)> {
    let (client_width, client_height) = gameplay_input_dimensions(client_width, client_height);
    let area = gameplay_input_area(client_width, client_height)?;
    Some(area.clamp_drag_start(
        client_width / 2 + dx_px,
        client_height / 2 + dy_px,
        release_dx,
        release_dy,
    ))
}

#[cfg(test)]
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

fn client_size_for_hwnd(hwnd: HWND) -> Result<(i32, i32)> {
    let mut rect = RECT::default();
    unsafe {
        GetClientRect(hwnd, &mut rect).map_err(|e| anyhow!("GetClientRect 失敗: {e:#}"))?;
    }
    if rect.right <= 0 || rect.bottom <= 0 {
        return Err(anyhow!(
            "遊戲 client rect 無效: {}x{}",
            rect.right,
            rect.bottom
        ));
    }
    Ok((rect.right, rect.bottom))
}

/// `Fn` (n=1..12) 對應 Win32 VK code:VK_F1=0x70 ... VK_F12=0x7B
#[cfg(test)]
fn fkey_vk(n: u8) -> Option<u32> {
    if (1..=12).contains(&n) {
        Some(0x6F + n as u32) // VK_F1=0x70 即 0x6F+1
    } else {
        None
    }
}

/// 拿自家 game 的 HWND — 走 `aux::game_window` cache(多開安全)。
///
/// Why: 舊版 `FindWindowW(NULL, title)` 多開時回 topmost/focused 的視窗,user 切焦點
/// 到 game B 時 bot A 的 tick 就抓到 B → 「B 執行 A 的封包」(2026-05-17 user 回報)。
/// 改走 cache 後每個 launcher 鎖自己 game 的 HWND,焦點怎麼切都不會跑掉。
/// init 在 `main.rs::run_stage2` visible 後 wire。 cache miss 時 fallback FindWindowW
/// 保留舊行為(早期 boot 階段不破)。
fn find_game_window() -> Result<HWND> {
    crate::aux::game_window::cached_or_find_game_hwnd()
        .ok_or_else(|| anyhow!("找不到 Lineage 視窗"))
}

/// 對遊戲視窗模擬一次 Fn 按鍵。
///
/// `n`:1..12;`delayed`:true → down 與 up 間 sleep 80ms。
///
/// 失敗條件:
/// - `n` 不在範圍 → 立即 Err
/// - 視窗找不到 → Err
/// - PostMessage 失敗(視窗已關閉/handle 失效)→ Err
#[cfg(test)]
pub fn press_fkey(n: u8, delayed: bool) -> Result<()> {
    let vk = fkey_vk(n).ok_or_else(|| anyhow!("F{} 超出範圍 (僅支援 F1..F12)", n))?;
    let hwnd = find_game_window()?;

    // 構造 lParam:bit0..15=repeat=1, bit16..23=scancode(F1=0x3B..F12=0x58),
    // bit24=extended(0), bit29=context(0), bit30=prev_state, bit31=transition
    let scancode_base: u32 = match n {
        1..=10 => 0x3B + (n as u32 - 1),
        11 => 0x57,
        12 => 0x58,
        _ => 0,
    };
    let lparam_down: usize = 1 | (scancode_base << 16) as usize;
    let lparam_up: usize = lparam_down | (1 << 30) | (1 << 31);

    unsafe {
        PostMessageW(
            Some(hwnd),
            WM_KEYDOWN,
            WPARAM(vk as usize),
            LPARAM(lparam_down as isize),
        )
        .map_err(|e| anyhow!("PostMessageW WM_KEYDOWN F{n} 失敗: {e:#}"))?;
    }

    if delayed {
        std::thread::sleep(std::time::Duration::from_millis(80));
    }

    unsafe {
        PostMessageW(
            Some(hwnd),
            WM_KEYUP,
            WPARAM(vk as usize),
            LPARAM(lparam_up as isize),
        )
        .map_err(|e| anyhow!("PostMessageW WM_KEYUP F{n} 失敗: {e:#}"))?;
    }
    Ok(())
}

/// 計算 client-area 中心 + heading 方向 ~100px 的螢幕 pixel,組 PostMessage 用的 lParam。
#[cfg(test)]
fn heading_lparam(hwnd: HWND, heading: u8) -> Result<(LPARAM, HWND)> {
    /// 從 client-area 中心往 heading 方向偏的螢幕 pixel offset。 ~100px 確保落在隔壁 tile
    /// 而非當下 tile(Lineage 3.8 isometric tile ~ 32x16 px,100px 跨 3+ tile)。
    const HEADING_PIXEL_OFFSETS: [(i32, i32); 8] = [
        (0, -100),  // 0  N
        (70, -70),  // 1  NE
        (100, 0),   // 2  E
        (70, 70),   // 3  SE
        (0, 100),   // 4  S
        (-70, 70),  // 5  SW
        (-100, 0),  // 6  W
        (-70, -70), // 7  NW
    ];
    let (dx, dy) = HEADING_PIXEL_OFFSETS[heading as usize];

    let (client_width, client_height) = client_size_for_hwnd(hwnd)?;
    let (tx, ty) = gameplay_click_point_from_offset(client_width, client_height, dx, dy)
        .ok_or_else(|| anyhow!("遊戲 client rect 無效: {}x{}", client_width, client_height))?;
    // lParam: LOWORD=x, HIWORD=y(Win32 MAKELPARAM 規格)
    let lparam = ((ty as u32) << 16) | (tx as u32 & 0xFFFF);
    Ok((LPARAM(lparam as isize), hwnd))
}

/// 對遊戲視窗發「按住左鍵 + 移動到 heading 方向」 — 持續行走模式。
///
/// 行為:
/// - 首次呼叫(MOUSE_HELD=false):送 WM_LBUTTONDOWN 然後 WM_MOUSEMOVE,標記 held。
/// - 後續呼叫(MOUSE_HELD=true):只送 WM_MOUSEMOVE 更新 cursor pos,
///   讓 game 的 click handler 繼續把目的地推往新方向 — **不重新 LBUTTONDOWN**,
///   避免每次 click 都重 anchor 目標、玩家視覺上「走一步停一下」抖動。
///
/// 切換到別的動作(cast、無目標等)時必須呼叫 [`walk_release`] 釋放,否則 game 會
/// 一路走到 cursor 位置才停 → 失控。
///
/// **2026-05-13 第五輪 pivot**:從 walk_step(每 tick LBUTTONDOWN+UP 三連發)改成
/// hold-and-move。 前者 click-release-click 之間遊戲會中斷走路動畫;後者按住不放,
/// 走路連續、視覺平順、跟玩家手動操作完全一致。
///
/// **不影響使用者實體 cursor / launcher UI**:訊息只進 game HWND message queue,
/// launcher 自己的 BotWindow / 設定面板是獨立 HWND,兩條訊息流平行不衝突。
///
/// `heading` 對齊 `bot/action/walk.rs` 的 8-direction 表(0=N, 1=NE, ..., 7=NW)。
#[cfg(test)]
pub fn walk_hold(heading: u8) -> Result<()> {
    if heading >= 8 {
        bail!("walk_hold heading 必須 0..7, 收到 {heading}");
    }
    let hwnd = find_game_window()?;
    let (lparam, hwnd) = heading_lparam(hwnd, heading)?;

    /// MK_LBUTTON = 0x0001 — wParam modifier flag「左鍵按住中」
    const MK_LBUTTON: usize = 0x0001;

    // **不**在 heading 變動時 re-LBUTTONDOWN — 那會讓 game 每次 heading flicker (NE↔E,
    // 怪移動 1 px 就翻轉)時 re-anchor 路徑,玩家視覺看起來「繞 2-3 格才走對方向」。
    // 走路期間 game 看 MOUSEMOVE+MK_LBUTTON 就會持續更新拖曳目的地,LBUTTONDOWN 只需要:
    //   (a) 第一次按下(`MOUSE_HELD=false`)
    //   (b) heartbeat 到期(`> LBUTTONDOWN_HEARTBEAT_MS`),保險恢復「首次 LBUTTONDOWN 掉訊」
    //       這種 corner case(BotWindow 切焦點時 message queue 滿等)
    let need_down = {
        let heartbeat_expired = LAST_DOWN_AT.lock().unwrap().map_or(true, |t| {
            t.elapsed() >= Duration::from_millis(LBUTTONDOWN_HEARTBEAT_MS)
        });
        !MOUSE_HELD.load(Ordering::Relaxed) || heartbeat_expired
    };

    unsafe {
        if need_down {
            PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(MK_LBUTTON), lparam)
                .map_err(|e| anyhow!("PostMessage WM_LBUTTONDOWN 失敗: {e:#}"))?;
            MOUSE_HELD.store(true, Ordering::Relaxed);
            *LAST_DOWN_AT.lock().unwrap() = Some(Instant::now());
        }
        LAST_HEADING.store(heading, Ordering::Relaxed); // 純 diagnostic,不再觸發 LBUTTONDOWN
                                                        // MOUSEMOVE 每 tick 都送:wParam 帶 MK_LBUTTON 告訴 game「拖曳中」→ 持續更新目的地。
                                                        // 不會 re-anchor 路徑(因為沒新 LBUTTONDOWN),只是 cursor pos 變動 → 平順轉向。
        PostMessageW(Some(hwnd), WM_MOUSEMOVE, WPARAM(MK_LBUTTON), lparam)
            .map_err(|e| anyhow!("PostMessage WM_MOUSEMOVE 失敗: {e:#}"))?;
    }
    Ok(())
}

/// 釋放模擬左鍵 — 對應 [`walk_hold`] 的按住狀態。
///
/// 重複呼叫安全:若 MOUSE_HELD=false 就 no-op,不發 PostMessage。 caller(hunt tick)
/// 在「進入 cast 範圍」「無目標」「設定未填」這些不需走路的分支前都該呼叫一次。
/// 「按住左鍵 + cursor 在指定 client 像素 offset」 — 用於攻擊模式 hover-click 怪物。
///
/// 跟 [`walk_hold`] 的差別:
/// - `walk_hold(heading)` 用 heading 推 cursor 到 client 中心 +100px 方向(3 tile 外,過頭),
///   game 看成「拖往遠方走路」
/// - `click_hold_at_pixel(dx, dy)` 直接指定 cursor offset(玩家螢幕中心相對),通常算成
///   怪物在螢幕上的實際位置 → game 看成「按住左鍵 + cursor 在怪身上」→ 觸發攻擊 loop
///
/// **PX_PER_TILE ≈ 32**(從現有 walk heading 100px ≈ 3 tile 反推)。 caller 算 dx/dy 時用
/// `(monster.x - player.x) * 32`、`(monster.y - player.y) * 32`。
///
/// 跟 walk_hold 共用 [`MOUSE_HELD`] / [`LAST_DOWN_AT`] 狀態 — 從 walk 切到 attack 不需要
/// LBUTTONUP,只是 cursor 位置改變,game 自己會偵測「拖到了怪身上」切到攻擊模式。
#[cfg(test)]
pub fn click_hold_at_pixel(dx_px: i32, dy_px: i32) -> Result<()> {
    let hwnd = find_game_window()?;

    let (client_width, client_height) = client_size_for_hwnd(hwnd)?;
    let (tx, ty) = gameplay_click_point_from_offset(client_width, client_height, dx_px, dy_px)
        .ok_or_else(|| anyhow!("遊戲 client rect 無效: {}x{}", client_width, client_height))?;
    let lparam = LPARAM((((ty as u32) << 16) | (tx as u32 & 0xFFFF)) as isize);

    const MK_LBUTTON: usize = 0x0001;

    // 跟 walk_hold 同樣的 heartbeat 策略:首次按下或 1s 沒按過才重送 LBUTTONDOWN,
    // 避免每 tick re-anchor 造成 game 重新解析點擊目標(可能誤判從攻擊變成走路)。
    let need_down = {
        let heartbeat_expired = LAST_DOWN_AT.lock().unwrap().map_or(true, |t| {
            t.elapsed() >= Duration::from_millis(LBUTTONDOWN_HEARTBEAT_MS)
        });
        !MOUSE_HELD.load(Ordering::Relaxed) || heartbeat_expired
    };

    unsafe {
        if need_down {
            PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(MK_LBUTTON), lparam)
                .map_err(|e| anyhow!("PostMessage WM_LBUTTONDOWN 失敗: {e:#}"))?;
            MOUSE_HELD.store(true, Ordering::Relaxed);
            *LAST_DOWN_AT.lock().unwrap() = Some(Instant::now());
        }
        // MOUSEMOVE 每 tick 都送,wParam=MK_LBUTTON → game 看到「拖曳中」+「cursor 在怪身上」,
        // 切到攻擊模式自己跑 attack loop(動畫 + 自送 C_ATTACK packet,跟玩家手動按住一樣)。
        PostMessageW(Some(hwnd), WM_MOUSEMOVE, WPARAM(MK_LBUTTON), lparam)
            .map_err(|e| anyhow!("PostMessage WM_MOUSEMOVE 失敗: {e:#}"))?;
    }
    Ok(())
}

pub fn click_hold_at_player_anchor_pixel(dx_px: i32, dy_px: i32) -> Result<()> {
    let hwnd = find_game_window()?;

    let (client_width, client_height) = client_size_for_hwnd(hwnd)?;
    let (tx, ty) =
        gameplay_click_point_from_player_anchor_offset(client_width, client_height, dx_px, dy_px)
            .ok_or_else(|| anyhow!("遊戲 client rect 無效: {}x{}", client_width, client_height))?;
    let lparam = LPARAM((((ty as u32) << 16) | (tx as u32 & 0xFFFF)) as isize);

    const MK_LBUTTON: usize = 0x0001;

    let need_down = {
        let heartbeat_expired = LAST_DOWN_AT.lock().unwrap().map_or(true, |t| {
            t.elapsed() >= Duration::from_millis(LBUTTONDOWN_HEARTBEAT_MS)
        });
        !MOUSE_HELD.load(Ordering::Relaxed) || heartbeat_expired
    };

    unsafe {
        if need_down {
            PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(MK_LBUTTON), lparam)
                .map_err(|e| anyhow!("PostMessage WM_LBUTTONDOWN 失敗: {e:#}"))?;
            MOUSE_HELD.store(true, Ordering::Relaxed);
            *LAST_DOWN_AT.lock().unwrap() = Some(Instant::now());
        }
        PostMessageW(Some(hwnd), WM_MOUSEMOVE, WPARAM(MK_LBUTTON), lparam)
            .map_err(|e| anyhow!("PostMessage WM_MOUSEMOVE 失敗: {e:#}"))?;
    }
    Ok(())
}

pub fn walk_release() -> Result<()> {
    if !MOUSE_HELD.swap(false, Ordering::Relaxed) {
        return Ok(()); // 沒按住 → 不需 release
    }
    LAST_HEADING.store(u8::MAX, Ordering::Relaxed);
    *LAST_DOWN_AT.lock().unwrap() = None;
    let hwnd = find_game_window()?;
    unsafe {
        // lParam = 0(座標不重要,只是表示 button up 事件)
        PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(0))
            .map_err(|e| anyhow!("PostMessage WM_LBUTTONUP 失敗: {e:#}"))?;
    }
    Ok(())
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
    fn walk_hold_rejects_out_of_range_heading() {
        // 不依賴遊戲視窗存在,因為 heading check 在 find_game_window 之前
        assert!(walk_hold(8).is_err());
        assert!(walk_hold(255).is_err());
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
            "bot input should follow the actual client size instead of forcing 800x600"
        );
        assert_eq!(
            gameplay_click_point_from_offset(1904, 1041, 10_000, 10_000),
            Some((1427, 863)),
            "bot input must stay inside the actual gameplay area"
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
    fn player_anchor_tracks_actual_client_size() {
        assert_eq!(gameplay_player_anchor(800, 600), (400, 240));
        assert_eq!(gameplay_player_anchor(1904, 1041), (952, 416));
        assert_eq!(
            gameplay_click_point_from_player_anchor_offset(800, 600, 0, -32),
            Some((400, 208))
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
