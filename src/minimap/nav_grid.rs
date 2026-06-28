//! BOT navigation view derived from parsed map blocks.
//!
//! This is the launcher-owned map format. S32 parsing stays in `s32_parser`;
//! hunt code consumes this tile-query API instead of reading S32 blocks.

use std::collections::HashMap;

use super::coord::{tile_to_block, tile_to_local};
use super::s32_parser::Block;

const WALKABLE: u8 = 0x01;
const BLOCKS_SIGHT: u8 = 0x02;
const ATTR1_OPEN: u8 = 0x04;
const ATTR2_OPEN: u8 = 0x08;

#[derive(Debug, Clone)]
pub struct NavGrid {
    blocks: HashMap<(i32, i32), NavBlock>,
}

#[derive(Debug, Clone)]
struct NavBlock {
    cells: Box<[[u8; 64]; 64]>,
}

impl NavGrid {
    pub fn from_blocks(blocks: &HashMap<(i32, i32), Block>) -> Self {
        let mut nav_blocks = HashMap::with_capacity(blocks.len());
        for (&coord, block) in blocks {
            let mut cells = Box::new([[BLOCKS_SIGHT; 64]; 64]);
            for y in 0..64 {
                for x in 0..64 {
                    let mut flags = if block.walkable[y][x] {
                        WALKABLE
                    } else {
                        BLOCKS_SIGHT
                    };
                    if block.attr1_open(x, y) {
                        flags |= ATTR1_OPEN;
                    }
                    if block.attr2_open(x, y) {
                        flags |= ATTR2_OPEN;
                    }
                    cells[y][x] = flags;
                }
            }
            nav_blocks.insert(coord, NavBlock { cells });
        }
        Self { blocks: nav_blocks }
    }

    pub fn is_walkable(&self, tile_x: i32, tile_y: i32) -> bool {
        self.flags(tile_x, tile_y)
            .is_some_and(|flags| flags & WALKABLE != 0)
    }

    pub fn blocks_sight(&self, tile_x: i32, tile_y: i32) -> bool {
        self.flags(tile_x, tile_y)
            .map_or(true, |flags| flags & BLOCKS_SIGHT != 0)
    }

    pub fn can_step(&self, from: (i32, i32), to: (i32, i32)) -> bool {
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        if dx == 0 && dy == 0 {
            return self.is_walkable(from.0, from.1);
        }
        if dx.abs() > 1 || dy.abs() > 1 || !self.is_walkable(to.0, to.1) {
            return false;
        }

        if dx != 0 && dy != 0 {
            let side_x = (from.0 + dx, from.1);
            let side_y = (from.0, from.1 + dy);
            return self.can_cardinal_step(from, side_x)
                && self.can_cardinal_step(side_x, to)
                && self.can_cardinal_step(from, side_y)
                && self.can_cardinal_step(side_y, to);
        }

        self.can_cardinal_step(from, to)
    }

    fn flags(&self, tile_x: i32, tile_y: i32) -> Option<u8> {
        let (bx, by) = tile_to_block(tile_x, tile_y);
        let (lx, ly) = tile_to_local(tile_x, tile_y);
        self.blocks.get(&(bx, by)).map(|block| block.cells[ly][lx])
    }

    fn has_flag(&self, tile_x: i32, tile_y: i32, flag: u8) -> bool {
        self.flags(tile_x, tile_y)
            .is_some_and(|flags| flags & flag != 0)
    }

