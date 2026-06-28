//! BotWindow - 內掛設定獨立視窗。
//!
//! ## 視窗 lifecycle
//!
//! - 玩家按 INS 會呼叫 `show_bot_window()` 啟動 UI thread 並顯示視窗。
//! - 視窗關閉後會清掉 IS_OPEN，之後可再用 INS 重開。
//! - launcher shutdown 時視窗會隨 UI thread 結束。
//!
//! ## 跟 engine 的資料來往
//!
//! - 開窗時讀 `BotEngine::hunt_config_snapshot()` 填初始值。
//! - 按「儲存」會 parse 欄位、寫回 `BotEngine::set_hunt_config(new)`，再依角色名寫入 JSON。
//! - master enable checkbox 會即時呼叫 `BotEngine::set_enabled`。

extern crate native_windows_derive as nwd;
extern crate native_windows_gui as nwg;

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use nwd::NwgUi;
use nwg::NativeUi;
use windows::Win32::Foundation::HANDLE;

use crate::aux::profile::read_player_name;
use crate::aux::spell_book::SpellBook;
use crate::aux::weapon::is_ranged_weapon_equipped;
use crate::bot::config::{self, BotConfig};
use crate::bot::decide::hunt::{
    AttackSequenceStep, AttackStepKind, HuntConfig, MAX_ATTACK_SEQUENCE_STEPS,
};
use crate::bot::engine::BotEngine;
use crate::bot::hunt4::observe::ActionLabel;
use crate::bot::hunt4::step::{LastOutcome, StateLabel, TransitionCause};
use crate::log_line;

/// 視窗目前是否開著，避免 INS 連按多開。
static IS_OPEN: AtomicBool = AtomicBool::new(false);

pub fn is_bot_window_open() -> bool {
    IS_OPEN.load(Ordering::Acquire)
}

/// 從 hotkey listener 呼叫；若視窗未開就開新視窗，若已開則 no-op。
/// 不主動 bring-to-front，避免跨 thread 取 HWND 帶來額外 unsafe FFI。
pub fn show_bot_window() {
    if IS_OPEN.swap(true, Ordering::AcqRel) {
        log_line!("[bot/ui] 視窗已開，INS 無效，請從工作列切換");
        return;
    }

    std::thread::spawn(|| {
        run_window();
        IS_OPEN.store(false, Ordering::Release);
    });
}

fn run_window() {
    if let Err(e) = nwg::init() {
        log_line!("[bot/ui] nwg init 失敗: {e:?}");
        return;
    }
    let mut font = nwg::Font::default();
    let _ = nwg::Font::builder()
        .family("Microsoft JhengHei UI")
        .size(16)
        .build(&mut font);
    nwg::Font::set_global_default(Some(font));

    // Snapshot hunt config before building UI controls.
    let initial_hunt = with_engine(|e| e.hunt_config_snapshot()).unwrap_or_default();

    // SpellBook 讀取失敗時只讓技能下拉空白，避免 UI 因技能表不可讀而打不開。
    // attack_names_iter 已過濾成可攻擊技能，buff/heal/passive 不顯示在攻擊序列。
    let (skill_options, weapon_label) = match with_engine(|e| e.h_raw()) {
        Some(h_raw) => {
            let h = HANDLE(h_raw as *mut _);
            let opts = SpellBook::build(h)
                .map(|book| {
                    let mut names: Vec<String> =
                        book.attack_names_iter().map(|s| s.to_string()).collect();
                    names.sort();
                    names
                })
                .unwrap_or_default();
            let label = if is_ranged_weapon_equipped(h) {
                ui_text("遠程")
            } else {
                ui_text("近戰")
            };
            (opts, label)
        }
        None => (Vec::new(), "尚未連到 engine".to_string()),
    };

    let initial = BotWindow {
        initial_hunt_blacklist: initial_hunt.monster_blacklist.join("\r\n"),
        initial_hunt_skill: initial_hunt.skill_name.clone(),
        initial_attack_sequence: initial_hunt.effective_attack_sequence(),
        initial_attack_cycle_secs: ms_to_secs_text(initial_hunt.attack_sequence_cycle_ms),
        initial_hunt_range_tiles: initial_hunt.hunt_range_tiles.to_string(),
        initial_skill_options: skill_options,
        initial_weapon_label: weapon_label,
        initial_teleport_index: name_to_teleport_index(&initial_hunt.teleport_scroll_name),
        initial_teleport_secs: initial_hunt.idle_teleport_secs.to_string(),
        initial_v4_dispatch_takeover: initial_hunt.v4_dispatch_takeover,
        ..Default::default()
    };

    let app = match BotWindow::build_ui(initial) {
        Ok(a) => a,
        Err(e) => {
            log_line!("[bot/ui] build_ui 失敗: {e:?}");
            return;
        }
    };

    app.apply_initial_values();
    if let Some(raw) = app.window.handle.hwnd() {
        crate::i18n::retranslate_lhx(windows::Win32::Foundation::HWND(raw as *mut _));
    }
    nwg::dispatch_thread_events();
    log_line!("[bot/ui] BotWindow thread 結束");
}

fn with_engine<R>(f: impl FnOnce(&BotEngine) -> R) -> Option<R> {
    let slot = crate::bot::ENGINE.lock().expect("ENGINE mutex poisoned");
    slot.as_ref().map(f)
}

fn ui_text(s: &str) -> String {
    crate::i18n::tr(s).into_owned()
}

fn ui_dynamic(s: String) -> String {
    crate::i18n::tr(&s).into_owned()
}

