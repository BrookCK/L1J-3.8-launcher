use anyhow::Result;
use windows::Win32::Foundation::HANDLE;

use crate::aux::spell_book::SpellBook;
use crate::memory::read_u16;

const SKILL_CD_TABLE: u32 = 0x0096_D630;
const TABLE_MAX_PACKED: u32 = 0xDB;

pub const FALLBACK_GATE_MS: u64 = 2000;

pub fn lookup_gate_ms(h: HANDLE, skill_name: &str) -> Result<u64> {
    let book = SpellBook::build(h)?;
    let packed = book
        .lookup(skill_name)
        .ok_or_else(|| anyhow::anyhow!("spell_book missing {skill_name}"))?;
    if packed > TABLE_MAX_PACKED {
        anyhow::bail!("packed_id 0x{packed:X} exceeds CD table bound 0xDB");
    }
    let cd_ms = read_skill_cd_ms(h, packed)?;
    Ok(effective_gate_ms(cd_ms))
}

fn read_skill_cd_ms(h: HANDLE, packed: u32) -> Result<u16> {
    let addr = SKILL_CD_TABLE + packed * 2;
    let raw = read_u16(h, addr)?;
    let signed = raw as i16;
    Ok(signed.max(0) as u16)
}

fn effective_gate_ms(table_ms: u16) -> u64 {
    table_ms as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_gate_uses_client_table_value_without_floor() {
        assert_eq!(effective_gate_ms(0), 0);
        assert_eq!(effective_gate_ms(10), 10);
        assert_eq!(effective_gate_ms(50), 50);
        assert_eq!(effective_gate_ms(100), 100);
        assert_eq!(effective_gate_ms(500), 500);
        assert_eq!(effective_gate_ms(600), 600);
    }

    #[test]
    fn effective_gate_uses_table_value_for_slow_skills() {
        assert_eq!(effective_gate_ms(1000), 1000);
        assert_eq!(effective_gate_ms(2000), 2000);
        assert_eq!(effective_gate_ms(12000), 12000);
    }

    #[test]
    fn table_address_and_bounds_match_re_findings() {
        assert_eq!(SKILL_CD_TABLE, 0x0096_D630);
        assert_eq!(TABLE_MAX_PACKED, 0xDB);
    }
}
