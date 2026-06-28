use anyhow::Result;
use windows::Win32::Foundation::HANDLE;

use crate::bot::action::walk::WalkDriver;
use crate::bot::action::{attack, bot_drink_handle, skill, walk};
use crate::bot::hunt4::model::{EntityView, Snapshot};
use crate::bot::hunt4::step::{Action, LastOutcome};
use crate::bot::perception::position::PlayerPosition;
use crate::bot::scroll_match::teleport_scroll_item_matches;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchIntent {
    Noop,
    Walk {
        target_x: i32,
        target_y: i32,
    },
    BootstrapAttack {
        target_id: u32,
        entity_ptr: u32,
        target_raw_x: u32,
        target_y: u32,
        fresh_target: bool,
    },
    CastSkill {
        skill_name: String,
        target_id: u32,
    },
    UseScroll {
        name_keyword: String,
    },
    AttackLookupFailed {
        target_id: u32,
    },
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DispatchState {
    pub last_bootstrap_target: Option<u32>,
}

pub fn intent_for(action: &Action, snapshot: &Snapshot, state: &DispatchState) -> DispatchIntent {
    match action {
        Action::Wait => DispatchIntent::Noop,
        Action::WalkTo { tile } => DispatchIntent::Walk {
            target_x: tile.0,
            target_y: tile.1,
        },
        Action::AttackTarget {
            target_id,
            entity_ptr,
            skill,
        } => {
            let Some(entity) = find_live_attack_entity(snapshot, *target_id, *entity_ptr) else {
                return DispatchIntent::AttackLookupFailed {
                    target_id: *target_id,
                };
            };

            if let Some(name) = skill.as_ref().filter(|s| !s.is_empty()) {
                return DispatchIntent::CastSkill {
                    skill_name: name.clone(),
                    target_id: *target_id,
                };
            }

            DispatchIntent::BootstrapAttack {
                target_id: *target_id,
                entity_ptr: entity.entity_ptr,
                target_raw_x: entity.raw_x,
                target_y: entity.y,
                fresh_target: state.last_bootstrap_target != Some(*target_id),
            }
        }
        Action::UseTeleportScroll { name_keyword } => DispatchIntent::UseScroll {
            name_keyword: name_keyword.clone(),
        },
    }
}

fn find_live_attack_entity<'a>(
    snapshot: &'a Snapshot,
    target_id: u32,
    entity_ptr: u32,
) -> Option<&'a EntityView> {
    snapshot
        .entities
        .iter()
        .find(|entity| {
            entity.target_id == target_id
                && entity.entity_ptr == entity_ptr
                && entity.is_live_attackable()
        })
        .or_else(|| {
            snapshot
                .entities
                .iter()
                .find(|entity| entity.target_id == target_id && entity.is_live_attackable())
        })
}

fn release_walk_before_dispatch(intent: &DispatchIntent) {
    if should_release_walk_before_dispatch(intent) {
        let _ = walk::walk_release();
    }
}

pub(crate) fn should_release_walk_before_dispatch(intent: &DispatchIntent) -> bool {
    !matches!(intent, DispatchIntent::Walk { .. })
}

pub fn dispatch(
    h: HANDLE,
    intent: &DispatchIntent,
    player: Option<PlayerPosition>,
    walk_driver: WalkDriver,
    state: &mut DispatchState,
) -> LastOutcome {
    release_walk_before_dispatch(intent);
    match intent {
        DispatchIntent::Noop => LastOutcome::None,
        DispatchIntent::Walk { target_x, target_y } => {
            let Some(player) = player else {
                return LastOutcome::WalkFailed {
                    attempted_tile: (*target_x, *target_y),
                };
            };
            match walk::walk_toward_tile_with_driver(h, walk_driver, player, *target_x, *target_y) {
                Ok(_) => LastOutcome::WalkOk,
                Err(_) => LastOutcome::WalkFailed {
                    attempted_tile: (*target_x, *target_y),
                },
            }
        }
        DispatchIntent::BootstrapAttack {
            target_id,
            entity_ptr,
            target_raw_x,
            target_y,
            fresh_target,
        } => {
            match attack::bootstrap_click_attack(
                h,
                *entity_ptr,
                *target_raw_x,
                *target_y,
                *fresh_target,
            ) {
                Ok(_) => {
                    state.last_bootstrap_target = Some(*target_id);
                    LastOutcome::AttackOk {
                        target_id: *target_id,
                    }
                }
                Err(_) => LastOutcome::AttackFailed {
                    target_id: *target_id,
                },
            }
        }
        DispatchIntent::CastSkill {
            skill_name,
            target_id,
        } => {
            state.last_bootstrap_target = None;
            match skill::cast_damage_skill_at(h, skill_name, *target_id) {
                Ok(_) => LastOutcome::AttackOk {
                    target_id: *target_id,
                },
                Err(_) => LastOutcome::AttackFailed {
                    target_id: *target_id,
                },
            }
        }
        DispatchIntent::UseScroll { name_keyword } => {
            let _ = attack::stop_client_auto_attack(h);
            state.last_bootstrap_target = None;
            match use_scroll_via_inventory(h, name_keyword) {
                Ok(item_name) => {
                    let _ = attack::stop_client_auto_attack(h);
                    crate::log_line!(
                        "[bot/hunt4/teleport] use_scroll packet_sent keyword=\"{}\" item=\"{}\"",
                        name_keyword,
                        item_name
                    );
                    LastOutcome::ScrollOk
                }
                Err(e) => {
                    crate::log_line!(
                        "[bot/hunt4/teleport] use_scroll failed keyword=\"{}\": {e:#}",
                        name_keyword
                    );
                    LastOutcome::ScrollFailed
                }
            }
        }
        DispatchIntent::AttackLookupFailed { target_id } => LastOutcome::AttackFailed {
            target_id: *target_id,
        },
    }
}