    fn can_cardinal_step(&self, from: (i32, i32), to: (i32, i32)) -> bool {
        if !self.is_walkable(to.0, to.1) {
            return false;
        }
        match (to.0 - from.0, to.1 - from.1) {
            (0, 1) | (0, -1) => {
                self.has_flag(from.0, from.1, ATTR1_OPEN) && self.has_flag(to.0, to.1, ATTR1_OPEN)
            }
            (1, 0) | (-1, 0) => {
                self.has_flag(from.0, from.1, ATTR2_OPEN) && self.has_flag(to.0, to.1, ATTR2_OPEN)
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::minimap::coord::BLOCK_ORIGIN;
    use crate::minimap::s32_parser::{PASS_ATTR1, PASS_ATTR2};

    fn single_block(walkable_cells: &[(usize, usize)]) -> HashMap<(i32, i32), Block> {
        let mut walkable = Box::new([[false; 64]; 64]);
        for &(x, y) in walkable_cells {
            walkable[y][x] = true;
        }
        HashMap::from([((0, 0), Block::from_walkable(walkable))])
    }

    fn single_passability_block(
        passable_cells: &[((usize, usize), u8)],
    ) -> HashMap<(i32, i32), Block> {
        let mut passability = Box::new([[0u8; 64]; 64]);
        for &((x, y), flags) in passable_cells {
            passability[y][x] = flags;
        }
        HashMap::from([((0, 0), Block::from_passability_for_tests(passability))])
    }

    #[test]
    fn converts_walkable_cells_to_navigation_flags() {
        let grid = NavGrid::from_blocks(&single_block(&[(0, 0)]));

        assert!(grid.is_walkable(BLOCK_ORIGIN, BLOCK_ORIGIN));
        assert!(!grid.blocks_sight(BLOCK_ORIGIN, BLOCK_ORIGIN));
    }

    #[test]
    fn converts_blocked_cells_to_sight_blockers() {
        let grid = NavGrid::from_blocks(&single_block(&[(0, 0)]));

        assert!(!grid.is_walkable(BLOCK_ORIGIN + 1, BLOCK_ORIGIN));
        assert!(grid.blocks_sight(BLOCK_ORIGIN + 1, BLOCK_ORIGIN));
    }

    #[test]
    fn missing_blocks_are_blocked_and_sight_blocking() {
        let grid = NavGrid::from_blocks(&HashMap::new());

        assert!(!grid.is_walkable(BLOCK_ORIGIN, BLOCK_ORIGIN));
        assert!(grid.blocks_sight(BLOCK_ORIGIN, BLOCK_ORIGIN));
    }

    #[test]
    fn can_step_uses_directional_attributes() {
        let grid = NavGrid::from_blocks(&single_block(&[(0, 0), (1, 0), (2, 0), (1, 1)]));

        assert!(grid.can_step(
            (BLOCK_ORIGIN + 1, BLOCK_ORIGIN),
            (BLOCK_ORIGIN + 1, BLOCK_ORIGIN + 1)
        ));
        assert!(grid.can_step(
            (BLOCK_ORIGIN + 1, BLOCK_ORIGIN),
            (BLOCK_ORIGIN + 2, BLOCK_ORIGIN)
        ));
        assert!(!grid.can_step(
            (BLOCK_ORIGIN + 1, BLOCK_ORIGIN),
            (BLOCK_ORIGIN + 3, BLOCK_ORIGIN)
        ));
    }

    #[test]
    fn vertical_wall_edge_blocks_step_even_when_tiles_are_walkable() {
        let both = PASS_ATTR1 | PASS_ATTR2;
        let grid = NavGrid::from_blocks(&single_passability_block(&[
            ((10, 10), PASS_ATTR2),
            ((10, 11), both),
        ]));

        assert!(grid.is_walkable(BLOCK_ORIGIN + 10, BLOCK_ORIGIN + 10));
        assert!(grid.is_walkable(BLOCK_ORIGIN + 10, BLOCK_ORIGIN + 11));
        assert!(!grid.can_step(
            (BLOCK_ORIGIN + 10, BLOCK_ORIGIN + 10),
            (BLOCK_ORIGIN + 10, BLOCK_ORIGIN + 11)
        ));
    }

    #[test]
    fn horizontal_wall_edge_blocks_step_even_when_tiles_are_walkable() {
        let both = PASS_ATTR1 | PASS_ATTR2;
        let grid = NavGrid::from_blocks(&single_passability_block(&[
            ((10, 10), PASS_ATTR1),
            ((11, 10), both),
        ]));

        assert!(grid.is_walkable(BLOCK_ORIGIN + 10, BLOCK_ORIGIN + 10));
        assert!(grid.is_walkable(BLOCK_ORIGIN + 11, BLOCK_ORIGIN + 10));
        assert!(!grid.can_step(
            (BLOCK_ORIGIN + 10, BLOCK_ORIGIN + 10),
            (BLOCK_ORIGIN + 11, BLOCK_ORIGIN + 10)
        ));
    }
}
