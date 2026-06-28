use super::super::{AuxSettings, CastTarget, PotionRow};

pub(in crate::aux::runtime) fn has_drink_work(s: &AuxSettings) -> bool {
    let has_potion_rows = s
        .potion_rows
        .iter()
        .any(|r| r.enabled && !r.item.trim().is_empty());
    let has_mp_when_safe = s.mp_when_safe.enabled && !s.mp_when_safe.item.trim().is_empty();
    has_potion_rows || has_mp_when_safe
}

pub(in crate::aux::runtime) fn potion_row_triggered(
    s: &AuxSettings,
    row: &PotionRow,
    state: &crate::aux::player_state::PlayerState,
) -> bool {
    if !row.enabled || row.item.is_empty() {
        return false;
    }

    if s.potion_use_percent {
        let hp_pct = state.hp.saturating_mul(100) / state.max_hp.max(1);
        hp_pct < row.threshold
    } else {
        state.hp < row.threshold
    }
}

pub(in crate::aux::runtime) fn drink_skill_target_mode(
    target: &CastTarget,
) -> Option<crate::aux::drink_hook::SkillTargetMode> {
    match target {
        CastTarget::Self_ => Some(crate::aux::drink_hook::SkillTargetMode::ForceSelfPacket),
        CastTarget::NoSpec => Some(crate::aux::drink_hook::SkillTargetMode::NoSpec),
        _ => None,
    }
}
