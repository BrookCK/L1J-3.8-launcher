//! S32 Layer3 parser.
//!
//! Layer3 is not a simple per-tile wall bitmap. Each 64x64 cell stores two
//! directional attributes:
//! - Attribute1 bit0 clear: one axis is passable.
//! - Attribute2 bit0 clear: the other axis is passable.
//!
//! A tile is occupiable when at least one direction is open. Movement and line
//! checks consume the per-attribute flags through `NavGrid`.

use anyhow::{anyhow, Result};

pub const PASS_ATTR1: u8 = 0x01;
pub const PASS_ATTR2: u8 = 0x02;

const LAYER1_SIZE: usize = 0x8000;
const LAYER2_ITEM_SIZE: usize = 6;
const LAYER3_CELL_SIZE: usize = 4;

#[derive(Debug, Clone)]
pub struct Block {
    pub walkable: Box<[[bool; 64]; 64]>,
    passability: Box<[[u8; 64]; 64]>,
}

impl Block {
    #[cfg(test)]
    pub fn from_walkable(walkable: Box<[[bool; 64]; 64]>) -> Self {
        let mut passability = Box::new([[0u8; 64]; 64]);
        for y in 0..64 {
            for x in 0..64 {
                if walkable[y][x] {
                    passability[y][x] = PASS_ATTR1 | PASS_ATTR2;
                }
            }
        }
        Self {
            walkable,
            passability,
        }
    }

    #[cfg(test)]
    pub fn from_passability_for_tests(passability: Box<[[u8; 64]; 64]>) -> Self {
        let mut walkable = Box::new([[false; 64]; 64]);
        for y in 0..64 {
            for x in 0..64 {
                walkable[y][x] = passability[y][x] != 0;
            }
        }
        Self {
            walkable,
            passability,
        }
    }

    pub fn attr1_open(&self, x: usize, y: usize) -> bool {
        self.passability[y][x] & PASS_ATTR1 != 0
    }

    pub fn attr2_open(&self, x: usize, y: usize) -> bool {
        self.passability[y][x] & PASS_ATTR2 != 0
    }
}

