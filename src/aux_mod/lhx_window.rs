//! LinHelperZ 8-tab 輔助視窗(GUI 骨架階段)
//!
//! 設計參考:docs/superpowers/specs/2026-04-27-lhx-window-skeleton-design.md
//! UI 條列:docs/lhx-design.md
//!
//! 視窗在獨立 thread 跑(NWG event loop block),跟 run_home_key_listener
//! 透過 Arc<AtomicU8> visible flag + Arc<RwLock<AuxSettings>> 共享狀態。

extern crate native_windows_derive as nwd;
extern crate native_windows_gui as nwg;

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use windows::Win32::Foundation::{BOOL, HANDLE, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::MapWindowPoints;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetSystemMetrics, GetWindowRect, SetWindowPos, SM_CXSCREEN, SM_CYSCREEN,
    SWP_NOACTIVATE, SWP_NOZORDER,
};

use nwd::NwgUi;
use nwg::NativeUi;
use parking_lot::RwLock;

use crate::aux::runtime::AuxSettings;
use crate::log_line;

/// visible flag 狀態值
pub const VISIBLE_HIDDEN: u8 = 0;
pub const VISIBLE_SHOWN: u8 = 1;
pub const VISIBLE_CLOSE: u8 = 2;
const APP_ICON_BYTES: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/app.ico"));
const LHX_BG: [u8; 3] = [255, 255, 255];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LhxTabLayout {
    window_size: (u32, u32),
    tabs_size: (u32, u32),
}

fn lhx_tab_layout(tab_index: usize) -> LhxTabLayout {
    match tab_index {
        // 喝水
        0 => LhxTabLayout {
            window_size: (420, 395),
            tabs_size: (410, 360),
        },
        // 輔助
        1 => LhxTabLayout {
            window_size: (485, 380),
            tabs_size: (475, 345),
        },
        // 狀態
        2 => LhxTabLayout {
            window_size: (455, 430),
            tabs_size: (445, 395),
        },
        // 刪物
        3 => LhxTabLayout {
            window_size: (485, 430),
            tabs_size: (475, 395),
        },
        // 喊話
        4 => LhxTabLayout {
            window_size: (485, 385),
            tabs_size: (475, 350),
        },
        // 其他
        5 => LhxTabLayout {
            window_size: (350, 270),
            tabs_size: (340, 235),
        },
        // 定時
        _ => LhxTabLayout {
            window_size: (485, 430),
            tabs_size: (475, 395),
        },
    }
}

fn scale_px(value: u32, scale: f32) -> u32 {
    ((value as f32) * scale).round().max(1.0) as u32
}

fn scale_i32(value: i32, scale: f32) -> i32 {
    ((value as f32) * scale).round() as i32
}

fn lhx_visual_scale(dpi: u32, screen_width: i32, screen_height: i32) -> f32 {
    let dpi_scale = if dpi >= 96 { dpi as f32 / 96.0 } else { 1.0 };
    let capped_dpi_scale = dpi_scale.clamp(1.0, 1.25);
    let resolution_floor: f32 = if screen_width >= 3840 || screen_height >= 2160 {
        1.35
    } else if screen_width >= 3000 || screen_height >= 1700 {
        1.25
    } else {
        1.0
    };

    capped_dpi_scale.max(resolution_floor).clamp(1.0, 1.35)
}

fn current_lhx_visual_scale() -> f32 {
    unsafe {
        lhx_visual_scale(
            GetDpiForSystem(),
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
        )
    }
}

fn lhx_font_size_for_scale(scale: f32) -> u32 {
    scale_i32(15, scale).clamp(15, 22) as u32
}

fn scaled_lhx_tab_layout(tab_index: usize, scale: f32) -> LhxTabLayout {
    let base = lhx_tab_layout(tab_index);
    LhxTabLayout {
        window_size: (
            scale_px(base.window_size.0, scale),
            scale_px(base.window_size.1, scale),
        ),
        tabs_size: (
            scale_px(base.tabs_size.0, scale),
            scale_px(base.tabs_size.1, scale),
        ),
    }
}

struct ChildScaleContext {
    parent: HWND,
    scale: f32,
}

unsafe extern "system" fn scale_lhx_child_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &*(lparam.0 as *const ChildScaleContext);
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_ok() {
        let mut points = [
            POINT {
                x: rect.left,
                y: rect.top,
            },
            POINT {
                x: rect.right,
                y: rect.bottom,
            },
        ];
        MapWindowPoints(None, Some(ctx.parent), &mut points);
        let x = scale_i32(points[0].x, ctx.scale);
        let y = scale_i32(points[0].y, ctx.scale);
        let width = scale_i32(points[1].x - points[0].x, ctx.scale).max(1);
        let height = scale_i32(points[1].y - points[0].y, ctx.scale).max(1);
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    BOOL(1)
}

fn scale_lhx_child_controls(window: &nwg::Window, scale: f32) {
    if scale <= 1.05 {
        return;
    }
    if let Some(raw_hwnd) = window.handle.hwnd() {
        let hwnd = HWND(raw_hwnd as *mut _);
        let mut ctx = ChildScaleContext {
            parent: hwnd,
            scale,
        };
        unsafe {
            let _ = EnumChildWindows(
                Some(hwnd),
                Some(scale_lhx_child_window),
                LPARAM((&mut ctx as *mut ChildScaleContext) as isize),
            );
        }
    }
}

fn should_clear_lhx_topmost_after_owner_attached(has_game_owner: bool) -> bool {
    has_game_owner
}

fn should_attach_lhx_to_game_owner() -> bool {
    false
}

fn desired_lhx_visibility(visible_flag: u8, game_minimized: bool) -> Option<bool> {
    match visible_flag {
        VISIBLE_HIDDEN => Some(false),
        VISIBLE_SHOWN => Some(!game_minimized),
        VISIBLE_CLOSE => None,
        _ => None,
    }
}

fn combo_dropdown_visible_rows(item_count: usize) -> usize {
    item_count.clamp(1, 50)
}

fn delete_combo_dropdown_visible_rows(item_count: usize) -> usize {
    combo_dropdown_visible_rows(item_count)
}

fn set_combo_dropdown_visible_rows(combo: &nwg::ComboBox<String>, item_count: usize) {
    set_combo_dropdown_visible_rows_to(combo, combo_dropdown_visible_rows(item_count));
}

fn set_delete_combo_dropdown_visible_rows(combo: &nwg::ComboBox<String>, item_count: usize) {
    set_combo_dropdown_visible_rows_to(combo, delete_combo_dropdown_visible_rows(item_count));
}

fn delete_list_entry_name(name: &str) -> String {
    strip_qty(name).to_string()
}

fn set_combo_dropdown_visible_rows_to(combo: &nwg::ComboBox<String>, rows: usize) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

    const CB_SETMINVISIBLE: u32 = 0x1701;
    if let Some(hwnd) = combo.handle.hwnd() {
        unsafe {
            let h = HWND(hwnd as *mut _);
            let _ = SendMessageW(h, CB_SETMINVISIBLE, Some(WPARAM(rows)), Some(LPARAM(0)));
        }
    }
}

fn enable_listbox_vertical_scroll(listbox: &nwg::ListBox<String>) {
    if let Some(raw) = listbox.handle.hwnd() {
        unsafe { add_ws_vscroll_to_hwnd(raw as *mut _) };
    }
}

