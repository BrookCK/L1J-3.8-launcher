use super::selectors::*;
use super::*;

#[test]
fn runtime_selector_module_is_available() {
    let messages = vec!["A".to_string(), "B".to_string()];
    assert_eq!(
        super::selectors::pick_shout_message(&messages, 1),
        Some(("B".to_string(), 0))
    );
}

#[test]
fn runtime_tick_modules_are_available() {
    let _timer: for<'a> fn(super::timer::TimerTickCtx<'a>) = super::timer::timer_tick;
    let _shout: fn(
        windows::Win32::Foundation::HANDLE,
        &std::sync::Arc<parking_lot::RwLock<AuxSettings>>,
        &std::sync::Arc<
            parking_lot::RwLock<Option<std::sync::Arc<crate::aux::drink_hook::DrinkHandle>>>,
        >,
        &mut Option<std::time::Instant>,
        &mut usize,
    ) = super::shout::shout_tick;
    let _delete: fn(
        windows::Win32::Foundation::HANDLE,
        &std::sync::Arc<parking_lot::RwLock<AuxSettings>>,
        &std::sync::Arc<
            parking_lot::RwLock<Option<std::sync::Arc<crate::aux::drink_hook::DrinkHandle>>>,
        >,
    ) = super::delete::delete_tick;
}

#[test]
fn runtime_drink_buff_status_modules_are_available() {
    let _drink: fn(
        windows::Win32::Foundation::HANDLE,
        &std::sync::Arc<parking_lot::RwLock<AuxSettings>>,
        &std::sync::Arc<
            parking_lot::RwLock<Option<std::sync::Arc<crate::aux::drink_hook::DrinkHandle>>>,
        >,
        &std::sync::Arc<parking_lot::RwLock<Option<crate::aux::spell_book::SpellBook>>>,
    ) = super::drink::drink_tick;
    let _buff: fn(
        windows::Win32::Foundation::HANDLE,
        &std::sync::Arc<parking_lot::RwLock<AuxSettings>>,
        &std::sync::Arc<
            parking_lot::RwLock<Option<std::sync::Arc<crate::aux::drink_hook::DrinkHandle>>>,
        >,
        &std::sync::Arc<parking_lot::RwLock<Option<crate::aux::spell_db::SpellDb>>>,
        &std::sync::Arc<parking_lot::RwLock<Option<crate::aux::spell_book::SpellBook>>>,
        &mut std::collections::HashMap<(char, i32), std::time::Instant>,
    ) = super::buff::buff_tick;
    let _status: fn(
        windows::Win32::Foundation::HANDLE,
        &std::sync::Arc<parking_lot::RwLock<AuxSettings>>,
        &std::sync::Arc<
            parking_lot::RwLock<Option<std::sync::Arc<crate::aux::drink_hook::DrinkHandle>>>,
        >,
        &std::sync::Arc<parking_lot::RwLock<Option<crate::aux::spell_book::SpellBook>>>,
        &mut std::collections::HashMap<&'static str, std::time::Instant>,
    ) = super::status::status_tick;
}

#[test]
fn runtime_guard_module_is_available() {
    let _in_game: fn(u32) -> bool = super::guards::in_game_world;
    let _process_in_game: fn(windows::Win32::Foundation::HANDLE) -> bool =
        super::guards::process_in_game_world;
    let _drink: fn(
        &std::sync::Arc<
            parking_lot::RwLock<Option<std::sync::Arc<crate::aux::drink_hook::DrinkHandle>>>,
        >,
    ) -> Option<std::sync::Arc<crate::aux::drink_hook::DrinkHandle>> =
        super::guards::clone_drink_handle;
}

#[test]
fn runtime_guard_in_game_world_accepts_only_state_3() {
    assert!(!super::guards::in_game_world(0));
    assert!(!super::guards::in_game_world(1));
    assert!(!super::guards::in_game_world(2));
    assert!(super::guards::in_game_world(3));
    assert!(!super::guards::in_game_world(4));
}

#[test]
fn runtime_guard_clone_drink_handle_returns_none_when_uninstalled() {
    let drink = std::sync::Arc::new(parking_lot::RwLock::new(None));
    assert!(super::guards::clone_drink_handle(&drink).is_none());
}

