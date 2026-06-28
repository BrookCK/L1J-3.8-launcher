//! 已解析 map 的 LRU cache — 玩家在 map 之間切換時不必重 parse。
//!
//! 8 個 entry 上限。 Phase 2 bot pathfinding 將直接共用 `global()` 的 wall data 跑 A*。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use once_cell::sync::Lazy;

use super::map_loader::Map;

const MAX_ENTRIES: usize = 8;

/// 全域 cache instance — minimap window 與 (Phase 2) bot 共享。
pub fn global() -> &'static MapCache {
    static INSTANCE: Lazy<MapCache> = Lazy::new(MapCache::new);
    &INSTANCE
}

pub struct MapCache {
    inner: Mutex<Inner>,
}

struct Inner {
    /// LRU order — front = newest。 同時當 lookup table 用(O(N) 但 N=8 可接受)。
    entries: VecDeque<Arc<Map>>,
}

impl MapCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: VecDeque::with_capacity(MAX_ENTRIES + 1),
            }),
        }
    }

    /// 找 cached map。 命中 → bump 到 front。 沒命中 → None,caller 自己 load + insert。
    pub fn get(&self, map_id: u32) -> Option<Arc<Map>> {
        let mut inner = self.inner.lock().unwrap();
        let pos = inner.entries.iter().position(|m| m.map_id == map_id)?;
        let entry = inner.entries.remove(pos).unwrap();
        inner.entries.push_front(Arc::clone(&entry));
        Some(entry)
    }

    /// 找 cached map；cache miss 時由 caller 提供 loader 載入並插回 cache。
    ///
    /// 這是 bot 與 minimap 共用的資料入口，避免「小地圖沒開 → bot 沒有牆資料」。
    pub fn get_or_load<F>(&self, map_id: u32, load: F) -> Result<Arc<Map>>
    where
        F: FnOnce() -> Result<Map>,
    {
        if let Some(map) = self.get(map_id) {
            return Ok(map);
        }
        Ok(self.insert(load()?))
    }

    /// 插入新 map(或替換同 id 既有)。 超過 MAX_ENTRIES 時 evict 最舊。
    /// 回傳的 Arc 是 cache 內這次 insert 的引用,caller 可直接用。
    pub fn insert(&self, map: Map) -> Arc<Map> {
        let arc = Arc::new(map);
        let mut inner = self.inner.lock().unwrap();
        if let Some(pos) = inner.entries.iter().position(|m| m.map_id == arc.map_id) {
            inner.entries.remove(pos);
        }
        inner.entries.push_front(Arc::clone(&arc));
        if inner.entries.len() > MAX_ENTRIES {
            inner.entries.pop_back();
        }
        arc
    }
}

impl Default for MapCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::minimap::map_loader::Bounds;
    use std::collections::HashMap;

    fn fake_map(id: u32) -> Map {
        let blocks = HashMap::new();
        let bounds = Bounds::empty();
        let nav = crate::minimap::nav_grid::NavGrid::from_blocks(&blocks);
        let profile = crate::minimap::nav_profile::NavProfile::from_blocks(&nav, &blocks);
        Map {
            map_id: id,
            nav,
            profile,
            blocks,
            bounds,
        }
    }

    #[test]
    fn get_miss_returns_none() {
        let cache = MapCache::new();
        assert!(cache.get(42).is_none());
    }

    #[test]
    fn insert_then_get_returns_same() {
        let cache = MapCache::new();
        cache.insert(fake_map(42));
        let got = cache.get(42).expect("hit");
        assert_eq!(got.map_id, 42);
    }

    #[test]
    fn lru_evicts_oldest_after_max() {
        let cache = MapCache::new();
        // 插 11 個,最舊 3 個被 evict
        for i in 0..(MAX_ENTRIES as u32 + 3) {
            cache.insert(fake_map(i));
        }
        for i in 0..3 {
            assert!(cache.get(i).is_none(), "map {i} 應被 evict");
        }
        for i in 3..(MAX_ENTRIES as u32 + 3) {
            assert!(cache.get(i).is_some(), "map {i} 應仍在 cache");
        }
    }

    #[test]
    fn get_bumps_to_front() {
        let cache = MapCache::new();
        // 先插 8 個塞滿(0..7)
        for i in 0..(MAX_ENTRIES as u32) {
            cache.insert(fake_map(i));
        }
        // 拿 id=0 → 變成 newest;此時 id=1 變最舊
        cache.get(0).expect("should hit");
        // 再插一個 → 應 evict id=1
        cache.insert(fake_map(99));
        assert!(cache.get(0).is_some(), "id=0 因為剛 get 過,應仍在");
        assert!(cache.get(1).is_none(), "id=1 應 evict");
        assert!(cache.get(99).is_some(), "id=99 新插的,應在");
    }

    #[test]
    fn insert_same_id_replaces() {
        let cache = MapCache::new();
        cache.insert(fake_map(42));
        cache.insert(fake_map(42));
        // 仍在,但只佔一個 slot
        assert!(cache.get(42).is_some());
        // 塞滿其他 7 個還能裝下,沒 evict id=42
        for i in 0..(MAX_ENTRIES as u32 - 1) {
            cache.insert(fake_map(i));
        }
        assert!(cache.get(42).is_some(), "id=42 不該被當重複 entry 算兩次");
    }

    #[test]
    fn get_or_load_loads_once_then_reuses_cache() {
        let cache = MapCache::new();
        let mut calls = 0u32;

        let first = cache
            .get_or_load(7, || {
                calls += 1;
                Ok(fake_map(7))
            })
            .expect("第一次應載入成功");
        let second = cache
            .get_or_load(7, || {
                calls += 1;
                Ok(fake_map(7))
            })
            .expect("第二次應走 cache");

        assert_eq!(first.map_id, 7);
        assert_eq!(second.map_id, 7);
        assert_eq!(calls, 1, "cache hit 不應重新載入 map");
    }
}
