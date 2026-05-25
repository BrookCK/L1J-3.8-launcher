//! 觀察層 — bot 讀記憶體的入口,**全部走既有 `aux::*` 的 read-only 函數**。
//!
//! ## 為什麼包一層
//!
//! 既有 `aux::player_state` / `aux::entity_scan` / `aux::inventory` 是 helper 用的
//! 通用查詢介面,API 偏 raw。 bot 需要的常用判斷(「玩家還活著嗎」「背包剩幾格」
//! 「我設定的怪物名字在場上嗎」)是這些 raw 查詢的特定組合,在 bot 自己這層包一次:
//!
//! 1. **語意化** — 把 `state.hp > 0` 變成 `player.alive()`,可讀性提升
//! 2. **快取點** — 一個 tick 內多次查 HP%不需要重複 ReadProcessMemory
//! 3. **隔離** — bot 升級時不會回 aux/* 改 API
//!
//! ## Phase 進度
//!
//! - **player.rs**: HP/MP/alive(完整)
//! - **inventory.rs**: 物品數量 + 名稱查詢(Phase 1 用得到的)
//! - **world.rs**: entity 列舉(Phase 1 stub,**怪物 vfptr 待 RE**)

pub mod classifier;
#[cfg(test)]
pub mod inventory;
pub mod player;
pub mod position;
pub mod world;

#[cfg(test)]
mod classifier_contract_tests {
    use super::classifier::{classify_entity, EntityClass, EntityFacts};

    #[test]
    fn live_world_mob_is_attackable_with_full_confidence() {
        let class = classify_entity(EntityFacts {
            target_id: 0x0BEC_2D4E,
            sprite_id: 2489,
            action_state: 0x00,
            name: "奇岩盜賊",
        });

        assert_eq!(class.entity_class, EntityClass::AttackableMonster);
        assert_eq!(class.visible_confidence, 100);
        assert_eq!(class.hostile_confidence, 100);
    }

    #[test]
    fn live_action_kind_mob_remains_attackable() {
        let class = classify_entity(EntityFacts {
            target_id: 0x0BEC_2D4E,
            sprite_id: 2489,
            action_state: 0x01,
            name: "奇岩盜賊#97062:2489",
        });

        assert_eq!(class.entity_class, EntityClass::AttackableMonster);
        assert_eq!(class.visible_confidence, 100);
        assert_eq!(class.hostile_confidence, 100);
    }

    #[test]
    fn blank_named_mob_is_unconfirmed_not_attackable() {
        let class = classify_entity(EntityFacts {
            target_id: 0x0BEC_2D4E,
            sprite_id: 2489,
            action_state: 0x00,
            name: "",
        });

        assert_ne!(class.entity_class, EntityClass::AttackableMonster);
        assert_eq!(class.hostile_confidence, 0);
    }

    #[test]
    fn unknown_entity_kind_mob_is_not_attackable() {
        let class = classify_entity(EntityFacts {
            target_id: 0x0BEC_2D4E,
            sprite_id: 2489,
            action_state: 0x7F,
            name: "奇岩盜賊",
        });

        assert_ne!(class.entity_class, EntityClass::AttackableMonster);
        assert_eq!(class.hostile_confidence, 0);
    }

    #[test]
    fn shadow_sprite_is_not_attackable_even_with_high_target_id() {
        let class = classify_entity(EntityFacts {
            target_id: 0x0BEC_2D4E,
            sprite_id: 85,
            action_state: 0x00,
            name: "#81177:85",
        });

        assert_ne!(class.entity_class, EntityClass::AttackableMonster);
        assert_eq!(class.hostile_confidence, 0);
    }
}
