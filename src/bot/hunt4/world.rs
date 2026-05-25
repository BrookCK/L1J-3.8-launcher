//! Hunt4 world frame??//!
//! ?謕??????????tick ???秋播?????????????膩??game state?蹍仟p??//! player?蹍矣sition?蹍占mory entity scan ?改???filtered snapshot?蹇???V4 policy
//! ??血???嗽謕?frame,?????? runtime ????∵??鈭? aux / memory helper??
use std::time::Instant;

use windows::Win32::Foundation::HANDLE;

use crate::aux::address::{G_GAME_STATE, G_MAP_ID};
use crate::bot::decide::hunt::HuntConfig;
use crate::bot::hunt4::model::Snapshot;
use crate::bot::hunt4::perception::{filter_snapshot, snapshot_from_world};
use crate::bot::perception::player::PlayerView;
use crate::bot::perception::position::{decode_x, PlayerPosition};
use crate::bot::perception::world::WorldView;
use crate::memory::read_u32;

const MIN_RENDER_OBJECT_ID: u32 = 0x0100_0000;

#[derive(Debug, Clone)]
pub struct WorldFrame {
    pub now: Instant,
    pub in_game: bool,
    pub map_id: Option<u32>,
    pub player_view: Option<PlayerView>,
    pub player_pos_data: Option<PlayerPosition>,
    pub snapshot: Snapshot,
}

pub struct WorldFrameInput<'a> {
    pub now: Instant,
    pub in_game: bool,
    pub map_id: Option<u32>,
    pub player_view: Option<PlayerView>,
    pub player_pos_data: Option<PlayerPosition>,
    pub world: Option<&'a WorldView>,
    pub cfg: &'a HuntConfig,
}

impl WorldFrame {
    pub fn player_tile(&self) -> Option<(i32, i32)> {
        self.player_pos_data.map(|pos| (pos.x, pos.y))
    }

    pub fn player_alive(&self) -> bool {
        self.in_game
            && self
                .player_view
                .as_ref()
                .map(|player| player.alive())
                .unwrap_or(false)
    }

    pub fn cur_hp(&self) -> Option<u32> {
        self.player_view.as_ref().map(|player| player.raw.hp)
    }

    pub fn cur_max_hp(&self) -> u32 {
        self.player_view
            .as_ref()
            .map(|player| player.raw.max_hp)
            .unwrap_or(0)
    }

    pub fn weight_pct(&self) -> u8 {
        self.player_view
            .as_ref()
            .map(|player| player.raw.weight)
            .unwrap_or(0)
    }
}

pub fn read_frame(h: HANDLE, cfg: &HuntConfig, now: Instant) -> WorldFrame {
    let game_state = read_u32(h, G_GAME_STATE).unwrap_or(0);
    let in_game = game_state == 3;
    let map_id = if in_game {
        read_u32(h, G_MAP_ID).ok().filter(|&id| id != 0)
    } else {
        None
    };
    let player_view = if in_game {
        PlayerView::read(h).ok()
    } else {
        None
    };
    let player_pos_data = if in_game {
        PlayerPosition::read(h)
    } else {
        None
    };
    let world = if in_game {
        WorldView::read(h).ok()
    } else {
        None
    };

    build_frame(WorldFrameInput {
        now,
        in_game,
        map_id,
        player_view,
        player_pos_data,
        world: world.as_ref(),
        cfg,
    })
}

pub fn build_frame(input: WorldFrameInput<'_>) -> WorldFrame {
    if !input.in_game {
        return WorldFrame {
            now: input.now,
            in_game: false,
            map_id: None,
            player_view: None,
            player_pos_data: None,
            snapshot: Snapshot::default(),
        };
    }

    let player_pos_data = input
        .player_pos_data
        .or_else(|| infer_player_position_from_local_entity(input.world));
    let snapshot = input
        .world
        .map(|world| {
            let raw = snapshot_from_world(world, player_pos_data);
            filter_snapshot(&raw, input.cfg)
        })
        .unwrap_or_default();

    WorldFrame {
        now: input.now,
        in_game: true,
        map_id: input.map_id.filter(|&id| id != 0),
        player_view: input.player_view,
        player_pos_data,
        snapshot,
    }
}

