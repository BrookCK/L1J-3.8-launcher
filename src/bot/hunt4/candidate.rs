use std::collections::HashSet;
use std::time::Instant;

use crate::bot::decide::pathfind::Walkable;
use crate::bot::hunt4::memory::TacticalMemory;
use crate::bot::hunt4::model::Snapshot;
use crate::bot::hunt4::planner;
use crate::bot::hunt4::step::TargetCandidate;
use crate::bot::hunt4::targeting::{self, TargetSelectionFrame};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFrame {
    pub now: Instant,
    pub player_tile: (i32, i32),
    pub attack_range: u32,
    pub recent_attacker_count: usize,
    pub candidates: Vec<TargetCandidate>,
    pub target_selection: TargetSelectionFrame,
}

pub struct CandidateFrameInput<'a, G: Walkable> {
    pub snapshot: &'a Snapshot,
    pub player_tile: (i32, i32),
    pub attack_range: u32,
    pub grid: Option<&'a G>,
    pub memory: &'a TacticalMemory,
    pub recent_attackers: &'a HashSet<u32>,
    pub now: Instant,
}

pub fn build_candidate_frame<G: Walkable>(input: CandidateFrameInput<'_, G>) -> CandidateFrame {
    let candidates = planner::build_candidates(
        input.snapshot,
        input.player_tile,
        input.attack_range,
        input.grid,
        input.memory,
        input.recent_attackers,
        input.now,
    );

    let target_selection = targeting::select_target(&candidates, input.snapshot);

    CandidateFrame {
        now: input.now,
        player_tile: input.player_tile,
        attack_range: input.attack_range,
        recent_attacker_count: input.recent_attackers.len(),
        candidates,
        target_selection,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Instant;

    use crate::bot::decide::pathfind::Walkable;
    use crate::bot::hunt4::memory::TacticalMemory;
    use crate::bot::hunt4::model::{EntityView, Snapshot};
    use crate::bot::perception::classifier::EntityClass;

    struct OpenGrid;

    impl Walkable for OpenGrid {
        fn is_walkable(&self, _tile_x: i32, _tile_y: i32) -> bool {
            true
        }
    }

    fn live_mob(target_id: u32, tile: (i32, i32)) -> EntityView {
        EntityView {
            target_id,
            entity_ptr: target_id + 0x1000,
            name: format!("mob_{target_id:X}"),
            sprite_id: 2489,
            action_state: 0,
            tile,
            raw_x: tile.0 as u32,
            y: tile.1 as u32,
            class: EntityClass::AttackableMonster,
            visible_confidence: 100,
            hostile_confidence: 100,
        }
    }

    fn hidden_mob(target_id: u32, tile: (i32, i32)) -> EntityView {
        EntityView {
            target_id,
            entity_ptr: target_id + 0x1000,
            name: format!("hidden_{target_id:X}"),
            sprite_id: 2489,
            action_state: 0x0D,
            tile,
            raw_x: tile.0 as u32,
            y: tile.1 as u32,
            class: EntityClass::NonWorldMonsterState,
            visible_confidence: 100,
            hostile_confidence: 0,
        }
    }

    #[test]
    fn build_candidate_frame_records_attacker_context() {
        let now = Instant::now();
        let snapshot = Snapshot {
            player: Some((100, 100)),
            entities: vec![
                live_mob(0x0100_0001, (101, 100)),
                live_mob(0x0100_0002, (104, 100)),
                hidden_mob(0x0100_0003, (102, 100)),
            ],
        };
        let memory = TacticalMemory::default();
        let grid = OpenGrid;
        let recent_attackers = HashSet::from([0x0100_0002]);

        let frame = super::build_candidate_frame(super::CandidateFrameInput {
            snapshot: &snapshot,
            player_tile: (100, 100),
            attack_range: 1,
            grid: Some(&grid),
            memory: &memory,
            recent_attackers: &recent_attackers,
            now,
        });

        assert_eq!(frame.now, now);
        assert_eq!(frame.player_tile, (100, 100));
        assert_eq!(frame.attack_range, 1);
        assert_eq!(frame.recent_attacker_count, 1);
        assert_eq!(frame.candidates.len(), 2);
        assert_eq!(frame.candidates[0].target_id, 0x0100_0001);
        assert!(frame
            .candidates
            .iter()
            .any(|candidate| candidate.target_id == 0x0100_0002 && candidate.is_attacker));
        assert_eq!(
            frame
                .target_selection
                .selected
                .as_ref()
                .map(|target| target.target_id),
            Some(0x0100_0001)
        );
        assert!(frame
            .target_selection
            .rejected
            .iter()
            .any(|reject| reject.target_id == 0x0100_0003));
    }
}