#[test]
fn drink_selector_work_enabled_by_potion_or_mp_safe() {
    let mut s = AuxSettings::default();
    assert!(!super::drink::selectors::has_drink_work(&s));

    s.potion_rows[0].enabled = true;
    assert!(!super::drink::selectors::has_drink_work(&s));

    s.potion_rows[0].item = "紅色藥水".to_string();
    assert!(super::drink::selectors::has_drink_work(&s));

    let mut s = AuxSettings::default();
    s.mp_when_safe.enabled = true;
    assert!(!super::drink::selectors::has_drink_work(&s));

    s.mp_when_safe.item = "魂體轉換/M".to_string();
    assert!(super::drink::selectors::has_drink_work(&s));
}

#[test]
fn drink_selector_potion_row_uses_percent_hp() {
    let s = AuxSettings {
        potion_use_percent: true,
        ..Default::default()
    };
    let row = PotionRow {
        enabled: true,
        threshold: 50,
        item: "紅色藥水".to_string(),
    };
    let low = crate::aux::player_state::PlayerState {
        hp: 49,
        max_hp: 100,
        ..Default::default()
    };
    let equal = crate::aux::player_state::PlayerState {
        hp: 50,
        max_hp: 100,
        ..Default::default()
    };

    assert!(super::drink::selectors::potion_row_triggered(
        &s, &row, &low
    ));
    assert!(!super::drink::selectors::potion_row_triggered(
        &s, &row, &equal
    ));
}

#[test]
fn drink_selector_potion_row_uses_raw_hp() {
    let s = AuxSettings::default();
    let row = PotionRow {
        enabled: true,
        threshold: 300,
        item: "紅色藥水".to_string(),
    };
    let low = crate::aux::player_state::PlayerState {
        hp: 299,
        max_hp: 1000,
        ..Default::default()
    };
    let equal = crate::aux::player_state::PlayerState {
        hp: 300,
        max_hp: 1000,
        ..Default::default()
    };

    assert!(super::drink::selectors::potion_row_triggered(
        &s, &row, &low
    ));
    assert!(!super::drink::selectors::potion_row_triggered(
        &s, &row, &equal
    ));
}

#[test]
fn drink_selector_skill_target_mode_accepts_only_supported_targets() {
    assert!(matches!(
        super::drink::selectors::drink_skill_target_mode(&CastTarget::Self_),
        Some(crate::aux::drink_hook::SkillTargetMode::ForceSelfPacket)
    ));
    assert!(matches!(
        super::drink::selectors::drink_skill_target_mode(&CastTarget::NoSpec),
        Some(crate::aux::drink_hook::SkillTargetMode::NoSpec)
    ));
    assert!(super::drink::selectors::drink_skill_target_mode(&CastTarget::Item).is_none());
    assert!(super::drink::selectors::drink_skill_target_mode(&CastTarget::HoverTarget).is_none());
}

#[test]
fn status_selector_work_enabled_by_any_feature() {
    assert!(!super::status::selectors::status_work_enabled(
        &AuxSettings::default()
    ));

    let mut s = AuxSettings {
        status_eat_meat: true,
        ..Default::default()
    };
    assert!(super::status::selectors::status_work_enabled(&s));

    s = AuxSettings {
        status_antidote_enabled: true,
        ..Default::default()
    };
    assert!(super::status::selectors::status_work_enabled(&s));

    s = AuxSettings {
        status_whetstone: true,
        ..Default::default()
    };
    assert!(super::status::selectors::status_work_enabled(&s));

    s = AuxSettings {
        status_transform_enabled: true,
        ..Default::default()
    };
    assert!(super::status::selectors::status_work_enabled(&s));
}

#[test]
fn status_selector_cooldown_due_handles_missing_and_recent_timestamps() {
    let now = std::time::Instant::now();
    let cooldown = std::time::Duration::from_secs(5);

    assert!(super::status::selectors::cooldown_due(None, now, cooldown));
    assert!(!super::status::selectors::cooldown_due(
        Some(now - std::time::Duration::from_secs(4)),
        now,
        cooldown
    ));
    assert!(super::status::selectors::cooldown_due(
        Some(now - cooldown),
        now,
        cooldown
    ));
}

#[test]
fn status_selector_eat_meat_needed_only_below_max() {
    assert!(super::status::selectors::eat_meat_needed(224, 225));
    assert!(!super::status::selectors::eat_meat_needed(225, 225));
    assert!(!super::status::selectors::eat_meat_needed(226, 225));
}

