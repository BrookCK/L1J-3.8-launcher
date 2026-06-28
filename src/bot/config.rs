//! BotConfig — per-character 內掛設定持久化。
//!
//! ## 設計
//!
//! 仿 `aux::profile` 模式但**分開檔案** — bot 的設定不混入 `AuxSettings`,讓既有
//! 喝水助手 / LHX 設定跟新內掛設定各自獨立。
//!
//! 檔案位置:`launcher.exe/../aux_settings/<charname>.bot.json`(注意 `.bot.json`
//! 後綴跟既有 `<charname>.json` 區隔)。
//!
//! 進場時(state 0→3)讀,離場時(state 3→0)存 — 觸發點由 `bot::install`/`bot::shutdown`
//! 接到 `main.rs` 的場景 lifecycle 處理。
//!
//! ## 為什麼用 JSON
//!
//! 跟 `aux::profile` 一致;BotConfig 結構簡單(無嵌套 Vec/enum),手寫 INI 也行,但
//! 為保持兩邊體系一致,沿用 serde_json。

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::log_line;

use super::action::walk::WalkDriver;
use super::decide::hunt::HuntConfig;

/// 內掛完整設定 — 序列化到 JSON,跨遊戲 session 保留。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotConfig {
    /// master 開關 — true 時 install 後自動進 Hunting(若已進遊戲)
    pub master_enabled: bool,
    /// Hunting 細節:狩獵範圍(tile)/ 黑名單 / 技能名 / cooldown / 攻擊模式
    pub hunt: HuntConfig,
    /// 死亡時自動結束狩獵(預設 true,操8.8 截圖中該勾選也預設 on)
    pub death_stops_hunt: bool,
    /// 斷線時自動結束狩獵(預設 true)
    pub disconnect_stops_hunt: bool,
    /// 卡死偵測:連續 N 秒無動作 → Stopped(預設 30s)
    pub stuck_timeout_secs: u32,
    #[serde(default)]
    pub walk_driver: WalkDriver,
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            master_enabled: false, // 安全預設 — 必須使用者明確 enable
            hunt: HuntConfig::default(),
            death_stops_hunt: true,
            disconnect_stops_hunt: true,
            stuck_timeout_secs: 30,
            walk_driver: WalkDriver::PostMessage,
        }
    }
}

fn settings_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("取得 launcher.exe 路徑")?;
    let dir = exe
        .parent()
        .context("launcher.exe 沒有 parent dir")?
        .join("aux_settings");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).with_context(|| format!("建立資料夾 {dir:?}"))?;
    }
    Ok(dir)
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

fn bot_profile_path(name: &str) -> Result<PathBuf> {
    let safe = sanitize_filename(name);
    Ok(settings_dir()?.join(format!("{safe}.bot.json")))
}

/// 對外暴露 `.bot.json` 路徑 — file watcher 用來監聽。 跟 `load` / `save` 走同一條
/// 解析路徑,玩家編 JSON 時的檔案就是這個。
pub fn profile_path_for(name: &str) -> Result<PathBuf> {
    bot_profile_path(name)
}

/// 讀指定角色的內掛設定。 檔案不存在 / parse 失敗 → 回 default(log warning)。
pub fn load(name: &str) -> BotConfig {
    let path = match bot_profile_path(name) {
        Ok(p) => p,
        Err(e) => {
            log_line!("[bot/config] 取得 {name} 路徑失敗,用 default: {e:#}");
            return BotConfig::default();
        }
    };
    if !path.exists() {
        return BotConfig::default();
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            log_line!("[bot/config] 讀 {} 失敗,用 default: {e:#}", path.display());
            return BotConfig::default();
        }
    };
    match serde_json::from_str::<BotConfig>(&raw) {
        Ok(cfg) => {
            let cfg = normalize_runtime_fields(cfg);
            log_line!("[bot/config] 載入 {}", path.display());
            cfg
        }
        Err(e) => {
            log_line!(
                "[bot/config] parse {} 失敗,用 default: {e:#}",
                path.display()
            );
            BotConfig::default()
        }
    }
}

/// 把當前內掛設定存到指定角色的 JSON 檔。 失敗 log warning 不爆炸。
fn normalize_runtime_fields(mut cfg: BotConfig) -> BotConfig {
    let requested_walk_driver = cfg.walk_driver;
    cfg.walk_driver = normalize_walk_driver(cfg.walk_driver);
    if requested_walk_driver != cfg.walk_driver {
        log_line!(
            "[bot/config] walk_driver={requested_walk_driver:?} redirected to {:?}",
            cfg.walk_driver
        );
    }
    cfg.hunt.walk_driver = cfg.walk_driver;
    cfg
}

fn normalize_walk_driver(driver: WalkDriver) -> WalkDriver {
    match driver {
        WalkDriver::PostMessage => WalkDriver::PostMessage,
    }
}

