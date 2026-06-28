use std::time::{Duration, Instant};

use super::{AuxSettings, TimerRow};

pub(in crate::aux::runtime) fn pick_delete_action(
    delete_list: &[String],
    dissolve_list: &[String],
    inv_names: &[String],
) -> Option<(&'static str, String)> {
    let safe = |n: &str| -> bool { !n.contains("(雿輻銝?") && !n.contains("(?株?)") };
    let find_inventory_match = |needle: &str| -> Option<String> {
        let needle = crate::aux::lhx_window::strip_qty(needle);
        inv_names
            .iter()
            .find(|n| safe(n) && crate::aux::lhx_window::strip_qty(n) == needle)
            .cloned()
    };
    for needle in delete_list {
        if let Some(name) = find_inventory_match(needle) {
            return Some(("delete", name));
        }
    }
    for needle in dissolve_list {
        if let Some(name) = find_inventory_match(needle) {
            return Some(("dissolve", name));
        }
    }
    None
}

pub(in crate::aux::runtime) fn pick_shout_message(
    messages: &[String],
    next_idx: usize,
) -> Option<(String, usize)> {
    if messages.is_empty() {
        return None;
    }
    let len = messages.len();
    let i = next_idx % len;
    Some((messages[i].clone(), (i + 1) % len))
}

pub(in crate::aux::runtime) fn pick_timer_action(
    rows: &[TimerRow; 6],
    last_fire: &[Option<Instant>; 6],
    master_enabled: bool,
    now: Instant,
) -> Option<usize> {
    if !master_enabled {
        return None;
    }
    for i in 0..6 {
        let row = &rows[i];
        if !row.enabled || row.command.is_empty() {
            continue;
        }
        let due = match last_fire[i] {
            None => true,
            Some(t) => now.duration_since(t) >= Duration::from_secs(row.interval_sec as u64),
        };
        if due {
            return Some(i);
        }
    }
    None
}

pub(in crate::aux::runtime) fn mp_when_safe_triggered(
    s: &AuxSettings,
    state: &crate::aux::player_state::PlayerState,
) -> bool {
    let rule = &s.mp_when_safe;
    if !rule.enabled || rule.item.trim().is_empty() {
        return false;
    }

    if s.potion_use_percent {
        let hp_pct = state.hp.saturating_mul(100) / state.max_hp.max(1);
        let mp_pct = state.mp.saturating_mul(100) / state.max_mp.max(1);
        hp_pct >= rule.hp_lower && mp_pct <= rule.mp_upper
    } else {
        state.hp >= rule.hp_lower && state.mp <= rule.mp_upper
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::aux::runtime) enum AutoDrinkItemRequest {
    DirectUsePacket { item_param: u32 },
}

pub(in crate::aux::runtime) fn auto_drink_item_request(
    item: &crate::aux::inventory::Item,
) -> AutoDrinkItemRequest {
    AutoDrinkItemRequest::DirectUsePacket {
        item_param: item.item_param,
    }
}