#[test]
fn status_selector_antidote_requires_enabled_item_and_poison() {
    let mut s = AuxSettings::default();
    assert!(!super::status::selectors::antidote_action_enabled(&s, true));

    s.status_antidote_enabled = true;
    assert!(!super::status::selectors::antidote_action_enabled(&s, true));

    s.status_antidote_item = "解毒術/ME".to_string();
    assert!(!super::status::selectors::antidote_action_enabled(
        &s, false
    ));
    assert!(super::status::selectors::antidote_action_enabled(&s, true));
}

#[test]
fn status_selector_transform_requires_enabled_item() {
    let mut s = AuxSettings::default();
    assert!(!super::status::selectors::transform_action_enabled(&s));

    s.status_transform_enabled = true;
    assert!(!super::status::selectors::transform_action_enabled(&s));

    s.status_transform_item = "狼人變身藥水".to_string();
    assert!(super::status::selectors::transform_action_enabled(&s));
}

#[test]
fn potion_row_default() {
    let r = PotionRow::default();
    assert!(!r.enabled);
    assert_eq!(r.threshold, 0);
    assert_eq!(r.item, "");
}

#[test]
fn mp_when_safe_default() {
    let m = MpWhenSafe::default();
    assert!(!m.enabled);
    assert_eq!(m.hp_lower, 0);
    assert_eq!(m.mp_upper, 0);
    assert_eq!(m.item, "");
}

#[test]
fn mp_when_safe_triggers_when_raw_hp_is_safe_and_mp_is_low() {
    let mut s = AuxSettings::default();
    s.mp_when_safe.enabled = true;
    s.mp_when_safe.hp_lower = 800;
    s.mp_when_safe.mp_upper = 20;
    s.mp_when_safe.item = "心靈轉換/M".to_string();

    let state = crate::aux::player_state::PlayerState {
        hp: 900,
        max_hp: 1000,
        mp: 10,
        max_mp: 100,
        ..Default::default()
    };

    assert!(mp_when_safe_triggered(&s, &state));
}

#[test]
fn mp_when_safe_triggers_when_percent_hp_is_safe_and_mp_is_low() {
    // 以 struct 更新語法一次設定欄位(避免 clippy field_reassign_with_default)
    let mut s = AuxSettings {
        potion_use_percent: true,
        ..Default::default()
    };
    s.mp_when_safe.enabled = true;
    s.mp_when_safe.hp_lower = 80;
    s.mp_when_safe.mp_upper = 20;
    s.mp_when_safe.item = "魂體轉換/M".to_string();

    let state = crate::aux::player_state::PlayerState {
        hp: 850,
        max_hp: 1000,
        mp: 19,
        max_mp: 100,
        ..Default::default()
    };

    assert!(mp_when_safe_triggered(&s, &state));
}

#[test]
fn mp_when_safe_does_not_trigger_when_disabled_or_empty() {
    let mut s = AuxSettings::default();
    s.mp_when_safe.enabled = true;
    s.mp_when_safe.hp_lower = 800;
    s.mp_when_safe.mp_upper = 20;

    let state = crate::aux::player_state::PlayerState {
        hp: 900,
        max_hp: 1000,
        mp: 10,
        max_mp: 100,
        ..Default::default()
    };

    assert!(!mp_when_safe_triggered(&s, &state));

    s.mp_when_safe.item = "心靈轉換/M".to_string();
    s.mp_when_safe.enabled = false;
    assert!(!mp_when_safe_triggered(&s, &state));
}

#[test]
fn auto_drink_item_request_uses_packet_param_not_client_entry() {
    let item = crate::aux::inventory::Item {
        entry_addr: 0x1111_1111,
        item_param: 0x2222_2222,
        item_type: 0,
        icon: 0,
        equipped: false,
        count: 1,
        name_raw: b"red potion".to_vec(),
    };

    assert_eq!(
        auto_drink_item_request(&item),
        AutoDrinkItemRequest::DirectUsePacket {
            item_param: 0x2222_2222
        }
    );
}

#[test]
fn auto_drink_item_execution_uses_uncooldowned_packet_path() {
    let source = include_str!("drink.rs");
    let production = source.split("#[cfg(test)]").next().unwrap();
    let execute_fn = production
        .split("fn execute_auto_drink_item(")
        .nth(1)
        .expect("execute_auto_drink_item exists")
        .split("/// timer_2")
        .next()
        .expect("execute_auto_drink_item body is bounded by drink_tick docs");

    assert!(
        execute_fn.contains("execute_use_item_packet"),
        "auto-drink item rows must use the packet path without DrinkHandle's global drink cooldown"
    );
    assert!(
        !execute_fn.contains("execute_drink_packet"),
        "auto-drink item rows must not use the global drink cooldown path"
    );
}