unsafe fn add_ws_vscroll_to_hwnd(raw: *mut std::ffi::c_void) {
    if raw.is_null() {
        return;
    }
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, SetWindowLongW, SetWindowPos, GWL_STYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_VSCROLL,
    };

    let h = HWND(raw);
    let style = GetWindowLongW(h, GWL_STYLE);
    let new_style = style | WS_VSCROLL.0 as i32;
    if new_style != style {
        let _ = SetWindowLongW(h, GWL_STYLE, new_style);
        let _ = SetWindowPos(
            h,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

/// 防止 WM_MOUSEWHEEL 轉發迴圈:child 被 SendMessage 之後若沒處理掉,
/// DefWindowProc 會把 WM_MOUSEWHEEL bubble 回 parent,parent subclass 又會
/// 再轉發出去 → 死迴圈。用 thread-local AtomicBool 標記 reentry 中時直接放行。
static IN_WHEEL_FORWARD: AtomicBool = AtomicBool::new(false);

/// Parent dialog 的 WM_MOUSEWHEEL subclass:把訊息轉發到游標下的子視窗。
///
/// **為什麼要這麼做** — Windows 預設行為是 WM_MOUSEWHEEL 只送給「focused window」,
/// 不一定是游標下那個。nwg 又把 dialog 設為 focused,所以就算游標放在 ComboBox
/// dropdown 上,wheel 訊息還是丟到 dialog,dropdown 完全收不到 → 看起來像「滾輪沒反應」。
///
/// 修法:dialog 收到 WM_MOUSEWHEEL 時用 `WindowFromPoint(cursor)` 找出真正在游標下
/// 的子視窗(ComboBox / 內部 dropdown listbox / ListBox 等),把訊息原樣轉過去。
unsafe extern "system" fn wheel_forward_subclass_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
    _id: usize,
    _data: usize,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::Shell::DefSubclassProc;
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WindowFromPoint, WM_MOUSEWHEEL};

    if msg == WM_MOUSEWHEEL && !IN_WHEEL_FORWARD.swap(true, Ordering::SeqCst) {
        // lparam 高 16 / 低 16 = 游標 y / x(螢幕座標,signed)
        let raw = lparam.0 as i32;
        let x = (raw & 0xFFFF) as i16 as i32;
        let y = ((raw >> 16) & 0xFFFF) as i16 as i32;
        let pt = POINT { x, y };
        let target = WindowFromPoint(pt);
        let result = if !target.is_invalid() && target.0 != hwnd.0 {
            SendMessageW(target, WM_MOUSEWHEEL, Some(wparam), Some(lparam))
        } else {
            DefSubclassProc(hwnd, msg, wparam, lparam)
        };
        IN_WHEEL_FORWARD.store(false, Ordering::SeqCst);
        return result;
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

fn install_wheel_forwarding(window_hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::Shell::SetWindowSubclass;
    unsafe {
        let _ = SetWindowSubclass(window_hwnd, Some(wheel_forward_subclass_proc), 1, 0);
    }
}

/// ComboBox 內部 dropdown listbox 的 WM_MOUSEWHEEL subclass。
///
/// **為什麼需要** — ComboBox 展開後,dropdown popup 是獨立的 top-level WS_POPUP
/// 視窗,不在 dialog 的子視窗鏈上。Parent dialog 的 wheel forwarding 收不到,
/// 也無法轉發給它。Popup 本身在 Win10/11 預設「捲動非作用中視窗」開啟時會直接
/// 收到 WM_MOUSEWHEEL,但實測對 ComboLBox 並不會自動捲動(可能因為它不是
/// real listbox 而是 ComboBox 內部特殊 class) — 所以這裡明確攔截 WM_MOUSEWHEEL,
/// 轉成 WM_VSCROLL SB_LINEDOWN/UP 自己發給自己,強制捲動。
unsafe extern "system" fn dropdown_listbox_wheel_subclass(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
    _id: usize,
    _data: usize,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{LPARAM as FLPARAM, LRESULT, WPARAM as FWPARAM};
    use windows::Win32::UI::Shell::DefSubclassProc;
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageW, SB_LINEDOWN, SB_LINEUP, WM_MOUSEWHEEL, WM_VSCROLL,
    };

    if msg == WM_MOUSEWHEEL {
        // wparam 高 16 = wheel delta(signed,正 = 向上滾,負 = 向下滾)
        let delta = ((wparam.0 >> 16) as i16) as i32;
        if delta != 0 {
            // 一個 wheel notch = WHEEL_DELTA(120) → 預設 3 行/notch,跟標準一致
            let notches = (delta.abs() / 120).max(1);
            let cmd = if delta > 0 { SB_LINEUP } else { SB_LINEDOWN };
            for _ in 0..(notches * 3) {
                SendMessageW(
                    hwnd,
                    WM_VSCROLL,
                    Some(FWPARAM(cmd.0 as usize)),
                    Some(FLPARAM(0)),
                );
            }
            return LRESULT(0);
        }
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

fn install_combo_dropdown_wheel(combo: &nwg::ComboBox<String>) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Controls::{GetComboBoxInfo, COMBOBOXINFO};
    use windows::Win32::UI::Shell::SetWindowSubclass;

    if let Some(raw) = combo.handle.hwnd() {
        unsafe {
            let h = HWND(raw as *mut _);
            let mut cbi = COMBOBOXINFO {
                cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
                ..Default::default()
            };
            if GetComboBoxInfo(h, &mut cbi).is_ok() && !cbi.hwndList.is_invalid() {
                let _ =
                    SetWindowSubclass(cbi.hwndList, Some(dropdown_listbox_wheel_subclass), 2, 0);
            }
        }
    }
}

pub(crate) fn load_app_icon() -> Option<nwg::Icon> {
    let mut icon = nwg::Icon::default();
    match nwg::Icon::builder()
        .source_bin(Some(APP_ICON_BYTES))
        .build(&mut icon)
    {
        Ok(()) => Some(icon),
        Err(e) => {
            log_line!("[icon] 載入 app icon 失敗: {e:?}");
            None
        }
    }
}

/// 對外 control handle — 由 home key listener 持有
pub struct WindowControl {
    pub visible: Arc<AtomicU8>,
    pub thread: JoinHandle<()>,
    /// 遊戲 HANDLE(以 usize 儲存以便 Send),由 main 設定。
    /// 視窗 thread 用它讀背包做 dropdown 同步。
    pub game_handle: Arc<AtomicUsize>,
}

#[derive(Default, NwgUi)]
pub struct LhxWindow {
    #[nwg_control(
        size: (520, 460),
        position: (300, 200),
        title: "LinHelperZ",
        topmost: true,
        flags: "WINDOW|MINIMIZE_BOX"
    )]
    #[nwg_events(OnWindowClose: [LhxWindow::on_close])]
    window: nwg::Window,

    #[nwg_control(parent: window, position: (5, 5), size: (410, 360))]
    #[nwg_events(TabsContainerChanged: [LhxWindow::on_tab_changed])]
    tabs: nwg::TabsContainer,

    #[nwg_control(parent: tabs, text: "喝水")]
    tab_potion: nwg::Tab,

    #[nwg_control(parent: tabs, text: "輔助")]
    tab_buff: nwg::Tab,

    #[nwg_control(parent: tabs, text: "狀態")]
    tab_status: nwg::Tab,

    #[nwg_control(parent: tabs, text: "刪物")]
    tab_delete: nwg::Tab,

    #[nwg_control(parent: tabs, text: "喊話")]
    tab_shout: nwg::Tab,

    #[nwg_control(parent: tabs, text: "其他")]
    tab_misc: nwg::Tab,

    #[nwg_control(parent: tabs, text: "定時")]
    tab_timer: nwg::Tab,

    #[nwg_control(parent: window, interval: std::time::Duration::from_millis(50), active: false)]
    #[nwg_events(OnTimerTick: [LhxWindow::on_visible_tick])]
    visible_timer: nwg::AnimationTimer,

    // 背包 dropdown 即時更新 timer — 勾「顯示背包道具」時每 500ms 重抓一次背包,
    // 但只在 dropdown 沒展開時更新(展開時 user 正在挑,不能打斷)。
    // 一直 active,內部判斷 settings.potion_show_inventory 才實際做事。
    #[nwg_control(parent: window, interval: std::time::Duration::from_millis(500), active: true)]
    #[nwg_events(OnTimerTick: [LhxWindow::on_inv_refresh_tick])]
    inv_refresh_timer: nwg::AnimationTimer,

    // ════════════ tab1 喝水:7 row + mp_when_safe + 2 toggle ════════════

    // ── row 0 ──
    #[nwg_control(parent: tab_potion, text: "HP小於", position: (15, 18), size: (60, 22),
                  background_color: Some(LHX_BG))]
    potion_lbl_0: nwg::Label,
    #[nwg_control(parent: tab_potion, text: "0", position: (80, 16), size: (50, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_potion_change])]
    potion_num_0: nwg::TextInput,
    #[nwg_control(parent: tab_potion, position: (140, 16), size: (220, 22),
                  collection: vec!["（測試項目1）".to_string(), "（測試項目2）".to_string()],
                  selected_index: Some(0))]
    #[nwg_events(OnComboxBoxSelection: [LhxWindow::on_potion_change])]
    potion_combo_0: nwg::ComboBox<String>,
    #[nwg_control(parent: tab_potion, text: "", position: (370, 16), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_potion_change])]
    potion_cb_0: nwg::CheckBox,

    // ── row 1 ──
    #[nwg_control(parent: tab_potion, text: "HP小於", position: (15, 46), size: (60, 22),
                  background_color: Some(LHX_BG))]
    potion_lbl_1: nwg::Label,
    #[nwg_control(parent: tab_potion, text: "0", position: (80, 44), size: (50, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_potion_change])]
    potion_num_1: nwg::TextInput,
    #[nwg_control(parent: tab_potion, position: (140, 44), size: (220, 22),
                  collection: vec!["（測試項目1）".to_string()],
                  selected_index: Some(0))]
    #[nwg_events(OnComboxBoxSelection: [LhxWindow::on_potion_change])]
    potion_combo_1: nwg::ComboBox<String>,
    #[nwg_control(parent: tab_potion, text: "", position: (370, 44), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_potion_change])]
    potion_cb_1: nwg::CheckBox,

    // ── row 2 ──
    #[nwg_control(parent: tab_potion, text: "HP小於", position: (15, 74), size: (60, 22),
                  background_color: Some(LHX_BG))]
    potion_lbl_2: nwg::Label,
    #[nwg_control(parent: tab_potion, text: "0", position: (80, 72), size: (50, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_potion_change])]
    potion_num_2: nwg::TextInput,
    #[nwg_control(parent: tab_potion, position: (140, 72), size: (220, 22),
                  collection: vec!["（測試項目1）".to_string()],
                  selected_index: Some(0))]
    #[nwg_events(OnComboxBoxSelection: [LhxWindow::on_potion_change])]
    potion_combo_2: nwg::ComboBox<String>,
    #[nwg_control(parent: tab_potion, text: "", position: (370, 72), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_potion_change])]
    potion_cb_2: nwg::CheckBox,

    // ── row 3 ──
    #[nwg_control(parent: tab_potion, text: "HP小於", position: (15, 102), size: (60, 22),
                  background_color: Some(LHX_BG))]
    potion_lbl_3: nwg::Label,
    #[nwg_control(parent: tab_potion, text: "0", position: (80, 100), size: (50, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_potion_change])]
    potion_num_3: nwg::TextInput,
    #[nwg_control(parent: tab_potion, position: (140, 100), size: (220, 22),
                  collection: vec!["（測試項目1）".to_string()],
                  selected_index: Some(0))]
    #[nwg_events(OnComboxBoxSelection: [LhxWindow::on_potion_change])]
    potion_combo_3: nwg::ComboBox<String>,
    #[nwg_control(parent: tab_potion, text: "", position: (370, 100), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_potion_change])]
    potion_cb_3: nwg::CheckBox,

    // ── row 4 ──
    #[nwg_control(parent: tab_potion, text: "HP小於", position: (15, 130), size: (60, 22),
                  background_color: Some(LHX_BG))]
    potion_lbl_4: nwg::Label,
    #[nwg_control(parent: tab_potion, text: "0", position: (80, 128), size: (50, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_potion_change])]
    potion_num_4: nwg::TextInput,
    #[nwg_control(parent: tab_potion, position: (140, 128), size: (220, 22),
                  collection: vec!["（測試項目1）".to_string()],
                  selected_index: Some(0))]
    #[nwg_events(OnComboxBoxSelection: [LhxWindow::on_potion_change])]
    potion_combo_4: nwg::ComboBox<String>,
    #[nwg_control(parent: tab_potion, text: "", position: (370, 128), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_potion_change])]
    potion_cb_4: nwg::CheckBox,

    // ── row 5 ──
    #[nwg_control(parent: tab_potion, text: "HP小於", position: (15, 158), size: (60, 22),
                  background_color: Some(LHX_BG))]
    potion_lbl_5: nwg::Label,
    #[nwg_control(parent: tab_potion, text: "0", position: (80, 156), size: (50, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_potion_change])]
    potion_num_5: nwg::TextInput,
    #[nwg_control(parent: tab_potion, position: (140, 156), size: (220, 22),
                  collection: vec!["（測試項目1）".to_string()],
                  selected_index: Some(0))]
    #[nwg_events(OnComboxBoxSelection: [LhxWindow::on_potion_change])]
    potion_combo_5: nwg::ComboBox<String>,
    #[nwg_control(parent: tab_potion, text: "", position: (370, 156), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_potion_change])]
    potion_cb_5: nwg::CheckBox,

    // ── row 6 ──
    #[nwg_control(parent: tab_potion, text: "HP小於", position: (15, 186), size: (60, 22),
                  background_color: Some(LHX_BG))]
    potion_lbl_6: nwg::Label,
    #[nwg_control(parent: tab_potion, text: "0", position: (80, 184), size: (50, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_potion_change])]
    potion_num_6: nwg::TextInput,
    #[nwg_control(parent: tab_potion, position: (140, 184), size: (220, 22),
                  collection: vec!["（測試項目1）".to_string()],
                  selected_index: Some(0))]
    #[nwg_events(OnComboxBoxSelection: [LhxWindow::on_potion_change])]
    potion_combo_6: nwg::ComboBox<String>,
    #[nwg_control(parent: tab_potion, text: "", position: (370, 184), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_potion_change])]
    potion_cb_6: nwg::CheckBox,

    // ── 洗魔 mp_when_safe ──
    #[nwg_control(parent: tab_potion, text: "洗魔 HP大於", position: (15, 218), size: (90, 22),
                  background_color: Some(LHX_BG))]
    mp_safe_lbl1: nwg::Label,
    #[nwg_control(parent: tab_potion, text: "0", position: (110, 216), size: (50, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_potion_change])]
    mp_safe_hp_num: nwg::TextInput,
    #[nwg_control(parent: tab_potion, text: "及 MP小於", position: (15, 246), size: (90, 22),
                  background_color: Some(LHX_BG))]
    mp_safe_lbl2: nwg::Label,
    #[nwg_control(parent: tab_potion, text: "0", position: (110, 244), size: (50, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_potion_change])]
    mp_safe_mp_num: nwg::TextInput,
    #[nwg_control(parent: tab_potion, position: (170, 230), size: (190, 22),
                  collection: vec!["（測試項目1）".to_string()],
                  selected_index: Some(0))]
    #[nwg_events(OnComboxBoxSelection: [LhxWindow::on_potion_change])]
    mp_safe_combo: nwg::ComboBox<String>,
    #[nwg_control(parent: tab_potion, text: "", position: (370, 230), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_potion_change])]
    mp_safe_cb: nwg::CheckBox,

    // ── 兩個 toggle ──
    #[nwg_control(parent: tab_potion, text: "使用百分比(%)判斷HP以及MP",
                  position: (15, 280), size: (260, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_potion_change])]
    potion_use_percent_cb: nwg::CheckBox,

    #[nwg_control(parent: tab_potion, text: "顯示背包道具",
                  position: (15, 305), size: (180, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_show_inv_toggle])]
    potion_show_inv_cb: nwg::CheckBox,

    // ════════════ tab2 輔助 ════════════
    #[nwg_control(parent: tab_buff, text: "啟用", position: (10, 10), size: (75, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_buff_change])]
    buff_enabled_cb: nwg::CheckBox,

    #[nwg_control(parent: tab_buff, text: "新增", position: (200, 8), size: (50, 24))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_buff_add])]
    buff_btn_add: nwg::Button,

    #[nwg_control(parent: tab_buff, text: "移除", position: (255, 8), size: (50, 24))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_buff_remove])]
    buff_btn_remove: nwg::Button,

    #[nwg_control(parent: tab_buff, text: "上移", position: (310, 8), size: (50, 24))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_buff_up])]
    buff_btn_up: nwg::Button,

    #[nwg_control(parent: tab_buff, text: "下移", position: (365, 8), size: (50, 24))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_buff_down])]
    buff_btn_down: nwg::Button,

    #[nwg_control(parent: tab_buff, position: (10, 40), size: (220, 270),
                  collection: vec![
                      "0_強化 自我加速藥水".to_string(),
                      "73_永久巧克力蛋糕".to_string(),
                      "_名譽貨幣".to_string(),
                      "1_慎重藥水".to_string(),
                      "_藍色藥水".to_string(),
                      "38_加速藥水".to_string(),
                      "153_生命之樹果汁".to_string(),
                  ])]
    buff_list_left: nwg::ListBox<String>,

    #[nwg_control(parent: tab_buff, position: (240, 40), size: (220, 270),
                  collection: Vec::<String>::new())]
    buff_list_right: nwg::ListBox<String>,

    // ════════════ tab3 狀態 ════════════
    #[nwg_control(parent: tab_status, text: "顯示經驗值", position: (15, 18), size: (140, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_status_change])]
    status_show_exp_cb: nwg::CheckBox,
    #[nwg_control(parent: tab_status, text: "磨刀石修武器", position: (15, 44), size: (140, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_status_change])]
    status_whetstone_cb: nwg::CheckBox,
    #[nwg_control(parent: tab_status, text: "自動吃肉", position: (15, 70), size: (140, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_status_change])]
    status_eat_meat_cb: nwg::CheckBox,
    #[nwg_control(parent: tab_status, text: "變身", position: (15, 100), size: (50, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_status_change])]
    status_transform_cb: nwg::CheckBox,
    #[nwg_control(parent: tab_status, position: (70, 98), size: (160, 22),
                  collection: vec!["（測試項目1）".to_string()],
                  selected_index: Some(0))]
    #[nwg_events(OnComboxBoxSelection: [LhxWindow::on_status_change])]
    status_transform_combo: nwg::ComboBox<String>,
    #[nwg_control(parent: tab_status, position: (240, 98), size: (160, 22),
                  collection: vec!["（測試條件1）".to_string()],
                  selected_index: Some(0))]
    #[nwg_events(OnComboxBoxSelection: [LhxWindow::on_status_change])]
    status_transform_cond_combo: nwg::ComboBox<String>,

    #[nwg_control(parent: tab_status, text: "解毒", position: (15, 128), size: (50, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_status_change])]
    status_antidote_cb: nwg::CheckBox,
    #[nwg_control(parent: tab_status, position: (70, 126), size: (160, 22),
                  collection: vec!["（測試項目1）".to_string()],
                  selected_index: Some(0))]
    #[nwg_events(OnComboxBoxSelection: [LhxWindow::on_status_change])]
    status_antidote_combo: nwg::ComboBox<String>,

    // F1-F4
    #[nwg_control(parent: tab_status, text: "F1", position: (15, 196), size: (32, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_status_change])]
    fkey_cb_0: nwg::CheckBox,
    #[nwg_control(parent: tab_status, text: "", position: (52, 194), size: (260, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_status_change])]
    fkey_text_0: nwg::TextInput,

    #[nwg_control(parent: tab_status, text: "F2", position: (15, 224), size: (32, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_status_change])]
    fkey_cb_1: nwg::CheckBox,
    #[nwg_control(parent: tab_status, text: "", position: (52, 222), size: (260, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_status_change])]
    fkey_text_1: nwg::TextInput,

    #[nwg_control(parent: tab_status, text: "F3", position: (15, 252), size: (32, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_status_change])]
    fkey_cb_2: nwg::CheckBox,
    #[nwg_control(parent: tab_status, text: "", position: (52, 250), size: (260, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_status_change])]
    fkey_text_2: nwg::TextInput,

    #[nwg_control(parent: tab_status, text: "F4", position: (15, 280), size: (32, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_status_change])]
    fkey_cb_3: nwg::CheckBox,
    #[nwg_control(parent: tab_status, text: "", position: (52, 278), size: (260, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_status_change])]
    fkey_text_3: nwg::TextInput,

    // ════════════ tab4 刪物 ════════════
    #[nwg_control(parent: tab_delete, text: "啟用", position: (15, 14), size: (75, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_delete_change])]
    delete_enabled_cb: nwg::CheckBox,

    /// 共用 combo — 展開時刷當下背包
    #[nwg_control(parent: tab_delete, position: (15, 44), size: (380, 22),
                  collection: Vec::<String>::new())]
    #[nwg_events(OnComboBoxDropdown: [LhxWindow::on_delete_combo_dropdown])]
    delete_combo: nwg::ComboBox<String>,

    // ─── 刪除清單 ───
    #[nwg_control(parent: tab_delete, text: "刪除清單", position: (15, 76), size: (200, 16),
                  background_color: Some(LHX_BG))]
    delete_label_section_del: nwg::Label,
    #[nwg_control(parent: tab_delete, text: "+ 加入刪除", position: (15, 96), size: (90, 24))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_delete_add_delete])]
    delete_btn_add_delete: nwg::Button,
    #[nwg_control(parent: tab_delete, text: "− 移除", position: (110, 96), size: (60, 24))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_delete_remove_delete])]
    delete_btn_remove_delete: nwg::Button,
    #[nwg_control(parent: tab_delete, position: (15, 124), size: (440, 90),
                  collection: Vec::<String>::new())]
    delete_listbox: nwg::ListBox<String>,

    // ─── 溶解清單 ───
    #[nwg_control(parent: tab_delete, text: "溶解清單", position: (15, 220), size: (200, 16),
                  background_color: Some(LHX_BG))]
    delete_label_section_dis: nwg::Label,
    #[nwg_control(parent: tab_delete, text: "+ 加入溶解", position: (15, 240), size: (90, 24))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_delete_add_dissolve])]
    delete_btn_add_dissolve: nwg::Button,
    #[nwg_control(parent: tab_delete, text: "− 移除", position: (110, 240), size: (60, 24))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_delete_remove_dissolve])]
    delete_btn_remove_dissolve: nwg::Button,
    #[nwg_control(parent: tab_delete, position: (15, 268), size: (440, 90),
                  collection: Vec::<String>::new())]
    dissolve_listbox: nwg::ListBox<String>,

    // ════════════ tab5 喊話 ════════════
    #[nwg_control(parent: tab_shout, text: "啟用", position: (15, 14), size: (75, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_shout_change])]
    shout_enabled_cb: nwg::CheckBox,

    #[nwg_control(parent: tab_shout, text: "間隔秒數", position: (250, 14), size: (90, 22),
                  background_color: Some(LHX_BG))]
    shout_interval_lbl: nwg::Label,
    #[nwg_control(parent: tab_shout, text: "0", position: (340, 12), size: (60, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_shout_change])]
    shout_interval_num: nwg::TextInput,

    #[nwg_control(parent: tab_shout, text: "", position: (15, 48), size: (310, 22))]
    shout_input: nwg::TextInput,
    #[nwg_control(parent: tab_shout, text: "新增", position: (335, 46), size: (55, 26))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_shout_add])]
    shout_btn_add: nwg::Button,
    #[nwg_control(parent: tab_shout, text: "移除", position: (395, 46), size: (55, 26))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_shout_remove])]
    shout_btn_remove: nwg::Button,

    #[nwg_control(parent: tab_shout, position: (15, 84), size: (440, 230),
                  collection: Vec::<String>::new())]
    shout_listbox: nwg::ListBox<String>,

    // ════════════ tab6 其他(6 toggle) ════════════
    #[nwg_control(parent: tab_misc, text: "全白天", position: (15, 18), size: (200, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_misc_change])]
    misc_all_day_cb: nwg::CheckBox,
    #[nwg_control(parent: tab_misc, text: "海底抽水", position: (15, 44), size: (200, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_misc_change])]
    misc_underwater_pump_cb: nwg::CheckBox,
    #[nwg_control(parent: tab_misc, text: "降低CPU", position: (15, 70), size: (200, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_misc_change])]
    misc_low_cpu_cb: nwg::CheckBox,
    #[nwg_control(parent: tab_misc, text: "怪物等級色彩", position: (15, 96), size: (200, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_misc_change])]
    misc_monster_color_cb: nwg::CheckBox,
    #[nwg_control(parent: tab_misc, text: "顯示遊戲時鐘", position: (15, 122), size: (200, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_misc_change])]
    misc_show_clock_cb: nwg::CheckBox,
    #[nwg_control(parent: tab_misc, text: "顯示攻擊傷害(頭上)", position: (15, 148), size: (200, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_misc_change])]
    misc_show_attack_dmg_cb: nwg::CheckBox,
    #[nwg_control(parent: tab_misc, text: "顯示攻擊傷害(腳下)", position: (15, 174), size: (200, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_misc_change])]
    misc_damage_at_feet_cb: nwg::CheckBox,

    // ════════════ tab7 定時 ════════════
    #[nwg_control(parent: tab_timer, text: "啟用",
                  position: (15, 14), size: (75, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_change])]
    timer_enabled_cb: nwg::CheckBox,

    // row 0 (y=44)
    #[nwg_control(parent: tab_timer, text: "間隔", position: (15, 46), size: (40, 22),
                  background_color: Some(LHX_BG))]
    timer_lbl_0: nwg::Label,
    #[nwg_control(parent: tab_timer, text: "5", position: (60, 44), size: (40, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_timer_change])]
    timer_num_0: nwg::TextInput,
    #[nwg_control(parent: tab_timer, text: "", position: (105, 44), size: (260, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_timer_change])]
    timer_text_0: nwg::TextInput,
    #[nwg_control(parent: tab_timer, text: "重計", position: (370, 42), size: (50, 26))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_reset_0])]
    timer_btn_0: nwg::Button,
    #[nwg_control(parent: tab_timer, text: "", position: (425, 44), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_change])]
    timer_cb_0: nwg::CheckBox,

    // row 1 (y=76)
    #[nwg_control(parent: tab_timer, text: "間隔", position: (15, 78), size: (40, 22),
                  background_color: Some(LHX_BG))]
    timer_lbl_1: nwg::Label,
    #[nwg_control(parent: tab_timer, text: "5", position: (60, 76), size: (40, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_timer_change])]
    timer_num_1: nwg::TextInput,
    #[nwg_control(parent: tab_timer, text: "", position: (105, 76), size: (260, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_timer_change])]
    timer_text_1: nwg::TextInput,
    #[nwg_control(parent: tab_timer, text: "重計", position: (370, 74), size: (50, 26))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_reset_1])]
    timer_btn_1: nwg::Button,
    #[nwg_control(parent: tab_timer, text: "", position: (425, 76), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_change])]
    timer_cb_1: nwg::CheckBox,

    // row 2 (y=108)
    #[nwg_control(parent: tab_timer, text: "間隔", position: (15, 110), size: (40, 22),
                  background_color: Some(LHX_BG))]
    timer_lbl_2: nwg::Label,
    #[nwg_control(parent: tab_timer, text: "5", position: (60, 108), size: (40, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_timer_change])]
    timer_num_2: nwg::TextInput,
    #[nwg_control(parent: tab_timer, text: "", position: (105, 108), size: (260, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_timer_change])]
    timer_text_2: nwg::TextInput,
    #[nwg_control(parent: tab_timer, text: "重計", position: (370, 106), size: (50, 26))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_reset_2])]
    timer_btn_2: nwg::Button,
    #[nwg_control(parent: tab_timer, text: "", position: (425, 108), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_change])]
    timer_cb_2: nwg::CheckBox,

    // row 3 (y=140)
    #[nwg_control(parent: tab_timer, text: "間隔", position: (15, 142), size: (40, 22),
                  background_color: Some(LHX_BG))]
    timer_lbl_3: nwg::Label,
    #[nwg_control(parent: tab_timer, text: "5", position: (60, 140), size: (40, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_timer_change])]
    timer_num_3: nwg::TextInput,
    #[nwg_control(parent: tab_timer, text: "", position: (105, 140), size: (260, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_timer_change])]
    timer_text_3: nwg::TextInput,
    #[nwg_control(parent: tab_timer, text: "重計", position: (370, 138), size: (50, 26))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_reset_3])]
    timer_btn_3: nwg::Button,
    #[nwg_control(parent: tab_timer, text: "", position: (425, 140), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_change])]
    timer_cb_3: nwg::CheckBox,

    // row 4 (y=172)
    #[nwg_control(parent: tab_timer, text: "間隔", position: (15, 174), size: (40, 22),
                  background_color: Some(LHX_BG))]
    timer_lbl_4: nwg::Label,
    #[nwg_control(parent: tab_timer, text: "5", position: (60, 172), size: (40, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_timer_change])]
    timer_num_4: nwg::TextInput,
    #[nwg_control(parent: tab_timer, text: "", position: (105, 172), size: (260, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_timer_change])]
    timer_text_4: nwg::TextInput,
    #[nwg_control(parent: tab_timer, text: "重計", position: (370, 170), size: (50, 26))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_reset_4])]
    timer_btn_4: nwg::Button,
    #[nwg_control(parent: tab_timer, text: "", position: (425, 172), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_change])]
    timer_cb_4: nwg::CheckBox,

    // row 5 (y=204)
    #[nwg_control(parent: tab_timer, text: "間隔", position: (15, 206), size: (40, 22),
                  background_color: Some(LHX_BG))]
    timer_lbl_5: nwg::Label,
    #[nwg_control(parent: tab_timer, text: "5", position: (60, 204), size: (40, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_timer_change])]
    timer_num_5: nwg::TextInput,
    #[nwg_control(parent: tab_timer, text: "", position: (105, 204), size: (260, 22))]
    #[nwg_events(OnTextInput: [LhxWindow::on_timer_change])]
    timer_text_5: nwg::TextInput,
    #[nwg_control(parent: tab_timer, text: "重計", position: (370, 202), size: (50, 26))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_reset_5])]
    timer_btn_5: nwg::Button,
    #[nwg_control(parent: tab_timer, text: "", position: (425, 204), size: (22, 22),
                  background_color: Some(LHX_BG))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_change])]
    timer_cb_5: nwg::CheckBox,

    #[nwg_control(parent: tab_timer, text: "指令說明",
                  position: (370, 280), size: (90, 26))]
    #[nwg_events(OnButtonClick: [LhxWindow::on_timer_help])]
    timer_help_btn: nwg::Button,

    // 共享狀態(由 spawn_window_thread 透過 initial instance 帶入)
    settings: Arc<RwLock<AuxSettings>>,
    visible: Arc<AtomicU8>,
    game_hwnd: Arc<AtomicUsize>,
    /// 遊戲 HANDLE 的 usize 版本(0 = 尚未設定)。refresh_inventory 用。
    game_handle: Arc<AtomicUsize>,

    /// applying=true 期間,所有 on_*_change handler 直接 return,
    /// 避免 NWG 對 set_text 也觸發 OnTextInput → on_*_change →
    /// settings.write() 與 apply_settings 的 settings.read() 死鎖。
    applying: Arc<AtomicBool>,

    /// 定時分頁重計 epoch — UI 點重計就 bump,timer_tick 端比對偵測變動。
    /// Weak 是因為 owner 是 AuxControl;LhxWindow drop 後不該卡住 AuxControl。
    timer_resets: std::sync::Weak<[std::sync::atomic::AtomicU64; 6]>,
}