fn hunt4_report_lines(report: &crate::bot::hunt4::observe::StateReport) -> [String; 5] {
    let state = ui_dynamic(format!("狀態 {}", state_label_text(report.current_label)));
    let since = ui_dynamic(format!(
        "持續時間 {:.1} 秒",
        report.since.elapsed().as_secs_f32()
    ));
    let transition = report
        .last_transition
        .as_ref()
        .map(|t| {
            ui_dynamic(format!(
                "狀態轉換 {} -> {} ({})",
                state_label_text(t.from),
                state_label_text(t.to),
                transition_cause_text(t.cause)
            ))
        })
        .unwrap_or_else(|| ui_text("狀態轉換 -"));
    let observed = observed_progress_text(report);
    let action = match &report.last_action {
        Some(action) => ui_dynamic(format!(
            "本次指令: {} | 回傳={} | {}",
            action_label_text(action),
            outcome_text(report.last_outcome.as_ref()),
            observed
        )),
        None => ui_dynamic(format!(
            "本次指令: - | 回傳={} | {}",
            outcome_text(report.last_outcome.as_ref()),
            observed
        )),
    };
    let target = report
        .target_summary
        .as_ref()
        .map(|summary| {
            ui_dynamic(format!(
                " | 目標 #{} 0x{:X} {}",
                summary.rank, summary.target_id, summary.name
            ))
        })
        .unwrap_or_else(|| ui_text(" | 目標: -"));
    let route = report
        .route_reason
        .as_ref()
        .map(|_| match report.route_next_tile {
            Some(tile) => ui_dynamic(format!(" | 路徑 {:?}", tile)),
            None => ui_text(" | 路徑 -"),
        })
        .unwrap_or_default();
    let teleport = report
        .teleport_reason
        .as_ref()
        .map(|_| ui_text(" | 順移: 已判斷"))
        .unwrap_or_default();
    let policy = report
        .policy_comparison
        .as_ref()
        .map(|comparison| {
            ui_dynamic(format!(
                " | 策略 對齊={} | 統計 {}/{} 不符={} 連續={}",
                comparison.aligned,
                report.policy_telemetry.aligned,
                report.policy_telemetry.total,
                report.policy_telemetry.mismatched,
                report.policy_telemetry.aligned_streak
            ))
        })
        .unwrap_or_else(|| {
            ui_dynamic(format!(
                " | 策略 - | 統計 {}/{} 不符={} 連續={}",
                report.policy_telemetry.aligned,
                report.policy_telemetry.total,
                report.policy_telemetry.mismatched,
                report.policy_telemetry.aligned_streak
            ))
        });
    let dispatch_choice = report
        .dispatch_choice
        .as_ref()
        .map(|_| ui_text(" | 派發: 已選擇"))
        .unwrap_or_default();
    let mut lock = match &report.lock_summary {
        Some(lock) => ui_dynamic(format!(
            "鎖定: 0x{:X} {} [{}] | 障礙記憶 {} / 失敗目標 {}",
            lock.target_id,
            lock.name,
            lock_intent_text(lock.intent),
            report.memory_summary.obstacle_count,
            report.memory_summary.failed_target_count
        )),
        None => ui_dynamic(format!(
            "鎖定: - | 障礙記憶 {} / 失敗目標 {}",
            report.memory_summary.obstacle_count, report.memory_summary.failed_target_count
        )),
    };
    lock.push_str(&target);
    lock.push_str(&route);
    lock.push_str(&teleport);
    lock.push_str(&policy);
    lock.push_str(&dispatch_choice);
    [state, since, transition, action, lock]
}

fn state_label_text(label: StateLabel) -> &'static str {
    match label {
        StateLabel::DisabledMasterOff => "停用(主開關關閉)",
        StateLabel::DisabledNotInGame => "停用(未進入遊戲)",
        StateLabel::Idle => "待機",
        StateLabel::EngagingApproach => "接近目標",
        StateLabel::EngagingAttack => "攻擊中",
        StateLabel::EngagingKillConfirm => "確認擊殺",
        StateLabel::Exploring => "探索",
        StateLabel::RecoveringWalkStuck => "恢復:走路卡住",
        StateLabel::RecoveringAttackFailed => "恢復:攻擊失敗",
        StateLabel::RecoveringDamageSpike => "恢復:血量突降",
        StateLabel::RecoveringNoReachableTarget => "恢復:沒有可到達目標",
        StateLabel::RecoveringCriticalHp => "恢復:低血量",
        StateLabel::Escaping => "脫離",
        StateLabel::StoppedDied => "停止:死亡",
        #[cfg(test)]
        StateLabel::StoppedDisconnected => "停止:斷線",
        #[cfg(test)]
        StateLabel::StoppedManual => "停止:手動",
    }
}

fn transition_cause_text(cause: TransitionCause) -> &'static str {
    match cause {
        TransitionCause::MasterOff => "主開關關閉",
        TransitionCause::NotInGame => "未進入遊戲",
        TransitionCause::PlayerDied => "角色死亡",
        TransitionCause::Idle => "待機",
        TransitionCause::TargetAcquired => "取得目標",
        TransitionCause::StartApproach => "開始接近",
        TransitionCause::StartAttack => "開始攻擊",
        TransitionCause::TargetDead => "目標死亡",
        TransitionCause::StartExploration => "開始探索",
        TransitionCause::AllTargetsUnreachable => "所有目標不可到達",
        TransitionCause::WalkStuck => "走路卡住",
        TransitionCause::AttackFailed => "攻擊失敗",
        TransitionCause::DamageSpike => "血量突降",
        TransitionCause::CriticalHp => "低血量",
        TransitionCause::UseTeleportScroll => "使用順移",
        TransitionCause::RecoveryElapsed => "恢復時間結束",
    }
}

fn action_label_text(action: &ActionLabel) -> String {
    match action {
        ActionLabel::Wait => ui_text("等待"),
        ActionLabel::WalkTo { tile } => ui_dynamic(format!("走路到 {:?}", tile)),
        ActionLabel::Attack {
            target_id,
            with_skill,
        } => {
            if *with_skill {
                ui_dynamic(format!("技能攻擊 0x{target_id:X}"))
            } else {
                ui_dynamic(format!("普攻 0x{target_id:X}"))
            }
        }
        ActionLabel::UseScroll => ui_text("使用順移"),
    }
}

fn outcome_text(outcome: Option<&LastOutcome>) -> String {
    match outcome {
        None | Some(LastOutcome::None) => ui_text("無"),
        Some(LastOutcome::WalkOk) => ui_text("走路指令已送出"),
        Some(LastOutcome::WalkFailed { attempted_tile }) => {
            ui_dynamic(format!("走路失敗 {:?}", attempted_tile))
        }
        Some(LastOutcome::AttackOk { target_id }) => {
            ui_dynamic(format!("攻擊指令已送出 0x{target_id:X}"))
        }
        Some(LastOutcome::AttackFailed { target_id }) => {
            ui_dynamic(format!("攻擊失敗 0x{target_id:X}"))
        }
        Some(LastOutcome::AttackNoProgress { target_id }) => {
            ui_dynamic(format!("攻擊無進展 0x{target_id:X}"))
        }
        Some(LastOutcome::ScrollOk) => ui_text("順移指令已送出"),
        Some(LastOutcome::ScrollFailed) => ui_text("順移失敗"),
    }
}