#[test]
fn drink_tick_potion_rows_are_not_limited_to_first_triggered_row() {
    let source = include_str!("drink.rs");
    let production = source.split("#[cfg(test)]").next().unwrap();
    let drink_tick = production
        .split("fn drink_tick(")
        .nth(1)
        .expect("drink_tick exists");
    let row_loop = drink_tick
        .find("for row in s.potion_rows.iter()")
        .expect("potion row loop exists");
    let mp_check = drink_tick
        .find("if !mp_when_safe_triggered(&s, &state)")
        .expect("mp_when_safe check exists");
    let row_body = &drink_tick[row_loop..mp_check];

    assert!(
        !row_body.contains("One potion row per tick"),
        "drink_tick must evaluate every enabled triggered potion row, not stop after the first"
    );
    assert!(
        !row_body.contains("then still allow mp_when_safe"),
        "drink_tick must not keep a single-row limiter before mp_when_safe"
    );
}

#[test]
fn drink_tick_potion_row_failures_do_not_return_before_mp_when_safe() {
    let source = include_str!("drink.rs");
    let production = source.split("#[cfg(test)]").next().unwrap();
    let drink_tick = production
        .split("fn drink_tick(")
        .nth(1)
        .expect("drink_tick exists");
    let row_loop = drink_tick
        .find("for row in s.potion_rows.iter()")
        .expect("potion row loop exists");
    let mp_check = drink_tick
        .find("if !mp_when_safe_triggered(&s, &state)")
        .expect("mp_when_safe check exists");
    let row_body = &drink_tick[row_loop..mp_check];

    assert!(
            !row_body.contains("return;"),
            "a triggered potion row failure must not return before mp_when_safe; keep later drink work reachable"
        );
}

#[test]
fn misc_toggles_default_all_false() {
    let m = MiscToggles::default();
    assert!(!m.all_day);
    assert!(!m.underwater_pump);
    assert!(!m.low_cpu);
    assert!(!m.monster_level_color);
    assert!(!m.show_clock);
    assert!(!m.show_attack_dmg);
}

#[test]
fn attack_damage_hook_module_is_available() {
    assert!(!crate::aux::attack_damage_hook::is_installed());
}

#[test]
fn timer_row_default() {
    let t = TimerRow::default();
    assert!(!t.enabled);
    assert_eq!(t.interval_sec, 5);
    assert_eq!(t.command, "");
}

#[test]
fn aux_settings_default_8tabs() {
    let s = AuxSettings::default();
    assert_eq!(s.current_profile, "");

    // tab1 喝水
    assert_eq!(s.potion_rows.len(), 7);
    assert!(!s.potion_rows[0].enabled);
    assert!(!s.mp_when_safe.enabled);
    assert!(!s.potion_use_percent);
    assert!(!s.potion_show_inventory);

    // tab2 輔助
    assert!(!s.buff_enabled);
    assert_eq!(s.buff_items.len(), 0);

    // tab3 狀態
    assert!(!s.status_show_exp);
    assert_eq!(s.fkey_macros.len(), 4);
    assert!(!s.fkey_macros[0].enabled);

    // tab4 刪物
    assert!(!s.delete_enabled);
    assert!(s.delete_list.is_empty());
    assert!(s.dissolve_list.is_empty());

    // tab5 喊話
    assert!(!s.shout_enabled);
    assert_eq!(s.shout_interval_sec, 0);

    // tab6 其他
    assert!(!s.misc.all_day);

    // tab7 定時
    assert!(!s.timer_master_enabled);
    assert_eq!(s.timer_rows.len(), 6);
    assert_eq!(s.timer_rows[0].interval_sec, 5);
}

#[test]
fn aux_settings_is_clone() {
    let s = AuxSettings::default();
    let s2 = s.clone();
    assert_eq!(s.current_profile, s2.current_profile);
}

#[test]
fn delete_lists_default_empty() {
    let s = AuxSettings::default();
    assert!(!s.delete_enabled);
    assert!(s.delete_list.is_empty());
    assert!(s.dissolve_list.is_empty());
}

#[test]
fn delete_lists_serde_roundtrip() {
    // 以 struct 更新語法一次設定欄位(避免 clippy field_reassign_with_default)
    let s = AuxSettings {
        delete_enabled: true,
        delete_list: vec!["+7 馬爾斯奇古劍".to_string(), "破布".to_string()],
        dissolve_list: vec!["+0 鋼刀".to_string()],
        ..Default::default()
    };
    let json = serde_json::to_string(&s).expect("serialize");
    let back: AuxSettings = serde_json::from_str(&json).expect("deserialize");
    assert!(back.delete_enabled);
    assert_eq!(back.delete_list, vec!["+7 馬爾斯奇古劍", "破布"]);
    assert_eq!(back.dissolve_list, vec!["+0 鋼刀"]);
}

