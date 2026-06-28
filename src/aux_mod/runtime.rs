//! 輔助功能執行緒框架 — AuxScheduler
//!
//! 為什麼:每個輔助功能(自動喝水/補 buff/解毒/吃肉/磨刀石/變身)都需要 polling
//! 才能即時反應遊戲狀態。排程器保留獨立 polling thread,讓喝水/buff/status
//! 等 tick 不互相阻塞,實作細節拆在 `runtime/*` 模組內。
//! GUI 修改設定 → 寫 AuxSettings(RwLock)→ polling thread 下次 tick 自動讀新值,
//! user 不需要重啟 launcher 也不需要重 attach 遊戲。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::RwLock;
use windows::Win32::Foundation::HANDLE;

use crate::log_line;

mod types;
pub use types::*;

mod buff;
mod delete;
mod drink;
mod guards;
mod selectors;
mod shout;
mod status;
mod timer;

/// 控制 handle — 由 main 持有，可發信讓所有 thread 退出
pub struct AuxControl {
    pub cancel: Arc<AtomicBool>,
    pub settings: Arc<RwLock<AuxSettings>>,
    /// 自動喝水 codecave handle(HOME 第一次按下時 install,跟 scheduler 共享)
    pub drink: Arc<RwLock<Option<Arc<crate::aux::drink_hook::DrinkHandle>>>>,
    /// Spell DB(進場後 lazy build,buff_tick 'S' 路徑用 name → packed_skill_id)
    pub spell_db: Arc<RwLock<Option<crate::aux::spell_db::SpellDb>>>,
    /// Spell Book(玩家已學技能,ForceSelfPacket 路徑用 — 拿玩家實際 level 的 packed)
    pub spell_book: Arc<RwLock<Option<crate::aux::spell_book::SpellBook>>>,
    /// EXP 追蹤狀態 — LinHelperZ status_show_exp toggle 共享
    pub exp_tracker: Arc<RwLock<crate::aux::exp_tracker::ExpTracker>>,
    /// 定時分頁 6 row 的重計 epoch counter — UI 點重計就 fetch_add(1),
    /// timer_tick 比對到變動就重設該 row 的 last_fire(重新計時)。
    pub timer_resets: Arc<[AtomicU64; 6]>,
}

impl AuxControl {
    /// 用初始 AuxSettings 建立(會新開 Arc — 注意:跟 LHX window 不同步!)
    /// 推薦改用 [`AuxControl::from_shared`] 共享 Arc。
    #[allow(dead_code)]
    pub fn new(initial: AuxSettings) -> Self {
        Self::from_shared(Arc::new(RwLock::new(initial)))
    }

