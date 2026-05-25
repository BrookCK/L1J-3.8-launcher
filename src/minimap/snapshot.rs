//! 每 tick 從遊戲記憶體拍照 — map_id + 玩家 + 場上實體。
//!
//! ## 效能考量
//!
//! `aux::entity_scan::list_all_entities` 每次掃整個 heap + 對每個 entity 做 3 個 name
//! ptr deref + decode,N=30 entity 大概 100-300ms。 minimap tick 跟不上 → 紅點 lag
//! 滿一個 tick。 因此這裡用 **address cache + per-tick 純位置讀**:
//!
//! - 每 1500ms 重新掃 heap(`list_entity_positions`)取得 entity addr list
//! - 中間 tick 只對 cached addr 讀 `vfptr + raw_x + y`(每 entity 3 個 u32 read)
//! - 讀到 vfptr 變了的就丟掉(entity 離場 / slot 被改寫)→ 等下次 refresh 重撈
//!
//! 純位置讀:30 entity × 3 u32 ≈ 60ms,200ms tick 內可從容跑完 + 留 buffer 給 render。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use windows::Win32::Foundation::HANDLE;

use crate::aux::address::G_MAP_ID;
use crate::aux::entity_scan::{self, ScannedEntity};
use crate::bot::perception::classifier::{is_attackable_monster, EntityFacts};
use crate::bot::perception::position::{decode_x, PlayerPosition};
use crate::log_line;
use crate::memory::read_u32;
use crate::sprite_catalog;

#[derive(Debug, Clone, Default)]
pub struct MapSnapshot {
    pub map_id: u32,
    pub player: Option<PlayerPosition>,
    /// 場上 entity — 跟 entity_scan 共用 ScannedEntity 結構,但 minimap 用不到 name /
    /// target_id,都填空值。 entity 種類目前限 player class(vfptr `0x008DC08C`),
    /// 怪物 vfptr 待 RE。
    pub entities: Vec<ScannedEntity>,
}

/// addr cache 重新掃描的最短間隔。 期間中間 tick 只 refresh 位置。
const RESCAN_INTERVAL: Duration = Duration::from_millis(1500);

#[derive(Clone)]
struct CachedEntity {
    addr: u32,
    target_id: u32,
    name: String,
}

struct AddrCache {
    entities: Vec<CachedEntity>,
    last_rescan: Option<Instant>,
}

static CACHE: Lazy<Mutex<AddrCache>> = Lazy::new(|| {
    Mutex::new(AddrCache {
        entities: Vec::new(),
        last_rescan: None,
    })
});

/// 從 game process 讀一個 tick 的 snapshot。
pub fn capture(h: HANDLE) -> MapSnapshot {
    let map_id = read_u32(h, G_MAP_ID).unwrap_or(0);
    let player = PlayerPosition::read(h);
    let entities = capture_entities(h);
    MapSnapshot {
        map_id,
        player,
        entities,
    }
}

fn capture_entities(h: HANDLE) -> Vec<ScannedEntity> {
    let mut cache = CACHE.lock().unwrap();
    let need_rescan = cache
        .last_rescan
        .map(|t| t.elapsed() >= RESCAN_INTERVAL)
        .unwrap_or(true);

    if need_rescan {
        let positions = entity_scan::list_entity_positions(h);
        cache.last_rescan = Some(Instant::now());
        log_entity_dump(h, &positions);

        let entities: Vec<ScannedEntity> = positions
            .into_iter()
            .filter_map(|(addr, target_id, sprite_id, action_state, raw_x, y)| {
                if !is_minimap_attackable_monster(target_id, sprite_id, action_state) {
                    return None;
                }
                let name = entity_scan::read_entity_display_name(h, addr);
                if !is_minimap_confirmed_attackable_monster(
                    target_id,
                    sprite_id,
                    action_state,
                    &name,
                ) {
                    return None;
                }
                Some(ScannedEntity {
                    addr,
                    target_id,
                    name,
                    sprite_id,
                    action_state,
                    raw_x,
                    y,
                })
            })
            .collect();

        cache.entities = entities
            .iter()
            .map(|e| CachedEntity {
                addr: e.addr,
                target_id: e.target_id,
                name: e.name.clone(),
            })
            .collect();
        return entities;
    }

    let mut out = Vec::with_capacity(cache.entities.len());
    let mut still_alive = Vec::with_capacity(cache.entities.len());
    for cached in &cache.entities {
        if let Some((target_id, sprite_id, action_state, raw_x, y)) =
            entity_scan::read_entity_position(h, cached.addr)
        {
            if target_id != cached.target_id {
                continue;
            }
            if !is_minimap_confirmed_attackable_monster(
                target_id,
                sprite_id,
                action_state,
                &cached.name,
            ) {
                continue;
            }
            out.push(ScannedEntity {
                addr: cached.addr,
                target_id,
                name: cached.name.clone(),
                sprite_id,
                action_state,
                raw_x,
                y,
            });
            still_alive.push(cached.clone());
        }
    }
    cache.entities = still_alive;
    out
}

