//! 內掛系統(internal bot)— 自動打怪 / 自動回城 / 自動補物資 / 自動傳送回戰場
//!
//! ## 設計目標
//!
//! 對標 操8.8 內掛 UI(使用者提供 4 張截圖),最終 ship 完整 6 分頁(移動 / 狩獵 /
//! 攻擊 / 消耗品 / 買賣 / 傳送回家)。 Phase 1 只開「狩獵 + 攻擊 + 安全」3 分頁,
//! 配技能攻擊跑通核心循環。 視覺仿真留到 Phase 6 等使用者提供 操8.8 資源檔再做。
//!
//! ## 跟既有 aux/ 的隔離
//!
//! 本模組**獨立於 `aux/drink_hook`、`aux/buff_dispatch`、`aux/hotkey` 等既有 helper**。
//! 兩邊可同時啟用,各自管理自己的 tick / cooldown / 設定。
//!
//! - **資料層**:read-only 引用 `aux::player_state` / `aux::entity_scan` /
//!   `aux::inventory` 的查詢函數(它們是 pure read,並發安全)
//! - **動作層**:呼叫 `aux::drink_hook::DrinkHandle::execute_*` 系列送封包
//!   (RemoteThread, thread-safe)
//! - **設定**:bot 有自己的 `BotConfig` JSON,不混入 `AuxSettings`
//! - **GUI**:bot 自己開獨立 NWG top-level 視窗,**不掛 LHX**
//!
//! ## 啟動條件
//!
//! `AuxConfig.internal_bot_enabled == true`(encoder 旗標)時,launcher 啟動鏈呼叫
//! `bot::install`,在遊戲 `G_GAME_STATE == 3`(已進遊戲)後顯示 HUD icon overlay。
//! 點 icon → 開啟 BotWindow 設定視窗。
//!
//! ## Phase 進度
//!
//! Phase 1: 核心 + 狩獵(固定範圍)+ 技能攻擊 ← **目前**
//! Phase 2: 消耗品 + 一般回家卷
//! Phase 3: 移動 + 紀錄點 + 傳送狩獵
//! Phase 4: 買賣 + 存倉
//! Phase 5: C_ATTACK 基本攻擊(平行 RE)
//! Phase 6: UI 1:1 仿 操8.8 像素級自繪

pub mod action;
pub mod config;
pub mod decide;
pub mod engine;
pub mod hotkey;
pub mod hunt4;
pub mod packet_events;
pub mod perception;
pub(crate) mod scroll_match;
pub mod state;
pub mod ui;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use once_cell::sync::Lazy;
use windows::Win32::Foundation::HANDLE;

use crate::aux::profile::read_player_name;
use crate::log_line;

use self::engine::BotEngine;

/// 全域 engine — main.rs `install`/`shutdown` 透過這個操作。 Phase 1 step 6(UI)後
/// 也會被 BotWindow 拿來查狀態 + toggle enable。
pub(crate) static ENGINE: Lazy<Mutex<Option<BotEngine>>> = Lazy::new(|| Mutex::new(None));

/// 暫存:install 時還沒進場(讀不到角色名),延後到背景 thread 等進場後 load。
static PROFILE_LOAD_HANDLE: Lazy<Mutex<Option<thread::JoinHandle<()>>>> =
    Lazy::new(|| Mutex::new(None));

/// BOT hotkey listener thread cancel signal — shutdown 時 set true 結束 thread
static HOTKEY_CANCEL: Lazy<Arc<AtomicBool>> = Lazy::new(|| Arc::new(AtomicBool::new(false)));

/// install — 啟動 BotEngine tick thread + BOT hotkey listener + 背景等進場後載入 BotConfig。
///
/// 後續 phase 會在這裡加:
/// - 註冊 HUD icon overlay(Phase 1 step 6c)
///
/// idempotent:重複呼叫會 log warning 不重啟。
pub fn install(h: HANDLE, pid: u32) -> Result<()> {
    let mut slot = ENGINE.lock().expect("ENGINE mutex poisoned");
    if slot.is_some() {
        log_line!("[bot] install 重複呼叫,跳過");
        return Ok(());
    }
    let engine = BotEngine::start(h);

    // BOT hotkey listener — 100ms polling,focus 限定遊戲視窗
    HOTKEY_CANCEL.store(false, std::sync::atomic::Ordering::Relaxed);
    let _ = hotkey::spawn_toggle_listener(pid, Arc::clone(&HOTKEY_CANCEL));

    // 背景等進場後讀角色名 + 載 BotConfig(read_player_name 在 G_PLAYER_PTR 還沒
    // 初始化前會回 None,要 poll;進場後就抓得到)。
    // 載完後再 spawn 一條 file watcher,監聽該角色 .bot.json — 玩家在 launcher 跑時
    // 直接 notepad 改檔案 / BotWindow 按儲存 → 下一 tick 自動套新值,免重啟。
    let h_raw = h.0 as usize;
    let load_thread = thread::spawn(move || {
        let h = HANDLE(h_raw as *mut _);
        // 最多等 5 分鐘(packer 解密 + 登入時間,寬鬆 budget)
        for _ in 0..600 {
            if let Some(name) = read_player_name(h) {
                let cfg = config::load(&name);
                if let Some(engine) = ENGINE.lock().expect("ENGINE mutex poisoned").as_ref() {
                    engine.set_hunt_config(cfg.hunt.clone());
                    if cfg.master_enabled {
                        log_line!("[bot] 角色 {name} 載入: 設定檔 master=ON,本版先不自動 hunt;確認不閃退後再按 F8");
                    } else {
                        log_line!("[bot] 角色 {name} 載入(master OFF,F8 啟用)");
                    }
                }
                spawn_config_file_watcher(name);
                return;
            }
            thread::sleep(Duration::from_millis(500));
        }
        log_line!("[bot] 等待進場逾時(5 分鐘),用 default config");
    });
    *PROFILE_LOAD_HANDLE
        .lock()
        .expect("PROFILE_LOAD_HANDLE poisoned") = Some(load_thread);

    *slot = Some(engine);
    log_line!("[bot] Phase 1 scaffold 已載入(F8 master toggle 已啟用,INS 開設定視窗)");
    Ok(())
}