pub fn parse(bytes: &[u8]) -> Result<Block> {
    if bytes.len() < LAYER1_SIZE + 2 {
        return Err(anyhow!(
            ".s32 too small: {} bytes, cannot read Layer2 count",
            bytes.len()
        ));
    }
    let layer2_count = u16::from_le_bytes([bytes[LAYER1_SIZE], bytes[LAYER1_SIZE + 1]]) as usize;
    let layer3_offset = LAYER1_SIZE + 2 + layer2_count * LAYER2_ITEM_SIZE;
    let layer3_size = 64 * 64 * LAYER3_CELL_SIZE;
    if bytes.len() < layer3_offset + layer3_size {
        return Err(anyhow!(
            ".s32 too small: {} bytes < required {} bytes (layer2_count={})",
            bytes.len(),
            layer3_offset + layer3_size,
            layer2_count
        ));
    }

    let mut walkable = Box::new([[false; 64]; 64]);
    let mut passability = Box::new([[0u8; 64]; 64]);
    for y in 0..64 {
        for x in 0..64 {
            let off = layer3_offset + (y * 64 + x) * LAYER3_CELL_SIZE;
            let attr1 = i16::from_le_bytes([bytes[off], bytes[off + 1]]);
            let attr2 = i16::from_le_bytes([bytes[off + 2], bytes[off + 3]]);

            let mut flags = 0u8;
            if (attr1 & 1) == 0 {
                flags |= PASS_ATTR1;
            }
            if (attr2 & 1) == 0 {
                flags |= PASS_ATTR2;
            }
            passability[y][x] = flags;
            walkable[y][x] = flags != 0;
        }
    }

    Ok(Block {
        walkable,
        passability,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_bytes(cell: impl Fn(usize, usize) -> (i16, i16)) -> Vec<u8> {
        let layer3_offset = LAYER1_SIZE + 2;
        let mut buf = vec![0u8; layer3_offset + 64 * 64 * LAYER3_CELL_SIZE];
        for y in 0..64 {
            for x in 0..64 {
                let (attr1, attr2) = cell(x, y);
                let off = layer3_offset + (y * 64 + x) * LAYER3_CELL_SIZE;
                buf[off..off + 2].copy_from_slice(&attr1.to_le_bytes());
                buf[off + 2..off + 4].copy_from_slice(&attr2.to_le_bytes());
            }
        }
        buf
    }

    #[test]
    fn all_zero_attributes_means_fully_walkable() {
        let bytes = synthetic_bytes(|_, _| (0, 0));
        let block = parse(&bytes).unwrap();
        assert!(block.walkable.iter().all(|row| row.iter().all(|&w| w)));
        assert!(block.attr1_open(0, 0));
        assert!(block.attr2_open(0, 0));
    }

    #[test]
    fn attr1_bit0_set_keeps_attr2_only_cell_walkable() {
        let bytes = synthetic_bytes(|_, _| (1, 0));
        let block = parse(&bytes).unwrap();
        assert!(block.walkable.iter().all(|row| row.iter().all(|&w| w)));
        assert!(!block.attr1_open(0, 0));
        assert!(block.attr2_open(0, 0));
    }

    #[test]
    fn attr2_bit0_set_keeps_attr1_only_cell_walkable() {
        let bytes = synthetic_bytes(|_, _| (0, 1));
        let block = parse(&bytes).unwrap();
        assert!(block.walkable.iter().all(|row| row.iter().all(|&w| w)));
        assert!(block.attr1_open(0, 0));
        assert!(!block.attr2_open(0, 0));
    }

    #[test]
    fn both_direction_bits_set_means_blocked() {
        let bytes = synthetic_bytes(|_, _| (1, 1));
        let block = parse(&bytes).unwrap();
        assert!(block.walkable.iter().all(|row| row.iter().all(|&w| !w)));
        assert!(!block.attr1_open(0, 0));
        assert!(!block.attr2_open(0, 0));
    }

    #[test]
    fn high_bits_dont_affect_walkability() {
        let bytes = synthetic_bytes(|_, _| (-2i16, -2i16));
        let block = parse(&bytes).unwrap();
        assert!(block.walkable.iter().all(|row| row.iter().all(|&w| w)));
    }

    #[test]
    fn diagonal_blocked_pattern() {
        let bytes = synthetic_bytes(|x, y| if x == y { (1, 1) } else { (0, 0) });
        let block = parse(&bytes).unwrap();
        for i in 0..64 {
            assert!(
                !block.walkable[i][i],
                "diagonal ({i},{i}) should be blocked"
            );
            if i + 1 < 64 {
                assert!(
                    block.walkable[i][i + 1],
                    "({},{i}) should be walkable",
                    i + 1
                );
            }
        }
    }

    #[test]
    fn layer2_count_shifts_layer3_offset() {
        let layer3_offset = LAYER1_SIZE + 2 + 3 * LAYER2_ITEM_SIZE;
        let mut buf = vec![0u8; layer3_offset + 64 * 64 * LAYER3_CELL_SIZE];
        buf[LAYER1_SIZE] = 3;
        for b in buf[LAYER1_SIZE + 2..layer3_offset].iter_mut() {
            *b = 0xFF;
        }
        buf[layer3_offset] = 1;
        buf[layer3_offset + 2] = 1;
        let block = parse(&buf).unwrap();
        assert!(!block.walkable[0][0], "first Layer3 cell should be blocked");
        assert!(block.walkable[0][1], "other cells should be walkable");
    }

    #[test]
    fn short_input_errors() {
        let too_short = vec![0u8; LAYER1_SIZE + 100];
        assert!(parse(&too_short).is_err());
    }

    #[test]
    #[ignore = "requires local game files"]
    fn real_game_file_parses() {
        let bytes = std::fs::read("D:/lineage3.81C/map/4/7ffc7ff7.s32")
            .expect("real game file should be present");
        let block = parse(&bytes).expect("real .s32 should parse");
        let walkable_count = block.walkable.iter().flatten().filter(|&&w| w).count();
        assert!(
            walkable_count > 100,
            "walkable cells {walkable_count} too few"
        );
        assert!(
            walkable_count < 4096 - 100,
            "walkable cells {walkable_count} too many"
        );
    }
}