    /// 跟 LHX window 共享同一個 settings Arc — UI 即時生效。
    pub fn from_shared(settings: Arc<RwLock<AuxSettings>>) -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            settings,
            drink: Arc::new(RwLock::new(None)),
            spell_db: Arc::new(RwLock::new(None)),
            spell_book: Arc::new(RwLock::new(None)),
            exp_tracker: Arc::new(RwLock::new(crate::aux::exp_tracker::ExpTracker::default())),
            timer_resets: Arc::new([
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ]),
        }
    }

    pub fn shutdown(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// 輔助功能總排程器
pub struct AuxScheduler {
    pub h_process: HANDLE,
    pub pid: u32,
    pub control: Arc<AuxControl>,
}

impl AuxScheduler {
    pub fn new(h_process: HANDLE, pid: u32, control: Arc<AuxControl>) -> Self {
        Self {
            h_process,
            pid,
            control,
        }
    }

    /// 啟動所有 polling thread。每個輔助功能族群對應一條 polling thread。
    pub fn spawn_all(&self) -> Vec<JoinHandle<()>> {
        let mut handles = Vec::new();
        // HANDLE = *mut c_void 不是 Send，跨 thread 必須先轉為 usize，thread 內再轉回
        let h_raw = self.h_process.0 as usize;
        let _ = self.pid;

        // state_poll (1Hz) — 玩家狀態(只在變化時 log,避免 spam 蓋掉 [inv]/[scan])
        // + 第一次成功讀到狀態時 dump 一次物品欄(驗證 INVENTORY_BASE)
        {
            let cancel = self.control.cancel.clone();
            let inv_dumped = Arc::new(AtomicBool::new(false));
            handles.push(std::thread::spawn(move || {
                let h = HANDLE(h_raw as *mut _);
                let mut last: Option<crate::aux::player_state::PlayerState> = None;
                tick_loop("state_poll", Duration::from_secs(1), cancel, move |_| {
                    match crate::aux::player_state::read_player_state(h) {
                        Ok(s) if s.max_hp > 0 => {
                            let changed = match &last {
                                None => true,
                                Some(p) => {
                                    p.hp != s.hp
                                        || p.max_hp != s.max_hp
                                        || p.mp != s.mp
                                        || p.max_mp != s.max_mp
                                        || p.food != s.food
                                        || p.weight != s.weight
                                        || p.map_id != s.map_id
                                }
                            };
                            if changed {
                                log_line!(
                                    "[state] HP={}/{} MP={}/{} food={}% weight={}% map={}",
                                    s.hp,
                                    s.max_hp,
                                    s.mp,
                                    s.max_mp,
                                    s.food,
                                    s.weight,
                                    s.map_id
                                );
                                last = Some(s);
                            }
                            // 第一次成功讀到狀態 → dump 物品欄一次
                            if !inv_dumped.swap(true, Ordering::Relaxed) {
                                dump_inventory(h);
                            }
                        }
                        Ok(_) => {} // 角色未進場(gameState != 3 或 max_hp=0),靜默
                        Err(e) => log_line!("[state] 讀取失敗: {e}"),
                    }
                });
            }));
        }

        // timer_drink (~500ms) — 補水 / 補魔 tick(獨立 thread)
        {
            let cancel = self.control.cancel.clone();
            let settings = self.control.settings.clone();
            handles.push(std::thread::spawn(move || {
                let h = HANDLE(h_raw as *mut _);
                let mut last_want: Option<bool> = None;
                let mut last_error: Option<String> = None;
                tick_loop(
                    "all_day_sync",
                    Duration::from_millis(500),
                    cancel,
                    move |_| {
                        all_day_sync_tick(h, &settings, &mut last_want, &mut last_error);
                    },
                );
            }));
        }

        {
            let cancel = self.control.cancel.clone();
            let settings = self.control.settings.clone();
            handles.push(std::thread::spawn(move || {
                let h = HANDLE(h_raw as *mut _);
                let mut last_want: Option<bool> = None;
                let mut last_error: Option<String> = None;
                tick_loop(
                    "underwater_pump_sync",
                    Duration::from_millis(500),
                    cancel,
                    move |_| {
                        underwater_pump_sync_tick(h, &settings, &mut last_want, &mut last_error);
                    },
                );
            }));
        }

        {
            let cancel = self.control.cancel.clone();
            let settings = self.control.settings.clone();
            let drink = self.control.drink.clone();
            let spell_book = self.control.spell_book.clone();
            handles.push(std::thread::spawn(move || {
                let h = HANDLE(h_raw as *mut _);
                tick_loop(
                    "timer_drink",
                    Duration::from_millis(500),
                    cancel,
                    move |_| {
                        drink::drink_tick(h, &settings, &drink, &spell_book);
                    },
                );
            }));
        }

        // timer_buff (~500ms) — buff 自動補 tick(獨立 thread,自帶 cooldown HashMap)
        {
            let cancel = self.control.cancel.clone();
            let settings = self.control.settings.clone();
            let drink = self.control.drink.clone();
            let spell_db = self.control.spell_db.clone();
            let spell_book = self.control.spell_book.clone();
            handles.push(std::thread::spawn(move || {
                let h = HANDLE(h_raw as *mut _);
                // buff 觸發 cooldown(state_id → 上次觸發時間),per-thread 持有
                // 2 秒 cooldown:遊戲端 RTT 通常 < 1 秒,給 buggy 網路一點 buffer
                // key = (item_type, state_id) — item 跟 skill 共用同一 state_id 時必須分開
                // 計 cooldown(否則 item 先 fire 會以 ITEM cooldown 寫入,skill 緊接著看到
                // 還沒過 SKILL cooldown 就被擋下 180s)。
                let mut buff_cooldowns: std::collections::HashMap<(char, i32), std::time::Instant> =
                    std::collections::HashMap::new();
                tick_loop(
                    "timer_buff",
                    Duration::from_millis(500),
                    cancel,
                    move |_| {
                        buff::buff_tick(
                            h,
                            &settings,
                            &drink,
                            &spell_db,
                            &spell_book,
                            &mut buff_cooldowns,
                        );
                    },
                );
            }));
        }

        // timer_status (~500ms) — 自動吃肉 / 解毒 tick(獨立 thread,自帶 cooldown HashMap)
        {
            let cancel = self.control.cancel.clone();
            let settings = self.control.settings.clone();
            let drink = self.control.drink.clone();
            let spell_book = self.control.spell_book.clone();
            handles.push(std::thread::spawn(move || {
                let h = HANDLE(h_raw as *mut _);
                let mut status_cooldowns: std::collections::HashMap<
                    &'static str,
                    std::time::Instant,
                > = std::collections::HashMap::new();
                tick_loop(
                    "timer_status",
                    Duration::from_millis(500),
                    cancel,
                    move |_| {
                        status::status_tick(
                            h,
                            &settings,
                            &drink,
                            &spell_book,
                            &mut status_cooldowns,
                        );
                    },
                );
            }));
        }

        // timer_delete (~500ms) — 刪物 / 溶解 tick(獨立 thread)
        {
            let cancel = self.control.cancel.clone();
            let settings = self.control.settings.clone();
            let drink = self.control.drink.clone();
            handles.push(std::thread::spawn(move || {
                let h = HANDLE(h_raw as *mut _);
                tick_loop(
                    "timer_delete",
                    Duration::from_millis(500),
                    cancel,
                    move |_| {
                        delete::delete_tick(h, &settings, &drink);
                    },
                );
            }));
        }

        // timer_timer (~500ms) — 定時分頁 tick(獨立 thread,跨 row 共享 last_fire / last_seen)
        {
            let cancel = self.control.cancel.clone();
            let settings = self.control.settings.clone();
            let drink = self.control.drink.clone();
            let spell_book = self.control.spell_book.clone();
            let spell_db = self.control.spell_db.clone();
            let resets = self.control.timer_resets.clone();
            handles.push(std::thread::spawn(move || {
                let h = HANDLE(h_raw as *mut _);
                let mut last_fire: [Option<std::time::Instant>; 6] = [None; 6];
                let mut last_seen: [u64; 6] = [0; 6];
                tick_loop(
                    "timer_timer",
                    Duration::from_millis(500),
                    cancel,
                    move |_| {
                        timer::timer_tick(timer::TimerTickCtx {
                            h,
                            settings: &settings,
                            drink: &drink,
                            spell_book: &spell_book,
                            spell_db: &spell_db,
                            resets: &resets,
                            last_fire: &mut last_fire,
                            last_seen: &mut last_seen,
                        });
                    },
                );
            }));
        }

        // timer_shout (~500ms) — 喊話分頁一般對話 tick(獨立 thread,輪播訊息)
        // 真正觸發節奏由使用者設定的 shout_interval_sec 控制(last_fire / next_idx 跨 tick 持有)
        {
            let cancel = self.control.cancel.clone();
            let settings = self.control.settings.clone();
            let drink = self.control.drink.clone();
            handles.push(std::thread::spawn(move || {
                let h = HANDLE(h_raw as *mut _);
                let mut last_fire: Option<std::time::Instant> = None;
                let mut next_idx: usize = 0;
                tick_loop(
                    "timer_shout",
                    Duration::from_millis(500),
                    cancel,
                    move |_| {
                        shout::shout_tick(h, &settings, &drink, &mut last_fire, &mut next_idx);
                    },
                );
            }));
        }

        // timer_8 (100ms) — EXP 追蹤(LinHelperZ 顯示經驗值)
        {
            let cancel = self.control.cancel.clone();
            let exp_tracker = self.control.exp_tracker.clone();
            let settings = self.control.settings.clone();
            handles.push(std::thread::spawn(move || {
                let h = HANDLE(h_raw as *mut _);
                tick_loop("timer_8", Duration::from_millis(100), cancel, move |_| {
                    exp_tick(h, &settings, &exp_tracker);
                });
            }));
        }

        // F1-F4 全域 hotkey — 獨立模組,不參與 timer 系統
        {
            let h = HANDLE(h_raw as *mut _);
            let pid = self.pid;
            let settings = self.control.settings.clone();
            let drink = self.control.drink.clone();
            let spell_book = self.control.spell_book.clone();
            let spell_db = self.control.spell_db.clone();
            let cancel = self.control.cancel.clone();
            handles.extend(crate::aux::hotkey::install(
                h, pid, settings, drink, spell_book, spell_db, cancel,
            ));
        }

        log_line!(
            "[aux] AuxScheduler 啟動 {} 個 polling thread",
            handles.len()
        );
        handles
    }
}

/// 從清單跟 inventory name 列表挑出第一個要動作的(mode, name)。
///
/// **可單元測試的純函式** — `delete_tick` 走遊戲記憶體取資料,這個只負責 match logic。
///
/// 規則:
/// 1. 先掃 `delete_list`(直接刪)— 第一個在 inventory 找到的就回傳 ("delete", name)
/// 2. 都沒 match 才掃 `dissolve_list`(用溶解劑)— 同樣回第一個 match
/// 3. 名稱含 `(使用中)` / `(揮舞)` 一律忽略(雙保險:UI 加入時已擋,這裡再擋一次)
fn all_day_sync_tick(
    h: HANDLE,
    settings: &Arc<RwLock<AuxSettings>>,
    last_want: &mut Option<bool>,
    last_error: &mut Option<String>,
) {
    let want = settings.read().misc.all_day;
    let patch = crate::aux::toggle::all_day::AllDay;
    let result = if want {
        crate::aux::toggle::Toggle::enable(&patch, h)
    } else {
        crate::aux::toggle::Toggle::disable(&patch, h)
    };

    match result {
        Ok(()) => {
            if *last_want != Some(want) {
                log_line!("[all_day] sync enabled={want}");
            }
            *last_want = Some(want);
            *last_error = None;
        }
        Err(err) => {
            let msg = err.to_string();
            if last_error.as_deref() != Some(msg.as_str()) {
                log_line!("[all_day] sync failed: {msg}");
            }
            *last_error = Some(msg);
        }
    }
}

fn underwater_pump_sync_tick(
    h: HANDLE,
    settings: &Arc<RwLock<AuxSettings>>,
    last_want: &mut Option<bool>,
    last_error: &mut Option<String>,
) {
    let want = settings.read().misc.underwater_pump;
    let patch = crate::aux::toggle::underwater_pump::UnderwaterPump;
    let result = if want {
        crate::aux::toggle::Toggle::enable(&patch, h)
    } else {
        crate::aux::toggle::Toggle::disable(&patch, h)
    };

    match result {
        Ok(()) => {
            if *last_want != Some(want) {
                log_line!("[underwater_pump] sync enabled={want}");
            }
            *last_want = Some(want);
            *last_error = None;
        }
        Err(err) => {
            let msg = err.to_string();
            if last_error.as_deref() != Some(msg.as_str()) {
                log_line!("[underwater_pump] sync failed: {msg}");
            }
            *last_error = Some(msg);
        }
    }
}

/// 物品欄一次性 dump(供 state_poll 第一輪呼叫)
/// 同時寫一份到 launcher.exe 旁的 `inventory_dump.txt`,讓使用者直接記事本打開看
fn dump_inventory(h: HANDLE) {
    use std::fmt::Write as _;
    let mut report = String::new();

    let _ = writeln!(
        &mut report,
        "=== Lineage 3.8 物品欄 dump @ {:?} ===",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );

    match crate::aux::inventory::list_items(h) {
        Ok(items) => {
            log_line!("[inv] 物品欄共 {} 件", items.len());
            let _ = writeln!(&mut report, "物品欄共 {} 件", items.len());
            for (i, it) in items.iter().enumerate() {
                let line = format!(
                    "#{:02}  entry=0x{:08X}  param=0x{:08X}  type=0x{:02X}  icon={}  eq={}  name={:?}",
                    i, it.entry_addr, it.item_param, it.item_type, it.icon, it.equipped, it.name_lossy()
                );
                log_line!("[inv] {line}");
                let _ = writeln!(&mut report, "{line}");
            }
        }
        Err(e) => {
            log_line!("[inv] 列舉失敗: {e:#}");
            let _ = writeln!(&mut report, "列舉失敗: {e:#}");
        }
    }

    // 寫一份 snapshot 到 launcher.exe 旁
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        let path = dir.join("inventory_dump.txt");
        if let Err(e) = std::fs::write(&path, &report) {
            log_line!("[inv] 寫 {path:?} 失敗: {e}");
        } else {
            log_line!("[inv] 完整列表已存到 {path:?}");
        }
    }
}