impl LhxWindow {
    fn on_tab_changed(&self) {
        self.apply_tab_layout();
        if self.tabs.selected_tab() == 3 {
            self.refresh_delete_combo_from_inventory();
        }
    }

    fn apply_tab_layout(&self) {
        let layout = scaled_lhx_tab_layout(self.tabs.selected_tab(), current_lhx_visual_scale());
        self.tabs.set_size(layout.tabs_size.0, layout.tabs_size.1);
        self.window
            .set_size(layout.window_size.0, layout.window_size.1);
    }

    fn on_close(&self) {
        // 視窗 X 關閉 → 等同隱藏(不真的 destroy,下次 home 鍵 set visible=1 復原)
        self.visible.store(VISIBLE_HIDDEN, Ordering::Relaxed);
        self.window.set_visible(false);
    }

    /// 50ms 輪詢 visible flag,根據外部訊號切換顯示/隱藏/結束
    fn on_visible_tick(&self) {
        let v = self.visible.load(Ordering::Relaxed);
        let cur = self.window.visible();
        if v == VISIBLE_CLOSE {
            self.visible_timer.stop();
            nwg::stop_thread_dispatch();
            return;
        }

        let game_minimized = self.is_owned_game_minimized();
        // catch-all `_` 保留 → 收成 guard 後仍窮盡;非命中組合(已是目標狀態 / None)無動作,與原本一致
        match desired_lhx_visibility(v, game_minimized) {
            Some(false) if cur => self.window.set_visible(false),
            Some(true) if !cur => {
                self.show_window_no_activate();
                self.apply_settings(); // 還原 UI(settings 可能在隱藏期間外部更新)
            }
            _ => {}
        }
    }