fn infer_player_position_from_local_entity(world: Option<&WorldView>) -> Option<PlayerPosition> {
    world?
        .entities()
        .iter()
        .filter(|entity| entity.target_id != 0 && entity.target_id < MIN_RENDER_OBJECT_ID)
        .filter(|entity| !entity.name.trim().is_empty())
        .min_by_key(|entity| entity.target_id)
        .map(|entity| PlayerPosition {
            x: decode_x(entity.raw_x),
            y: entity.y as i32,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aux::entity_scan::ScannedEntity;
    use crate::aux::player_state::PlayerState;
    use crate::bot::decide::hunt::HuntConfig;
    use crate::bot::perception::player::PlayerView;
    use crate::bot::perception::position::{encode_x, PlayerPosition};
    use crate::bot::perception::world::WorldView;
    use std::time::Instant;

    fn player(hp: u32, max_hp: u32, weight: u8) -> PlayerView {
        PlayerView {
            raw: PlayerState {
                hp,
                max_hp,
                mp: 0,
                max_mp: 0,
                food: 0,
                weight,
                map_id: 0,
            },
        }
    }

    fn entity(target_id: u32, name: &str, x: i32, y: i32) -> ScannedEntity {
        ScannedEntity {
            addr: target_id,
            target_id,
            name: name.to_string(),
            sprite_id: 2489,
            action_state: 0,
            raw_x: encode_x(x),
            y: y as u32,
        }
    }

    #[test]
    fn build_frame_filters_snapshot_and_summarizes_player() {
        let now = Instant::now();
        let cfg = HuntConfig {
            hunt_range_tiles: 5,
            monster_blacklist: vec!["blocked".to_string()],
            ..HuntConfig::default()
        };
        let mut hidden = entity(0x0100_0004, "hidden", 33002, 33000);
        hidden.sprite_id = 145;
        hidden.action_state = 0x0D;
        let world = WorldView::__for_test(vec![
            entity(0x0100_0001, "target", 33003, 33000),
            entity(0x0100_0002, "far", 33020, 33000),
            entity(0x0100_0003, "blocked", 33001, 33000),
            hidden,
        ]);

        let frame = build_frame(WorldFrameInput {
            now,
            in_game: true,
            map_id: Some(4),
            player_view: Some(player(100, 200, 37)),
            player_pos_data: Some(PlayerPosition { x: 33000, y: 33000 }),
            world: Some(&world),
            cfg: &cfg,
        });

        assert_eq!(frame.now, now);
        assert!(frame.in_game);
        assert_eq!(frame.map_id, Some(4));
        assert_eq!(frame.player_tile(), Some((33000, 33000)));
        assert!(frame.player_alive());
        assert_eq!(frame.cur_hp(), Some(100));
        assert_eq!(frame.cur_max_hp(), 200);
        assert_eq!(frame.weight_pct(), 37);
        assert_eq!(
            frame
                .snapshot
                .valid_targets()
                .map(|entity| entity.target_id)
                .collect::<Vec<_>>(),
            vec![0x0100_0001]
        );
    }

    #[test]
    fn build_frame_keeps_normal_mob_ranged_animation_states_targetable() {
        let cfg = HuntConfig {
            hunt_range_tiles: 5,
            ..HuntConfig::default()
        };
        let mut mob = entity(0x0100_0001, "ranged mob", 33003, 33000);
        mob.action_state = 0x0D;
        let world = WorldView::__for_test(vec![mob]);

        let frame = build_frame(WorldFrameInput {
            now: Instant::now(),
            in_game: true,
            map_id: Some(4),
            player_view: Some(player(100, 200, 37)),
            player_pos_data: Some(PlayerPosition { x: 33000, y: 33000 }),
            world: Some(&world),
            cfg: &cfg,
        });

        assert_eq!(
            frame
                .snapshot
                .valid_targets()
                .map(|entity| entity.target_id)
                .collect::<Vec<_>>(),
            vec![0x0100_0001]
        );
    }

    #[test]
    fn build_frame_infers_player_position_from_local_entity_when_pointer_position_missing() {
        let cfg = HuntConfig {
            hunt_range_tiles: 5,
            ..HuntConfig::default()
        };
        let world = WorldView::__for_test(vec![
            entity(0x0000_00BF, "self", 33000, 33000),
            entity(0x0100_0001, "near mob", 33001, 33000),
        ]);

        let frame = build_frame(WorldFrameInput {
            now: Instant::now(),
            in_game: true,
            map_id: Some(4),
            player_view: Some(player(100, 200, 37)),
            player_pos_data: None,
            world: Some(&world),
            cfg: &cfg,
        });

        assert_eq!(frame.player_tile(), Some((33000, 33000)));
        assert_eq!(
            frame
                .snapshot
                .valid_targets()
                .map(|entity| entity.target_id)
                .collect::<Vec<_>>(),
            vec![0x0100_0001]
        );
    }

    #[test]
    fn build_frame_clears_game_dependent_data_when_not_in_game() {
        let cfg = HuntConfig::default();
        let world = WorldView::__for_test(vec![entity(0x0100_0001, "target", 33003, 33000)]);

        let frame = build_frame(WorldFrameInput {
            now: Instant::now(),
            in_game: false,
            map_id: Some(4),
            player_view: Some(player(100, 200, 37)),
            player_pos_data: Some(PlayerPosition { x: 33000, y: 33000 }),
            world: Some(&world),
            cfg: &cfg,
        });

        assert!(!frame.in_game);
        assert_eq!(frame.map_id, None);
        assert_eq!(frame.player_tile(), None);
        assert!(!frame.player_alive());
        assert!(frame.snapshot.entities.is_empty());
    }
}