/// timer_8 EXP tick — LinHelperZ 顯示經驗值
///
/// 由 `AuxSettings::status_show_exp` 驅動 enable/disable;
/// settings off→on 時抓 baseline、推「啟動」綠字提示;on→off 時推「停止」白字提示。
///
/// 早期 return:settings 是 off 且 tracker 已 disable;或 game_state != 3。
fn exp_tick(
    h: HANDLE,
    settings: &Arc<RwLock<AuxSettings>>,
    tracker: &Arc<RwLock<crate::aux::exp_tracker::ExpTracker>>,
) {
    let want = settings.read().status_show_exp;
    let is_enabled = tracker.read().enabled;

    // settings off→on 與 on→off 的轉換都必須先驗證 game_state == 3,
    // 否則進場前 settings 變動會讀到 0 當 baseline、第一隻怪 delta 會大爆炸。
    if !guards::process_in_game_world(h) {
        // 在主畫面 / 選角時:若 settings 改 off,允許關掉(避免下次進場誤推延遲訊息);
        // 但 settings 改 on 必須等進場後才生效。
        if !want && is_enabled {
            tracker.write().disable();
        }
        return;
    }

    // 同步狀態 — 不推任何提示訊息,只內部抓 baseline / 清狀態
    if want && !is_enabled {
        if let Ok(total) = crate::aux::exp_tracker::read_total_exp(h) {
            tracker.write().enable(total);
        }
        return;
    }
    if !want && is_enabled {
        tracker.write().disable();
        return;
    }
    if !want {
        return;
    }

    let total = match crate::aux::exp_tracker::read_total_exp(h) {
        Ok(v) => v,
        Err(_) => return,
    };
    let report = {
        let mut t = tracker.write();
        t.tick(total)
    };
    if let Some(r) = report {
        // 推 in-game chat 走 path B(ChatDispatch + channel=-1),保留 auto-tail。
        // path A 直接寫 buffer 會破壞自動捲動到底,故不採用。\F2 = palette 綠。
        let mut line_bytes = b"\\F2".to_vec();
        line_bytes.extend_from_slice(&crate::aux::exp_tracker::format_chat_line(&r));
        if let Err(e) = crate::aux::chat::push_chat_via_dispatch(
            h,
            &line_bytes,
            0xFFFF,
            crate::aux::chat::color::GREEN,
        ) {
            log_line!("[exp_tracker] push chat 失敗: {e}");
        }
    }
}

/// 通用 tick 迴圈 — 直到 cancel 為止
fn tick_loop<F: FnMut(&str)>(name: &str, interval: Duration, cancel: Arc<AtomicBool>, mut work: F) {
    log_line!("[aux/{}] thread 啟動", name);
    while !cancel.load(Ordering::Relaxed) {
        work(name);
        std::thread::sleep(interval);
    }
    log_line!("[aux/{}] thread 結束", name);
}

#[cfg(test)]
mod tests;
