//! 依 map_id 載入整張 map(多個 64×64 block 拼起來)。
//!
//! 檔案結構(see `memory/s32_map_format.md`):
//! `<local game dir>\map\<map_id>\<hex>.s32`,檔名 8-char hex u32 拆 hi16/lo16:
//! - `bx = (lo16 as i32) - 0x8000`
//! - `by = (hi16 as i32) - 0x8000`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{Context, Result};
use once_cell::sync::Lazy;

use super::nav_grid::NavGrid;
use super::nav_profile::NavProfile;
use super::s32_parser::{self, Block};

/// 整張 map 的解析結果。
#[derive(Debug)]
pub struct Map {
    pub map_id: u32,
    pub nav: NavGrid,
    pub profile: NavProfile,
    /// (block_x, block_y) → Block。 不存在的 block 被視為「全 blocked」(out-of-bounds)。
    pub blocks: HashMap<(i32, i32), Block>,
    pub bounds: Bounds,
}

/// 整張 map 在 block 座標系的 bbox(min/max inclusive)。
/// 空 map 用 `min > max` 表示。
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub min_block_x: i32,
    pub max_block_x: i32,
    pub min_block_y: i32,
    pub max_block_y: i32,
}

impl Bounds {
    /// 沒任何 block 時用的 sentinel — `is_empty()` 為 true。
    pub fn empty() -> Self {
        Self {
            min_block_x: i32::MAX,
            max_block_x: i32::MIN,
            min_block_y: i32::MAX,
            max_block_y: i32::MIN,
        }
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.min_block_x > self.max_block_x || self.min_block_y > self.max_block_y
    }

    fn extend(&mut self, bx: i32, by: i32) {
        self.min_block_x = self.min_block_x.min(bx);
        self.max_block_x = self.max_block_x.max(bx);
        self.min_block_y = self.min_block_y.min(by);
        self.max_block_y = self.max_block_y.max(by);
    }
}

/// game 預設安裝路徑 — Phase 1 寫死,後續可改成可設。
static GAME_ROOT: Lazy<RwLock<Option<PathBuf>>> = Lazy::new(|| RwLock::new(None));

pub fn set_game_root(path: impl Into<PathBuf>) {
    *GAME_ROOT.write().expect("minimap game root lock poisoned") = Some(path.into());
}

fn game_root() -> PathBuf {
    if let Some(path) = GAME_ROOT
        .read()
        .expect("minimap game root lock poisoned")
        .clone()
    {
        return path;
    }

    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn map_dir_for_root(root: &Path, map_id: u32) -> PathBuf {
    root.join("map").join(map_id.to_string())
}

pub fn load(map_id: u32) -> Result<Map> {
    let root = game_root();
    let dir = map_dir_for_root(&root, map_id);
    if !dir.is_dir() {
        anyhow::bail!("map dir 不存在: {}", dir.display());
    }
    let mut blocks = HashMap::new();
    let mut bounds = Bounds::empty();
    for entry in std::fs::read_dir(&dir).context("read map dir")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("s32") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let Some((bx, by)) = decode_filename(stem) else {
            continue;
        };
        let bytes = std::fs::read(&path).context("read .s32")?;
        let block =
            s32_parser::parse(&bytes).with_context(|| format!("parse {}", path.display()))?;
        blocks.insert((bx, by), block);
        bounds.extend(bx, by);
    }
    let nav = NavGrid::from_blocks(&blocks);
    let profile = NavProfile::from_blocks(&nav, &blocks);
    Ok(Map {
        map_id,
        nav,
        profile,
        blocks,
        bounds,
    })
}