    /// 從 settings 還原全部 UI 控件(顯示視窗時呼叫)。
    ///
    /// 流程:
    /// 1. clone settings 後立刻釋放 read 鎖,避免下面 set_text 觸發
    ///    OnTextInput → on_*_change 試圖 settings.write() 而死鎖。
    /// 2. 設 applying=true,讓 on_*_change 期間直接 return
    ///    (節省無謂的 write 與 log)。
    fn is_owned_game_minimized(&self) -> bool {
        let hwnd = self.game_hwnd.load(Ordering::Relaxed);
        if hwnd == 0 {
            return false;
        }

        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::IsIconic;
            IsIconic(HWND(hwnd as *mut _)).as_bool()
        }
    }

    fn show_window_no_activate(&self) {
        if let Some(nwg_hwnd) = self.window.handle.hwnd() {
            unsafe {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};
                let _ = ShowWindow(HWND(nwg_hwnd as *mut _), SW_SHOWNOACTIVATE);
            }
        } else {
            self.window.set_visible(true);
        }
    }

    fn force_hide_window(&self) {
        if let Some(nwg_hwnd) = self.window.handle.hwnd() {
            unsafe {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
                let _ = ShowWindow(HWND(nwg_hwnd as *mut _), SW_HIDE);
            }
        } else {
            self.window.set_visible(false);
        }
    }

    pub fn apply_settings(&self) {
        // 先做一次性 migration:把 saved buff_items 的 cast_target 對齊 INI 規範
        // (修舊版 launcher 把 /M 寫成 Self_ 等壞資料)。
        // 必須在 read clone 之前做完,讓後面拿到的 snapshot 是已修過的。
        {
            let mut sw = self.settings.write();
            migrate_buff_items_against_ini(&mut sw.buff_items);
        }
        let s = self.settings.read().clone();
        self.applying.store(true, Ordering::SeqCst);
        // dropdown 先 populate(勾顯示背包道具 → 真實物品;否則 → 預設清單),
        // 後面 apply_potion_tab 才能根據存的字串還原 selection
        self.refresh_inventory(s.potion_show_inventory);
        self.apply_potion_tab(&s);
        self.apply_buff_tab(&s);
        self.apply_status_tab(&s);
        self.apply_delete_tab(&s);
        self.apply_shout_tab(&s);
        self.apply_misc_tab(&s);
        self.apply_timer_tab(&s);
        self.applying.store(false, Ordering::SeqCst);
    }

    /// 給有捲動需求的 listbox 加上 WS_VSCROLL,確保滾輪在內容超出可視範圍時能滾。
    /// ComboBox 不要加 WS_VSCROLL — 那會在 ComboBox 本體右側畫出醜醜的 ▲▼,
    /// 而且也不會影響 dropdown 內部的 listbox(那是另一個 hwnd)。
    /// dropdown 內部捲軸 Windows 預設就會在內容超過可視行數時自動畫出來。
    fn enable_all_widget_wheel_scroll(&self) {
        let listboxes: [&nwg::ListBox<String>; 5] = [
            &self.buff_list_left,
            &self.buff_list_right,
            &self.delete_listbox,
            &self.dissolve_listbox,
            &self.shout_listbox,
        ];
        for lb in listboxes {
            enable_listbox_vertical_scroll(lb);
        }

        // 給每個 ComboBox 內部 dropdown listbox 裝 WM_MOUSEWHEEL → WM_VSCROLL
        // subclass。Parent dialog 的 wheel forwarding 對 dropdown popup(top-level
        // WS_POPUP)無效,得直接 hook 它本身。
        let combos: [&nwg::ComboBox<String>; 12] = [
            &self.potion_combo_0,
            &self.potion_combo_1,
            &self.potion_combo_2,
            &self.potion_combo_3,
            &self.potion_combo_4,
            &self.potion_combo_5,
            &self.potion_combo_6,
            &self.mp_safe_combo,
            &self.status_transform_combo,
            &self.status_transform_cond_combo,
            &self.status_antidote_combo,
            &self.delete_combo,
        ];
        for combo in combos {
            install_combo_dropdown_wheel(combo);
        }
    }

    /// 預設藥水清單 — 「顯示背包道具」未勾時用這個 populate dropdown。
    /// 進場前(讀不到背包)也用這個讓使用者預先設定。
    /// base name 直接對應 strip_qty 後的字串,跟背包顯示對得上 (`it.name_lossy()` strip 後)。
    ///
    /// 來源優先序:
    /// 1. launcher.exe 旁 `linhelperZ.ini` 的 `[AllHP]` section(user 可編輯)
    /// 2. 找不到/解析失敗 → 寫死的 fallback,並順手寫一份 INI 範本讓 user 知道怎麼改
    fn default_potion_names() -> Vec<String> {
        // 先確保檔案存在 + 把舊版 launcher 留下的 INI 補上新加的 section。
        // 順序:write template if missing → migrate sections → 再讀。
        let fallback = hardcoded_potion_names();
        write_potion_list_template_if_missing(&fallback);
        migrate_potion_list_ini();

        if let Some(list) = load_potion_list_ini() {
            if !list.is_empty() {
                return list;
            }
        }
        fallback
    }

    /// Populate 喝水分頁所有 dropdown。
    ///
    /// 7 個 potion combo:
    /// - `use_real_inventory=true` → 讀身上真實物品(進場後才有,失敗就 fallback INI 預設清單)
    /// - `use_real_inventory=false` → 用 INI `[AllHP]` Item* 預設清單
    ///
    /// 洗魔 mp_safe combo 永遠用 INI `[AllHP]` HPMP* 清單(心靈轉換/魂體轉換之類),
    /// 那是技能不是物品,不能跟著 inventory 變。
    fn refresh_inventory(&self, use_real_inventory: bool) {
        let potion_names: Vec<String> = if use_real_inventory {
            let h_raw = self.game_handle.load(Ordering::Relaxed);
            if h_raw == 0 {
                Self::default_potion_names()
            } else {
                let h = HANDLE(h_raw as *mut _);
                match crate::aux::inventory::list_items(h) {
                    Ok(items) if !items.is_empty() => {
                        items.iter().map(|it| it.name_lossy()).collect()
                    }
                    _ => Self::default_potion_names(),
                }
            }
        } else {
            Self::default_potion_names()
        };

        let potion_combos: [&nwg::ComboBox<String>; 7] = [
            &self.potion_combo_0,
            &self.potion_combo_1,
            &self.potion_combo_2,
            &self.potion_combo_3,
            &self.potion_combo_4,
            &self.potion_combo_5,
            &self.potion_combo_6,
        ];
        for combo in potion_combos {
            combo.set_collection(potion_names.clone());
            set_combo_dropdown_visible_rows(combo, potion_names.len());
            // 沒有預設選 — 上層 apply_potion_tab 會根據 settings 嘗試還原
        }

        // 洗魔下拉:HPMP* 清單(「心靈轉換/M」「魂體轉換/M」這類 HP→MP 轉換技能)
        let hpmp_names = default_hpmp_names();
        self.mp_safe_combo.set_collection(hpmp_names.clone());
        set_combo_dropdown_visible_rows(&self.mp_safe_combo, hpmp_names.len());
    }

    /// 背包 dropdown 即時更新 timer — 每 500ms 一次。
    /// 只在「顯示背包道具」勾起、視窗顯示中、dropdown 沒展開、user 沒輸入時 refresh,
    /// 不打斷 user 操作。dropdown 展開的當下我們不更新,但下次 user 關閉再開,500ms 內就是新的。
    fn on_inv_refresh_tick(&self) {
        // 視窗隱藏時 skip
        if self.visible.load(Ordering::Relaxed) != VISIBLE_SHOWN {
            return;
        }
        // applying 期間 skip,避免跟 apply_settings 對打
        if self.applying.load(Ordering::SeqCst) {
            return;
        }
        // 只在勾「顯示背包道具」時才動真實背包
        let show_inv = self.settings.read().potion_show_inventory;
        if !show_inv {
            return;
        }
        // 任何 dropdown 展開中 → skip 整輪(避免讓 user 正在挑的選單突然消失)
        let combos: [&nwg::ComboBox<String>; 7] = [
            &self.potion_combo_0,
            &self.potion_combo_1,
            &self.potion_combo_2,
            &self.potion_combo_3,
            &self.potion_combo_4,
            &self.potion_combo_5,
            &self.potion_combo_6,
        ];
        if combos.iter().any(|c| combo_dropdown_open(c)) {
            return;
        }
        // 重 populate + 還原 selection(屏蔽 on_potion_change)
        let s = self.settings.read().clone();
        self.applying.store(true, Ordering::SeqCst);
        self.refresh_inventory(true);
        self.apply_potion_combo_selections(&s);
        self.applying.store(false, Ordering::SeqCst);
    }

    fn apply_potion_combo_selections(&self, s: &AuxSettings) {
        let combos: [&nwg::ComboBox<String>; 7] = [
            &self.potion_combo_0,
            &self.potion_combo_1,
            &self.potion_combo_2,
            &self.potion_combo_3,
            &self.potion_combo_4,
            &self.potion_combo_5,
            &self.potion_combo_6,
        ];

        for (i, combo) in combos.iter().enumerate() {
            let stored = strip_qty(&s.potion_rows[i].item);
            if stored.is_empty() {
                continue;
            }

            let coll = combo.collection();
            if let Some(idx) = coll.iter().position(|n| strip_qty(n) == stored) {
                combo.set_selection(Some(idx));
            }
        }

        let stored = strip_qty(&s.mp_when_safe.item);
        if !stored.is_empty() {
            let coll = self.mp_safe_combo.collection();
            if let Some(idx) = coll.iter().position(|n| strip_qty(n) == stored) {
                self.mp_safe_combo.set_selection(Some(idx));
            }
        }
    }

    fn apply_potion_tab(&self, s: &AuxSettings) {
        let rows: [(&nwg::CheckBox, &nwg::TextInput); 7] = [
            (&self.potion_cb_0, &self.potion_num_0),
            (&self.potion_cb_1, &self.potion_num_1),
            (&self.potion_cb_2, &self.potion_num_2),
            (&self.potion_cb_3, &self.potion_num_3),
            (&self.potion_cb_4, &self.potion_num_4),
            (&self.potion_cb_5, &self.potion_num_5),
            (&self.potion_cb_6, &self.potion_num_6),
        ];
        for (i, (cb, num)) in rows.iter().enumerate() {
            cb.set_check_state(if s.potion_rows[i].enabled {
                nwg::CheckBoxState::Checked
            } else {
                nwg::CheckBoxState::Unchecked
            });
            num.set_text(&s.potion_rows[i].threshold.to_string());
            // 還原 selection — 找 base name 相符的 index(忽略數量後綴)
        }
        self.apply_potion_combo_selections(s);

        self.mp_safe_cb.set_check_state(if s.mp_when_safe.enabled {
            nwg::CheckBoxState::Checked
        } else {
            nwg::CheckBoxState::Unchecked
        });
        self.mp_safe_hp_num
            .set_text(&s.mp_when_safe.hp_lower.to_string());
        self.mp_safe_mp_num
            .set_text(&s.mp_when_safe.mp_upper.to_string());

        self.potion_use_percent_cb
            .set_check_state(if s.potion_use_percent {
                nwg::CheckBoxState::Checked
            } else {
                nwg::CheckBoxState::Unchecked
            });
        self.potion_show_inv_cb
            .set_check_state(if s.potion_show_inventory {
                nwg::CheckBoxState::Checked
            } else {
                nwg::CheckBoxState::Unchecked
            });
    }

    fn apply_buff_tab(&self, s: &AuxSettings) {
        self.buff_enabled_cb.set_check_state(if s.buff_enabled {
            nwg::CheckBoxState::Checked
        } else {
            nwg::CheckBoxState::Unchecked
        });
        // 左:[AllState] 完整清單(從 INI 讀,parse 後統一顯示為 `name_id_suffix`
        // 3 段底線格式 — 跟右側 collection 一致,user 看左右會是同一種格式)
        let left_raw = load_state_list_ini();
        let left: Vec<String> = left_raw
            .iter()
            .map(|raw| format_buff_item(&parse_buff_item(raw)))
            .collect();
        self.buff_list_left.set_collection(left);
        // 右:從 settings 還原。`migrate_buff_items_against_ini` 已在 settings 載入後
        // 修過 cast_target,這裡直接 format 即可。
        let right: Vec<String> = s.buff_items.iter().map(format_buff_item).collect();
        self.buff_list_right.set_collection(right);
    }

    /// 任何喝水分頁控件變動 → 寫回 settings
    fn on_potion_change(&self) {
        if self.applying.load(Ordering::SeqCst) {
            return;
        }
        let mut s = self.settings.write();
        let rows: [(&nwg::CheckBox, &nwg::TextInput, &nwg::ComboBox<String>); 7] = [
            (&self.potion_cb_0, &self.potion_num_0, &self.potion_combo_0),
            (&self.potion_cb_1, &self.potion_num_1, &self.potion_combo_1),
            (&self.potion_cb_2, &self.potion_num_2, &self.potion_combo_2),
            (&self.potion_cb_3, &self.potion_num_3, &self.potion_combo_3),
            (&self.potion_cb_4, &self.potion_num_4, &self.potion_combo_4),
            (&self.potion_cb_5, &self.potion_num_5, &self.potion_combo_5),
            (&self.potion_cb_6, &self.potion_num_6, &self.potion_combo_6),
        ];
        for (i, (cb, num, combo)) in rows.iter().enumerate() {
            s.potion_rows[i].enabled = matches!(cb.check_state(), nwg::CheckBoxState::Checked);
            s.potion_rows[i].threshold = num.text().parse().unwrap_or(0);
            s.potion_rows[i].item =
                strip_qty(&combo.selection_string().unwrap_or_default()).to_string();
        }

        s.mp_when_safe.enabled =
            matches!(self.mp_safe_cb.check_state(), nwg::CheckBoxState::Checked);
        s.mp_when_safe.hp_lower = self.mp_safe_hp_num.text().parse().unwrap_or(0);
        s.mp_when_safe.mp_upper = self.mp_safe_mp_num.text().parse().unwrap_or(0);
        s.mp_when_safe.item =
            strip_qty(&self.mp_safe_combo.selection_string().unwrap_or_default()).to_string();

        s.potion_use_percent = matches!(
            self.potion_use_percent_cb.check_state(),
            nwg::CheckBoxState::Checked
        );
        s.potion_show_inventory = matches!(
            self.potion_show_inv_cb.check_state(),
            nwg::CheckBoxState::Checked
        );

        log_line!(
            "[lhx] potion 變動:use_percent={} mp_safe.enabled={} row[0].enabled={} row[0].threshold={} row[0].item={:?}",
            s.potion_use_percent,
            s.mp_when_safe.enabled,
            s.potion_rows[0].enabled,
            s.potion_rows[0].threshold,
            s.potion_rows[0].item
        );
    }

    /// 「顯示背包道具」checkbox 切換 — 寫回 settings + 立刻 re-populate dropdown + 還原 selection
    fn on_show_inv_toggle(&self) {
        if self.applying.load(Ordering::SeqCst) {
            return;
        }
        let new_show = matches!(
            self.potion_show_inv_cb.check_state(),
            nwg::CheckBoxState::Checked
        );
        // 寫回 settings(短暫 lock,clone 出來給後續 apply_potion_tab 還原 selection)
        let s = {
            let mut s = self.settings.write();
            s.potion_show_inventory = new_show;
            s.clone()
        };
        // re-populate + 還原 selection 期間屏蔽 on_potion_change(避免 set_selection 觸發再寫回)
        self.applying.store(true, Ordering::SeqCst);
        self.refresh_inventory(new_show);
        self.apply_potion_tab(&s);
        self.applying.store(false, Ordering::SeqCst);
        log_line!("[lhx] 顯示背包道具 = {} → dropdown 已刷新", new_show);
    }

    fn on_buff_change(&self) {
        if self.applying.load(Ordering::SeqCst) {
            return;
        }
        let mut s = self.settings.write();
        s.buff_enabled = matches!(
            self.buff_enabled_cb.check_state(),
            nwg::CheckBoxState::Checked
        );
        log_line!("[lhx] buff 變動:enabled={}", s.buff_enabled);
    }

    fn on_buff_add(&self) {
        if let Some(idx) = self.buff_list_left.selection() {
            if let Some(text) = self.buff_list_left.collection().get(idx).cloned() {
                let mut right = self.buff_list_right.collection().to_vec();
                right.push(text.clone());
                self.buff_list_right.set_collection(right);
                self.write_buff_items();
                log_line!("[lhx] buff 新增:{}", text);
            }
        }
    }

    fn on_buff_remove(&self) {
        if let Some(idx) = self.buff_list_right.selection() {
            let mut right = self.buff_list_right.collection().to_vec();
            if idx < right.len() {
                let removed = right.remove(idx);
                self.buff_list_right.set_collection(right);
                self.write_buff_items();
                log_line!("[lhx] buff 移除:{}", removed);
            }
        }
    }

    fn on_buff_up(&self) {
        if let Some(idx) = self.buff_list_right.selection() {
            if idx > 0 {
                let mut right = self.buff_list_right.collection().to_vec();
                right.swap(idx, idx - 1);
                self.buff_list_right.set_collection(right);
                self.buff_list_right.set_selection(Some(idx - 1));
                self.write_buff_items();
            }
        }
    }

    fn on_buff_down(&self) {
        if let Some(idx) = self.buff_list_right.selection() {
            let mut right = self.buff_list_right.collection().to_vec();
            if idx + 1 < right.len() {
                right.swap(idx, idx + 1);
                self.buff_list_right.set_collection(right);
                self.buff_list_right.set_selection(Some(idx + 1));
                self.write_buff_items();
            }
        }
    }

    /// 把 buff_list_right 內容寫回 settings.buff_items
    fn write_buff_items(&self) {
        let mut s = self.settings.write();
        s.buff_items = self
            .buff_list_right
            .collection()
            .iter()
            .map(|raw| parse_buff_item(raw))
            .collect();
    }

    fn on_status_change(&self) {
        if self.applying.load(Ordering::SeqCst) {
            return;
        }
        let mut s = self.settings.write();
        s.status_show_exp = matches!(
            self.status_show_exp_cb.check_state(),
            nwg::CheckBoxState::Checked
        );
        s.status_whetstone = matches!(
            self.status_whetstone_cb.check_state(),
            nwg::CheckBoxState::Checked
        );
        s.status_eat_meat = matches!(
            self.status_eat_meat_cb.check_state(),
            nwg::CheckBoxState::Checked
        );
        s.status_transform_enabled = matches!(
            self.status_transform_cb.check_state(),
            nwg::CheckBoxState::Checked
        );
        s.status_transform_item = self
            .status_transform_combo
            .selection_string()
            .unwrap_or_default();
        s.status_transform_cond = self
            .status_transform_cond_combo
            .selection_string()
            .unwrap_or_default();
        s.status_antidote_enabled = matches!(
            self.status_antidote_cb.check_state(),
            nwg::CheckBoxState::Checked
        );
        s.status_antidote_item = self
            .status_antidote_combo
            .selection_string()
            .unwrap_or_default();

        let fkey_cbs = [
            &self.fkey_cb_0,
            &self.fkey_cb_1,
            &self.fkey_cb_2,
            &self.fkey_cb_3,
        ];
        let fkey_texts = [
            &self.fkey_text_0,
            &self.fkey_text_1,
            &self.fkey_text_2,
            &self.fkey_text_3,
        ];
        for i in 0..4 {
            s.fkey_macros[i].enabled =
                matches!(fkey_cbs[i].check_state(), nwg::CheckBoxState::Checked);
            let cmd = fkey_texts[i].text();
            s.fkey_macros[i].command = if cmd.trim().is_empty() {
                String::new()
            } else {
                format_command_item(&parse_buff_item(&cmd))
            };
        }

        log_line!(
            "[lhx] status 變動:show_exp={} whetstone={} eat_meat={} antidote={}({:?}) F1.enabled={}",
            s.status_show_exp, s.status_whetstone, s.status_eat_meat,
            s.status_antidote_enabled, s.status_antidote_item,
            s.fkey_macros[0].enabled
        );
    }

    fn apply_status_tab(&self, s: &AuxSettings) {
        let set_cb = |cb: &nwg::CheckBox, v: bool| {
            cb.set_check_state(if v {
                nwg::CheckBoxState::Checked
            } else {
                nwg::CheckBoxState::Unchecked
            });
        };
        set_cb(&self.status_show_exp_cb, s.status_show_exp);
        set_cb(&self.status_whetstone_cb, s.status_whetstone);
        set_cb(&self.status_eat_meat_cb, s.status_eat_meat);
        set_cb(&self.status_transform_cb, s.status_transform_enabled);
        set_cb(&self.status_antidote_cb, s.status_antidote_enabled);

        // 解毒清單從 INI [AllAntidote] 讀(中毒時自動使用的物品名單)
        let antidote_items = load_section_items("AllAntidote", "Item");
        if !antidote_items.is_empty() {
            self.status_antidote_combo
                .set_collection(antidote_items.clone());
            set_combo_dropdown_visible_rows(&self.status_antidote_combo, antidote_items.len());
            // 還原存的選項;沒有 / 找不到就選第一個
            let idx = antidote_items
                .iter()
                .position(|n| n == &s.status_antidote_item)
                .unwrap_or(0);
            self.status_antidote_combo.set_selection(Some(idx));
        }

        // 變身卷軸物品清單從 INI [AllPolyItems] 讀(背包要有的物品名)
        // 純物品名(背包點選用),例如「象牙塔變形卷軸」、「變形卷軸」、「黑暗安特的樹皮」。
        let poly_items = load_section_items("AllPolyItems", "Item");
        if !poly_items.is_empty() {
            self.status_transform_combo
                .set_collection(poly_items.clone());
            set_combo_dropdown_visible_rows(&self.status_transform_combo, poly_items.len());
            let idx = poly_items
                .iter()
                .position(|n| n == &s.status_transform_item)
                .unwrap_or(0);
            self.status_transform_combo.set_selection(Some(idx));
        }

        // 變身選項清單從 INI [AllPolymorphs] 讀(IP 封包送出的英文 option 字串來源)
        // 條目格式:`<中文顯示>_<英文 option>_<spr_id>`(範例:`狼人_re werewolf_3865`)
        // combo 存原始整行,執行時用 [`extract_polymorph_option`] 抽英文 option 進封包。
        let polymorph_items = load_section_items("AllPolymorphs", "Item");
        if !polymorph_items.is_empty() {
            self.status_transform_cond_combo
                .set_collection(polymorph_items.clone());
            set_combo_dropdown_visible_rows(
                &self.status_transform_cond_combo,
                polymorph_items.len(),
            );
            let idx = polymorph_items
                .iter()
                .position(|n| n == &s.status_transform_cond)
                .unwrap_or(0);
            self.status_transform_cond_combo.set_selection(Some(idx));
        }

        let fkey_cbs = [
            &self.fkey_cb_0,
            &self.fkey_cb_1,
            &self.fkey_cb_2,
            &self.fkey_cb_3,
        ];
        let fkey_texts = [
            &self.fkey_text_0,
            &self.fkey_text_1,
            &self.fkey_text_2,
            &self.fkey_text_3,
        ];
        for i in 0..4 {
            set_cb(fkey_cbs[i], s.fkey_macros[i].enabled);
            let cmd = s.fkey_macros[i].command.trim();
            if cmd.is_empty() {
                fkey_texts[i].set_text("");
            } else {
                fkey_texts[i].set_text(&format_command_item(&parse_buff_item(cmd)));
            }
        }
    }

    fn on_delete_change(&self) {
        if self.applying.load(Ordering::SeqCst) {
            return;
        }
        let mut s = self.settings.write();
        s.delete_enabled = matches!(
            self.delete_enabled_cb.check_state(),
            nwg::CheckBoxState::Checked
        );
        log_line!("[lhx] delete 變動:enabled={}", s.delete_enabled);
    }

    /// Combo dropdown 展開時即時刷當下背包(避免顯示舊 snapshot)
    fn on_delete_combo_dropdown(&self) {
        self.refresh_delete_combo_from_inventory();
    }

    fn refresh_delete_combo_from_inventory(&self) {
        if self.applying.load(Ordering::SeqCst) {
            return;
        }
        let h_raw = self.game_handle.load(Ordering::Relaxed);
        if h_raw == 0 {
            return;
        }
        let h = HANDLE(h_raw as *mut _);
        let names: Vec<String> = match crate::aux::inventory::list_items(h) {
            Ok(items) => items.into_iter().map(|it| it.name_lossy()).collect(),
            Err(_) => Vec::new(),
        };
        // applying 期間 set_collection 不應觸發 settings.write
        self.applying.store(true, Ordering::SeqCst);
        log_line!(
            "[lhx] delete combo inventory refresh: {} items",
            names.len()
        );
        self.delete_combo.set_collection(names.clone());
        set_delete_combo_dropdown_visible_rows(&self.delete_combo, names.len());
        self.applying.store(false, Ordering::SeqCst);
    }

    fn on_delete_add_delete(&self) {
        self.add_to_delete_list(false);
    }

    fn on_delete_add_dissolve(&self) {
        self.add_to_delete_list(true);
    }

    /// 共用實作:把 combo 選的物品加進對應 list,擋裝備中物品
    fn add_to_delete_list(&self, to_dissolve: bool) {
        let Some(idx) = self.delete_combo.selection() else {
            return;
        };
        let Some(text) = self.delete_combo.collection().get(idx).cloned() else {
            return;
        };
        if text.contains("(使用中)") || text.contains("(揮舞)") {
            nwg::modal_info_message(
                &self.window,
                &crate::i18n::tr("警告"),
                &crate::i18n::tr("無法刪除或溶解正在使用的裝備!"),
            );
            return;
        }
        let listbox = if to_dissolve {
            &self.dissolve_listbox
        } else {
            &self.delete_listbox
        };
        let mut list = listbox.collection().to_vec();
        list.push(delete_list_entry_name(&text));
        listbox.set_collection(list);
        self.write_delete_lists();
    }

    fn on_delete_remove_delete(&self) {
        self.remove_from_delete_list(false);
    }

    fn on_delete_remove_dissolve(&self) {
        self.remove_from_delete_list(true);
    }

    /// 共用實作:從對應 list 移除選中項,並寫回 settings
    fn remove_from_delete_list(&self, from_dissolve: bool) {
        let listbox = if from_dissolve {
            &self.dissolve_listbox
        } else {
            &self.delete_listbox
        };
        let Some(idx) = listbox.selection() else {
            return;
        };
        let mut list = listbox.collection().to_vec();
        if idx < list.len() {
            list.remove(idx);
            listbox.set_collection(list);
            self.write_delete_lists();
        }
    }

    fn write_delete_lists(&self) {
        let mut s = self.settings.write();
        s.delete_list = self.delete_listbox.collection().to_vec();
        s.dissolve_list = self.dissolve_listbox.collection().to_vec();
    }

    fn apply_delete_tab(&self, s: &AuxSettings) {
        self.delete_enabled_cb.set_check_state(if s.delete_enabled {
            nwg::CheckBoxState::Checked
        } else {
            nwg::CheckBoxState::Unchecked
        });
        self.delete_listbox.set_collection(s.delete_list.clone());
        self.dissolve_listbox
            .set_collection(s.dissolve_list.clone());
    }

    fn on_shout_change(&self) {
        if self.applying.load(Ordering::SeqCst) {
            return;
        }
        let mut s = self.settings.write();
        s.shout_enabled = matches!(
            self.shout_enabled_cb.check_state(),
            nwg::CheckBoxState::Checked
        );
        s.shout_interval_sec = self.shout_interval_num.text().parse().unwrap_or(0);
        log_line!(
            "[lhx] shout 變動:enabled={} interval_sec={}",
            s.shout_enabled,
            s.shout_interval_sec
        );
    }

    fn on_shout_add(&self) {
        let text = self.shout_input.text();
        if text.trim().is_empty() {
            return;
        }
        let mut list = self.shout_listbox.collection().to_vec();
        list.push(text);
        self.shout_listbox.set_collection(list);
        self.shout_input.set_text("");
        self.write_shout_messages();
    }

    fn on_shout_remove(&self) {
        if let Some(idx) = self.shout_listbox.selection() {
            let mut list = self.shout_listbox.collection().to_vec();
            if idx < list.len() {
                list.remove(idx);
                self.shout_listbox.set_collection(list);
                self.write_shout_messages();
            }
        }
    }

    fn write_shout_messages(&self) {
        let mut s = self.settings.write();
        s.shout_messages = self.shout_listbox.collection().to_vec();
    }

    fn apply_shout_tab(&self, s: &AuxSettings) {
        self.shout_enabled_cb.set_check_state(if s.shout_enabled {
            nwg::CheckBoxState::Checked
        } else {
            nwg::CheckBoxState::Unchecked
        });
        self.shout_interval_num
            .set_text(&s.shout_interval_sec.to_string());
        self.shout_listbox.set_collection(s.shout_messages.clone());
    }

    // ════════════ tab6 其他(6 toggle) handlers ════════════
    fn on_misc_change(&self) {
        if self.applying.load(Ordering::SeqCst) {
            return;
        }

        let h_raw = self.game_handle.load(Ordering::SeqCst);
        let cb = |c: &nwg::CheckBox| matches!(c.check_state(), nwg::CheckBoxState::Checked);

        let (
            new_all_day,
            new_underwater_pump,
            new_low_cpu,
            new_show_clock,
            new_attack_dmg,
            new_monster_color,
            new_damage_at_feet,
        ) = {
            let mut s = self.settings.write();

            s.misc.all_day = cb(&self.misc_all_day_cb);
            s.misc.underwater_pump = cb(&self.misc_underwater_pump_cb);
            s.misc.low_cpu = cb(&self.misc_low_cpu_cb);
            s.misc.monster_level_color = cb(&self.misc_monster_color_cb);
            s.misc.show_clock = cb(&self.misc_show_clock_cb);

            // 互斥:頭上 / 腳下 不能同時勾選。同時為 true 時看前一次哪個是 false,
            // 那個就是使用者剛勾的,保留它,另一個強制取消。
            let prev_head = s.misc.show_attack_dmg;
            let cur_head = cb(&self.misc_show_attack_dmg_cb);
            let cur_feet = cb(&self.misc_damage_at_feet_cb);
            let (final_head, final_feet) = if cur_head && cur_feet {
                if !prev_head {
                    (true, false)
                } else {
                    (false, true)
                }
            } else {
                (cur_head, cur_feet)
            };
            if final_head != cur_head {
                self.misc_show_attack_dmg_cb.set_check_state(if final_head {
                    nwg::CheckBoxState::Checked
                } else {
                    nwg::CheckBoxState::Unchecked
                });
            }
            if final_feet != cur_feet {
                self.misc_damage_at_feet_cb.set_check_state(if final_feet {
                    nwg::CheckBoxState::Checked
                } else {
                    nwg::CheckBoxState::Unchecked
                });
            }
            s.misc.show_attack_dmg = final_head;
            s.misc.damage_at_feet = final_feet;

            log_line!(
                "[lhx] misc 變動:all_day={} show_attack_dmg={} damage_at_feet={}",
                s.misc.all_day,
                s.misc.show_attack_dmg,
                s.misc.damage_at_feet
            );

            (
                s.misc.all_day,
                s.misc.underwater_pump,
                s.misc.low_cpu,
                s.misc.show_clock,
                s.misc.show_attack_dmg,
                s.misc.monster_level_color,
                s.misc.damage_at_feet,
            )
        };

        // settings 寫入完成,釋鎖後再做 hook 系統呼叫
        self.sync_all_day_patch(h_raw, new_all_day);
        self.sync_underwater_pump_patch(h_raw, new_underwater_pump);
        self.sync_low_cpu_hook(h_raw, new_low_cpu);
        self.sync_show_clock_patch(h_raw, new_show_clock);
        // base hook 產生紅色(BGR565 0xF800) 傷害氣泡;頭上 OR 腳下 任一勾選都需要它
        self.sync_attack_damage_hook(h_raw, new_attack_dmg || new_damage_at_feet);
        self.sync_monster_color_patch(h_raw, new_monster_color);
        self.sync_damage_at_feet_hook(h_raw, new_damage_at_feet);
    }

    fn apply_misc_tab(&self, s: &AuxSettings) {
        let set_cb = |cb: &nwg::CheckBox, v: bool| {
            cb.set_check_state(if v {
                nwg::CheckBoxState::Checked
            } else {
                nwg::CheckBoxState::Unchecked
            });
        };
        set_cb(&self.misc_all_day_cb, s.misc.all_day);
        set_cb(&self.misc_underwater_pump_cb, s.misc.underwater_pump);
        set_cb(&self.misc_low_cpu_cb, s.misc.low_cpu);
        set_cb(&self.misc_monster_color_cb, s.misc.monster_level_color);
        set_cb(&self.misc_show_clock_cb, s.misc.show_clock);
        set_cb(&self.misc_show_attack_dmg_cb, s.misc.show_attack_dmg);
        set_cb(&self.misc_damage_at_feet_cb, s.misc.damage_at_feet);

        // 載入 profile 時也要把 hook 狀態對齊到 setting
        let h_raw = self.game_handle.load(Ordering::SeqCst);
        self.sync_all_day_patch(h_raw, s.misc.all_day);
        self.sync_underwater_pump_patch(h_raw, s.misc.underwater_pump);
        self.sync_low_cpu_hook(h_raw, s.misc.low_cpu);
        self.sync_show_clock_patch(h_raw, s.misc.show_clock);
        self.sync_attack_damage_hook(h_raw, s.misc.show_attack_dmg || s.misc.damage_at_feet);
        self.sync_monster_color_patch(h_raw, s.misc.monster_level_color);
        self.sync_damage_at_feet_hook(h_raw, s.misc.damage_at_feet);
    }

    fn sync_all_day_patch(&self, h_raw: usize, want_enabled: bool) {
        if h_raw == 0 {
            return;
        }

        let h = HANDLE(h_raw as *mut _);
        let patch = crate::aux::toggle::all_day::AllDay;
        let result = if want_enabled {
            crate::aux::toggle::Toggle::enable(&patch, h)
        } else {
            crate::aux::toggle::Toggle::disable(&patch, h)
        };

        if let Err(e) = result {
            log_line!("[all_day] sync failed: {e}");
        }
    }

    fn sync_underwater_pump_patch(&self, h_raw: usize, want_enabled: bool) {
        if h_raw == 0 {
            return;
        }

        let h = HANDLE(h_raw as *mut _);
        let patch = crate::aux::toggle::underwater_pump::UnderwaterPump;
        let result = if want_enabled {
            crate::aux::toggle::Toggle::enable(&patch, h)
        } else {
            crate::aux::toggle::Toggle::disable(&patch, h)
        };

        if let Err(e) = result {
            log_line!("[underwater_pump] sync failed: {e}");
        }
    }

    fn sync_low_cpu_hook(&self, h_raw: usize, want_enabled: bool) {
        if h_raw == 0 {
            return;
        }
        let installed = crate::aux::low_cpu_hook::is_installed();
        if want_enabled == installed {
            return;
        }

        let h = HANDLE(h_raw as *mut _);
        if want_enabled {
            let pid = unsafe { windows::Win32::System::Threading::GetProcessId(h) };
            if pid == 0 {
                log_line!("[low_cpu] GetProcessId 失敗,放棄安裝");
                return;
            }
            if let Err(e) = crate::aux::low_cpu_hook::install(h, pid) {
                log_line!("[low_cpu] 安裝失敗: {e}");
            }
        } else if let Err(e) = crate::aux::low_cpu_hook::uninstall(h) {
            log_line!("[low_cpu] 卸載失敗: {e}");
        }
    }

    /// 對齊 show_clock patch 安裝狀態與設定值。
    fn sync_show_clock_patch(&self, h_raw: usize, want_enabled: bool) {
        if h_raw == 0 {
            return;
        }
        let installed = crate::aux::show_clock_patch::is_installed();
        if want_enabled == installed {
            return;
        }

        let h = HANDLE(h_raw as *mut _);
        if want_enabled {
            if let Err(e) = crate::aux::show_clock_patch::install(h) {
                log_line!("[show_clock] 安裝失敗: {e}");
            }
        } else if let Err(e) = crate::aux::show_clock_patch::uninstall(h) {
            log_line!("[show_clock] 卸載失敗: {e}");
        }
    }

    // ════════════ tab7 定時 handlers ════════════
    fn sync_attack_damage_hook(&self, h_raw: usize, want_enabled: bool) {
        if h_raw == 0 {
            return;
        }
        let installed = crate::aux::attack_damage_hook::is_installed();
        if want_enabled == installed {
            return;
        }

        let h = HANDLE(h_raw as *mut _);
        if want_enabled {
            if let Err(e) = crate::aux::attack_damage_hook::install(h) {
                log_line!("[attack_damage] install failed: {e}");
            }
        } else if let Err(e) = crate::aux::attack_damage_hook::uninstall(h) {
            log_line!("[attack_damage] uninstall failed: {e}");
        }
    }

    /// 對齊 damage_at_feet patch:打勾 → 傷害氣泡顯示在怪物腳下並翻轉箭頭朝上
    fn sync_damage_at_feet_hook(&self, h_raw: usize, want_enabled: bool) {
        if h_raw == 0 {
            return;
        }
        let installed = crate::aux::attack_damage_feet_hook::is_installed();
        if want_enabled == installed {
            return;
        }

        let h = HANDLE(h_raw as *mut _);
        if want_enabled {
            if let Err(e) = crate::aux::attack_damage_feet_hook::install(h) {
                log_line!("[damage_feet] install failed: {e}");
            }
        } else if let Err(e) = crate::aux::attack_damage_feet_hook::uninstall(h) {
            log_line!("[damage_feet] uninstall failed: {e}");
        }
    }

    /// 對齊怪物等級色彩 patch 安裝狀態與設定值。
    ///
    /// 啟用時啟動 3.8 world entity scanner，直接更新可見 entity 的 `+0x30`
    /// 名稱顏色欄位；卸載時還原本次曾碰過的原始顏色。
    fn sync_monster_color_patch(&self, h_raw: usize, want_enabled: bool) {
        if h_raw == 0 {
            return;
        }
        let installed = crate::aux::monster_color_patch::is_installed();
        if want_enabled == installed {
            return;
        }

        let h = HANDLE(h_raw as *mut _);
        if want_enabled {
            if let Err(e) = crate::aux::monster_color_patch::install(h) {
                log_line!("[monster_color] 安裝失敗: {e}");
            }
        } else if let Err(e) = crate::aux::monster_color_patch::uninstall(h) {
            log_line!("[monster_color] 卸載失敗: {e}");
        }
    }

    fn on_timer_change(&self) {
        if self.applying.load(Ordering::SeqCst) {
            return;
        }
        let mut s = self.settings.write();
        s.timer_master_enabled = matches!(
            self.timer_enabled_cb.check_state(),
            nwg::CheckBoxState::Checked
        );
        let cbs = [
            &self.timer_cb_0,
            &self.timer_cb_1,
            &self.timer_cb_2,
            &self.timer_cb_3,
            &self.timer_cb_4,
            &self.timer_cb_5,
        ];
        let nums = [
            &self.timer_num_0,
            &self.timer_num_1,
            &self.timer_num_2,
            &self.timer_num_3,
            &self.timer_num_4,
            &self.timer_num_5,
        ];
        let texts = [
            &self.timer_text_0,
            &self.timer_text_1,
            &self.timer_text_2,
            &self.timer_text_3,
            &self.timer_text_4,
            &self.timer_text_5,
        ];
        for i in 0..6 {
            s.timer_rows[i].enabled = matches!(cbs[i].check_state(), nwg::CheckBoxState::Checked);
            s.timer_rows[i].interval_sec = nums[i].text().parse().unwrap_or(5);
            s.timer_rows[i].command = texts[i].text();
        }
        log_line!(
            "[lhx] timer 變動:master={} row[0].enabled={} row[0].interval={}",
            s.timer_master_enabled,
            s.timer_rows[0].enabled,
            s.timer_rows[0].interval_sec
        );
    }

    /// 推 row N 的 reset epoch — timer thread 比對到差異就重設該 row 的 last_fire。
    fn bump_reset(&self, idx: usize) {
        if let Some(resets) = self.timer_resets.upgrade() {
            resets[idx].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            log_line!("[lhx] timer row {} 重計", idx);
        } else {
            log_line!(
                "[lhx] timer row {} 重計但 timer_resets 已釋放(視窗 outlived AuxControl?)",
                idx
            );
        }
    }

    fn on_timer_reset_0(&self) {
        self.bump_reset(0);
    }
    fn on_timer_reset_1(&self) {
        self.bump_reset(1);
    }
    fn on_timer_reset_2(&self) {
        self.bump_reset(2);
    }
    fn on_timer_reset_3(&self) {
        self.bump_reset(3);
    }
    fn on_timer_reset_4(&self) {
        self.bump_reset(4);
    }
    fn on_timer_reset_5(&self) {
        self.bump_reset(5);
    }

    fn on_timer_help(&self) {
        let title = crate::i18n::tr("指令說明");
        let body = crate::i18n::tr(
            "格式:<名稱>[/<後綴>](對齊喝水/Buff 分頁)\n\
             沒寫後綴 = 物品(/I)。\n\
             \n\
             物品:\n\
             ・肉                  → USE_ITEM(吃肉/喝水/卷軸…)\n\
             ・治癒藥水/I          → 同上，顯式寫 /I 也行\n\
             ・<卷軸>/IA           → 對「(使用中)」防具(找第一件)\n\
             ・<卷軸>/IA=<裝備名>  → 對指定名稱的「(使用中)」裝備\n\
             ・<卷軸>/IW           → 對「(揮舞)」武器(找第一件)\n\
             ・<卷軸>/IW=<武器名>  → 對指定名稱的「(揮舞)」武器\n\
             ・<卷軸>/I=<物品名>   → 對指定名稱物品(不限狀態)\n\
             ・<卷軸>/IT           → 快捷鍵觸發 USE_ITEM,進入目標選擇模式(再手動點目標)\n\
             ・<卷軸>/IT=<entity名>→ 全自動對指定名玩家/召喚物施放(掃 heap 找 entity)\n\
             ・<卷軸>/IME          → 對自己施放(治癒卷軸等需 target 卷軸,自施專用)\n\
             \n\
             技能:\n\
             ・加速術/M            → 自身 buff(不指定 target)\n\
             ・冰錐術/M            → 攻擊技能(鼠標當下目標)\n\
             ・保護罩/ME           → 對自己施法\n\
             ・體魄強健術/ME       → 對自己施法(走 ForceSelfPacket)\n\
             ・<技能>/MIA          → 對「(使用中)」物品施法\n\
             ・<技能>/MIW          → 對「(揮舞)」物品施法\n\
             ・<技能>/MI=<物品名>  → 對指定名稱物品施法\n\
             \n\
             每 tick 只觸發一個 row,多個同時到期由上而下取最小 idx。\n\
             重計按鈕 = 重新從現在開始計時(要再等滿間隔秒數)。",
        );
        nwg::modal_info_message(&self.window, &title, &body);
    }

    fn apply_timer_tab(&self, s: &AuxSettings) {
        self.timer_enabled_cb
            .set_check_state(if s.timer_master_enabled {
                nwg::CheckBoxState::Checked
            } else {
                nwg::CheckBoxState::Unchecked
            });
        let cbs = [
            &self.timer_cb_0,
            &self.timer_cb_1,
            &self.timer_cb_2,
            &self.timer_cb_3,
            &self.timer_cb_4,
            &self.timer_cb_5,
        ];
        let nums = [
            &self.timer_num_0,
            &self.timer_num_1,
            &self.timer_num_2,
            &self.timer_num_3,
            &self.timer_num_4,
            &self.timer_num_5,
        ];
        let texts = [
            &self.timer_text_0,
            &self.timer_text_1,
            &self.timer_text_2,
            &self.timer_text_3,
            &self.timer_text_4,
            &self.timer_text_5,
        ];
        for i in 0..6 {
            cbs[i].set_check_state(if s.timer_rows[i].enabled {
                nwg::CheckBoxState::Checked
            } else {
                nwg::CheckBoxState::Unchecked
            });
            nums[i].set_text(&s.timer_rows[i].interval_sec.to_string());
            texts[i].set_text(&s.timer_rows[i].command);
        }
    }
}

