use crate::bot::perception::classifier::EntityClass;

pub const LOCAL_CLEAR_RADIUS_TILES: u32 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityView {
    pub target_id: u32,
    pub entity_ptr: u32,
    pub name: String,
    pub sprite_id: u16,
    pub action_state: u8,
    pub tile: (i32, i32),
    pub raw_x: u32,
    pub y: u32,
    pub class: EntityClass,
    pub visible_confidence: u8,
    pub hostile_confidence: u8,
}

impl EntityView {
    pub fn distance_from(&self, player: (i32, i32)) -> u32 {
        self.tile
            .0
            .abs_diff(player.0)
            .max(self.tile.1.abs_diff(player.1))
    }

    pub fn is_live_attackable(&self) -> bool {
        self.class == EntityClass::AttackableMonster && self.hostile_confidence > 0
    }

    pub fn is_dead(&self) -> bool {
        self.class == EntityClass::DeadMonster || self.action_state == 0x08
    }

    pub fn blocks_movement(&self) -> bool {
        self.target_id != 0 && !self.is_dead() && self.class != EntityClass::DecorationOrShadow
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub player: Option<(i32, i32)>,
    pub entities: Vec<EntityView>,
}

impl Snapshot {
    pub fn valid_targets(&self) -> impl Iterator<Item = &EntityView> {
        self.entities
            .iter()
            .filter(|entity| entity.is_live_attackable())
    }

    #[cfg(test)]
    pub fn find(&self, target_id: u32) -> Option<&EntityView> {
        self.entities
            .iter()
            .find(|entity| entity.target_id == target_id)
    }
}
