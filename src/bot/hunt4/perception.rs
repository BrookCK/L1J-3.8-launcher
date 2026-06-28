use crate::bot::decide::hunt::HuntConfig;
use crate::bot::perception::classifier::{classify_entity, EntityFacts};
use crate::bot::perception::position::{decode_x, PlayerPosition};
use crate::bot::perception::world::WorldView;

use super::model::{EntityView, Snapshot};

pub fn snapshot_from_world(world: &WorldView, player: Option<PlayerPosition>) -> Snapshot {
    Snapshot {
        player: player.map(|p| (p.x, p.y)),
        entities: world.entities().iter().map(entity_from_scanned).collect(),
    }
}

pub fn filter_snapshot(snapshot: &Snapshot, cfg: &HuntConfig) -> Snapshot {
    let blacklist: Vec<String> = cfg
        .monster_blacklist
        .iter()
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect();
    let Some(player) = snapshot.player else {
        return Snapshot::default();
    };

    // 探索範圍(2026-05-17):0 = 無限制(原行為);>0 = 只挑距離 ≤ N(Chebyshev)的活怪。
    // 死怪不受 range 限制 — 死怪只給 lock cleanup 用,過濾掉反而會卡 lock。
    let max_range = cfg.hunt_range_tiles as i32;
    let entities = snapshot
        .entities
        .iter()
        .filter(|entity| {
            if entity.is_dead() {
                return true;
            }
            if entity.tile == player {
                return false;
            }
            let in_range = max_range <= 0 || {
                let dx = (entity.tile.0 - player.0).abs();
                let dy = (entity.tile.1 - player.1).abs();
                dx.max(dy) <= max_range
            };
            if !in_range {
                return false;
            }
            if !entity.is_live_attackable() {
                return entity.blocks_movement();
            }
            !blacklist
                .iter()
                .any(|prefix| entity.name.starts_with(prefix))
        })
        .cloned()
        .collect();

    Snapshot {
        player: Some(player),
        entities,
    }
}