fn observed_progress_text(report: &crate::bot::hunt4::observe::StateReport) -> String {
    observed_progress_text_at(
        report.last_position_change,
        report.last_target_progress,
        Instant::now(),
    )
}

fn observed_progress_text_at(
    last_position_change: Option<Instant>,
    last_target_progress: Option<Instant>,
    now: Instant,
) -> String {
    ui_dynamic(format!(
        "觀測=座標{} / 傷害{}",
        observation_age_text(last_position_change, now),
        observation_age_text(last_target_progress, now)
    ))
}

fn observation_age_text(event_at: Option<Instant>, now: Instant) -> String {
    event_at
        .map(|at| format!("{:.1}秒前", now.saturating_duration_since(at).as_secs_f32()))
        .unwrap_or_else(|| "-".to_string())
}

fn lock_intent_text(intent: &str) -> String {
    match intent {
        "Approach" => ui_text("接近"),
        "Attack" => ui_text("攻擊"),
        "KillConfirm" => ui_text("確認擊殺"),
        _ => ui_dynamic(intent.to_string()),
    }
}

const TELEPORT_HOME_TRAD: &str = "\u{50b3}\u{9001}\u{56de}\u{5bb6}\u{5377}\u{8ef8}";
const TELEPORT_HOME_SIMP: &str = "\u{4f20}\u{9001}\u{56de}\u{5bb6}\u{5377}\u{8f74}";
const TELEPORT_RANDOM_TRAD: &str = "\u{77ac}\u{9593}\u{79fb}\u{52d5}\u{5377}\u{8ef8}";
const TELEPORT_RANDOM_SIMP: &str = "\u{77ac}\u{95f4}\u{79fb}\u{52a8}\u{5377}\u{8f74}";

fn bot_ui_simplified() -> bool {
    crate::legacy_text::text_encoding_mode() == crate::legacy_text::TextEncodingMode::Gbk
}

fn teleport_combo_options() -> Vec<String> {
    vec![
        ui_text("(不使用)"),
        teleport_name_for_index(1).to_string(),
        teleport_name_for_index(2).to_string(),
    ]
}

fn teleport_name_for_index(index: usize) -> &'static str {
    match (index, bot_ui_simplified()) {
        (1, true) => TELEPORT_HOME_SIMP,
        (1, false) => TELEPORT_HOME_TRAD,
        (2, true) => TELEPORT_RANDOM_SIMP,
        (2, false) => TELEPORT_RANDOM_TRAD,
        _ => "",
    }
}

fn selected_teleport_scroll_name(idx: Option<usize>) -> String {
    teleport_index_to_name(idx)
}

fn teleport_name_to_index_canonical(name: &str) -> Option<usize> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some(0);
    }
    if trimmed.contains(TELEPORT_HOME_TRAD) || trimmed.contains(TELEPORT_HOME_SIMP) {
        return Some(1);
    }
    if trimmed.contains(TELEPORT_RANDOM_TRAD) || trimmed.contains(TELEPORT_RANDOM_SIMP) {
        return Some(2);
    }
    None
}

/// 將 ComboBox index 轉成 `teleport_scroll_name` 設定值:
/// - 0 / None / 空字串 = 不使用順移
/// - 1 = 傳送回家卷軸
/// - 2 = 瞬間移動卷軸
///
fn teleport_index_to_name(idx: Option<usize>) -> String {
    idx.map(teleport_name_for_index).unwrap_or("").to_string()
}

fn name_to_teleport_index(name: &str) -> usize {
    teleport_name_to_index_canonical(name).unwrap_or(0)
}

#[derive(Default, NwgUi)]
pub struct BotWindow {
    // Values copied from the engine snapshot before build_ui.
    initial_hunt_blacklist: String,
    initial_hunt_skill: String,
    initial_attack_sequence: Vec<AttackSequenceStep>,
    initial_attack_cycle_secs: String,
    initial_hunt_range_tiles: String,
    /// Attack-capable skill names loaded from the spell book.
    initial_skill_options: Vec<String>,
    /// Weapon mode label shown in the status bar.
    initial_weapon_label: String,
    /// ComboBox 初始 index(0=不使用 / 1=回家卷 / 2=順移卷)。
    initial_teleport_index: usize,
    initial_teleport_secs: String,
    initial_v4_dispatch_takeover: bool,
    attack_sequence_len: Cell<usize>,

