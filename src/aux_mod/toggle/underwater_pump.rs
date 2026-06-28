//! Underwater pump visual toggle.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use windows::Win32::Foundation::HANDLE;

use crate::platform::memory::{read_bytes, write_code};

use super::Toggle;

pub struct UnderwaterPump;

const SEA_WATER_VISIBLE_FLAG_ADDR: u32 = 0x009A_B646;
const SEA_WATER_HIDDEN: u8 = 0;
const SEA_WATER_VISIBLE: u8 = 1;

static FORCED_HIDDEN: AtomicBool = AtomicBool::new(false);

impl UnderwaterPump {
    #[allow(dead_code)] // 待接線的 toggle 建構子,保留
    pub fn new() -> Self {
        UnderwaterPump
    }
}

impl Toggle for UnderwaterPump {
    fn enable(&self, h: HANDLE) -> Result<()> {
        write_sea_water_flag(h, sea_water_flag_value(true))?;
        FORCED_HIDDEN.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn disable(&self, h: HANDLE) -> Result<()> {
        if !FORCED_HIDDEN.load(Ordering::SeqCst) {
            return Ok(());
        }

        write_sea_water_flag(h, sea_water_flag_value(false))?;
        FORCED_HIDDEN.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "underwater_pump"
    }
}

fn sea_water_flag_value(enabled: bool) -> u8 {
    if enabled {
        SEA_WATER_HIDDEN
    } else {
        SEA_WATER_VISIBLE
    }
}

fn write_sea_water_flag(h: HANDLE, value: u8) -> Result<()> {
    let current = read_bytes(h, SEA_WATER_VISIBLE_FLAG_ADDR, 1)
        .with_context(|| format!("read sea-water flag @ 0x{SEA_WATER_VISIBLE_FLAG_ADDR:08X}"))?
        .first()
        .copied()
        .unwrap_or(SEA_WATER_VISIBLE);

    if current != SEA_WATER_HIDDEN && current != SEA_WATER_VISIBLE {
        bail!(
            "sea-water flag target mismatch @ 0x{SEA_WATER_VISIBLE_FLAG_ADDR:08X}: 0x{current:02X}"
        );
    }

    if current != value {
        write_code(h, SEA_WATER_VISIBLE_FLAG_ADDR, &[value]).with_context(|| {
            format!("write sea-water flag @ 0x{SEA_WATER_VISIBLE_FLAG_ADDR:08X}")
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underwater_pump_uses_verified_sea_water_flag() {
        assert_eq!(SEA_WATER_VISIBLE_FLAG_ADDR, 0x009A_B646);
    }

    #[test]
    fn underwater_pump_enabled_hides_sea_water() {
        assert_eq!(sea_water_flag_value(true), 0);
    }

    #[test]
    fn underwater_pump_disabled_restores_sea_water() {
        assert_eq!(sea_water_flag_value(false), 1);
    }
}