/// 檔名 stem(e.g. "7ff88000")→ (block_x, block_y)。
///
/// **對齊 L1MapViewer `Helper/L1MapHelper.cs:174-175`**:
/// ```csharp
/// int nBlockX = Convert.ToInt32(szFileName.Substring(0, 4), 16); // 前 4 char = X
/// int nBlockY = Convert.ToInt32(szFileName.Substring(4, 4), 16); // 後 4 char = Y
/// ```
///
/// 各值以 `0x8000` 為原點 → normalized offset。 例:
/// - "7ff88000" → bX=0x7FF8 (norm=-8), bY=0x8000 (norm=0)
/// - "80008004" → bX=0x8000 (norm=0),  bY=0x8004 (norm=4)
pub fn decode_filename(stem: &str) -> Option<(i32, i32)> {
    if stem.len() != 8 {
        return None;
    }
    let bx_raw = i32::from_str_radix(&stem[0..4], 16).ok()?;
    let by_raw = i32::from_str_radix(&stem[4..8], 16).ok()?;
    Some((bx_raw - 0x8000, by_raw - 0x8000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_filename_origin() {
        // 前 4="8000" 後 4="8000" → (0, 0)
        let (bx, by) = decode_filename("80008000").unwrap();
        assert_eq!((bx, by), (0, 0));
    }

    #[test]
    fn decode_filename_x_axis_from_first_4_chars() {
        // 7ff88000 → bX=0x7FF8 (norm=-8), bY=0x8000 (norm=0)
        let (bx, by) = decode_filename("7ff88000").unwrap();
        assert_eq!((bx, by), (-8, 0));
    }

    #[test]
    fn decode_filename_y_axis_from_last_4_chars() {
        // 80007ff7 → bX=0x8000 (norm=0), bY=0x7FF7 (norm=-9)
        let (bx, by) = decode_filename("80007ff7").unwrap();
        assert_eq!((bx, by), (0, -9));
    }

    #[test]
    fn decode_filename_both_axes() {
        // 7ffc7ff7 → bX=0x7FFC (norm=-4), bY=0x7FF7 (norm=-9)
        let (bx, by) = decode_filename("7ffc7ff7").unwrap();
        assert_eq!((bx, by), (-4, -9));
    }

    #[test]
    fn decode_filename_bad_hex_returns_none() {
        assert!(decode_filename("notHex42").is_none());
    }

    #[test]
    fn decode_filename_wrong_length_returns_none() {
        assert!(decode_filename("8000").is_none());
        assert!(decode_filename("8000800000").is_none());
    }

    #[test]
    fn map_dir_uses_supplied_game_root() {
        let root = std::path::Path::new(r"D:\client");

        assert_eq!(
            map_dir_for_root(root, 54),
            std::path::PathBuf::from(r"D:\client")
                .join("map")
                .join("54")
        );
    }

    #[test]
    fn bounds_empty_sentinel() {
        let b = Bounds::empty();
        assert!(b.is_empty());
    }

    #[test]
    fn bounds_extend_tracks_min_max() {
        let mut b = Bounds::empty();
        b.extend(3, 5);
        assert!(!b.is_empty());
        b.extend(-1, 7);
        b.extend(2, 4);
        assert_eq!((b.min_block_x, b.max_block_x), (-1, 3));
        assert_eq!((b.min_block_y, b.max_block_y), (4, 7));
    }

    #[test]
    fn navigation_profile_groups_walkable_tiles_by_connected_area() {
        use crate::minimap::coord::BLOCK_ORIGIN;
        use crate::minimap::nav_grid::NavGrid;
        use crate::minimap::nav_profile::NavProfile;
        use crate::minimap::s32_parser::Block;
        use std::collections::HashMap;

        let mut walkable = Box::new([[false; 64]; 64]);
        walkable[0][0] = true;
        walkable[0][1] = true;
        walkable[0][10] = true;
        walkable[0][11] = true;
        let blocks = HashMap::from([((0, 0), Block::from_walkable(walkable))]);
        let nav = NavGrid::from_blocks(&blocks);
        let profile = NavProfile::from_blocks(&nav, &blocks);

        assert_eq!(profile.component_count(), 2);
        assert!(profile.same_component(
            (BLOCK_ORIGIN, BLOCK_ORIGIN),
            (BLOCK_ORIGIN + 1, BLOCK_ORIGIN)
        ));
        assert!(!profile.same_component(
            (BLOCK_ORIGIN, BLOCK_ORIGIN),
            (BLOCK_ORIGIN + 10, BLOCK_ORIGIN)
        ));
        assert!(profile
            .component_id((BLOCK_ORIGIN + 5, BLOCK_ORIGIN))
            .is_none());
    }

    #[test]
    #[ignore = "需要遊戲安裝"]
    fn real_map_4_loads() {
        let map = load(4).expect("map_id=4 should load");
        assert!(!map.blocks.is_empty(), "map 4 沒讀到 block");
        assert_eq!(map.map_id, 4);
        assert!(!map.bounds.is_empty());
    }
}
