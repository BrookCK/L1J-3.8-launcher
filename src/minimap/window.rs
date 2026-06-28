//! Minimap NWG top-level 視窗 — `show_minimap` spawn thread 跑 `run_window`,X 鈕關閉退出。

extern crate native_windows_derive as nwd;
extern crate native_windows_gui as nwg;

use std::cell::RefCell;
use std::sync::Arc;

use nwd::NwgUi;
use nwg::NativeUi;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Graphics::Gdi::InvalidateRect;

use crate::log_line;

use super::coord::Viewport;
use super::map_loader::Map;
use super::renderer;
use super::snapshot::{self, MapSnapshot};

/// tick refresh interval — 100ms 取代原 200ms,更新更平滑。
/// 配合 snapshot.rs 的 addr cache(1500ms rescan + per-tick 純 pos 讀)後,
/// 每 tick 純讀 ~60ms,100ms tick 內有餘裕。
const REFRESH_INTERVAL_MS: u64 = 100;

#[derive(Default, NwgUi)]
pub struct MinimapWindow {
    /// game process HANDLE raw — show_minimap 開窗時傳進來
    h_raw: std::cell::Cell<usize>,
    /// 最近一次 tick 抓的 snapshot
    last_snap: RefCell<MapSnapshot>,
    /// 當前 viewport(zoom / center / 大小)
    viewport: RefCell<Viewport>,
    /// 當前 map 解析結果(Arc 可跟 cache 共用)
    current_map: RefCell<Option<Arc<Map>>>,
    /// 自動 follow 玩家(true = 視窗中心 = 玩家);右鍵拖曳會關掉,F 鍵打開
    follow_player: std::cell::Cell<bool>,
    /// 右鍵拖曳起點 cursor pos(global screen pixels)— None 表示沒在拖
    drag_anchor: std::cell::Cell<Option<(i32, i32)>>,

    #[nwg_control(
        size: (400, 400),
        position: (400, 200),
        title: "小地圖",
        flags: "WINDOW|VISIBLE|RESIZABLE|MINIMIZE_BOX"
    )]
    #[nwg_events(
        OnWindowClose: [MinimapWindow::on_close],
        OnPaint: [MinimapWindow::on_paint],
        OnResize: [MinimapWindow::on_resize],
        OnMouseWheel: [MinimapWindow::on_wheel(SELF, EVT_DATA)],
        OnMousePress: [MinimapWindow::on_press(SELF, EVT)],
        OnMouseMove: [MinimapWindow::on_move],
        OnKeyRelease: [MinimapWindow::on_key(SELF, EVT_DATA)]
    )]
    window: nwg::Window,

    #[nwg_control(
        parent: window,
        interval: std::time::Duration::from_millis(REFRESH_INTERVAL_MS),
        active: true,
    )]
    #[nwg_events(OnTimerTick: [MinimapWindow::on_tick])]
    timer: nwg::AnimationTimer,
}

impl MinimapWindow {
    /// 不擦背景的 invalidate — 避免 200ms tick 重畫狂閃白底。 我們 paint 路徑
    /// 在 mem DC 整片填 walkable 色,不需要 Windows 預擦。
    fn redraw(&self) {
        let hwnd_isize = self.window.handle.hwnd().unwrap_or(0 as _);
        let hwnd = HWND(hwnd_isize as *mut _);
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }

    fn on_tick(&self) {
        let h = HANDLE(self.h_raw.get() as *mut _);
        let snap = snapshot::capture(h);

        // map_id 變了 → 從 cache 拿或重新 load
        let need_load = self
            .current_map
            .borrow()
            .as_ref()
            .map(|m| m.map_id != snap.map_id)
            .unwrap_or(true);
        if need_load && snap.map_id != 0 {
            let map_arc = match super::get_or_load_map(snap.map_id) {
                Ok(m) => m,
                Err(e) => {
                    log_line!("[minimap] load map {} failed: {e:#}", snap.map_id);
                    *self.current_map.borrow_mut() = None;
                    *self.last_snap.borrow_mut() = snap;
                    self.redraw();
                    return;
                }
            };
            log_line!(
                "[minimap] map {} 可用,{} blocks, bounds bx=[{}..{}] by=[{}..{}]",
                map_arc.map_id,
                map_arc.blocks.len(),
                map_arc.bounds.min_block_x,
                map_arc.bounds.max_block_x,
                map_arc.bounds.min_block_y,
                map_arc.bounds.max_block_y,
            );
            if let Some(p) = &snap.player {
                let (bx, by) = super::coord::tile_to_block(p.x, p.y as i32);
                let (lx, ly) = super::coord::tile_to_local(p.x, p.y as i32);
                let hit = map_arc.blocks.contains_key(&(bx, by));
                log_line!(
                    "[minimap] player tile=({}, {}) → block=({}, {}) local=({}, {}) block_hit={}",
                    p.x,
                    p.y,
                    bx,
                    by,
                    lx,
                    ly,
                    hit
                );
            }
            *self.current_map.borrow_mut() = Some(map_arc);
        }

        // follow:viewport center 跟著 player(被右鍵拖曳過就關掉,直到按 F)
        if self.follow_player.get() {
            if let Some(p) = &snap.player {
                let mut v = self.viewport.borrow_mut();
                v.center_tile_x = p.x;
                v.center_tile_y = p.y as i32;
            }
        }

        *self.last_snap.borrow_mut() = snap;
        self.redraw();
    }