// ─── 預設清單(linhelperZ.ini,user 可編輯的 INI 設定檔) ────

const POTION_LIST_FILE: &str = "linhelperZ.ini";

/// 透過 Win32 `CB_GETDROPPEDSTATE` 訊息問某個 ComboBox 的下拉是否展開中。
/// NWG 沒公開 method,直接用 HWND 送訊息。
fn combo_dropdown_open(combo: &nwg::ComboBox<String>) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
    const CB_GETDROPPEDSTATE: u32 = 0x0157;
    match combo.handle.hwnd() {
        Some(hwnd) => unsafe {
            let h = HWND(hwnd as *mut _);
            SendMessageW(h, CB_GETDROPPEDSTATE, None, None).0 != 0
        },
        None => false,
    }
}

/// 寫死的 fallback 清單 — 當 INI [AllHP] 缺漏或讀不到時用此清單填 dropdown。
/// 順序從上到下對應補血力道由弱到強(基本藥水 → 進階藥水 → 治癒術 → 卷軸)。
///
/// 字尾類型標記:
/// - `/M`  = 魔法,**不指定 target**(packet 不送 target 欄位,server 由 session 推斷)
/// - `/ME` = 魔法,**指定自己**(packet target 欄位塞自己 char_id)
/// - `/I`  = 物品(含卷軸,3.8 USE_ITEM 對自身可用品預設打自己)
///
/// drink_tick 目前只處理一般物品(背包 type=0x33),選到 /M /ME 的會 silent skip。
/// 留著條目是為了 dropdown 完整,將來補施法功能可直接用同一份清單。
fn hardcoded_potion_names() -> Vec<String> {
    [
        "治癒藥水",
        "強力治癒藥水",
        "終極治癒藥水",
        "濃縮體力恢復劑",
        "濃縮強力體力恢復劑",
        "濃縮終極體力恢復劑",
        "古代體力恢復劑",
        "古代強力體力恢復劑",
        "古代終極體力恢復劑",
        "初級治癒術/ME",
        "中級治癒術/ME",
        "高級治癒術/ME",
        "全部治癒術/ME",
        "生命的祝福/M",
        "魔法卷軸 (初級治癒術)/IME",
        "魔法卷軸 (中級治癒術)/IME",
        "魔法卷軸 (高級治癒術)/IME",
        "傳送回家的卷軸/IME",
        "瞬間移動卷軸/IME",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// 洗魔下拉的 fallback 清單(INI [AllHP] 內 HPMP* 條目缺漏時的 default)。
fn hardcoded_hpmp_names() -> Vec<String> {
    ["心靈轉換/M", "魂體轉換/M"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// 洗魔下拉清單來源:`[AllHP]` section 內 `HPMP*` 條目,沒設定就 fallback。
fn default_hpmp_names() -> Vec<String> {
    if let Some(list) = load_hpmp_list_ini() {
        if !list.is_empty() {
            return list;
        }
    }
    hardcoded_hpmp_names()
}

/// 讀 `[AllState]` section 內所有 `Item*=value` 的 value(buff 自動補對應的條目)。
/// 回傳的字串是原始格式(例如 `0_自我加速藥水` `0_加速術/ME`),由 `parse_buff_item` 解析。
pub(crate) fn load_state_list_ini() -> Vec<String> {
    let path = launcher_exe_dir().join(POTION_LIST_FILE);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut in_section = false;
    let mut names: Vec<String> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed.eq_ignore_ascii_case("[AllState]");
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, val)) = trimmed.split_once('=') {
            if !key.trim().to_ascii_lowercase().starts_with("item") {
                continue;
            }
            let v = val.trim();
            if !v.is_empty() {
                names.push(v.to_string());
            }
        }
    }
    names
}

/// 讀 INI 任意 section 內 `<key_prefix>*=value` 的 value 清單。
/// 找不到檔案 / section 不存在都回空 Vec。
fn load_section_items(section: &str, key_prefix: &str) -> Vec<String> {
    let path = launcher_exe_dir().join(POTION_LIST_FILE);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let target_header = format!("[{section}]");
    let lc_prefix = key_prefix.to_ascii_lowercase();
    let mut in_section = false;
    let mut names: Vec<String> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed.eq_ignore_ascii_case(&target_header);
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, val)) = trimmed.split_once('=') {
            if !key.trim().to_ascii_lowercase().starts_with(&lc_prefix) {
                continue;
            }
            let v = val.trim();
            if !v.is_empty() {
                names.push(v.to_string());
            }
        }
    }
    names
}