    #[nwg_control(
        size: (440, 720),
        position: (300, 300),
        title: "內掛BOT - 自動練功設定",
        flags: "WINDOW|VISIBLE|MINIMIZE_BOX"
    )]
    #[nwg_events(OnWindowClose: [BotWindow::on_close])]
    window: nwg::Window,

    // Master toggle.
    #[nwg_control(
        parent: window,
        text: "啟用自動練功 (F8)",
        position: (15, 10),
        size: (195, 22)
    )]
    #[nwg_events(OnButtonClick: [BotWindow::on_master_changed])]
    cb_master: nwg::CheckBox,

    #[nwg_control(parent: window, text: "攻擊序列", position: (15, 42), size: (80, 20))]
    lbl_attack_sequence: nwg::Label,

    #[nwg_control(parent: window, text: "+", position: (100, 39), size: (32, 24))]
    #[nwg_events(OnButtonClick: [BotWindow::on_add_attack_step])]
    btn_add_attack_step: nwg::Button,

    #[nwg_control(parent: window, text: "-", position: (136, 39), size: (32, 24))]
    #[nwg_events(OnButtonClick: [BotWindow::on_remove_attack_step])]
    btn_remove_attack_step: nwg::Button,

    #[nwg_control(parent: window, text: "循環", position: (245, 42), size: (56, 20))]
    lbl_attack_cycle: nwg::Label,

    #[nwg_control(parent: window, position: (305, 39), size: (55, 24))]
    txt_attack_cycle_secs: nwg::TextInput,

    #[nwg_control(parent: window, text: "秒", position: (365, 42), size: (28, 20))]
    lbl_attack_cycle_unit: nwg::Label,

    #[nwg_control(parent: window, position: (40, 69), size: (70, 22))]
    #[nwg_events(OnComboxBoxSelection: [BotWindow::on_attack_mode_changed])]
    combo_attack_mode_1: nwg::ComboBox<String>,

    #[nwg_control(parent: window, position: (315, 69), size: (55, 24))]
    txt_attack_interval_1: nwg::TextInput,

    #[nwg_control(parent: window, text: "秒", position: (375, 72), size: (28, 20))]
    lbl_attack_interval_unit_1: nwg::Label,

    // 第一列保留舊欄位名稱，對應第一個攻擊序列步驟。
    #[nwg_control(
        parent: window,
        text: "1",
        position: (15, 72),
        size: (22, 20)
    )]
    lbl_skill: nwg::Label,

    #[nwg_control(parent: window, position: (115, 69), size: (195, 22))]
    #[nwg_events(OnComboBoxDropdown: [BotWindow::on_attack_skill_dropdown])]
    combo_skill: nwg::ComboBox<String>,

    #[nwg_control(parent: window, text: "2", position: (15, 100), size: (22, 20))]
    lbl_attack_rank_2: nwg::Label,
    #[nwg_control(parent: window, position: (40, 97), size: (70, 22))]
    #[nwg_events(OnComboxBoxSelection: [BotWindow::on_attack_mode_changed])]
    combo_attack_mode_2: nwg::ComboBox<String>,
    #[nwg_control(parent: window, position: (115, 97), size: (195, 22))]
    #[nwg_events(OnComboBoxDropdown: [BotWindow::on_attack_skill_dropdown])]
    combo_attack_skill_2: nwg::ComboBox<String>,
    #[nwg_control(parent: window, position: (315, 97), size: (55, 24))]
    txt_attack_interval_2: nwg::TextInput,
    #[nwg_control(parent: window, text: "秒", position: (375, 100), size: (28, 20))]
    lbl_attack_interval_unit_2: nwg::Label,

    #[nwg_control(parent: window, text: "3", position: (15, 128), size: (22, 20))]
    lbl_attack_rank_3: nwg::Label,
    #[nwg_control(parent: window, position: (40, 125), size: (70, 22))]
    #[nwg_events(OnComboxBoxSelection: [BotWindow::on_attack_mode_changed])]
    combo_attack_mode_3: nwg::ComboBox<String>,
    #[nwg_control(parent: window, position: (115, 125), size: (195, 22))]
    #[nwg_events(OnComboBoxDropdown: [BotWindow::on_attack_skill_dropdown])]
    combo_attack_skill_3: nwg::ComboBox<String>,
    #[nwg_control(parent: window, position: (315, 125), size: (55, 24))]
    txt_attack_interval_3: nwg::TextInput,
    #[nwg_control(parent: window, text: "秒", position: (375, 128), size: (28, 20))]
    lbl_attack_interval_unit_3: nwg::Label,

    #[nwg_control(parent: window, text: "4", position: (15, 156), size: (22, 20))]
    lbl_attack_rank_4: nwg::Label,
    #[nwg_control(parent: window, position: (40, 153), size: (70, 22))]
    #[nwg_events(OnComboxBoxSelection: [BotWindow::on_attack_mode_changed])]
    combo_attack_mode_4: nwg::ComboBox<String>,
    #[nwg_control(parent: window, position: (115, 153), size: (195, 22))]
    #[nwg_events(OnComboBoxDropdown: [BotWindow::on_attack_skill_dropdown])]
    combo_attack_skill_4: nwg::ComboBox<String>,
    #[nwg_control(parent: window, position: (315, 153), size: (55, 24))]
    txt_attack_interval_4: nwg::TextInput,
    #[nwg_control(parent: window, text: "秒", position: (375, 156), size: (28, 20))]
    lbl_attack_interval_unit_4: nwg::Label,

    #[nwg_control(parent: window, text: "5", position: (15, 184), size: (22, 20))]
    lbl_attack_rank_5: nwg::Label,
    #[nwg_control(parent: window, position: (40, 181), size: (70, 22))]
    #[nwg_events(OnComboxBoxSelection: [BotWindow::on_attack_mode_changed])]
    combo_attack_mode_5: nwg::ComboBox<String>,
    #[nwg_control(parent: window, position: (115, 181), size: (195, 22))]
    #[nwg_events(OnComboBoxDropdown: [BotWindow::on_attack_skill_dropdown])]
    combo_attack_skill_5: nwg::ComboBox<String>,
    #[nwg_control(parent: window, position: (315, 181), size: (55, 24))]
    txt_attack_interval_5: nwg::TextInput,
    #[nwg_control(parent: window, text: "秒", position: (375, 184), size: (28, 20))]
    lbl_attack_interval_unit_5: nwg::Label,

    #[nwg_control(parent: window, text: "6", position: (15, 212), size: (22, 20))]
    lbl_attack_rank_6: nwg::Label,
    #[nwg_control(parent: window, position: (40, 209), size: (70, 22))]
    #[nwg_events(OnComboxBoxSelection: [BotWindow::on_attack_mode_changed])]
    combo_attack_mode_6: nwg::ComboBox<String>,
    #[nwg_control(parent: window, position: (115, 209), size: (195, 22))]
    #[nwg_events(OnComboBoxDropdown: [BotWindow::on_attack_skill_dropdown])]
    combo_attack_skill_6: nwg::ComboBox<String>,
    #[nwg_control(parent: window, position: (315, 209), size: (55, 24))]
    txt_attack_interval_6: nwg::TextInput,
    #[nwg_control(parent: window, text: "秒", position: (375, 212), size: (28, 20))]
    lbl_attack_interval_unit_6: nwg::Label,

    // 攻擊間隔仍沿用既有 click_attack / skill dispatch gate。
    #[nwg_control(
        parent: window,
        text: "搜尋範圍 (0=不限):",
        position: (15, 248),
        size: (140, 20)
    )]
    lbl_hunt_range: nwg::Label,

    #[nwg_control(parent: window, position: (160, 245), size: (50, 24))]
    txt_hunt_range: nwg::TextInput,

    #[nwg_control(
        parent: window,
        text: "V4 接管: 等待/走路/普攻/技能",
        position: (15, 273),
        size: (280, 22)
    )]
    cb_v4_dispatch_takeover: nwg::CheckBox,

    // 黑名單用來排除 NPC、不可攻擊或特殊 entity。
    #[nwg_control(
        parent: window,
        text: "怪物黑名單:",
        position: (15, 300),
        size: (370, 20)
    )]
    lbl_blacklist: nwg::Label,

    #[nwg_control(parent: window, position: (15, 325), size: (410, 110))]
    txt_blacklist: nwg::TextBox,

    // 無目標太久時可使用順移卷軸。
    #[nwg_control(
        parent: window,
        text: "無目標幾秒後使用順移:",
        position: (15, 450),
        size: (220, 20)
    )]
    lbl_teleport_name: nwg::Label,

    #[nwg_control(parent: window, position: (15, 468), size: (50, 22))]
    txt_teleport_secs: nwg::TextInput,

    #[nwg_control(
        parent: window,
        position: (70, 468), size: (355, 22),
        collection: vec![
            "(不使用)".to_string(),
            TELEPORT_HOME_TRAD.to_string(),
            TELEPORT_RANDOM_TRAD.to_string(),
        ],
        selected_index: Some(0),
    )]
    combo_teleport: nwg::ComboBox<String>,

    // 操作按鈕。
    #[nwg_control(parent: window, text: "小地圖", position: (15, 498), size: (110, 28))]
    #[nwg_events(OnButtonClick: [BotWindow::on_open_minimap])]
    btn_minimap: nwg::Button,

    #[nwg_control(parent: window, text: "儲存", position: (200, 498), size: (90, 28))]
    #[nwg_events(OnButtonClick: [BotWindow::on_save])]
    btn_save: nwg::Button,

    #[nwg_control(parent: window, text: "關閉", position: (295, 498), size: (90, 28))]
    #[nwg_events(OnButtonClick: [BotWindow::on_close_button])]
    btn_close: nwg::Button,

    // 狀態列。
    #[nwg_control(
        parent: window,
        text: "狀態",
        position: (15, 535),
        size: (410, 20)
    )]
    lbl_status: nwg::Label,

    // Hunt4 狀態，每 200ms refresh，用來看 runtime 是否卡住。
    #[nwg_control(
        parent: window,
        text: "Hunt4 狀態 (每 200 毫秒更新):",
        position: (15, 563),
        size: (410, 18)
    )]
    lbl_state_header: nwg::Label,

    #[nwg_control(parent: window, text: "狀態 -", position: (15, 583), size: (410, 18))]
    lbl_state_label: nwg::Label,

    #[nwg_control(parent: window, text: "持續時間 -", position: (15, 603), size: (410, 18))]
    lbl_state_since: nwg::Label,

    #[nwg_control(parent: window, text: "狀態轉換 -", position: (15, 623), size: (410, 18))]
    lbl_state_transition: nwg::Label,

    #[nwg_control(parent: window, text: "本次指令: - | 回傳=- | 觀測=座標- / 傷害-", position: (15, 643), size: (410, 18))]
    lbl_state_action: nwg::Label,

    #[nwg_control(parent: window, text: "鎖定 / 記憶 -", position: (15, 663), size: (410, 18))]
    lbl_state_lock: nwg::Label,

    #[nwg_control(
        parent: window,
        interval: std::time::Duration::from_millis(200),
        active: true,
    )]
    #[nwg_events(OnTimerTick: [BotWindow::on_state_tick])]
    state_timer: nwg::AnimationTimer,
}