    fn on_paint(&self) {
        let hwnd_isize = self.window.handle.hwnd().unwrap_or(0 as _);
        let hwnd = windows::Win32::Foundation::HWND(hwnd_isize as *mut _);
        let snap = self.last_snap.borrow();
        let viewport = self.viewport.borrow();
        let map = self.current_map.borrow();
        renderer::paint(hwnd, &viewport, map.as_ref(), &snap);
    }

    fn on_resize(&self) {
        let (w, h) = self.window.size();
        let mut v = self.viewport.borrow_mut();
        v.screen_w = w as i32;
        v.screen_h = h as i32;
        drop(v);
        self.redraw();
    }

    fn on_close(&self) {
        nwg::stop_thread_dispatch();
    }

    fn on_wheel(&self, data: &nwg::EventData) {
        if let nwg::EventData::OnMouseWheel(delta) = data {
            let mut v = self.viewport.borrow_mut();
            v.zoom = if *delta > 0 {
                (v.zoom * 1.25).min(16.0)
            } else {
                (v.zoom / 1.25).max(0.5)
            };
            drop(v);
            self.redraw();
        }
    }

    fn on_press(&self, evt: nwg::Event) {
        if let nwg::Event::OnMousePress(btn) = evt {
            match btn {
                nwg::MousePressEvent::MousePressRightDown => {
                    let pos = nwg::GlobalCursor::position();
                    self.drag_anchor.set(Some(pos));
                    self.follow_player.set(false);
                }
                nwg::MousePressEvent::MousePressRightUp => {
                    self.drag_anchor.set(None);
                }
                _ => {}
            }
        }
    }

    fn on_move(&self) {
        let Some((ax, ay)) = self.drag_anchor.get() else {
            return;
        };
        let (cx, cy) = nwg::GlobalCursor::position();
        let dx = cx - ax;
        let dy = cy - ay;
        if dx == 0 && dy == 0 {
            return;
        }
        let mut v = self.viewport.borrow_mut();
        // iso 旋轉後:拖曳像素 → tile delta 要走 screen_delta_to_tile 反變換,
        // 不再是 pixel/zoom 的線性除法。 negate 是因為「拖曳方向」= viewport
        // 反向移動(拖右下 → 顯示內容看起來往右下 → viewport 左上移)。
        let (tdx, tdy) = v.screen_delta_to_tile(dx, dy);
        let (dt_x, dt_y) = (-tdx, -tdy);
        if dt_x == 0 && dt_y == 0 {
            return;
        }
        v.center_tile_x += dt_x;
        v.center_tile_y += dt_y;
        drop(v);
        self.drag_anchor.set(Some((cx, cy)));
        self.redraw();
    }

    fn on_key(&self, data: &nwg::EventData) {
        if let nwg::EventData::OnKey(k) = data {
            // F = 重新跟隨玩家
            if *k == 'F' as u32 {
                self.follow_player.set(true);
                self.redraw();
            }
        }
    }
}

pub fn run_window(h_raw: usize) {
    if let Err(e) = nwg::init() {
        log_line!("[minimap] nwg init 失敗: {e:?}");
        return;
    }
    let mut font = nwg::Font::default();
    let _ = nwg::Font::builder()
        .family("Microsoft JhengHei UI")
        .size(14)
        .build(&mut font);
    nwg::Font::set_global_default(Some(font));

    let initial = MinimapWindow::default();
    initial.h_raw.set(h_raw);
    initial.follow_player.set(true); // 預設跟玩家,右鍵拖才會關

    let app = match MinimapWindow::build_ui(initial) {
        Ok(a) => a,
        Err(e) => {
            log_line!("[minimap] build_ui 失敗: {e:?}");
            return;
        }
    };
    // 取消 window class 的 bg brush — Windows 不再自動擦白底,避免 200ms tick 狂閃
    // 32-bit target 沒有 SetClassLongPtrW,用 SetClassLongW (i32 即可,0 = NULL HBRUSH)
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{SetClassLongW, GCL_HBRBACKGROUND};
        let hwnd_isize = app.window.handle.hwnd().unwrap_or(0 as _);
        let hwnd = HWND(hwnd_isize as *mut _);
        SetClassLongW(hwnd, GCL_HBRBACKGROUND, 0);
    }
    nwg::dispatch_thread_events();
    log_line!("[minimap] 視窗結束");
}