fn use_scroll_via_inventory(h: HANDLE, name_keyword: &str) -> Result<String> {
    use crate::aux::inventory;
    use anyhow::{anyhow, bail};

    let items = inventory::list_items(h)?;
    let trimmed = name_keyword.trim();
    if trimmed.is_empty() {
        bail!("teleport scroll keyword is empty");
    }
    let item = items
        .iter()
        .filter(|it| teleport_scroll_item_matches(it, trimmed))
        .min_by_key(|it| teleport_scroll_item_priority(it, trimmed))
        .ok_or_else(|| {
            anyhow!(
                "teleport scroll not found: {trimmed} inventory_sample=[{}]",
                inventory_name_sample(&items)
            )
        })?;
    let name = item.name_lossy();
    bot_drink_handle().execute_teleport_scroll_packet(h, item.item_param)?;
    Ok(name)
}

fn teleport_scroll_item_priority(item: &crate::aux::inventory::Item, keyword: &str) -> u8 {
    if !is_random_teleport_keyword(keyword) {
        return 0;
    }

    let name = item.name_lossy();
    let blessed_or_cursed = name.contains("\u{795D}")
        || name.contains("\u{5492}")
        || name.contains("\u{8A5B}")
        || name.contains("\u{87E1}");
    let normal_name = name.starts_with("\u{77AC}\u{9593}\u{79FB}\u{52D5}")
        || name.starts_with("\u{50B3}\u{9001}")
        || name.starts_with("\u{96A8}\u{6A5F}");

    match (item.item_type, normal_name, blessed_or_cursed) {
        (0x06, true, false) => 0,
        (0x06, _, false) => 1,
        (_, true, false) => 2,
        (0x06, _, true) => 3,
        _ => 4,
    }
}

fn is_random_teleport_keyword(keyword: &str) -> bool {
    keyword.contains("\u{77AC}")
        || keyword.contains("\u{50B3}")
        || keyword.contains("\u{96A8}\u{6A5F}")
        || keyword.to_ascii_lowercase().contains("teleport")
}

fn inventory_name_sample(items: &[crate::aux::inventory::Item]) -> String {
    items
        .iter()
        .take(8)
        .map(|it| it.name_lossy())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use crate::bot::hunt4::model::{EntityView, Snapshot};
    use crate::bot::hunt4::step::Action;
    use crate::bot::perception::classifier::EntityClass;

    use super::{intent_for, DispatchIntent, DispatchState};

    fn entity(target_id: u32, entity_ptr: u32, class: EntityClass) -> EntityView {
        EntityView {
            target_id,
            entity_ptr,
            name: format!("mob_{target_id:X}"),
            sprite_id: 100,
            action_state: 0,
            tile: (100, 100),
            raw_x: 0x8123,
            y: 33000,
            class,
            visible_confidence: 1,
            hostile_confidence: u8::from(class == EntityClass::AttackableMonster),
        }
    }

    #[test]
    fn dispatch_intent_for_walk_keeps_action_tile_without_replanning() {
        let snapshot = Snapshot::default();
        let state = DispatchState::default();
        let action = Action::WalkTo { tile: (105, 99) };

        assert_eq!(
            intent_for(&action, &snapshot, &state),
            DispatchIntent::Walk {
                target_x: 105,
                target_y: 99,
            }
        );
    }

    #[test]
    fn attack_lookup_uses_live_entity_when_duplicate_target_id_has_stale_first_entry() {
        let target_id = 0x0BEB_DEC1;
        let stale_ptr = 0x1111_0000;
        let live_ptr = 0x2222_0000;
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                entity(target_id, stale_ptr, EntityClass::Unknown),
                entity(target_id, live_ptr, EntityClass::AttackableMonster),
            ],
        };
        let state = DispatchState::default();
        let action = Action::AttackTarget {
            target_id,
            entity_ptr: live_ptr,
            skill: None,
        };

        assert_eq!(
            intent_for(&action, &snapshot, &state),
            DispatchIntent::BootstrapAttack {
                target_id,
                entity_ptr: live_ptr,
                target_raw_x: 0x8123,
                target_y: 33000,
                fresh_target: true,
            }
        );
    }
}