/// 讀 `[AllHP]` section 內所有 `HPMP*=value` 的 value(心靈轉換/魂體轉換等 HP→MP 技能)。
fn load_hpmp_list_ini() -> Option<Vec<String>> {
    let path = launcher_exe_dir().join(POTION_LIST_FILE);
    let content = std::fs::read_to_string(&path).ok()?;
    let mut in_section = false;
    let mut names: Vec<String> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed.eq_ignore_ascii_case("[AllHP]");
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, val)) = trimmed.split_once('=') {
            if !key.trim().to_ascii_lowercase().starts_with("hpmp") {
                continue;
            }
            let v = val.trim();
            if !v.is_empty() {
                names.push(v.to_string());
            }
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}

/// 取得 launcher.exe 所在目錄
fn launcher_exe_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// 讀 launcher.exe 旁 `linhelperZ.ini`,取 `[AllHP]` section 內所有 `Item* = value` 的 value。
/// 找不到檔案 / 沒有 section / section 內無有效 Item entry → 回 None。
///
/// INI 格式:
/// ```ini
/// [AllHP]
/// Item0=治癒藥水
/// Item1=強力治癒藥水
/// ...
/// ```
/// 只取 key 以 `Item` 開頭的條目(略過 `GoHome*` / `HPMP*`,那些屬於其他子系統)。
/// `#` 或 `;` 開頭視為註解。
fn load_potion_list_ini() -> Option<Vec<String>> {
    let path = launcher_exe_dir().join(POTION_LIST_FILE);
    let content = std::fs::read_to_string(&path).ok()?;
    let mut in_section = false;
    let mut names: Vec<String> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed.eq_ignore_ascii_case("[AllHP]");
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, val)) = trimmed.split_once('=') {
            if !key.trim().to_ascii_lowercase().starts_with("item") {
                continue; // 略過 GoHome/HPMP 等其他類型
            }
            let v = val.trim();
            if !v.is_empty() {
                names.push(v.to_string());
            }
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}

