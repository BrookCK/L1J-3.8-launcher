//! Precomputed navigation profile for a loaded map.
//!
//! `NavGrid` answers tile-level questions. `NavProfile` adds map-level
//! connectivity so bot logic can avoid treating disconnected dungeon areas as
//! equally reachable exploration space.

use std::collections::{HashMap, HashSet, VecDeque};

use super::coord::{BLOCK_ORIGIN, TILES_PER_BLOCK};
use super::nav_grid::NavGrid;
use super::s32_parser::Block;

const STEPS: [(i32, i32); 8] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

#[derive(Debug, Clone, Default)]
pub struct NavProfile {
    tile_components: HashMap<(i32, i32), u32>,
    components: Vec<NavComponent>,
}

#[derive(Debug, Clone)]
pub struct NavComponent {
    id: u32,
    tiles: Vec<(i32, i32)>,
}

impl NavProfile {
    pub fn from_blocks(nav: &NavGrid, blocks: &HashMap<(i32, i32), Block>) -> Self {
        if blocks.is_empty() {
            return Self::default();
        }

        let mut unassigned = HashSet::new();
        let mut block_coords: Vec<(i32, i32)> = blocks.keys().copied().collect();
        block_coords.sort_unstable();
        for (bx, by) in block_coords {
            let Some(block) = blocks.get(&(bx, by)) else {
                continue;
            };
            let base_x = BLOCK_ORIGIN + bx * TILES_PER_BLOCK;
            let base_y = BLOCK_ORIGIN + by * TILES_PER_BLOCK;
            for ly in 0..TILES_PER_BLOCK as usize {
                for lx in 0..TILES_PER_BLOCK as usize {
                    if block.walkable[ly][lx] {
                        unassigned.insert((base_x + lx as i32, base_y + ly as i32));
                    }
                }
            }
        }

        Self::from_walkable_tiles(nav, unassigned)
    }

    fn from_walkable_tiles(nav: &NavGrid, mut unassigned: HashSet<(i32, i32)>) -> Self {
        let mut profile = Self::default();
        while let Some(&start) = unassigned.iter().next() {
            let id = profile.components.len() as u32;
            let mut queue = VecDeque::from([start]);
            let mut tiles = Vec::new();
            unassigned.remove(&start);

            while let Some(tile) = queue.pop_front() {
                profile.tile_components.insert(tile, id);
                tiles.push(tile);

                for (dx, dy) in STEPS {
                    let next = (tile.0 + dx, tile.1 + dy);
                    if unassigned.contains(&next) && nav.can_step(tile, next) {
                        unassigned.remove(&next);
                        queue.push_back(next);
                    }
                }
            }

            profile.components.push(NavComponent { id, tiles });
        }

        profile
    }

    #[cfg(test)]
    pub fn component_count(&self) -> usize {
        self.components.len()
    }

    pub fn component_id(&self, tile: (i32, i32)) -> Option<u32> {
        self.tile_components.get(&tile).copied()
    }

    #[cfg(test)]
    pub fn same_component(&self, a: (i32, i32), b: (i32, i32)) -> bool {
        self.component_id(a)
            .zip(self.component_id(b))
            .is_some_and(|(a, b)| a == b)
    }

    pub fn component_tiles(&self, component_id: u32) -> Option<&[(i32, i32)]> {
        self.components
            .get(component_id as usize)
            .filter(|component| component.id == component_id)
            .map(|component| component.tiles.as_slice())
    }
}
