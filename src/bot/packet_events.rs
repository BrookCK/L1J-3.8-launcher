pub const S_OPCODE_MOVE_OBJECT: u8 = 10;
pub const S_OPCODE_ATTACK: u8 = 30;
pub const S_OPCODE_RANGESKILLS: u8 = 42;
pub const S_OPCODE_PUT_OBJECT: u8 = 87;
pub const S_OPCODE_REMOVE_OBJECT: u8 = 120;
pub const S_OPCODE_ACTION: u8 = 158;
pub const S_OPCODE_HP_METER: u8 = 237;

pub const ACTION_DIE: u8 = 8;

const HEADING_DX: [i32; 8] = [0, 1, 1, 1, 0, -1, -1, -1];
const HEADING_DY: [i32; 8] = [-1, -1, 0, 1, 1, 1, 0, -1];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeSkillTarget {
    pub object_id: u32,
    pub hit: bool,
    pub damage: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotPacketEvent {
    PutObject {
        object_id: u32,
        x: i32,
        y: i32,
        gfx_id: u16,
        status: u8,
        heading: u8,
    },
    MoveObject {
        object_id: u32,
        from_x: i32,
        from_y: i32,
        heading: u8,
        to_x: i32,
        to_y: i32,
    },
    RemoveObject {
        object_id: u32,
    },
    HpMeter {
        object_id: u32,
        ratio: u16,
        clear: bool,
    },
    Action {
        object_id: u32,
        action: u8,
    },
    Attack {
        attacker_id: u32,
        target_id: u32,
        damage: u16,
        heading: u8,
        attacker_x: Option<i32>,
        attacker_y: Option<i32>,
        target_x: Option<i32>,
        target_y: Option<i32>,
    },
    RangeSkill {
        caster_id: u32,
        caster_x: i32,
        caster_y: i32,
        heading: u8,
        gfx_id: u16,
        range_type: u8,
        targets: Vec<RangeSkillTarget>,
    },
}

pub fn parse_server_packet(packet: &[u8]) -> Option<BotPacketEvent> {
    let opcode = *packet.first()?;
    match opcode {
        S_OPCODE_PUT_OBJECT => parse_put_object(packet),
        S_OPCODE_MOVE_OBJECT => parse_move_object(packet),
        S_OPCODE_REMOVE_OBJECT => Some(BotPacketEvent::RemoveObject {
            object_id: read_u32(packet, 1)?,
        }),
        S_OPCODE_HP_METER => {
            let ratio = read_u16(packet, 5)?;
            Some(BotPacketEvent::HpMeter {
                object_id: read_u32(packet, 1)?,
                ratio,
                clear: ratio == 0x00FF,
            })
        }
        S_OPCODE_ACTION => Some(BotPacketEvent::Action {
            object_id: read_u32(packet, 1)?,
            action: *packet.get(5)?,
        }),
        S_OPCODE_ATTACK => parse_attack(packet),
        S_OPCODE_RANGESKILLS => parse_range_skill(packet),
        _ => None,
    }
}

fn parse_put_object(packet: &[u8]) -> Option<BotPacketEvent> {
    Some(BotPacketEvent::PutObject {
        x: read_u16(packet, 1)? as i32,
        y: read_u16(packet, 3)? as i32,
        object_id: read_u32(packet, 5)?,
        gfx_id: read_u16(packet, 9)?,
        status: *packet.get(11)?,
        heading: *packet.get(12)?,
    })
}

fn parse_move_object(packet: &[u8]) -> Option<BotPacketEvent> {
    let object_id = read_u32(packet, 1)?;
    let from_x = read_u16(packet, 5)? as i32;
    let from_y = read_u16(packet, 7)? as i32;
    let heading = *packet.get(9)?;
    let idx = heading as usize;
    let to_x = from_x + *HEADING_DX.get(idx)?;
    let to_y = from_y + *HEADING_DY.get(idx)?;

    Some(BotPacketEvent::MoveObject {
        object_id,
        from_x,
        from_y,
        heading,
        to_x,
        to_y,
    })
}

fn parse_attack(packet: &[u8]) -> Option<BotPacketEvent> {
    let attacker_id = read_u32(packet, 2)?;
    let target_id = read_u32(packet, 6)?;
    let damage = read_u16(packet, 10)?;
    let heading = *packet.get(12)?;

    let has_projectile_coords = packet.len() >= 29;
    Some(BotPacketEvent::Attack {
        attacker_id,
        target_id,
        damage,
        heading,
        attacker_x: has_projectile_coords
            .then(|| read_u16(packet, 20).map(|v| v as i32))
            .flatten(),
        attacker_y: has_projectile_coords
            .then(|| read_u16(packet, 22).map(|v| v as i32))
            .flatten(),
        target_x: has_projectile_coords
            .then(|| read_u16(packet, 24).map(|v| v as i32))
            .flatten(),
        target_y: has_projectile_coords
            .then(|| read_u16(packet, 26).map(|v| v as i32))
            .flatten(),
    })
}

fn parse_range_skill(packet: &[u8]) -> Option<BotPacketEvent> {
    let caster_id = read_u32(packet, 2)?;
    let caster_x = read_u16(packet, 6)? as i32;
    let caster_y = read_u16(packet, 8)? as i32;
    let heading = *packet.get(10)?;
    let gfx_id = read_u16(packet, 15)?;
    let range_type = *packet.get(17)?;
    let count = read_u16(packet, 20)? as usize;
    let mut offset = 22;
    let mut targets = Vec::with_capacity(count);

    for _ in 0..count {
        let object_id = read_u32(packet, offset)?;
        let hit = read_u16(packet, offset + 4)? != 0;
        let damage = read_u32(packet, offset + 6)?;
        targets.push(RangeSkillTarget {
            object_id,
            hit,
            damage,
        });
        offset += 10;
    }

    Some(BotPacketEvent::RangeSkill {
        caster_id,
        caster_x,
        caster_y,
        heading,
        gfx_id,
        range_type,
        targets,
    })
}

fn read_u16(packet: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes([
        *packet.get(offset)?,
        *packet.get(offset + 1)?,
    ]))
}

