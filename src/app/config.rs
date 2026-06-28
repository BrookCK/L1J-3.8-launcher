//! 使用者偏好設定 — 持久化到 launcher.exe 旁的 launcher.ini
//!
//! 內容是「玩家自己的選擇」(視窗化、解析度等),跟 list.txt 裡 LauncherConfig
//! (伺服器管理員散發的 skin / 公告 URL)是兩件事 — 那邊不該存玩家偏好。
//!
//! 格式:
//! ```ini
//! [Settings]
//! windowed=true
//! window_mode=5
//! ```

use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = "launcher.ini";

/// 視窗解析度模式 — 對應遊戲 lineage.cfg 的 WindowMode 值(4..=7)。
///
/// 這是「window_mode 到底是什麼」的**單一真相源**:以前 4/5/6/7 這組裸數字與
/// `(4..=7)` 合法性檢查、預設值 5、解析度註解散落在 config.rs / gui.rs /
/// lineage_cfg.rs 至少四份,改一處要追四處。收斂成 enum 後,合法性由型別保證,
/// 解析(`from_raw`)與回寫(`as_raw`)是唯二的 u8 邊界轉換。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    /// 4 → 400x300
    W400x300,
    /// 5 → 800x600
    W800x600,
    /// 6 → 1200x900
    W1200x900,
    /// 7 → 1600x1200
    W1600x1200,
}

impl WindowMode {
    /// 預設值:800x600 — 最不會踩 W11 螢幕邊界 + DPI 殘影問題。
    pub const DEFAULT: WindowMode = WindowMode::W800x600;

    /// 從 ini / cfg 的原始值解析。超出 4..=7 → None(由呼叫端決定 fallback)。
    pub const fn from_raw(n: u8) -> Option<WindowMode> {
        match n {
            4 => Some(WindowMode::W400x300),
            5 => Some(WindowMode::W800x600),
            6 => Some(WindowMode::W1200x900),
            7 => Some(WindowMode::W1600x1200),
            _ => None,
        }
    }

    /// 回寫 ini / cfg / JS 用的原始值(4..=7)。
    pub const fn as_raw(self) -> u8 {
        match self {
            WindowMode::W400x300 => 4,
            WindowMode::W800x600 => 5,
            WindowMode::W1200x900 => 6,
            WindowMode::W1600x1200 => 7,
        }
    }

    /// 解析度 (寬, 高)。
    #[allow(dead_code)]
    pub const fn resolution(self) -> (u32, u32) {
        match self {
            WindowMode::W400x300 => (400, 300),
            WindowMode::W800x600 => (800, 600),
            WindowMode::W1200x900 => (1200, 900),
            WindowMode::W1600x1200 => (1600, 1200),
        }
    }
}

/// 玩家偏好(視窗化 + 視窗大小)。
#[derive(Debug, Clone)]
pub struct UserPrefs {
    pub windowed: bool,
    pub window_mode: WindowMode,
}

impl Default for UserPrefs {
    fn default() -> Self {
        // 預設視窗化 + 800x600,最不會踩 W11 螢幕邊界 + DPI 殘影問題
        Self {
            windowed: true,
            window_mode: WindowMode::DEFAULT,
        }
    }
}

impl UserPrefs {
    /// 從 launcher.exe 旁的 launcher.ini 載入,失敗或缺欄位 → 用 default
    pub fn load() -> Self {
        let mut prefs = Self::default();
        let path = match config_path() {
            Some(p) => p,
            None => return prefs,
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return prefs,
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('[') {
                continue;
            }
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "windowed" => prefs.windowed = matches!(val.trim(), "true" | "1" | "yes"),
                "window_mode" => {
                    if let Ok(n) = val.trim().parse::<u8>() {
                        if let Some(mode) = WindowMode::from_raw(n) {
                            prefs.window_mode = mode;
                        }
                    }
                }
                _ => {}
            }
        }
        prefs
    }

    /// 寫回 launcher.ini。失敗時靜默忽略 — 持久化失敗不該擋啟動。
    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        let content = format!(
            "[Settings]\nwindowed={}\nwindow_mode={}\n",
            self.windowed,
            self.window_mode.as_raw()
        );
        let _ = std::fs::write(&path, content);
    }
}

fn config_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    Some(dir.join(CONFIG_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_mode_raw_round_trips_for_all_valid_values() {
        for n in 4u8..=7 {
            let mode = WindowMode::from_raw(n).expect("4..=7 應全部合法");
            assert_eq!(mode.as_raw(), n, "as_raw 應還原原始值");
        }
    }

    #[test]
    fn window_mode_rejects_out_of_range_raw() {
        for n in [0u8, 1, 2, 3, 8, 9, 255] {
            assert_eq!(
                WindowMode::from_raw(n),
                None,
                "{n} 不該被視為合法 WindowMode"
            );
        }
    }

    #[test]
    fn window_mode_default_is_800x600() {
        assert_eq!(WindowMode::DEFAULT.as_raw(), 5);
        assert_eq!(WindowMode::DEFAULT.resolution(), (800, 600));
    }

    #[test]
    fn window_mode_resolution_matches_raw_table() {
        assert_eq!(WindowMode::W400x300.resolution(), (400, 300));
        assert_eq!(WindowMode::W1200x900.resolution(), (1200, 900));
        assert_eq!(WindowMode::W1600x1200.resolution(), (1600, 1200));
    }

    #[test]
    fn user_prefs_default_uses_window_mode_default() {
        let prefs = UserPrefs::default();
        assert!(prefs.windowed);
        assert_eq!(prefs.window_mode, WindowMode::DEFAULT);
    }
}