pub fn save(name: &str, cfg: &BotConfig) {
    let mut cfg = cfg.clone();
    cfg.walk_driver = cfg.hunt.walk_driver;
    let path = match bot_profile_path(name) {
        Ok(p) => p,
        Err(e) => {
            log_line!("[bot/config] 取得 {name} 路徑失敗,放棄存檔: {e:#}");
            return;
        }
    };
    let json = match serde_json::to_string_pretty(&cfg) {
        Ok(s) => s,
        Err(e) => {
            log_line!("[bot/config] 序列化失敗: {e:#}");
            return;
        }
    };
    match std::fs::write(&path, json) {
        Ok(()) => log_line!("[bot/config] 已存 {}", path.display()),
        Err(e) => log_line!("[bot/config] 寫 {} 失敗: {e:#}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_safe() {
        let cfg = BotConfig::default();
        assert!(!cfg.master_enabled, "預設應 OFF 避免意外啟動");
        assert!(cfg.death_stops_hunt);
        assert!(cfg.disconnect_stops_hunt);
        assert_eq!(cfg.stuck_timeout_secs, 30);
        assert_eq!(cfg.hunt.hunt_range_tiles, 0, "預設不限制狩獵範圍");
        assert!(cfg.hunt.monster_blacklist.is_empty());
        assert!(cfg.hunt.skill_name.is_empty());
    }

    #[test]
    fn default_walk_driver_uses_postmessage_to_avoid_memoryclick_window_stretch() {
        let cfg = BotConfig::default();
        assert_eq!(
            cfg.walk_driver,
            crate::bot::action::walk::WalkDriver::PostMessage
        );
    }

    #[test]
    fn round_trip_json() {
        let original = BotConfig {
            master_enabled: true,
            hunt: HuntConfig {
                hunt_range_tiles: 20,
                monster_blacklist: vec!["城衛兵".into()],
                skill_name: "靈魂之箭".into(),
                attack_sequence: Vec::new(),
                attack_sequence_cycle_ms: 0,
                teleport_scroll_name: String::new(),
                idle_teleport_secs: 10,
                walk_driver: WalkDriver::PostMessage,
                v4_dispatch_takeover: false,
                damage_spike_hp_percent: 10,
            },
            death_stops_hunt: false,
            disconnect_stops_hunt: true,
            stuck_timeout_secs: 60,
            walk_driver: WalkDriver::PostMessage,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: BotConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.master_enabled, true);
        assert_eq!(parsed.hunt.hunt_range_tiles, 20);
        assert_eq!(
            parsed.hunt.monster_blacklist,
            original.hunt.monster_blacklist
        );
        assert_eq!(parsed.hunt.skill_name, "靈魂之箭");
        assert_eq!(parsed.stuck_timeout_secs, 60);
    }

    #[test]
    fn legacy_config_without_range_field_loads_default() {
        // 既有設定檔(改前)沒有 hunt_range_tiles / monster_blacklist 欄位 → serde
        // default 補回。 reverse compat — 玩家升級 launcher 後不需重新填設定。
        let legacy = r#"{
            "master_enabled": true,
            "hunt": {
                "skill_name": "光箭",
                "skill_cooldown_ms": 2000,
                "attack_mode": "Skill",
                "physical_cooldown_ms": 800
            },
            "death_stops_hunt": true,
            "disconnect_stops_hunt": true,
            "stuck_timeout_secs": 30
        }"#;
        let parsed: BotConfig = serde_json::from_str(legacy).expect("舊版 JSON 應該還能 load");
        assert_eq!(parsed.hunt.hunt_range_tiles, 0, "missing → unlimited");
        assert!(parsed.hunt.monster_blacklist.is_empty());
        assert_eq!(parsed.hunt.skill_name, "光箭");
    }

    #[test]
    fn legacy_config_without_walk_driver_loads_postmessage_defaults() {
        let legacy = r#"{
            "master_enabled": true,
            "hunt": {
                "skill_name": "光箭",
                "skill_cooldown_ms": 2000,
                "attack_mode": "Skill",
                "physical_cooldown_ms": 800
            },
            "death_stops_hunt": true,
            "disconnect_stops_hunt": true,
            "stuck_timeout_secs": 30
        }"#;
        let parsed: BotConfig = serde_json::from_str(legacy).expect("舊版 JSON 應該還能 load");
        assert_eq!(
            parsed.walk_driver,
            crate::bot::action::walk::WalkDriver::PostMessage
        );
    }

    #[test]
    fn top_level_walk_driver_feeds_runtime_hunt_config() {
        let raw = r#"{
            "master_enabled": true,
            "hunt": {
                "skill_name": "",
                "skill_cooldown_ms": 2000,
                "attack_mode": "Skill",
                "physical_cooldown_ms": 800
            },
            "death_stops_hunt": true,
            "disconnect_stops_hunt": true,
            "stuck_timeout_secs": 30,
            "walk_driver": "move_packet"
        }"#;
        let parsed: BotConfig = serde_json::from_str(raw).expect("walk driver JSON should parse");
        let normalized = normalize_runtime_fields(parsed);

        assert_eq!(normalized.walk_driver, WalkDriver::PostMessage);
        assert_eq!(normalized.hunt.walk_driver, WalkDriver::PostMessage);
    }

    #[test]
    fn remote_internal_walk_config_is_loaded_as_postmessage_alias() {
        let raw = r#"{
            "master_enabled": true,
            "hunt": {
                "skill_name": "",
                "skill_cooldown_ms": 2000,
                "attack_mode": "Skill",
                "physical_cooldown_ms": 800
            },
            "death_stops_hunt": true,
            "disconnect_stops_hunt": true,
            "stuck_timeout_secs": 30,
            "walk_driver": "remote_internal_walk"
        }"#;
        let parsed: BotConfig = serde_json::from_str(raw).expect("remote driver JSON should parse");
        let normalized = normalize_runtime_fields(parsed);

        assert_eq!(normalized.walk_driver, WalkDriver::PostMessage);
        assert_eq!(normalized.hunt.walk_driver, WalkDriver::PostMessage);
    }

    #[test]
    fn normalize_walk_driver_keeps_postmessage() {
        assert_eq!(
            normalize_walk_driver(WalkDriver::PostMessage),
            WalkDriver::PostMessage
        );
    }

    #[test]
    fn sanitize_strips_bad_chars() {
        assert_eq!(sanitize_filename("玩家A"), "玩家A");
        assert_eq!(sanitize_filename("a/b\\c"), "a_b_c");
        assert_eq!(sanitize_filename("ab:cd*"), "ab_cd_");
    }
}