fn entity_from_scanned(entity: &crate::aux::entity_scan::ScannedEntity) -> EntityView {
    let class = classify_entity(EntityFacts {
        target_id: entity.target_id,
        sprite_id: entity.sprite_id,
        action_state: entity.action_state,
        name: &entity.name,
    });

    EntityView {
        target_id: entity.target_id,
        entity_ptr: entity.addr,
        name: entity.name.clone(),
        sprite_id: entity.sprite_id,
        action_state: entity.action_state,
        tile: (decode_x(entity.raw_x), entity.y as i32),
        raw_x: entity.raw_x,
        y: entity.y,
        class: class.entity_class,
        visible_confidence: class.visible_confidence,
        hostile_confidence: class.hostile_confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::perception::classifier::EntityClass;

    fn entity(target_id: u32, name: &str, class: EntityClass, action_state: u8) -> EntityView {
        EntityView {
            target_id,
            entity_ptr: target_id,
            name: name.to_string(),
            sprite_id: 1,
            action_state,
            tile: (1, 1),
            raw_x: 1,
            y: 1,
            class,
            visible_confidence: 100,
            hostile_confidence: if class == EntityClass::AttackableMonster {
                100
            } else {
                0
            },
        }
    }

    #[test]
    fn filter_keeps_dead_monsters_for_lock_cleanup() {
        let snapshot = Snapshot {
            player: Some((0, 0)),
            entities: vec![
                entity(7, "mob-7", EntityClass::DeadMonster, 0x08),
                entity(8, "mob-8", EntityClass::AttackableMonster, 0x00),
            ],
        };
        let cfg = HuntConfig {
            hunt_range_tiles: 10,
            ..HuntConfig::default()
        };

        let filtered = filter_snapshot(&snapshot, &cfg);

        assert!(filtered.find(7).is_some_and(|entity| entity.is_dead()));
        assert_eq!(filtered.valid_targets().count(), 1);
    }

    #[test]
    fn filter_snapshot_drops_live_target_outside_configured_hunt_range() {
        let snapshot = Snapshot {
            player: Some((0, 0)),
            entities: vec![entity(7, "mob-7", EntityClass::AttackableMonster, 0x00)],
        };
        let cfg = HuntConfig {
            hunt_range_tiles: 3,
            ..HuntConfig::default()
        };
        let mut far = snapshot.clone();
        far.entities[0].tile = (40, 0);

        assert!(
            filter_snapshot(&far, &cfg).valid_targets().next().is_none(),
            "範圍外活怪應被排除"
        );
    }

    #[test]
    fn filter_snapshot_keeps_live_target_within_configured_hunt_range() {
        let snapshot = Snapshot {
            player: Some((0, 0)),
            entities: vec![entity(7, "mob-7", EntityClass::AttackableMonster, 0x00)],
        };
        let cfg = HuntConfig {
            hunt_range_tiles: 5,
            ..HuntConfig::default()
        };
        // entity factory 預設 tile=(1,1),距離 1 < range 5
        assert_eq!(filter_snapshot(&snapshot, &cfg).valid_targets().count(), 1);
    }

    #[test]
    fn filter_snapshot_uses_chebyshev_distance_for_range() {
        // 對角 (3, 3) Chebyshev = 3,range 3 應該剛好包含
        let mut snapshot = Snapshot {
            player: Some((0, 0)),
            entities: vec![entity(7, "mob-7", EntityClass::AttackableMonster, 0x00)],
        };
        snapshot.entities[0].tile = (3, 3);
        let cfg = HuntConfig {
            hunt_range_tiles: 3,
            ..HuntConfig::default()
        };
        assert_eq!(
            filter_snapshot(&snapshot, &cfg).valid_targets().count(),
            1,
            "Chebyshev 距離 3 應該等於 range 3,不該排除"
        );

        // (4, 0) Chebyshev = 4 > 3,應該排除
        snapshot.entities[0].tile = (4, 0);
        assert_eq!(filter_snapshot(&snapshot, &cfg).valid_targets().count(), 0);
    }

    #[test]
    fn filter_snapshot_zero_hunt_range_means_unlimited() {
        let mut snapshot = Snapshot {
            player: Some((0, 0)),
            entities: vec![entity(7, "mob-7", EntityClass::AttackableMonster, 0x00)],
        };
        snapshot.entities[0].tile = (9999, 9999);
        let cfg = HuntConfig {
            hunt_range_tiles: 0,
            ..HuntConfig::default()
        };
        assert_eq!(
            filter_snapshot(&snapshot, &cfg).valid_targets().count(),
            1,
            "hunt_range_tiles=0 保持向下相容:無距離限制"
        );
    }

    #[test]
    fn filter_snapshot_keeps_far_dead_monster_for_lock_cleanup() {
        // 死怪不受 range 限制 — lock cleanup 路徑需要 (即使遠處死掉也要能 cleanup lock)
        let mut snapshot = Snapshot {
            player: Some((0, 0)),
            entities: vec![entity(7, "mob-7", EntityClass::DeadMonster, 0x08)],
        };
        snapshot.entities[0].tile = (100, 100);
        let cfg = HuntConfig {
            hunt_range_tiles: 5,
            ..HuntConfig::default()
        };
        assert!(filter_snapshot(&snapshot, &cfg)
            .find(7)
            .is_some_and(|e| e.is_dead()));
    }

    #[test]
    fn filter_snapshot_keeps_non_target_character_for_collision() {
        let mut character = entity(0x20, "other-player", EntityClass::LocalOrInvalid, 0x00);
        character.tile = (2, 0);
        let snapshot = Snapshot {
            player: Some((0, 0)),
            entities: vec![character],
        };
        let cfg = HuntConfig {
            hunt_range_tiles: 5,
            ..HuntConfig::default()
        };

        let filtered = filter_snapshot(&snapshot, &cfg);

        assert!(
            filtered.find(0x20).is_some(),
            "non-target character must stay in snapshot so planner can treat the tile as occupied"
        );
        assert_eq!(filtered.valid_targets().count(), 0);
    }

    #[test]
    fn filter_snapshot_drops_shadow_collision_noise() {
        let mut shadow = entity(0x30, "shadow", EntityClass::DecorationOrShadow, 0x00);
        shadow.tile = (2, 0);
        let snapshot = Snapshot {
            player: Some((0, 0)),
            entities: vec![shadow],
        };
        let cfg = HuntConfig {
            hunt_range_tiles: 5,
            ..HuntConfig::default()
        };

        let filtered = filter_snapshot(&snapshot, &cfg);

        assert!(
            filtered.entities.is_empty(),
            "decorations/shadows should not become movement collision blockers"
        );
    }
}
