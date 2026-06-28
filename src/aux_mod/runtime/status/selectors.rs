use std::time::{Duration, Instant};

use super::super::AuxSettings;

pub(in crate::aux::runtime) fn status_work_enabled(s: &AuxSettings) -> bool {
    s.status_eat_meat
        || s.status_antidote_enabled
        || s.status_whetstone
        || s.status_transform_enabled
}

pub(in crate::aux::runtime) fn cooldown_due(
    last: Option<Instant>,
    now: Instant,
    cooldown: Duration,
) -> bool {
    last.map(|t| now.duration_since(t) >= cooldown)
        .unwrap_or(true)
}

pub(in crate::aux::runtime) fn eat_meat_needed(raw: u32, max: u32) -> bool {
    raw < max
}

pub(in crate::aux::runtime) fn antidote_action_enabled(s: &AuxSettings, poisoned: bool) -> bool {
    s.status_antidote_enabled && !s.status_antidote_item.is_empty() && poisoned
}

pub(in crate::aux::runtime) fn transform_action_enabled(s: &AuxSettings) -> bool {
    s.status_transform_enabled && !s.status_transform_item.is_empty()
}
