//! Shared entity classification for minimap and bot targeting.

use crate::sprite_catalog;

const MIN_RENDER_MONSTER_OBJECT_ID: u32 = 0x0100_0000;
const ACTION_STATE_DIE: u8 = 0x08;
const NON_TARGETABLE_WORLD_ACTION_STATES: &[u8] = &[0x07];
const HIDDEN_MONSTER_ACTION_STATES: &[u8] = &[0x0B, 0x0D, 0x0E];
const HIDDEN_ACTION_MONSTER_SPRITES: &[u16] = &[145];
const MAX_KNOWN_LIVE_ACTION_STATE: u8 = 0x0F;
const NON_ATTACKABLE_COMPANION_SPRITES: &[u16] = &[
    12267, 12268, 12269, 12270, 12271, 12272, 12273, 12274, 12275, 12276, 12277, 12278, 12279,
    12316,
];
const NON_ATTACKABLE_STATIC_TARGET_KEYWORDS: &[&str] =
    &["木樁", "木人", "訓練假人", "稻草人", "training dummy"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityClass {
    AttackableMonster,
    DeadMonster,
    NonWorldMonsterState,
    DecorationOrShadow,
    LocalOrInvalid,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityClassification {
    pub entity_class: EntityClass,
    pub visible_confidence: u8,
    pub hostile_confidence: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct EntityFacts<'a> {
    pub target_id: u32,
    pub sprite_id: u16,
    pub action_state: u8,
    pub name: &'a str,
}

pub fn classify_entity(facts: EntityFacts<'_>) -> EntityClassification {
    if facts.target_id < MIN_RENDER_MONSTER_OBJECT_ID {
        return classification(EntityClass::LocalOrInvalid, 0, 0);
    }

    if is_non_attackable_companion(facts.sprite_id, facts.name)
        || is_non_attackable_static_target(facts.name)
    {
        return classification(EntityClass::DecorationOrShadow, 100, 0);
    }

    if !sprite_catalog::is_mob(facts.sprite_id) {
        return classification(EntityClass::DecorationOrShadow, 100, 0);
    }

    if facts.action_state == ACTION_STATE_DIE {
        return classification(EntityClass::DeadMonster, 100, 0);
    }

    if is_non_targetable_world_action_state(facts.action_state) {
        return classification(EntityClass::NonWorldMonsterState, 100, 0);
    }

    if is_hidden_action_monster(facts.sprite_id, facts.action_state) {
        return classification(EntityClass::NonWorldMonsterState, 100, 0);
    }

    if facts.action_state > MAX_KNOWN_LIVE_ACTION_STATE {
        return classification(EntityClass::Unknown, 100, 0);
    }

    if facts.name.trim().is_empty() {
        return classification(EntityClass::Unknown, 100, 0);
    }

    classification(EntityClass::AttackableMonster, 100, 100)
}

pub fn is_attackable_monster(facts: EntityFacts<'_>) -> bool {
    classify_entity(facts).entity_class == EntityClass::AttackableMonster
}

fn is_non_attackable_companion(sprite_id: u16, name: &str) -> bool {
    NON_ATTACKABLE_COMPANION_SPRITES.contains(&sprite_id)
        || name.contains("娃娃")
        || name.contains("憡")
        || name.to_ascii_lowercase().contains("magic doll")
}

fn is_non_attackable_static_target(name: &str) -> bool {
    let normalized = name.trim().to_ascii_lowercase();
    NON_ATTACKABLE_STATIC_TARGET_KEYWORDS
        .iter()
        .any(|keyword| normalized.contains(&keyword.to_ascii_lowercase()))
}

fn is_hidden_action_monster(sprite_id: u16, action_state: u8) -> bool {
    HIDDEN_MONSTER_ACTION_STATES.contains(&action_state)
        && HIDDEN_ACTION_MONSTER_SPRITES.contains(&sprite_id)
}

fn is_non_targetable_world_action_state(action_state: u8) -> bool {
    NON_TARGETABLE_WORLD_ACTION_STATES.contains(&action_state)
}

fn classification(
    entity_class: EntityClass,
    visible_confidence: u8,
    hostile_confidence: u8,
) -> EntityClassification {
    EntityClassification {
        entity_class,
        visible_confidence,
        hostile_confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_monster_kind_zero_is_live_attackable() {
        let result = classify_entity(EntityFacts {
            target_id: 0x0BEC_2D4D,
            sprite_id: 5110,
            action_state: 0x00,
            name: "mob",
        });

        assert_eq!(result.entity_class, EntityClass::AttackableMonster);
        assert_eq!(result.hostile_confidence, 100);
    }

    #[test]
    fn monster_kind_one_remains_attackable_while_animating() {
        let result = classify_entity(EntityFacts {
            target_id: 0x0BEC_2D4F,
            sprite_id: 4531,
            action_state: 0x01,
            name: "mob",
        });

        assert_eq!(result.entity_class, EntityClass::AttackableMonster);
        assert_eq!(result.hostile_confidence, 100);
    }

    #[test]
    fn monster_kind_three_remains_attackable_while_animating() {
        let result = classify_entity(EntityFacts {
            target_id: 0x0BEC_2D51,
            sprite_id: 4531,
            action_state: 0x03,
            name: "healing mob",
        });

        assert_eq!(result.entity_class, EntityClass::AttackableMonster);
        assert_eq!(result.hostile_confidence, 100);
    }

    #[test]
    fn magic_doll_mob_sprite_is_not_attackable() {
        let result = classify_entity(EntityFacts {
            target_id: 0x0BEC_2D53,
            sprite_id: 12268,
            action_state: 0x00,
            name: "magic doll",
        });

        assert_eq!(result.entity_class, EntityClass::DecorationOrShadow);
        assert_eq!(result.hostile_confidence, 0);
    }

    #[test]
    fn monster_sprite_with_action_three_is_still_attackable() {
        let result = classify_entity(EntityFacts {
            target_id: 0x0BEC_FF5B,
            sprite_id: 1082,
            action_state: 0x03,
            name: "mob in action",
        });

        assert_eq!(result.entity_class, EntityClass::AttackableMonster);
        assert_eq!(result.hostile_confidence, 100);
    }

    #[test]
    fn flying_or_evasive_action_state_is_non_world_not_attackable() {
        let result = classify_entity(EntityFacts {
            target_id: 0x0BEC_E8C6,
            sprite_id: 1024,
            action_state: 0x07,
            name: "harpy airborne",
        });

        assert_eq!(result.entity_class, EntityClass::NonWorldMonsterState);
        assert_eq!(result.hostile_confidence, 0);
    }

    #[test]
    fn ranged_animation_action_states_remain_attackable_for_normal_mobs() {
        for action_state in [0x0B, 0x0D, 0x0E] {
            let result = classify_entity(EntityFacts {
                target_id: 0x0BEC_FF5C,
                sprite_id: 2489,
                action_state,
                name: "ranged mob",
            });

            assert_eq!(
                result.entity_class,
                EntityClass::AttackableMonster,
                "action_state=0x{action_state:02X} should stay targetable"
            );
            assert_eq!(result.hostile_confidence, 100);
        }
    }

    #[test]
    fn monster_sprite_with_die_action_is_dead_not_attackable() {
        let result = classify_entity(EntityFacts {
            target_id: 0x0BEC_FF24,
            sprite_id: 1098,
            action_state: 0x08,
            name: "mob die",
        });

        assert_eq!(result.entity_class, EntityClass::DeadMonster);
        assert_eq!(result.hostile_confidence, 0);
    }

    #[test]
    fn monster_hide_action_is_non_world_not_attackable() {
        let result = classify_entity(EntityFacts {
            target_id: 0x0BEC_FF25,
            sprite_id: 145,
            action_state: 0x0B,
            name: "spartoi hiding",
        });

        assert_eq!(result.entity_class, EntityClass::NonWorldMonsterState);
        assert_eq!(result.hostile_confidence, 0);
    }

    #[test]
    fn monster_hide_damage_action_is_non_world_not_attackable() {
        let result = classify_entity(EntityFacts {
            target_id: 0x0BEC_FF26,
            sprite_id: 145,
            action_state: 0x0D,
            name: "spartoi hidden damage",
        });

        assert_eq!(result.entity_class, EntityClass::NonWorldMonsterState);
        assert_eq!(result.hostile_confidence, 0);
    }

    #[test]
    fn monster_hide_breath_action_is_non_world_not_attackable() {
        let result = classify_entity(EntityFacts {
            target_id: 0x0BEC_FF27,
            sprite_id: 145,
            action_state: 0x0E,
            name: "spartoi hidden breath",
        });

        assert_eq!(result.entity_class, EntityClass::NonWorldMonsterState);
        assert_eq!(result.hostile_confidence, 0);
    }

    #[test]
    fn magic_doll_sprite_range_is_not_attackable_even_when_catalog_says_mob() {
        let result = classify_entity(EntityFacts {
            target_id: 0x0BEC_2D55,
            sprite_id: 12277,
            action_state: 0x00,
            name: "magic doll variant",
        });

        assert_eq!(result.entity_class, EntityClass::DecorationOrShadow);
        assert_eq!(result.hostile_confidence, 0);
    }

    #[test]
    fn magic_doll_name_is_not_attackable_even_with_mob_sprite() {
        let result = classify_entity(EntityFacts {
            target_id: 0x0BEC_2D54,
            sprite_id: 2489,
            action_state: 0x00,
            name: "魔法娃娃",
        });

        assert_eq!(result.entity_class, EntityClass::DecorationOrShadow);
        assert_eq!(result.hostile_confidence, 0);
    }

    #[test]
    fn training_dummy_names_are_not_attackable_even_with_mob_sprite() {
        for name in ["木樁", "訓練木樁", "木人", "稻草人", "Training Dummy"] {
            let result = classify_entity(EntityFacts {
                target_id: 0x0BEC_2D56,
                sprite_id: 2489,
                action_state: 0x00,
                name,
            });

            assert_eq!(
                result.entity_class,
                EntityClass::DecorationOrShadow,
                "{name} should not be targeted"
            );
            assert_eq!(result.hostile_confidence, 0);
        }
    }
}