#[test]
fn delete_tick_picks_delete_list_first() {
    let delete_list = vec!["破布".to_string()];
    let dissolve_list = vec!["+0 鋼刀".to_string()];
    let inv = vec!["破布".to_string(), "+0 鋼刀".to_string()];
    let pick = pick_delete_action(&delete_list, &dissolve_list, &inv);
    assert_eq!(pick, Some(("delete", "破布".to_string())));
}

#[test]
fn delete_tick_matches_stack_item_after_quantity_suffix_changes() {
    let delete_list = vec!["肉 (191)".to_string()];
    let dissolve_list: Vec<String> = vec![];
    let inv = vec!["肉 (190)".to_string()];
    let pick = pick_delete_action(&delete_list, &dissolve_list, &inv);
    assert_eq!(pick, Some(("delete", "肉 (190)".to_string())));
}

#[test]
fn delete_tick_falls_back_to_dissolve_when_delete_list_empty() {
    let delete_list: Vec<String> = vec![];
    let dissolve_list = vec!["+0 鋼刀".to_string()];
    let inv = vec!["破布".to_string(), "+0 鋼刀".to_string()];
    let pick = pick_delete_action(&delete_list, &dissolve_list, &inv);
    assert_eq!(pick, Some(("dissolve", "+0 鋼刀".to_string())));
}

#[test]
fn dissolve_solvent_name_accepts_traditional_and_simplified_names() {
    assert!(super::delete::is_dissolve_solvent_name("溶解劑"));
    assert!(super::delete::is_dissolve_solvent_name("溶解剂"));
    assert!(super::delete::is_dissolve_solvent_name("溶解剂 (3)"));
    assert!(!super::delete::is_dissolve_solvent_name("骰子匕首"));
}

#[test]
fn delete_tick_skips_equipped_items() {
    let delete_list = vec!["+7 馬爾斯奇古劍".to_string()];
    let dissolve_list: Vec<String> = vec![];
    let inv = vec!["+7 馬爾斯奇古劍 (揮舞)".to_string()];
    let pick = pick_delete_action(&delete_list, &dissolve_list, &inv);
    assert_eq!(pick, None);
}

#[test]
fn delete_tick_no_match_returns_none() {
    let delete_list = vec!["不存在的物品".to_string()];
    let dissolve_list: Vec<String> = vec![];
    let inv = vec!["別的東西".to_string()];
    assert_eq!(pick_delete_action(&delete_list, &dissolve_list, &inv), None);
}

#[test]
fn pick_shout_message_empty_returns_none() {
    let msgs: Vec<String> = vec![];
    assert_eq!(pick_shout_message(&msgs, 0), None);
    // next_idx 任意值都不該 panic
    assert_eq!(pick_shout_message(&msgs, 999), None);
}

#[test]
fn pick_shout_message_round_robin_advances_idx() {
    let msgs = vec![
        "第一則".to_string(),
        "第二則".to_string(),
        "第三則".to_string(),
    ];
    // idx 0 → 拿第一則,下一個 idx 變 1
    assert_eq!(
        pick_shout_message(&msgs, 0),
        Some(("第一則".to_string(), 1))
    );
    // idx 1 → 拿第二則,下一個變 2
    assert_eq!(
        pick_shout_message(&msgs, 1),
        Some(("第二則".to_string(), 2))
    );
}

#[test]
fn pick_shout_message_idx_wraps_modulo_len() {
    let msgs = vec!["A".to_string(), "B".to_string()];
    // idx 2 % 2 = 0 → 拿 A,下一個 wrap 到 1
    assert_eq!(pick_shout_message(&msgs, 2), Some(("A".to_string(), 1)));
    // idx = len-1 → 下一個 wrap 回 0
    assert_eq!(pick_shout_message(&msgs, 1), Some(("B".to_string(), 0)));
    // 超大 idx 也照 modulo 處理
    assert_eq!(pick_shout_message(&msgs, 1001), Some(("B".to_string(), 0)));
}