/// 第一次啟動 launcher 沒看到 `linhelperZ.ini` 時,寫一份完整範本。
/// 已存在(不論內容)就完全不動,避免覆蓋 user 改過的清單。
///
/// 為什麼預寫範本而非全 fallback:user 容易看出 INI 結構並照樣加新條目,不必猜
/// section / key 命名。範本涵蓋所有 section:
/// - `[AllHP]`        — 喝水(目前唯一被讀取的 section,GoHome/Item/HPMP)
/// - `[AllState]`     — buff 類(留給未來自動施法/喝補品)
/// - `[AllPolyItems]` — 變身卷軸物品名(背包要有的物品名)
/// - `[AllPolymorphs]` — 變身選項(IP 封包送出的英文 option 字串)
/// - `[AllAntidote]`  — 解毒(未來)
///
/// `fallback` 參數保留是為了介面一致,但範本是寫死的完整清單,不從 fallback 帶入。
fn write_potion_list_template_if_missing(_fallback: &[String]) {
    let path = launcher_exe_dir().join(POTION_LIST_FILE);
    if path.exists() {
        return;
    }
    // 完整 INI 範本 — user 改檔不需重 build。
    // 目前只讀 [AllHP] Item* 條目,其他 section 預先留下將來擴充用。
    const TEMPLATE: &str = r#"# 自動喝水 / 輔助功能預設清單 — launcher.exe 旁的 linhelperZ.ini
# user 可直接編輯,改完不需重 build。
#
# 字尾標記:
#   /I    = 物品(背包點選使用,自喝藥水/食物用此)
#   /IME  = 物品對自己(治癒卷軸等需 target 卷軸,II packet target=self_char_id)
#   /M    = 魔法(不指定 target)
#   /ME   = 魔法(指定自己)
# 目前喝水(drink_tick)只處理一般物品(背包 type=0x33),選到 /M /ME 的會 silent skip;
# 字尾留著為了 dropdown 完整,將來補施法功能可直接用同一份清單。
#
# 區段功能對應:
#   [AllHP]         — 喝水補血(目前唯一被讀取)
#   [AllState]      — buff/狀態(未實作)
#   [AllPolyItems]  — 變身卷軸物品(背包要有的物品名)
#   [AllPolymorphs] — 變身選項(IP 封包用,英文 option 字串來源)
#   [AllAntidote]   — 解毒(未實作)

[AllHP]
GoHome0=傳送回家的卷軸
GoHome1=血盟傳送卷軸
GoHome2=瞬間移動卷軸
GoHome3=世界樹的呼喚/M
Item0=治癒藥水
Item1=強力治癒藥水
Item2=終極治癒藥水
Item3=濃縮體力恢復劑
Item4=濃縮強力體力恢復劑
Item5=濃縮終極體力恢復劑
Item6=古代體力恢復劑
Item7=古代強力體力恢復劑
Item8=古代終極體力恢復劑
Item9=初級治癒術/ME
Item10=中級治癒術/ME
Item11=高級治癒術/ME
Item12=全部治癒術/ME
Item13=生命的祝福/M
Item14=魔法卷軸 (初級治癒術)/IME
Item15=魔法卷軸 (中級治癒術)/IME
Item16=魔法卷軸 (高級治癒術)/IME
Item17=傳送回家的卷軸/IME
Item18=瞬間移動卷軸/IME
HPMP0=心靈轉換/M
HPMP1=魂體轉換/M

[AllState]
# 3.8 自身 buff 統一用 /ME(指定自己)— /M 不動 [0x97C910] 殘留 garbage 會讓 server ERROR
Item0=0_自我加速藥水
Item1=0_強化 自我加速藥水
Item2=0_加速術/ME
Item3=0_強力加速術/ME
Item4=0_綠色藥水
Item5=0_強化 綠色藥水
Item6=2_勇敢藥水
Item7=2_精靈餅乾
Item8=2_名譽貨幣
Item9=2_惡魔之血
Item10=153_生命之樹果實
Item11=3_慎重藥水
Item12=4_保護罩/ME
Item13=5_影之防護/ME
Item14=6_大地防護/ME
Item15=7_大地的祝福/ME
Item16=8_鋼鐵防護/ME
Item17=9_體魄強健術/ME
Item18=10_通暢氣脈術/ME
Item19=11_伊娃的祝福
Item20=11_人魚之鱗
Item21=12_神聖武器/ME
Item22=13_祝福魔法武器/ME
Item23=14_魔法防禦/ME
Item24=15_屬性防禦/ME
Item25=17_淨化精神/ME
Item26=18_火焰武器/ME
Item27=19_烈炎氣息/ME
Item28=20_烈炎武器/ME
Item29=21_風之神射/ME
Item30=22_暴風之眼/ME
Item31=23_暴風神射/ME
Item32=24_風之疾走/ME
Item33=25_激勵士氣/ME
Item34=26_鋼鐵士氣/ME
Item35=27_衝擊士氣/ME
Item36=29_附加劇毒/ME
Item37=30_燃燒鬥志/ME
Item38=31_雙重破壞/ME
Item39=32_暗影閃避/ME
Item40=33_毒性抵抗/ME
Item41=34_力量提升/ME
Item42=35_敏捷提升/ME
Item43=36_閃避提升/ME
Item44=37_行走加速/ME
Item45=38_藍色藥水
Item46=38_加速魔力回復藥水
Item47=40_狂暴術/ME
Item48=41_聖結界/ME
Item49=42_神聖疾走/ME
Item50=43_絕對屏障/ME
Item51=44_靈魂昇華/ME
Item52=46_生命之泉/ME
Item53=47_負重強化/ME
Item54=56_體能激發/ME
Item55=58_屬性之火/ME
Item56=82_魔法娃娃：野狼寶寶
Item57=82_魔法娃娃：肥肥
Item58=82_魔法娃娃：小思克巴
Item59=107_龍之護鎧/ME
Item60=110_血之渴望/ME
Item61=113_致命身軀/ME
Item62=116_鏡像/ME
Item63=118_幻覺：歐吉/ME
Item64=120_專注/ME
Item65=121_幻覺：巫妖/ME
Item66=123_耐力/ME
Item67=126_幻覺：鑽石高崙/ME
Item68=128_洞察/ME
Item69=130_幻覺：化身/ME

[AllPolyItems]
# 變身卷軸物品名 — 只放「選單型」卷軸(SQL use_type=sosc / Sosc_PolyReel)。
# 這類物品使用後 server 回 IP 封包等 client 送 option 字串,launcher 用 [AllPolymorphs] 選的英文 option 回應。
# 已知選單型(itemId/SQL):
#   40088  變形卷軸          Sosc_PolyReel
#   40096  象牙塔變身卷軸    Sosc_PolyReel
#   140088 變形卷軸          Sosc_PolyReel
# 直接變身的物品(例:黑暗安特的樹皮)不放這 — 那種用 mode 1 USE_ITEM 即可,option 留空。
Item0=象牙塔變身卷軸
Item1=變形卷軸

[AllPolymorphs]
Item0=狼人_re werewolf_3865
Item1=骷髏_re skeleton_2374
Item2=食人妖精_re bugbear_8859
Item3=萊肯_re lycanthrope_3874
Item4=妖魔巡守_re orc scout_8860
Item5=死亡騎士52_death 52_6137
Item6=黑暗精靈52_re darkelf bow_8808
Item7=黑暗騎士_neo black knight_138
Item8=黑暗法師_neo black mage_6268
Item9=黑暗巡守_neo black scouter_6269
Item10=黑暗刺客_neo black assassin_6279
Item11=銀光騎士_neo silver knight_6270
Item12=銀光法師_neo silver mage_6271
Item13=銀光巡守_neo silver scouter_6272
Item14=銀光刺客_neo silver assassin_6280
Item15=黃金騎士_neo gold knight_6273
Item16=黃金法師_neo gold mage_6274
Item17=黃金巡守_neo gold scouter_6275
Item18=黃金刺客_neo gold assassin_6281
Item19=白金騎士_neo platinum knight_6276
Item20=白金法師_neo platinum mage_6277
Item21=白金巡守_neo platinum scouter_6278
Item22=白金刺客_neo platinum assassin_6282
Item23=死亡騎士55_death 55_6142
Item24=死亡騎士60_death 60_6147
Item25=死亡騎士65_death 65_6152
Item26=死亡騎士70_death 70_6157
Item27=死亡騎士75_death 75_9205
Item28=死亡騎士80_death 80_9206
Item29=黑暗精靈55_darkelf 55_6145
Item30=黑暗精靈60_darkelf 60_6150
Item31=黑暗精靈65_darkelf 65_6155
Item32=黑暗精靈70_darkelf 70_6160
Item33=黑暗精靈75_darkelf 75_9225
Item34=黑暗精靈80_darkelf 80_9226

[AllAntidote]
Item0=解毒術/ME
Item1=安特之樹枝
Item2=解毒藥水
Item3=翡翠藥水
"#;
    let _ = std::fs::write(&path, TEMPLATE);
}