fn is_minimap_attackable_monster(target_id: u32, sprite_id: u16, action_state: u8) -> bool {
    is_minimap_confirmed_attackable_monster(target_id, sprite_id, action_state, "__confirmed__")
}

fn is_minimap_confirmed_attackable_monster(
    target_id: u32,
    sprite_id: u16,
    action_state: u8,
    name: &str,
) -> bool {
    is_attackable_monster(EntityFacts {
        target_id,
        sprite_id,
        action_state,
        name,
    })
}

/// 把 rescan 結果摘要印一次，協助分辨 raw/mob/可攻擊怪數量落差。
fn log_entity_dump(h: HANDLE, positions: &[(u32, u32, u16, u8, u32, u32)]) {
    let raw_n = positions.len();
    let mob_n = positions
        .iter()
        .filter(|&&(_, _, sprite_id, _, _, _)| sprite_catalog::is_mob(sprite_id))
        .count();
    let attackable_mob_n = positions
        .iter()
        .filter(|&&(_, target_id, sprite_id, action_state, _, _)| {
            is_minimap_attackable_monster(target_id, sprite_id, action_state)
        })
        .count();
    if raw_n <= 10 {
        log_line!(
            "[minimap] rescan: raw={raw_n} mob_only={mob_n} attackable_mob={attackable_mob_n}"
        );
        return;
    }
    let player = PlayerPosition::read(h);
    log_line!(
        "[minimap] rescan: raw={raw_n} mob_only={mob_n} attackable_mob={attackable_mob_n} (>10 印明細協助診斷)"
    );
    for (idx, &(addr, target_id, sprite_id, action_state, raw_x, y)) in
        positions.iter().take(20).enumerate()
    {
        let dx = decode_x(raw_x);
        let dy = y as i32;
        let dist = if let Some(p) = &player {
            let ddx = (dx - p.x).abs();
            let ddy = (dy - p.y).abs();
            format!("{ddx}/{ddy} tile")
        } else {
            "?".into()
        };
        let kind: String = match sprite_catalog::sprite_type(sprite_id) {
            Some(sprite_catalog::TYPE_MOB) => "MOB".into(),
            Some(0) => "影子".into(),
            Some(1) => "裝飾".into(),
            Some(5) => "玩家".into(),
            Some(6) => "NPC".into(),
            Some(t) => format!("type{t}"),
            None => "??".into(),
        };
        log_line!(
            "[minimap]   [{idx:02}] addr=0x{addr:08X} sprite={sprite_id} {kind} tid=0x{target_id:08X} action=0x{action_state:02X} pos=({dx},{dy}) dist={dist}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_on_null_handle_returns_defaults() {
        // null HANDLE → 所有讀記憶體都 fail(or 回空)→ snapshot 全 default
        let h = HANDLE(std::ptr::null_mut());
        let snap = capture(h);
        assert_eq!(snap.map_id, 0);
        assert!(snap.player.is_none());
        assert!(snap.entities.is_empty());
    }

    #[test]
    fn minimap_entities_are_attackable_mobs_only() {
        assert!(is_minimap_attackable_monster(0x0BEC_2D4E, 2489, 0x00));
        assert!(!is_minimap_attackable_monster(0x0000_0010, 2489, 0x00));
        assert!(!is_minimap_attackable_monster(0x0BEC_2D4E, 85, 0x00));
    }

    #[test]
    fn minimap_rejects_dead_but_keeps_non_death_monster_state() {
        assert!(is_minimap_attackable_monster(0x0BEC_2D4E, 2489, 0x00));
        assert!(is_minimap_attackable_monster(0x0BEC_2D4E, 2489, 0x09));
        assert!(!is_minimap_attackable_monster(0x0BEC_2D4E, 2489, 0x08));
    }

    #[test]
    fn minimap_rejects_blank_named_mob_slot() {
        assert!(!is_minimap_confirmed_attackable_monster(
            0x0BEC_2D4E,
            2489,
            0x00,
            ""
        ));
        assert!(!is_minimap_confirmed_attackable_monster(
            0x0BEC_2D4E,
            12268,
            0x00,
            "magic doll"
        ));
        assert!(!is_minimap_confirmed_attackable_monster(
            0x0BEC_2D4E,
            12277,
            0x00,
            "magic doll variant"
        ));
        assert!(!is_minimap_confirmed_attackable_monster(
            0x0BEC_2D4F,
            2489,
            0x00,
            "魔法娃娃"
        ));
        assert!(is_minimap_confirmed_attackable_monster(
            0x0BEC_2D4E,
            2489,
            0x00,
            "奇岩盜賊"
        ));
    }
}
