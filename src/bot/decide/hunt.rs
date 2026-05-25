//! Hunt ?獢?? / ??????豲? ????`bot::hunt4::runtime` ??`bot::ui::window` ??????//!
//! ?????2026-05-13 ???? `decide::hunt::tick` ??Hunting ?畾????謚?????;Phase 1 step 4 ??//! tick ??叟?????? `bot::hunt4::runtime::tick`,?雓?????嚚????鞊???撖???豲? ??`HuntConfig`??//! `HuntOutcome`?頩?ELEE_RANGE_TILES` / `RANGED_RANGE_TILES`??//!
//! ## ?????穿?????船?(2026-05-17)
//!
//! ?????`AttackMode { Skill, Melee, Ranged }` enum ??狗????????豰刈??馳??????????,
//! ??????瞏????憸??瞉? + ????????????頛?????
//!
//! - **?豲??冽伍雓???*:?豲?雓?????client `Function A` chain ???啾遜 CD??//! - **??镼踵??????*:????????skill CD ?????? fall through ??頦?????謜??,
//!   skill ?豰刈?賹?謚叟??豲? cast,?豲?????DPS??//!
//! ???雓■?西扈?(?????/ ?頩?)??`aux::weapon::is_ranged_weapon_equipped` **?????謚叟**
//! (??? client ??obfuscated weapon-class container @ `0xBDC7C8` / `0xBDC7D4`),
//! UI ?豲????狗??????????????擗?????????蹎?鞎?????
use crate::bot::action::walk::WalkDriver;
use crate::bot::perception::position::PlayerPosition;

pub const MAX_ATTACK_SEQUENCE_STEPS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttackStepKind {
    Basic,
    Skill,
}

impl Default for AttackStepKind {
    fn default() -> Self {
        Self::Basic
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttackSequenceStep {
    #[serde(default)]
    pub kind: AttackStepKind,
    #[serde(default)]
    /// Legacy single skill name. Empty means basic attack only.
    /// Legacy single skill name. Empty means basic attack only.
    pub skill_name: String,
    #[serde(default)]
    pub interval_ms: u64,
}

impl AttackSequenceStep {
    pub fn basic(interval_ms: u64) -> Self {
        Self {
            kind: AttackStepKind::Basic,
            skill_name: String::new(),
            interval_ms,
        }
    }

    pub fn skill(skill_name: String, interval_ms: u64) -> Self {
        Self {
            kind: AttackStepKind::Skill,
            skill_name,
            interval_ms,
        }
    }

    pub fn normalized(&self) -> Self {
        match self.kind {
            AttackStepKind::Basic => Self::basic(self.interval_ms),
            AttackStepKind::Skill => {
                let name = self.skill_name.trim();
                if name.is_empty() {
                    Self::basic(self.interval_ms)
                } else {
                    Self::skill(name.to_string(), self.interval_ms)
                }
            }
        }
    }

    pub fn skill_for_cd(&self) -> Option<&str> {
        if self.kind == AttackStepKind::Skill && !self.skill_name.trim().is_empty() {
            Some(self.skill_name.trim())
        } else {
            None
        }
    }
}

/// Melee attack range in tiles.
pub const MELEE_RANGE_TILES: u32 = 1;
/// Ranged attack range in tiles.
pub const RANGED_RANGE_TILES: u32 = 8;

/// Hunting configuration persisted by the bot UI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HuntConfig {
    /// ????????(tile,Chebyshev ?豲?雓??豯?撞?謢畸?隡敞??????頩????)
    ///
    /// - 0(???格?,?????啣?)= ?豲???????bot ??豲???????????????雓丐?
    /// - N > 0 = `bot::hunt4::perception::filter_snapshot` ???ｆ?????> N ??????    ///   (???????雓?????lock cleanup ?豲???
    #[serde(default = "default_hunt_range_tiles")]
    pub hunt_range_tiles: u32,
    /// Monster names to skip.
    #[serde(default)]
    pub monster_blacklist: Vec<String>,
    /// Legacy single skill name. Empty means basic attack only.
    pub skill_name: String,
    /// **??∴???蹓???謕?**(2026-05-14)???????/ ????????蝚阬頩??謕??????蹎∪眾?????????    /// ?謜????= ?雓??(???格?)??bot ??????豯剁?? item.name ?????????豲???????∴???謍???皞?    /// New attack sequence configured by the bot UI. Empty means "use legacy skill_name".
    #[serde(default)]
    pub attack_sequence: Vec<AttackSequenceStep>,
    /// Minimum duration of one configured attack-sequence round. 0 disables cycle waiting.
    #[serde(default)]
    pub attack_sequence_cycle_ms: u64,
    #[serde(default)]
    pub teleport_scroll_name: String,

    /// ???雓??∴?????????????????謆祁?????????????瞉????????僱???豰刈?謅???timer,
    /// Idle/no-actionable-target duration before random teleport is allowed.
    #[serde(default = "default_idle_teleport_secs")]
    pub idle_teleport_secs: u64,

    /// Movement execution layer used by the active hunt runtime.
    #[serde(default)]
    pub walk_driver: WalkDriver,

    /// V4 dispatch takeover gate. Default on; takeover remains conservative and
    /// only dispatches when its shadow intent exactly matches the backend.
    #[serde(default = "default_v4_dispatch_takeover")]
    pub v4_dispatch_takeover: bool,

    /// HP ????????**max_hp ???雲???**(1 tick ??? ??N% max_hp)????V3
    /// HP drop threshold as a percent of max HP for entering damage-spike recovery.
    #[serde(default = "default_damage_spike_hp_percent")]
    pub damage_spike_hp_percent: u8,
}

fn default_hunt_range_tiles() -> u32 {
    0
}

fn default_idle_teleport_secs() -> u64 {
    10
}

pub fn default_damage_spike_hp_percent() -> u8 {
    10
}

fn default_v4_dispatch_takeover() -> bool {
    true
}

impl Default for HuntConfig {
    fn default() -> Self {
        Self {
            hunt_range_tiles: default_hunt_range_tiles(),
            monster_blacklist: Vec::new(),
            skill_name: String::new(),
            attack_sequence: Vec::new(),
            attack_sequence_cycle_ms: 0,
            teleport_scroll_name: String::new(),
            idle_teleport_secs: default_idle_teleport_secs(),
            walk_driver: WalkDriver::PostMessage,
            v4_dispatch_takeover: default_v4_dispatch_takeover(),
            damage_spike_hp_percent: default_damage_spike_hp_percent(),
        }
    }
}

impl HuntConfig {
    pub fn effective_attack_sequence(&self) -> Vec<AttackSequenceStep> {
        let explicit: Vec<_> = self
            .attack_sequence
            .iter()
            .take(MAX_ATTACK_SEQUENCE_STEPS)
            .map(AttackSequenceStep::normalized)
            .collect();
        if !explicit.is_empty() {
            return explicit;
        }

        let skill = self.skill_name.trim();
        if skill.is_empty() {
            vec![AttackSequenceStep::basic(0)]
        } else {
            vec![AttackSequenceStep::skill(skill.to_string(), 0)]
        }
    }
}

/// Result emitted by the active hunt runtime.
#[derive(Debug)]
pub enum HuntOutcome {
    /// ????cooldown ???????tick ?豲?雓?????餈??豯?雓???謅?)
    Cooldown { remaining_ms: u64 },
    /// ????豲???????Ⅹ??????
    NoTarget,
    /// ??謕??豲????reserved ????Ⅹ? hunt ??踝????????獢??,?璈急活???safety / map ??stop ?豲???
    /// ???????頩陷??packet
    Cast {
        target_id: u32,
        name: String,
        player_pos: Option<PlayerPosition>,
    },
    /// ??梱???豰刈??????????
    Walked {
        target_id: u32,
        name: String,
        heading: u8,
        distance_tiles: u32,
    },
    /// Movement cooldown; retry on a later tick.
    /// ??? / ????????
    ActionFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_skill_name_is_empty_so_basic_only_mode_works_out_of_the_box() {
        let cfg = HuntConfig::default();
        assert!(cfg.skill_name.is_empty());
    }

