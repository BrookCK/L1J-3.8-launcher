use std::cmp::Ordering;

use crate::bot::decide::hunt::MELEE_RANGE_TILES;
use crate::bot::hunt4::model::{EntityView, Snapshot, LOCAL_CLEAR_RADIUS_TILES};
use crate::bot::hunt4::step::TargetCandidate;
use crate::bot::perception::classifier::EntityClass;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetViability {
    Viable,
    Dead,
    NonWorldMonsterState,
    HiddenOrBurrowed,
    DecorationOrShadow,
    LocalOrInvalid,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetReject {
    pub target_id: u32,
    pub entity_ptr: u32,
    pub name: String,
    pub tile: (i32, i32),
    pub class: EntityClass,
    pub action_state: u8,
    pub viability: TargetViability,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TargetSelectionFrame {
    pub candidates: Vec<TargetSelection>,
    pub rejected: Vec<TargetReject>,
    pub selected: Option<TargetSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSelection {
    pub target_id: u32,
    pub entity_ptr: u32,
    pub name: String,
    pub tile: (i32, i32),
    pub distance: u32,
    pub in_attack_range: bool,
    pub reachable: bool,
    pub is_attacker: bool,
    pub approach_steps: Option<usize>,
    pub approach_next_tile: Option<(i32, i32)>,
    pub viability: TargetViability,
    pub reason: String,
}

impl From<&TargetCandidate> for TargetSelection {
    fn from(candidate: &TargetCandidate) -> Self {
        let approach_steps = candidate.reachable_path.as_ref().map(Vec::len);
        let approach_next_tile = candidate
            .reachable_path
            .as_ref()
            .and_then(|path| path.first().copied());
        Self {
            target_id: candidate.target_id,
            entity_ptr: candidate.entity_ptr,
            name: candidate.name.clone(),
            tile: candidate.tile,
            distance: candidate.distance,
            in_attack_range: candidate.in_attack_range,
            reachable: candidate.reachable_path.is_some(),
            is_attacker: candidate.is_attacker,
            approach_steps,
            approach_next_tile,
            viability: TargetViability::Viable,
            reason: selection_reason(candidate),
        }
    }
}

fn selection_reason(candidate: &TargetCandidate) -> String {
    let reach = if candidate.reachable_path.is_some() {
        "reachable"
    } else {
        "unreachable"
    };
    let range = if candidate.in_attack_range {
        "in_range"
    } else {
        "approach"
    };
    let pressure = if candidate.is_attacker {
        "attacker"
    } else {
        "neutral"
    };
    format!("{reach} {range} {pressure} distance={}", candidate.distance)
}

pub fn viability_for_entity(entity: &EntityView) -> TargetViability {
    match entity.class {
        EntityClass::AttackableMonster if entity.hostile_confidence > 0 => TargetViability::Viable,
        EntityClass::AttackableMonster => TargetViability::Unknown,
        EntityClass::DeadMonster => TargetViability::Dead,
        EntityClass::NonWorldMonsterState if matches!(entity.action_state, 0x0B | 0x0D | 0x0E) => {
            TargetViability::HiddenOrBurrowed
        }
        EntityClass::NonWorldMonsterState => TargetViability::NonWorldMonsterState,
        EntityClass::DecorationOrShadow => TargetViability::DecorationOrShadow,
        EntityClass::LocalOrInvalid => TargetViability::LocalOrInvalid,
        EntityClass::Unknown => TargetViability::Unknown,
    }
}

pub fn candidate_allowed(entity: &EntityView) -> bool {
    viability_for_entity(entity) == TargetViability::Viable
}

pub fn sort_candidates(candidates: &mut [TargetCandidate]) {
    candidates.sort_by(compare_candidates);
}

pub fn compare_candidates(a: &TargetCandidate, b: &TargetCandidate) -> Ordering {
    priority_band(a)
        .cmp(&priority_band(b))
        .then_with(|| {
            if is_local(a) && is_local(b) {
                a.distance
                    .cmp(&b.distance)
                    .then_with(|| approach_cost(a).cmp(&approach_cost(b)))
            } else {
                approach_cost(a)
                    .cmp(&approach_cost(b))
                    .then_with(|| a.distance.cmp(&b.distance))
            }
        })
        .then_with(|| b.is_attacker.cmp(&a.is_attacker))
        .then_with(|| b.in_attack_range.cmp(&a.in_attack_range))
        .then_with(|| a.target_id.cmp(&b.target_id))
}

fn approach_cost(candidate: &TargetCandidate) -> usize {
    if candidate.in_attack_range {
        return 0;
    }
    candidate
        .reachable_path
        .as_ref()
        .map(Vec::len)
        .unwrap_or(usize::MAX)
}

fn priority_band(candidate: &TargetCandidate) -> u8 {
    if candidate.is_attacker && candidate.entity_ptr != 0 && candidate.distance <= MELEE_RANGE_TILES
    {
        return 0;
    }
    if candidate.in_attack_range && candidate.reachable_path.is_some() {
        return 1;
    }
    if candidate.reachable_path.is_some()
        && candidate.is_attacker
        && candidate.distance <= LOCAL_CLEAR_RADIUS_TILES
    {
        return 2;
    }
    if candidate.reachable_path.is_some() && candidate.distance <= LOCAL_CLEAR_RADIUS_TILES {
        return 3;
    }
    if candidate.reachable_path.is_some() && candidate.is_attacker {
        return 4;
    }
    if candidate.reachable_path.is_some() {
        return 5;
    }
    if candidate.is_attacker {
        return 6;
    }
    7
}

fn is_local(candidate: &TargetCandidate) -> bool {
    candidate.distance <= LOCAL_CLEAR_RADIUS_TILES
}

#[cfg(test)]
pub fn locked_target_should_abandon(entity: &EntityView) -> bool {
    matches!(
        viability_for_entity(entity),
        TargetViability::NonWorldMonsterState
            | TargetViability::HiddenOrBurrowed
            | TargetViability::DecorationOrShadow
            | TargetViability::LocalOrInvalid
            | TargetViability::Unknown
    )
}

pub fn rejected_targets(snapshot: &Snapshot) -> Vec<TargetReject> {
    snapshot
        .entities
        .iter()
        .filter_map(|entity| {
            let viability = viability_for_entity(entity);
            if viability == TargetViability::Viable {
                return None;
            }
            Some(TargetReject {
                target_id: entity.target_id,
                entity_ptr: entity.entity_ptr,
                name: entity.name.clone(),
                tile: entity.tile,
                class: entity.class,
                action_state: entity.action_state,
                viability,
                reason: format!("{viability:?} action_state=0x{:02X}", entity.action_state),
            })
        })
        .collect()
}

pub fn select_target(candidates: &[TargetCandidate], snapshot: &Snapshot) -> TargetSelectionFrame {
    let candidates: Vec<TargetSelection> = candidates.iter().map(TargetSelection::from).collect();
    let selected = candidates.first().cloned();
    TargetSelectionFrame {
        candidates,
        rejected: rejected_targets(snapshot),
        selected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::hunt4::step::TargetCandidate;

    fn entity(class: EntityClass, action_state: u8, hostile_confidence: u8) -> EntityView {
        EntityView {
            target_id: 0x0100_0001,
            entity_ptr: 0x2000,
            name: "mob".to_string(),
            sprite_id: 145,
            action_state,
            tile: (100, 100),
            raw_x: 100,
            y: 100,
            class,
            visible_confidence: 100,
            hostile_confidence,
        }
    }

    #[test]
    fn hidden_action_states_are_not_candidate_allowed() {
        for action_state in [0x0B, 0x0D, 0x0E] {
            let entity = entity(EntityClass::NonWorldMonsterState, action_state, 0);

            assert_eq!(
                viability_for_entity(&entity),
                TargetViability::HiddenOrBurrowed
            );
            assert!(!candidate_allowed(&entity));
            assert!(locked_target_should_abandon(&entity));
        }
    }

    fn candidate(
        target_id: u32,
        distance: u32,
        in_attack_range: bool,
        reachable_path: Option<Vec<(i32, i32)>>,
        is_attacker: bool,
    ) -> TargetCandidate {
        TargetCandidate {
            target_id,
            entity_ptr: target_id + 0x1000,
            name: format!("mob_{target_id:X}"),
            tile: (target_id as i32, target_id as i32 + 1),
            distance,
            in_attack_range,
            reachable_path,
            is_attacker,
        }
    }

    #[test]
    fn sort_candidates_prefers_in_range_local_target_over_far_attacker() {
        let mut candidates = vec![
            candidate(
                0x0100_0200,
                10,
                false,
                Some(vec![(101, 100), (102, 100)]),
                true,
            ),
            candidate(0x0100_0201, 1, true, Some(Vec::new()), false),
        ];

        sort_candidates(&mut candidates);

        assert_eq!(
            candidates[0].target_id, 0x0100_0201,
            "radar target selection should clear a local attackable monster before chasing a far attacker"
        );
    }

    #[test]
    fn sort_candidates_prefers_adjacent_attacker_even_when_path_is_unreachable() {
        let mut candidates = vec![
            candidate(
                0x0100_0200,
                3,
                false,
                Some(vec![(101, 100), (102, 100)]),
                false,
            ),
            candidate(0x0100_0201, 1, false, None, true),
        ];

        sort_candidates(&mut candidates);

        assert_eq!(
            candidates[0].target_id, 0x0100_0201,
            "a melee attacker already on top of the player should be the first counterattack target even if A* cannot route to it"
        );
    }

    #[test]
    fn select_target_preserves_ranked_candidates_and_records_rejects() {
        let selected_id = 0x0100_0200;
        let candidates = vec![
            candidate(
                selected_id,
                4,
                false,
                Some(vec![(101, 100), (102, 100)]),
                true,
            ),
            candidate(0x0100_0201, 1, true, Some(Vec::new()), false),
        ];
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                EntityView {
                    target_id: selected_id,
                    entity_ptr: selected_id + 0x1000,
                    name: "ranked".to_string(),
                    sprite_id: 2489,
                    action_state: 0,
                    tile: (104, 100),
                    raw_x: 104,
                    y: 100,
                    class: EntityClass::AttackableMonster,
                    visible_confidence: 100,
                    hostile_confidence: 100,
                },
                entity(EntityClass::NonWorldMonsterState, 0x0D, 0),
            ],
        };

        let frame = select_target(&candidates, &snapshot);

        assert_eq!(
            frame.selected.as_ref().map(|target| target.target_id),
            Some(selected_id)
        );
        assert_eq!(frame.candidates.len(), 2);
        assert_eq!(frame.candidates[0].target_id, selected_id);
        assert_eq!(frame.candidates[0].approach_steps, Some(2));
        assert_eq!(frame.candidates[0].approach_next_tile, Some((101, 100)));
        assert!(frame.candidates[0].reason.contains("attacker"));
        assert!(frame
            .rejected
            .iter()
            .any(|reject| reject.viability == TargetViability::HiddenOrBurrowed));
    }
}