/// shutdown — 停止 tick thread + BOT hotkey listener,釋放資源。
pub fn shutdown() {
    // BOT hotkey listener cancel(它自己 100ms 內會檢查 → 結束)
    HOTKEY_CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);

    // detach profile load thread(它看到 ENGINE 為 None 會 return)
    if let Some(handle) = PROFILE_LOAD_HANDLE
        .lock()
        .expect("PROFILE_LOAD_HANDLE poisoned")
        .take()
    {
        std::mem::drop(handle);
    }

    let mut slot = ENGINE.lock().expect("ENGINE mutex poisoned");
    if let Some(mut engine) = slot.take() {
        engine.shutdown();
        log_line!("[bot] shutdown 完成");
    } else {
        log_line!("[bot] shutdown 呼叫但 engine 未啟動");
    }
}

/// 監聽指定角色的 `.bot.json` mtime 變化,變了就重 load + 套用到 engine。
///
/// 設計重點:
/// - 用 `notify` crate 在背景 thread 跑;launcher 結束時 thread leak 不要緊
///   (process 結束自然回收)
/// - 監聽**目錄**而非單檔 — Windows 的 notepad 改檔習慣是「先寫 .tmp 再 rename」,
///   會把直接綁定的 inode/handle 失效。 watch parent + filter filename 比較穩。
/// - debounce:200ms 內多次 modify 只觸發一次 reload(notepad save 會連發多個 event)
/// - 多角色:目前實作只認**第一個進場的角色**;換角色不會切換 watcher(rare case,
///   accept restart cost)。
fn spawn_config_file_watcher(name: String) {
    use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;

    let path = match config::profile_path_for(&name) {
        Ok(p) => p,
        Err(e) => {
            log_line!("[bot/config] watcher 取路徑失敗 {name}: {e:#} — 熱重載停用");
            return;
        }
    };
    let Some(watch_dir) = path.parent().map(|p| p.to_path_buf()) else {
        log_line!(
            "[bot/config] watcher 取 parent dir 失敗 {} — 熱重載停用",
            path.display()
        );
        return;
    };
    let target_filename = path.file_name().map(|s| s.to_os_string());

    thread::spawn(move || {
        let (tx, rx) = channel::<notify::Result<Event>>();
        let mut watcher: RecommendedWatcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => {
                log_line!("[bot/config] notify watcher 建立失敗: {e:#} — 熱重載停用");
                return;
            }
        };
        if let Err(e) = watcher.watch(&watch_dir, RecursiveMode::NonRecursive) {
            log_line!(
                "[bot/config] notify watch({}) 失敗: {e:#}",
                watch_dir.display()
            );
            return;
        }
        log_line!("[bot/config] 熱重載已啟動 — 監聽 {}", path.display());

        let mut last_reload = std::time::Instant::now();
        for evt in rx {
            let Ok(event) = evt else { continue };
            // 只看 modify/create/remove 三類
            if !matches!(
                event.kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
            ) {
                continue;
            }
            // filter 目標檔名(其他 .bot.json 修改不觸發)
            let touched_target = match &target_filename {
                Some(t) => event.paths.iter().any(|p| p.file_name() == Some(t)),
                None => true,
            };
            if !touched_target {
                continue;
            }
            // 200ms debounce
            if last_reload.elapsed() < Duration::from_millis(200) {
                continue;
            }
            last_reload = std::time::Instant::now();

            let cfg = config::load(&name);
            if let Some(engine) = ENGINE.lock().expect("ENGINE mutex poisoned").as_ref() {
                engine.set_hunt_config(cfg.hunt.clone());
                log_line!("[bot/config] {} 變動,已重新套用", path.display());
            }
        }
    });
}

/// 暴露給 UI / hotkey / 外部 caller 控制 master toggle 的便利函數。
pub fn set_master_enabled(on: bool) {
    if let Some(engine) = ENGINE.lock().expect("ENGINE mutex poisoned").as_ref() {
        engine.set_enabled(on);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bot_module_exposes_only_hunt4_runtime_surface() {
        let source = include_str!("mod.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();

        assert!(production.contains("pub mod hunt4;"));
        assert!(!production.contains("pub mod hunt;"));
        assert!(!production.contains("pub mod hunt3;"));
        assert!(!production.contains("pub mod hunt_v2;"));
    }
}