    #[test]
    fn hunt_config_default_uses_helper_for_idle_teleport_secs() {
        let cfg = HuntConfig::default();
        assert_eq!(cfg.idle_teleport_secs, default_idle_teleport_secs());
    }

    #[test]
    fn hunt_config_ignores_old_engine_selector_field() {
        let cfg = HuntConfig::default();
        assert!(cfg.v4_dispatch_takeover);

        let parsed: HuntConfig =
            serde_json::from_str(r#"{"skill_name":"","legacy_selector_removed":true}"#).unwrap();
        assert!(parsed.v4_dispatch_takeover);
    }
    #[test]
    fn hunt_config_enables_v4_dispatch_takeover_by_default() {
        let cfg = HuntConfig::default();
        assert!(cfg.v4_dispatch_takeover);

        let parsed: HuntConfig = serde_json::from_str(r#"{"skill_name":""}"#).unwrap();
        assert!(parsed.v4_dispatch_takeover);
    }

    #[test]
    fn hunt_config_accepts_v4_dispatch_takeover_from_json() {
        let parsed: HuntConfig =
            serde_json::from_str(r#"{"skill_name":"","v4_dispatch_takeover":true}"#).unwrap();
        assert!(parsed.v4_dispatch_takeover);

        let parsed: HuntConfig =
            serde_json::from_str(r#"{"skill_name":"","v4_dispatch_takeover":false}"#).unwrap();
        assert!(!parsed.v4_dispatch_takeover);
    }

    #[test]
    fn melee_and_ranged_ranges_match_server_attack_packets() {
        assert_eq!(MELEE_RANGE_TILES, 1);
        assert_eq!(RANGED_RANGE_TILES, 8);
    }

    #[test]
    fn effective_attack_sequence_defaults_to_one_basic_step() {
        let cfg = HuntConfig::default();

        let steps = cfg.effective_attack_sequence();

        assert_eq!(steps, vec![AttackSequenceStep::basic(0)]);
    }

    #[test]
    fn effective_attack_sequence_migrates_legacy_skill_name() {
        let cfg = HuntConfig {
            skill_name: "skill-a".to_string(),
            ..HuntConfig::default()
        };

        let steps = cfg.effective_attack_sequence();

        assert_eq!(
            steps,
            vec![AttackSequenceStep::skill("skill-a".to_string(), 0)]
        );
    }
    #[test]
    fn explicit_attack_sequence_uses_only_configured_steps() {
        let cfg = HuntConfig {
            skill_name: "fallback-skill".to_string(),
            attack_sequence: vec![
                AttackSequenceStep::skill("skill-a".to_string(), 100),
                AttackSequenceStep::basic(250),
            ],
            attack_sequence_cycle_ms: 1500,
            ..HuntConfig::default()
        };

        let steps = cfg.effective_attack_sequence();

        assert_eq!(
            steps,
            vec![
                AttackSequenceStep::skill("skill-a".to_string(), 100),
                AttackSequenceStep::basic(250),
            ]
        );
    }
}