fn read_u32(packet: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *packet.get(offset)?,
        *packet.get(offset + 1)?,
        *packet.get(offset + 2)?,
        *packet.get(offset + 3)?,
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_object_packet(object_id: u32, x: u16, y: u16, status: u8, heading: u8) -> Vec<u8> {
        let mut packet = vec![S_OPCODE_PUT_OBJECT];
        packet.extend_from_slice(&x.to_le_bytes());
        packet.extend_from_slice(&y.to_le_bytes());
        packet.extend_from_slice(&object_id.to_le_bytes());
        packet.extend_from_slice(&0x1234u16.to_le_bytes());
        packet.push(status);
        packet.push(heading);
        packet
    }

    #[test]
    fn parses_put_object_absolute_position_and_dead_status() {
        let event = parse_server_packet(&put_object_packet(0x0102_0304, 32710, 32820, 8, 5))
            .expect("put object event");

        assert_eq!(
            event,
            BotPacketEvent::PutObject {
                object_id: 0x0102_0304,
                x: 32710,
                y: 32820,
                gfx_id: 0x1234,
                status: 8,
                heading: 5,
            }
        );
    }

    #[test]
    fn parses_move_object_previous_position_and_derives_destination() {
        let mut packet = vec![S_OPCODE_MOVE_OBJECT];
        packet.extend_from_slice(&7u32.to_le_bytes());
        packet.extend_from_slice(&100u16.to_le_bytes());
        packet.extend_from_slice(&200u16.to_le_bytes());
        packet.push(2);
        packet.extend_from_slice(&0u16.to_le_bytes());

        let event = parse_server_packet(&packet).expect("move object event");

        assert_eq!(
            event,
            BotPacketEvent::MoveObject {
                object_id: 7,
                from_x: 100,
                from_y: 200,
                heading: 2,
                to_x: 101,
                to_y: 200,
            }
        );
    }

    #[test]
    fn parses_range_skill_targets_with_server_damage_extension() {
        let mut packet = vec![S_OPCODE_RANGESKILLS, 18];
        packet.extend_from_slice(&1000u32.to_le_bytes());
        packet.extend_from_slice(&32768u16.to_le_bytes());
        packet.extend_from_slice(&32769u16.to_le_bytes());
        packet.push(3);
        packet.extend_from_slice(&55u32.to_le_bytes());
        packet.extend_from_slice(&777u16.to_le_bytes());
        packet.push(8);
        packet.extend_from_slice(&0u16.to_le_bytes());
        packet.extend_from_slice(&2u16.to_le_bytes());
        packet.extend_from_slice(&2000u32.to_le_bytes());
        packet.extend_from_slice(&0x20u16.to_le_bytes());
        packet.extend_from_slice(&123u32.to_le_bytes());
        packet.extend_from_slice(&3000u32.to_le_bytes());
        packet.extend_from_slice(&0u16.to_le_bytes());
        packet.extend_from_slice(&0u32.to_le_bytes());

        let event = parse_server_packet(&packet).expect("range skill event");

        assert_eq!(
            event,
            BotPacketEvent::RangeSkill {
                caster_id: 1000,
                caster_x: 32768,
                caster_y: 32769,
                heading: 3,
                gfx_id: 777,
                range_type: 8,
                targets: vec![
                    RangeSkillTarget {
                        object_id: 2000,
                        hit: true,
                        damage: 123,
                    },
                    RangeSkillTarget {
                        object_id: 3000,
                        hit: false,
                        damage: 0,
                    },
                ],
            }
        );
    }
}