#[test]
fn old_profile_json_without_delete_lists_still_loads() {
    // 模擬舊版 JSON(沒 delete_list / dissolve_list / delete_enabled)
    // 用最小可解析 JSON,加上 AuxSettings 裡其他必填欄位的預設值
    let s_default = AuxSettings::default();
    let mut json_value = serde_json::to_value(&s_default).expect("serialize default");
    // 模擬舊檔 — 把三個新欄位拿掉
    let obj = json_value.as_object_mut().expect("object");
    obj.remove("delete_enabled");
    obj.remove("delete_list");
    obj.remove("dissolve_list");
    let old_json = serde_json::to_string(&json_value).expect("re-serialize");

    // 反序列化必須成功(不是 fallback default,是 graceful)
    let s: AuxSettings = serde_json::from_str(&old_json)
        .expect("舊版 JSON 應能 deserialize 成功(透過 #[serde(default)])");
    assert!(!s.delete_enabled);
    assert!(s.delete_list.is_empty());
    assert!(s.dissolve_list.is_empty());
}

fn make_timer_row(enabled: bool, interval_sec: u32, command: &str) -> TimerRow {
    TimerRow {
        enabled,
        interval_sec,
        command: command.to_string(),
    }
}

#[test]
fn pick_timer_action_master_off_returns_none() {
    let now = std::time::Instant::now();
    let rows: [TimerRow; 6] = std::array::from_fn(|_| make_timer_row(true, 5, "肉/I"));
    let last_fire: [Option<std::time::Instant>; 6] = [None; 6];
    assert_eq!(pick_timer_action(&rows, &last_fire, false, now), None);
}

#[test]
fn pick_timer_action_all_disabled_returns_none() {
    let now = std::time::Instant::now();
    let rows: [TimerRow; 6] = std::array::from_fn(|_| make_timer_row(false, 5, "肉/I"));
    let last_fire: [Option<std::time::Instant>; 6] = [None; 6];
    assert_eq!(pick_timer_action(&rows, &last_fire, true, now), None);
}

#[test]
fn pick_timer_action_row0_due_picks_0() {
    let now = std::time::Instant::now();
    let mut rows: [TimerRow; 6] = std::array::from_fn(|_| make_timer_row(false, 5, ""));
    rows[0] = make_timer_row(true, 5, "肉/I");
    let last_fire: [Option<std::time::Instant>; 6] = [None; 6];
    assert_eq!(pick_timer_action(&rows, &last_fire, true, now), Some(0));
}

#[test]
fn pick_timer_action_multiple_due_picks_smallest_idx() {
    let now = std::time::Instant::now();
    let mut rows: [TimerRow; 6] = std::array::from_fn(|_| make_timer_row(false, 5, ""));
    rows[0] = make_timer_row(true, 5, "肉/I");
    rows[1] = make_timer_row(true, 5, "保護罩/ME");
    let last_fire: [Option<std::time::Instant>; 6] = [None; 6];
    assert_eq!(pick_timer_action(&rows, &last_fire, true, now), Some(0));
}

#[test]
fn pick_timer_action_empty_command_skipped() {
    let now = std::time::Instant::now();
    let mut rows: [TimerRow; 6] = std::array::from_fn(|_| make_timer_row(false, 5, ""));
    rows[0] = make_timer_row(true, 5, ""); // enabled 但 command 空
    rows[1] = make_timer_row(true, 5, "肉/I");
    let last_fire: [Option<std::time::Instant>; 6] = [None; 6];
    assert_eq!(pick_timer_action(&rows, &last_fire, true, now), Some(1));
}

#[test]
fn pick_timer_action_last_fire_none_treated_as_due() {
    let now = std::time::Instant::now();
    let mut rows: [TimerRow; 6] = std::array::from_fn(|_| make_timer_row(false, 5, ""));
    rows[2] = make_timer_row(true, 60, "強身術/M");
    let last_fire: [Option<std::time::Instant>; 6] = [None; 6];
    assert_eq!(pick_timer_action(&rows, &last_fire, true, now), Some(2));
}

#[test]
fn pick_timer_action_not_yet_due_returns_none() {
    let now = std::time::Instant::now();
    let mut rows: [TimerRow; 6] = std::array::from_fn(|_| make_timer_row(false, 5, ""));
    rows[0] = make_timer_row(true, 60, "強身術/M");
    // last_fire 1 秒前(interval=60s,還差 59 秒)
    let mut last_fire: [Option<std::time::Instant>; 6] = [None; 6];
    last_fire[0] = Some(now - std::time::Duration::from_secs(1));
    assert_eq!(pick_timer_action(&rows, &last_fire, true, now), None);
}