/// 對舊版 launcher 留下的 INI 補加缺漏的 section。
///
/// 範本是「不存在才寫」邏輯,使用者升級 launcher 後既有 INI 不會自動拿到新 section
/// (例 `[AllPolyItems]`)。這個函式檢查必要 section 是否存在,缺的就 append 在檔尾。
///
/// 為什麼不直接重寫整份範本:會把使用者自訂的清單砍掉。
fn migrate_potion_list_ini() {
    let path = launcher_exe_dir().join(POTION_LIST_FILE);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };

    let has_section = |name: &str| -> bool {
        let header = format!("[{name}]");
        content
            .lines()
            .any(|l| l.trim().eq_ignore_ascii_case(&header))
    };

    let mut additions = String::new();
    if !has_section("AllPolyItems") {
        additions.push_str("\n[AllPolyItems]\n");
        additions.push_str(
            "# 變身卷軸物品名 — 只放「選單型」卷軸(SQL use_type=sosc / Sosc_PolyReel)。\n",
        );
        additions.push_str(
            "# 已知選單型 itemId:40088/40096/140088。直接變身物品(例:黑暗安特的樹皮)不放這。\n",
        );
        additions.push_str("Item0=象牙塔變身卷軸\n");
        additions.push_str("Item1=變形卷軸\n");
    }

    if additions.is_empty() {
        return;
    }
    let mut new_content = content;
    if !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(&additions);
    let _ = std::fs::write(&path, new_content);
}

/// 拿掉物品名稱尾巴的「(數量)」例如:
/// "古代體力恢復劑 (208)" → "古代體力恢復劑"
/// 把 `[AllPolymorphs]` INI 條目抽出 server 要的英文 option string。
///
/// 條目格式 `<中文>_<英文 option>_<spr_id>`,範例 `狼人_re werewolf_3865`。
/// 找不到合法格式 → 整串原樣回(讓使用者自己手填純英文也能用,像 INI 直接寫 `re werewolf`)。
pub(crate) fn extract_polymorph_option(raw: &str) -> &str {
    let parts: Vec<&str> = raw.splitn(3, '_').collect();
    if parts.len() == 3 && parts[2].trim().parse::<u32>().is_ok() {
        return parts[1].trim();
    }
    raw.trim()
}

/// 把 `[AllPolymorphs]` INI 條目抽 sprite_id(鎖定變身比對用)。
///
/// 找不到合法 spr_id → 回 0(未來鎖定變身觸發時把 0 當「不比對」)。
#[allow(dead_code)]
pub(crate) fn extract_polymorph_spr_id(raw: &str) -> u32 {
    let parts: Vec<&str> = raw.splitn(3, '_').collect();
    if parts.len() == 3 {
        return parts[2].trim().parse::<u32>().unwrap_or(0);
    }
    0
}

mod buff_format;
pub(crate) use buff_format::*;

/// 還原所有 LHX session-scoped 的 misc 分頁 toggle/hook 對遊戲記憶體的修改。
///
/// 在 [`LhxActiveSession::shutdown`] 結尾呼叫,確保關閉 LHX(或換角)時 patch
/// 不會殘留在遊戲 process 裡 — 否則「不開輔助仍是全白天」「換角後仍開水底通行」
/// 等狀態會永久殘留(因為 game process 沒重啟,記憶體 bytes 仍是被 patch 的狀態)。
///
/// 全部 disable 都是 idempotent — 對沒裝過的 toggle 自然是 no-op。失敗只 log 不
/// bail,讓後面的 toggle 仍有機會還原。
///
/// 必須在所有 sync thread / GUI thread 退出**之後**才呼叫,避免 race。
pub fn restore_all_misc_patches(h: HANDLE) {
    use crate::aux::toggle::{all_day::AllDay, underwater_pump::UnderwaterPump, Toggle};

    if let Err(e) = AllDay.disable(h) {
        log_line!("[shutdown] all_day disable: {e}");
    }
    if let Err(e) = UnderwaterPump.disable(h) {
        log_line!("[shutdown] underwater_pump disable: {e}");
    }
    if crate::aux::low_cpu_hook::is_installed() {
        if let Err(e) = crate::aux::low_cpu_hook::uninstall(h) {
            log_line!("[shutdown] low_cpu uninstall: {e}");
        }
    }
    if crate::aux::show_clock_patch::is_installed() {
        if let Err(e) = crate::aux::show_clock_patch::uninstall(h) {
            log_line!("[shutdown] show_clock uninstall: {e}");
        }
    }
    if crate::aux::attack_damage_hook::is_installed() {
        if let Err(e) = crate::aux::attack_damage_hook::uninstall(h) {
            log_line!("[shutdown] attack_damage uninstall: {e}");
        }
    }
    if crate::aux::attack_damage_feet_hook::is_installed() {
        if let Err(e) = crate::aux::attack_damage_feet_hook::uninstall(h) {
            log_line!("[shutdown] damage_feet uninstall: {e}");
        }
    }
    if crate::aux::monster_color_patch::is_installed() {
        if let Err(e) = crate::aux::monster_color_patch::uninstall(h) {
            log_line!("[shutdown] monster_color uninstall: {e}");
        }
    }
}

/// 啟動視窗 thread。
///
/// 回傳 WindowControl,呼叫者透過 `visible` 控制顯示/隱藏 / 結束。
/// 若 visible 設為 VISIBLE_CLOSE,視窗 thread 會結束。
pub fn spawn_window_thread(
    settings: Arc<RwLock<AuxSettings>>,
    h: HANDLE,
    timer_resets_weak: std::sync::Weak<[std::sync::atomic::AtomicU64; 6]>,
) -> WindowControl {
    let visible = Arc::new(AtomicU8::new(VISIBLE_HIDDEN));
    let visible_clone = visible.clone();
    let game_handle = Arc::new(AtomicUsize::new(h.0 as usize));
    let game_handle_clone = game_handle.clone();
    let game_hwnd = Arc::new(AtomicUsize::new(0));
    let game_hwnd_clone = game_hwnd.clone();
    // 預先設 true,讓 build_ui 在初始化控件期間觸發的事件不會 settings.write
    let applying = Arc::new(AtomicBool::new(true));
    let applying_clone = applying.clone();

    let thread = std::thread::spawn(move || {
        if let Err(e) = nwg::init() {
            log_line!("[lhx] nwg init 失敗: {e:?}");
            return;
        }

        // 補老版本 launcher 留下的 INI(缺新加 section 例 [AllPolyItems])。
        // 必須在 build_ui / apply_settings 前跑,因為各分頁會即時讀 INI 來填 dropdown。
        write_potion_list_template_if_missing(&[]);
        migrate_potion_list_ini();

        // 字型
        let visual_scale = current_lhx_visual_scale();

        let mut font = nwg::Font::default();
        if let Err(e) = nwg::Font::builder()
            .family("Microsoft JhengHei UI")
            .size(lhx_font_size_for_scale(visual_scale))
            .build(&mut font)
        {
            log_line!("[lhx] 字型建立失敗: {e:?}");
        }
        nwg::Font::set_global_default(Some(font));

        let initial = LhxWindow {
            settings: settings.clone(),
            visible: visible_clone.clone(),
            game_handle: game_handle_clone.clone(),
            game_hwnd: game_hwnd_clone.clone(),
            applying: applying_clone.clone(),
            timer_resets: timer_resets_weak.clone(),
            ..Default::default()
        };
        let app = match LhxWindow::build_ui(initial) {
            Ok(a) => a,
            Err(e) => {
                log_line!("[lhx] build_ui 失敗: {e:?}");
                return;
            }
        };
        scale_lhx_child_controls(&app.window, visual_scale);
        if let Some(icon) = load_app_icon() {
            let icon = Box::leak(Box::new(icon));
            app.window.set_icon(Some(icon));
        }
        // 簡體模式時翻譯整個 LHX UI(含 tab labels)。繁體 / Auto 模式直接 no-op。
        if let Some(raw) = app.window.handle.hwnd() {
            crate::i18n::retranslate_lhx(windows::Win32::Foundation::HWND(raw as *mut _));
        }
        app.apply_tab_layout();
        app.apply_settings();
        app.enable_all_widget_wheel_scroll();
        if let Some(raw) = app.window.handle.hwnd() {
            install_wheel_forwarding(windows::Win32::Foundation::HWND(raw as *mut _));
        }
        app.force_hide_window();
        app.visible_timer.start();

        // 設 LHX 為遊戲視窗的 owned window 曾在部分環境觸發遊戲視窗被放大。
        // 先保守維持獨立 topmost 視窗，不再改遊戲 HWND parent/owner。
        if should_attach_lhx_to_game_owner() {
            unsafe {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::{
                    SetWindowLongW, SetWindowPos, GWLP_HWNDPARENT, HWND_NOTOPMOST, SWP_NOACTIVATE,
                    SWP_NOMOVE, SWP_NOSIZE,
                };
                // 多開安全:走 game_window cache。 cache miss fallback 老 FindWindowW(早期 boot 不破)。
                match crate::aux::game_window::cached_or_find_game_hwnd() {
                    Some(game_hwnd) if !game_hwnd.is_invalid() => {
                        game_hwnd_clone.store(game_hwnd.0 as usize, Ordering::Relaxed);
                        if let Some(nwg_hwnd) = app.window.handle.hwnd() {
                            let lhx_hwnd = HWND(nwg_hwnd as *mut _);
                            SetWindowLongW(lhx_hwnd, GWLP_HWNDPARENT, game_hwnd.0 as i32);
                            if should_clear_lhx_topmost_after_owner_attached(true) {
                                let _ = SetWindowPos(
                                    lhx_hwnd,
                                    Some(HWND_NOTOPMOST),
                                    0,
                                    0,
                                    0,
                                    0,
                                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                                );
                            }
                            log_line!(
                                "[lhx] 設為遊戲視窗 owned window (game HWND={:?})",
                                game_hwnd.0
                            );
                        }
                    }
                    _ => log_line!("[lhx] 找不到遊戲視窗,維持 topmost 獨立模式"),
                }
            }
        } else {
            log_line!("[lhx] owned window attach disabled; using standalone topmost mode");
        }
        if visible_clone.load(Ordering::Relaxed) == VISIBLE_HIDDEN {
            app.force_hide_window();
        }

        // 啟動通知 — 階段 1 stub:寫到 launcher console。
        // 後續階段會改成「遊戲內聊天框系統提示」(玩家熟悉的紅字訊息),
        // 需要先逆向出 chat display 函數位址 + thiscall codecave + ChatFrame 指標,
        // 屬於 Stage 4(codecave call)範圍。
        // 關鍵線索(2026-04-28 嗅到):
        //   - 0x008CD568: "addChatMsg"(scripting 反射用 key)
        //   - 0x009718C0: ?AUChatFrameSub@@ RTTI
        //   - 0x009717F4: ?AUChatFrameBitmap@@ RTTI
        log_line!("[lhx] LinHelper 輔助已啟動 (按 Home 鍵切換顯示/隱藏)");

        nwg::dispatch_thread_events();
        log_line!("[lhx] window thread 結束");
    });

    WindowControl {
        visible,
        thread,
        game_handle,
    }
}

#[cfg(test)]
mod parser_tests;
#[cfg(test)]
mod ui_layout_tests;