fn attack_mode_options() -> Vec<String> {
    vec![ui_text("普攻"), ui_text("技能")]
}

fn skill_combo_entries(skill_options: &[String]) -> Vec<String> {
    let mut entries = vec![ui_text("(無)")];
    entries.extend(skill_options.iter().cloned());
    entries
}

fn ms_to_secs_text(ms: u64) -> String {
    if ms == 0 {
        return "0".to_string();
    }
    if ms % 1000 == 0 {
        return (ms / 1000).to_string();
    }
    let mut s = format!("{:.2}", ms as f64 / 1000.0);
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

fn parse_secs_to_ms(text: &str, fallback_ms: u64) -> u64 {
    match text.trim().parse::<f64>() {
        Ok(v) if v.is_finite() && v >= 0.0 => (v * 1000.0).round() as u64,
        _ => fallback_ms,
    }
}

impl BotWindow {
    fn apply_initial_values(&self) {
        self.apply_static_texts();
        self.txt_hunt_range.set_text(&self.initial_hunt_range_tiles);
        self.txt_blacklist.set_text(&self.initial_hunt_blacklist);
        self.txt_teleport_secs.set_text(&self.initial_teleport_secs);
        self.combo_teleport.set_collection(teleport_combo_options());
        self.combo_teleport
            .set_selection(Some(self.initial_teleport_index));

        let mut entries: Vec<String> = vec!["(none)".to_string()];
        entries.extend(self.initial_skill_options.iter().cloned());
        let selected_idx = self
            .initial_skill_options
            .iter()
            .position(|n| n == &self.initial_hunt_skill)
            .map(|i| i + 1)
            .unwrap_or(0);
        let entry_count = entries.len();
        self.combo_skill.set_collection(entries);
        self.combo_skill.set_selection(Some(selected_idx));
        // CB_SETMINVISIBLE 讓 dropdown 顯示更多列，避免技能列表難選。
        set_combo_dropdown_visible_rows(&self.combo_skill, entry_count);

        self.apply_attack_sequence_initial_values();

        let master_on = with_engine(|e| e.is_enabled()).unwrap_or(false);
        self.cb_master.set_check_state(if master_on {
            nwg::CheckBoxState::Checked
        } else {
            nwg::CheckBoxState::Unchecked
        });
        self.cb_v4_dispatch_takeover
            .set_check_state(if self.initial_v4_dispatch_takeover {
                nwg::CheckBoxState::Checked
            } else {
                nwg::CheckBoxState::Unchecked
            });

        self.lbl_status
            .set_text(&ui_dynamic(format!("武器 {}", self.initial_weapon_label)));
    }

    fn apply_static_texts(&self) {
        self.window.set_text(&ui_text("內掛BOT - 自動練功設定"));
        self.cb_master.set_text(&ui_text("啟用自動練功 (F8)"));
        self.lbl_attack_sequence.set_text(&ui_text("攻擊序列"));
        self.lbl_attack_cycle.set_text(&ui_text("循環"));
        self.lbl_attack_cycle_unit.set_text(&ui_text("秒"));
        self.lbl_hunt_range.set_text(&ui_text("搜尋範圍 (0=不限):"));
        self.cb_v4_dispatch_takeover
            .set_text(&ui_text("V4 接管: 等待/走路/普攻/技能"));
        self.lbl_blacklist.set_text(&ui_text("怪物黑名單:"));
        self.lbl_teleport_name
            .set_text(&ui_text("無目標幾秒後使用順移:"));
        self.btn_minimap.set_text(&ui_text("小地圖"));
        self.btn_save.set_text(&ui_text("儲存"));
        self.btn_close.set_text(&ui_text("關閉"));
        self.lbl_status.set_text(&ui_text("狀態"));
        self.lbl_state_header
            .set_text(&ui_text("Hunt4 狀態 (每 200 毫秒更新):"));
        self.lbl_state_label.set_text(&ui_text("狀態 -"));
        self.lbl_state_since.set_text(&ui_text("持續時間 -"));
        self.lbl_state_transition.set_text(&ui_text("狀態轉換 -"));
        self.lbl_state_action
            .set_text(&ui_text("本次指令: - | 回傳=- | 觀測=座標- / 傷害-"));
        self.lbl_state_lock.set_text(&ui_text("鎖定 / 記憶 -"));
        for label in self.attack_interval_unit_labels() {
            label.set_text(&ui_text("秒"));
        }
    }

    fn attack_mode_combos(&self) -> [&nwg::ComboBox<String>; MAX_ATTACK_SEQUENCE_STEPS] {
        [
            &self.combo_attack_mode_1,
            &self.combo_attack_mode_2,
            &self.combo_attack_mode_3,
            &self.combo_attack_mode_4,
            &self.combo_attack_mode_5,
            &self.combo_attack_mode_6,
        ]
    }

    fn attack_skill_combos(&self) -> [&nwg::ComboBox<String>; MAX_ATTACK_SEQUENCE_STEPS] {
        [
            &self.combo_skill,
            &self.combo_attack_skill_2,
            &self.combo_attack_skill_3,
            &self.combo_attack_skill_4,
            &self.combo_attack_skill_5,
            &self.combo_attack_skill_6,
        ]
    }

    fn attack_interval_inputs(&self) -> [&nwg::TextInput; MAX_ATTACK_SEQUENCE_STEPS] {
        [
            &self.txt_attack_interval_1,
            &self.txt_attack_interval_2,
            &self.txt_attack_interval_3,
            &self.txt_attack_interval_4,
            &self.txt_attack_interval_5,
            &self.txt_attack_interval_6,
        ]
    }

    fn attack_rank_labels(&self) -> [&nwg::Label; MAX_ATTACK_SEQUENCE_STEPS] {
        [
            &self.lbl_skill,
            &self.lbl_attack_rank_2,
            &self.lbl_attack_rank_3,
            &self.lbl_attack_rank_4,
            &self.lbl_attack_rank_5,
            &self.lbl_attack_rank_6,
        ]
    }

    fn attack_interval_unit_labels(&self) -> [&nwg::Label; MAX_ATTACK_SEQUENCE_STEPS] {
        [
            &self.lbl_attack_interval_unit_1,
            &self.lbl_attack_interval_unit_2,
            &self.lbl_attack_interval_unit_3,
            &self.lbl_attack_interval_unit_4,
            &self.lbl_attack_interval_unit_5,
            &self.lbl_attack_interval_unit_6,
        ]
    }

    fn apply_attack_sequence_initial_values(&self) {
        self.txt_attack_cycle_secs
            .set_text(&self.initial_attack_cycle_secs);
        let mut steps = self.initial_attack_sequence.clone();
        if steps.is_empty() {
            steps.push(AttackSequenceStep::basic(0));
        }
        steps.truncate(MAX_ATTACK_SEQUENCE_STEPS);
        let count = steps.len().clamp(1, MAX_ATTACK_SEQUENCE_STEPS);
        self.attack_sequence_len.set(count);

        let mode_entries = attack_mode_options();
        let skill_entries = skill_combo_entries(&self.initial_skill_options);
        let skill_entry_count = skill_entries.len();
        for i in 0..MAX_ATTACK_SEQUENCE_STEPS {
            let step = steps
                .get(i)
                .cloned()
                .unwrap_or_else(|| AttackSequenceStep::basic(0))
                .normalized();
            self.attack_mode_combos()[i].set_collection(mode_entries.clone());
            self.attack_mode_combos()[i].set_selection(Some(match step.kind {
                AttackStepKind::Basic => 0,
                AttackStepKind::Skill => 1,
            }));
            self.attack_skill_combos()[i].set_collection(skill_entries.clone());
            let selected_skill = self
                .initial_skill_options
                .iter()
                .position(|n| n == &step.skill_name)
                .map(|idx| idx + 1)
                .unwrap_or(0);
            self.attack_skill_combos()[i].set_selection(Some(selected_skill));
            set_combo_dropdown_visible_rows(self.attack_skill_combos()[i], skill_entry_count);
            self.attack_interval_inputs()[i].set_text(&ms_to_secs_text(step.interval_ms));
        }
        self.update_attack_sequence_visibility();
    }

    fn update_attack_sequence_visibility(&self) {
        let count = self
            .attack_sequence_len
            .get()
            .clamp(1, MAX_ATTACK_SEQUENCE_STEPS);
        self.attack_sequence_len.set(count);
        for i in 0..MAX_ATTACK_SEQUENCE_STEPS {
            let visible = i < count;
            self.attack_rank_labels()[i].set_visible(visible);
            self.attack_mode_combos()[i].set_visible(visible);
            self.attack_skill_combos()[i].set_visible(visible);
            self.attack_interval_inputs()[i].set_visible(visible);
            self.attack_interval_unit_labels()[i].set_visible(visible);
            let skill_enabled =
                visible && matches!(self.attack_mode_combos()[i].selection(), Some(1));
            self.attack_skill_combos()[i].set_enabled(skill_enabled);
        }
        self.btn_add_attack_step
            .set_enabled(count < MAX_ATTACK_SEQUENCE_STEPS);
        self.btn_remove_attack_step.set_enabled(count > 1);
        redraw_bot_window(&self.window);
    }

    fn selected_attack_sequence(&self) -> Vec<AttackSequenceStep> {
        let count = self
            .attack_sequence_len
            .get()
            .clamp(1, MAX_ATTACK_SEQUENCE_STEPS);
        (0..count)
            .map(|i| {
                let interval_ms = parse_secs_to_ms(&self.attack_interval_inputs()[i].text(), 0);
                if matches!(self.attack_mode_combos()[i].selection(), Some(1)) {
                    let skill = self.selected_attack_skill_name(i);
                    if skill.is_empty() {
                        AttackSequenceStep::basic(interval_ms)
                    } else {
                        AttackSequenceStep::skill(skill, interval_ms)
                    }
                } else {
                    AttackSequenceStep::basic(interval_ms)
                }
            })
            .collect()
    }

    fn selected_attack_skill_name(&self, idx: usize) -> String {
        match self.attack_skill_combos()[idx].selection() {
            Some(0) | None => String::new(),
            Some(i) => self
                .initial_skill_options
                .get(i - 1)
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn first_sequence_skill_name(steps: &[AttackSequenceStep]) -> String {
        steps
            .iter()
            .find_map(|step| step.skill_for_cd().map(str::to_string))
            .unwrap_or_default()
    }

    /// 將攻擊技能 dropdown 捲到頂端。
    /// Windows ComboBox 會記住上次捲動位置，這裡固定回到第一列。
    fn on_attack_skill_dropdown(&self) {
        for combo in self.attack_skill_combos() {
            scroll_combo_dropdown_to_top(combo);
        }
    }

    fn on_attack_mode_changed(&self) {
        self.update_attack_sequence_visibility();
    }

    fn on_add_attack_step(&self) {
        let count = self.attack_sequence_len.get();
        if count < MAX_ATTACK_SEQUENCE_STEPS {
            let next = count + 1;
            self.attack_sequence_len.set(next);
            self.attack_mode_combos()[next - 1].set_selection(Some(0));
            self.attack_skill_combos()[next - 1].set_selection(Some(0));
            self.attack_interval_inputs()[next - 1].set_text("0");
            self.update_attack_sequence_visibility();
        }
    }

    fn on_remove_attack_step(&self) {
        let count = self.attack_sequence_len.get();
        if count > 1 {
            self.attack_sequence_len.set(count - 1);
            self.update_attack_sequence_visibility();
            self.on_save();
        }
    }

    fn on_master_changed(&self) {
        let on = matches!(self.cb_master.check_state(), nwg::CheckBoxState::Checked);
        crate::bot::set_master_enabled(on);
        self.lbl_status.set_text(&if on {
            ui_text("主開關 開")
        } else {
            ui_text("主開關 關")
        });
    }

    fn on_save(&self) {
        let attack_sequence = self.selected_attack_sequence();
        let skill = Self::first_sequence_skill_name(&attack_sequence);
        let attack_sequence_cycle_ms = parse_secs_to_ms(&self.txt_attack_cycle_secs.text(), 0);
        let hunt_range_tiles: u32 =
            self.txt_hunt_range
                .text()
                .trim()
                .parse()
                .unwrap_or_else(|_| {
                    self.lbl_status
                        .set_text(&ui_text("搜尋範圍格式錯誤，已改用 0(不限)"));
                    0
                });
        let blacklist: Vec<String> = self
            .txt_blacklist
            .text()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        let teleport_name = selected_teleport_scroll_name(self.combo_teleport.selection());
        let teleport_secs: u64 =
            self.txt_teleport_secs
                .text()
                .trim()
                .parse()
                .unwrap_or_else(|_| {
                    self.lbl_status
                        .set_text(&ui_text("順移秒數格式錯誤，已改用 30"));
                    30
                });

        let current_hunt = with_engine(|e| e.hunt_config_snapshot()).unwrap_or_default();

        let new_hunt = HuntConfig {
            hunt_range_tiles,
            monster_blacklist: blacklist.clone(),
            skill_name: skill.clone(),
            attack_sequence: attack_sequence.clone(),
            attack_sequence_cycle_ms,
            teleport_scroll_name: teleport_name,
            idle_teleport_secs: teleport_secs,
            walk_driver: current_hunt.walk_driver,
            v4_dispatch_takeover: matches!(
                self.cb_v4_dispatch_takeover.check_state(),
                nwg::CheckBoxState::Checked
            ),
            // Preserve the existing damage-spike threshold; the UI does not expose it.
            damage_spike_hp_percent: current_hunt.damage_spike_hp_percent,
        };

        // 寫回 engine(即時生效)。
        with_engine(|e| e.set_hunt_config(new_hunt.clone()));

        // 寫入 JSON(per-character 設定)。
        match with_engine(|e| e.h_raw()) {
            Some(h_raw) => {
                let h = HANDLE(h_raw as *mut _);
                match read_player_name(h) {
                    Some(name) => {
                        let full = BotConfig {
                            master_enabled: matches!(
                                self.cb_master.check_state(),
                                nwg::CheckBoxState::Checked
                            ),
                            hunt: new_hunt.clone(),
                            death_stops_hunt: true,
                            disconnect_stops_hunt: true,
                            stuck_timeout_secs: 30,
                            walk_driver: new_hunt.walk_driver,
                        };
                        config::save(&name, &full);
                        let mode_str = format!(
                            "攻擊序列 {} 步 | 循環 {} 秒 | {}",
                            attack_sequence.len(),
                            ms_to_secs_text(attack_sequence_cycle_ms),
                            self.initial_weapon_label
                        );
                        let range_str = if hunt_range_tiles == 0 {
                            ui_text("範圍不限")
                        } else {
                            ui_dynamic(format!("範圍 {hunt_range_tiles}"))
                        };
                        self.lbl_status.set_text(&ui_dynamic(format!(
                            "已儲存:{} | {} | 黑名單 {} | {}",
                            name,
                            range_str,
                            blacklist.len(),
                            mode_str
                        )));
                    }
                    None => {
                        self.lbl_status.set_text(&ui_text("無角色名；只儲存到引擎"));
                    }
                }
            }
            None => {
                self.lbl_status.set_text(&ui_text("引擎不可用"));
            }
        }
    }

    fn on_close_button(&self) {
        nwg::stop_thread_dispatch();
    }

    fn on_close(&self) {
        nwg::stop_thread_dispatch();
    }

    fn on_open_minimap(&self) {
        match with_engine(|e| e.h_raw()) {
            Some(h_raw) => {
                let h = HANDLE(h_raw as *mut _);
                crate::minimap::show_minimap(h);
            }
            None => {
                self.lbl_status.set_text(&ui_text("小地圖: 引擎不可用"));
            }
        }
    }

    fn on_state_tick(&self) {
        match with_engine(|e| e.hunt4_report_snapshot()).flatten() {
            Some(report) => {
                let [state, since, transition, action, lock] = hunt4_report_lines(&report);
                self.lbl_state_label.set_text(&state);
                self.lbl_state_since.set_text(&since);
                self.lbl_state_transition.set_text(&transition);
                self.lbl_state_action.set_text(&action);
                self.lbl_state_lock.set_text(&lock);
            }
            None => {
                self.lbl_state_label.set_text(&ui_text("無 Hunt4 report"));
                self.lbl_state_since.set_text(&ui_text("持續時間 -"));
                self.lbl_state_transition.set_text(&ui_text("狀態轉換 -"));
                self.lbl_state_action
                    .set_text(&ui_text("本次指令: - | 回傳=- | 觀測=座標- / 傷害-"));
                self.lbl_state_lock.set_text(&ui_text("鎖定: -"));
            }
        }
    }
}

/// 設定 ComboBox dropdown 可見列數，避免 Windows 預設只顯示 5-8 列。
/// 參考 `aux::lhx_window` 的 `CB_SETMINVISIBLE` 作法(Vista+ 支援)。
fn set_combo_dropdown_visible_rows(combo: &nwg::ComboBox<String>, item_count: usize) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
    const CB_SETMINVISIBLE: u32 = 0x1701;
    let rows = item_count.clamp(1, 50);
    if let Some(hwnd) = combo.handle.hwnd() {
        unsafe {
            let h = HWND(hwnd as *mut _);
            let _ = SendMessageW(h, CB_SETMINVISIBLE, Some(WPARAM(rows)), Some(LPARAM(0)));
        }
    }
}

/// 將 ComboBox dropdown 捲到頂端，避免 Windows 記住上次選項造成清單打開在中段。
/// 沒有 hook `CBN_DROPDOWN` 的情況下，開啟時直接送 `CB_SETTOPINDEX`。
fn scroll_combo_dropdown_to_top(combo: &nwg::ComboBox<String>) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
    const CB_SETTOPINDEX: u32 = 0x015C;
    if let Some(hwnd) = combo.handle.hwnd() {
        unsafe {
            let h = HWND(hwnd as *mut _);
            let _ = SendMessageW(h, CB_SETTOPINDEX, Some(WPARAM(0)), Some(LPARAM(0)));
        }
    }
}

fn redraw_bot_window(window: &nwg::Window) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::InvalidateRect;

    let Some(hwnd) = window.handle.hwnd() else {
        return;
    };
    let hwnd = HWND(hwnd as *mut _);
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy_text::{set_text_encoding_mode, text_encoding_mode, TextEncodingMode};
    use std::sync::Mutex;

    static MODE_LOCK: Mutex<()> = Mutex::new(());

    fn with_mode<F: FnOnce()>(mode: TextEncodingMode, f: F) {
        let _guard = MODE_LOCK.lock().unwrap();
        let prev = text_encoding_mode();
        set_text_encoding_mode(mode);
        f();
        set_text_encoding_mode(prev);
    }

    #[test]
    fn teleport_options_follow_simplified_encoding_mode() {
        with_mode(TextEncodingMode::Gbk, || {
            assert_eq!(
                teleport_combo_options(),
                vec![
                    "(不使用)".to_string(),
                    TELEPORT_HOME_SIMP.to_string(),
                    TELEPORT_RANDOM_SIMP.to_string(),
                ]
            );
            assert_eq!(selected_teleport_scroll_name(Some(1)), TELEPORT_HOME_SIMP);
            assert_eq!(selected_teleport_scroll_name(Some(2)), TELEPORT_RANDOM_SIMP);
        });
    }

    #[test]
    fn ui_text_follows_traditional_and_simplified_modes() {
        with_mode(TextEncodingMode::Big5, || {
            assert_eq!(ui_text("啟用自動練功 (F8)"), "啟用自動練功 (F8)");
            assert_eq!(attack_mode_options(), vec!["普攻", "技能"]);
        });
        with_mode(TextEncodingMode::Gbk, || {
            assert_eq!(ui_text("啟用自動練功 (F8)"), "启用自动练功 (F8)");
            assert_eq!(ui_text("狀態轉換 -"), "状态转换 -");
            assert_eq!(attack_mode_options(), vec!["普攻", "技能"]);
        });
    }

    #[test]
    fn teleport_name_to_index_accepts_traditional_and_simplified_names() {
        assert_eq!(
            teleport_name_to_index_canonical(TELEPORT_HOME_TRAD),
            Some(1)
        );
        assert_eq!(
            teleport_name_to_index_canonical(TELEPORT_HOME_SIMP),
            Some(1)
        );
        assert_eq!(
            teleport_name_to_index_canonical(TELEPORT_RANDOM_TRAD),
            Some(2)
        );
        assert_eq!(
            teleport_name_to_index_canonical(TELEPORT_RANDOM_SIMP),
            Some(2)
        );
    }

    #[test]
    fn remove_attack_step_applies_config_immediately() {
        let source = include_str!("window.rs");
        let body = source
            .split("fn on_remove_attack_step(&self)")
            .nth(1)
            .and_then(|rest| rest.split("fn on_master_changed").next())
            .expect("remove attack step handler should exist");

        assert!(body.contains("self.update_attack_sequence_visibility();"));
        assert!(body.contains("self.on_save();"));
    }

    #[test]
    fn attack_sequence_visibility_redraws_parent_window() {
        let source = include_str!("window.rs");
        let body = source
            .split("fn update_attack_sequence_visibility(&self)")
            .nth(1)
            .and_then(|rest| rest.split("fn selected_attack_sequence(&self)").next())
            .expect("visibility updater should exist");

        assert!(body.contains("redraw_bot_window(&self.window);"));
    }

    #[test]
    fn outcome_text_labels_dispatch_ack_not_real_game_success() {
        with_mode(TextEncodingMode::Big5, || {
            assert_eq!(outcome_text(Some(&LastOutcome::WalkOk)), "走路指令已送出");
            assert_eq!(
                outcome_text(Some(&LastOutcome::AttackOk { target_id: 0x1234 })),
                "攻擊指令已送出 0x1234"
            );
            assert_eq!(outcome_text(Some(&LastOutcome::ScrollOk)), "順移指令已送出");
        });
    }

    #[test]
    fn observed_progress_text_reports_actual_observations() {
        let now = Instant::now();

        with_mode(TextEncodingMode::Big5, || {
            assert_eq!(
                observed_progress_text_at(
                    Some(now - std::time::Duration::from_secs(2)),
                    Some(now - std::time::Duration::from_millis(500)),
                    now,
                ),
                "觀測=座標2.0秒前 / 傷害0.5秒前"
            );
            assert_eq!(
                observed_progress_text_at(None, None, now),
                "觀測=座標- / 傷害-"
            );
        });
    }
}
