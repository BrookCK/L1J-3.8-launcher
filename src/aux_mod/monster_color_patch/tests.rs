use super::*;

fn sample_entity(level: u8) -> Vec<u8> {
    let mut raw = vec![0u8; ENTITY_READ_LEN];
    raw[0..4].copy_from_slice(&PLAYER_VFPTR.to_le_bytes());
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&200_000_123u32.to_le_bytes());
    raw[ENTITY_KIND_OFFSET as usize] = 0x00;
    raw[ENTITY_SPRITE_OFFSET as usize..ENTITY_SPRITE_OFFSET as usize + 2]
        .copy_from_slice(&1022u16.to_le_bytes());
    raw[ENTITY_COLOR_OFFSET as usize..ENTITY_COLOR_OFFSET as usize + 2]
        .copy_from_slice(&DEFAULT_NAME_COLOR.to_le_bytes());
    raw[ENTITY_LEVEL_CANDIDATE_OFFSET as usize] = level;
    raw[ENTITY_NAME_PTR_OFFSET as usize..ENTITY_NAME_PTR_OFFSET as usize + 4]
        .copy_from_slice(&0x1234_5678u32.to_le_bytes());
    raw[ENTITY_MAP_OFFSET as usize..ENTITY_MAP_OFFSET as usize + 4]
        .copy_from_slice(&4u32.to_le_bytes());
    raw
}

#[test]
fn selects_lhx_monster_color_from_level_delta() {
    assert_eq!(select_level_color(129, 99), MonsterNameColor::DarkRed);
    assert_eq!(select_level_color(128, 99), MonsterNameColor::LightRed);
    assert_eq!(select_level_color(119, 99), MonsterNameColor::LightRed);
    assert_eq!(select_level_color(118, 99), MonsterNameColor::Blue);
    assert_eq!(select_level_color(110, 99), MonsterNameColor::Blue);
    assert_eq!(select_level_color(109, 99), MonsterNameColor::Green);
    assert_eq!(select_level_color(104, 99), MonsterNameColor::Green);
    assert_eq!(select_level_color(100, 99), MonsterNameColor::Green);
    assert_eq!(select_level_color(99, 99), MonsterNameColor::White);
    assert_eq!(select_level_color(8, 99), MonsterNameColor::White);
    assert_eq!(select_level_color(8, 5), MonsterNameColor::Green);
}

#[test]
fn low_level_white_uses_client_default_white() {
    assert_eq!(MonsterNameColor::White.rgb565(), DEFAULT_NAME_COLOR);
    assert!(!is_feature_color(DEFAULT_NAME_COLOR));
}

#[test]
fn low_level_white_band_is_not_written_over_client_color() {
    assert_eq!(select_level_color(8, 99), MonsterNameColor::White);
    assert_eq!(patch_color_for_level(8, 99), None);
}

#[test]
fn non_white_level_bands_are_patch_colors() {
    assert_eq!(
        patch_color_for_level(8, 5),
        Some(MonsterNameColor::Green.rgb565())
    );
    assert_eq!(
        patch_color_for_level(104, 99),
        Some(MonsterNameColor::Green.rgb565())
    );
    assert_eq!(
        patch_color_for_level(110, 99),
        Some(MonsterNameColor::Blue.rgb565())
    );
    assert_eq!(
        patch_color_for_level(119, 99),
        Some(MonsterNameColor::LightRed.rgb565())
    );
    assert_eq!(
        patch_color_for_level(129, 99),
        Some(MonsterNameColor::DarkRed.rgb565())
    );
}

#[test]
fn blue_level_band_uses_unambiguous_blue() {
    assert_eq!(MonsterNameColor::Blue.rgb565(), 0x001F);
}

#[test]
fn accepts_monster_level_after_two_stable_samples() {
    let levels = Arc::new(Mutex::new(HashMap::new()));
    assert_eq!(resolve_monster_level(&levels, 0x1234_0000, Some(65)), None);
    assert_eq!(
        resolve_monster_level(&levels, 0x1234_0000, Some(65)),
        Some(65)
    );
}

#[test]
fn resets_monster_level_when_candidate_changes() {
    let levels = Arc::new(Mutex::new(HashMap::new()));
    assert_eq!(resolve_monster_level(&levels, 0x1234_0000, Some(65)), None);
    assert_eq!(resolve_monster_level(&levels, 0x1234_0000, Some(41)), None);
    assert_eq!(
        resolve_monster_level(&levels, 0x1234_0000, Some(41)),
        Some(41)
    );
    assert_eq!(resolve_monster_level(&levels, 0x1234_0000, None), None);
}

#[test]
fn parses_visible_entity_candidate_level_and_color_field() {
    let mut raw = sample_entity(12);
    raw[0x50] = 0;
    raw[0x54] = 0;
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();
    assert!(entity.is_visible_world_entity());
    assert_eq!(entity.sprite, 1022);
    assert_eq!(entity.server_id, 200_000_123);
    assert_eq!(entity.entity_kind, 0x00);
    assert!(entity.is_probable_monster());
    assert_eq!(entity.map, 4);
    assert_eq!(entity.name_ptr, 0x1234_5678);
    assert_eq!(entity.current_color, DEFAULT_NAME_COLOR);
    assert_eq!(entity.sampled_level_byte, 12);
    assert_eq!(entity.monster_level, Some(12));
}

#[test]
fn treats_implausible_candidate_level_as_unknown_for_current_world_entities() {
    let raw = sample_entity(197);
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();
    assert_eq!(entity.sampled_level_byte, 197);
    assert_eq!(entity.monster_level, None);
}

#[test]
fn rejects_non_visible_or_non_entity_buffers() {
    let mut raw = sample_entity(10);
    raw[0..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    assert!(EntitySnapshot::parse(0x1234_0000, &raw).is_none());

    let mut raw = sample_entity(10);
    raw[ENTITY_MAP_OFFSET as usize..ENTITY_MAP_OFFSET as usize + 4]
        .copy_from_slice(&0u32.to_le_bytes());
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();
    assert!(!entity.is_visible_world_entity());
}

#[test]
fn normalizes_trusted_level_range() {
    assert_eq!(normalize_level(1), Some(1));
    assert_eq!(normalize_level(120), Some(120));
    assert_eq!(normalize_level(0), None);
    assert_eq!(normalize_level(121), None);
}

#[test]
fn decodes_direct_player_level_obfuscated_slot() {
    assert_eq!(decode_obfuscated_index(OBFUSCATED_INDEX_XOR ^ 2), Some(2));
    assert_eq!(decode_obfuscated_value(0x6F57_5B01, 0x6F57_5B07), 6);
    assert_eq!(normalize_level(6), Some(6));
}

#[test]
fn rejects_implausible_obfuscated_index() {
    assert_eq!(decode_obfuscated_index(OBFUSCATED_INDEX_XOR ^ 0x1000), None);
}

#[test]
fn rejects_visible_town_npcs_and_static_world_objects() {
    let mut raw = sample_entity(8);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&81_356u32.to_le_bytes());
    raw[ENTITY_KIND_OFFSET as usize] = 0x03;
    raw[ENTITY_SPRITE_OFFSET as usize..ENTITY_SPRITE_OFFSET as usize + 2]
        .copy_from_slice(&2143u16.to_le_bytes());
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();
    assert_eq!(
        resolve_entity_monster_level(entity, Some(12), false).resolved_level,
        None
    );

    let mut raw = sample_entity(0);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&200_030_391u32.to_le_bytes());
    raw[ENTITY_KIND_OFFSET as usize] = ENTITY_KIND_WORLD_MONSTER;
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();
    assert_eq!(
        resolve_entity_monster_level(entity, Some(SPRITE_TYPE_MONSTER), false).resolved_level,
        None
    );
}

#[test]
fn does_not_special_case_sprite_ids_when_runtime_type_is_not_monster() {
    let mut raw = sample_entity(1);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&81_356u32.to_le_bytes());
    raw[ENTITY_KIND_OFFSET as usize] = ENTITY_KIND_WORLD_MONSTER;
    raw[ENTITY_SPRITE_OFFSET as usize..ENTITY_SPRITE_OFFSET as usize + 2]
        .copy_from_slice(&2143u16.to_le_bytes());
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();
    assert_eq!(
        resolve_entity_monster_level(entity, Some(12), false).resolved_level,
        None
    );

    assert_eq!(
        resolve_entity_monster_level(entity, None, false).resolved_level,
        None
    );
}

#[test]
fn rejects_monster_sprite_when_entity_kind_is_not_world_monster() {
    let mut raw = sample_entity(10);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&200_004_962u32.to_le_bytes());
    raw[ENTITY_KIND_OFFSET as usize] = 0x03;
    raw[ENTITY_SPRITE_OFFSET as usize..ENTITY_SPRITE_OFFSET as usize + 2]
        .copy_from_slice(&1022u16.to_le_bytes());
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();

    assert_eq!(
        resolve_entity_monster_level(entity, Some(SPRITE_TYPE_MONSTER), false).resolved_level,
        None
    );
}

#[test]
fn keeps_previously_colored_monster_when_runtime_kind_changes_temporarily() {
    let mut raw = sample_entity(10);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&200_031_075u32.to_le_bytes());
    raw[ENTITY_KIND_OFFSET as usize] = 0x01;
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();

    assert_eq!(
        resolve_entity_monster_level(entity, Some(SPRITE_TYPE_MONSTER), true).resolved_level,
        Some(10)
    );
}

#[test]
fn does_not_keep_town_npc_colored_when_kind_is_npc_state() {
    let mut raw = sample_entity(65);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&200_029_192u32.to_le_bytes());
    raw[ENTITY_KIND_OFFSET as usize] = 0x03;
    raw[ENTITY_SPRITE_OFFSET as usize..ENTITY_SPRITE_OFFSET as usize + 2]
        .copy_from_slice(&335u16.to_le_bytes());
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();

    assert_eq!(
        resolve_entity_monster_level(entity, Some(5), true).resolved_level,
        None
    );
}

#[test]
fn rejects_local_player_identity_even_when_sprite_type_is_monster() {
    let mut raw = sample_entity(80);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&0xBFu32.to_le_bytes());
    let entity = EntitySnapshot::parse(0x2233_0000, &raw).unwrap();
    let identity = LocalPlayerIdentity {
        ptr: 0x1111_0000,
        target_id: 0xBF,
        self_char_id: 0xBF,
        name: "Me".to_string(),
        aliases: vec!["Me".to_string()],
    };

    assert!(is_local_player_entity(
        &identity,
        entity,
        &["Me".to_string()]
    ));
    assert_eq!(
        resolve_entity_monster_level_for_identity(
            &identity,
            entity,
            &["Me".to_string()],
            Some(SPRITE_TYPE_MONSTER),
            false
        )
        .resolved_level,
        None
    );
}

#[test]
fn rejects_local_player_world_avatar_by_name_when_object_id_differs() {
    let mut raw = sample_entity(80);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&200_123_456u32.to_le_bytes());
    let entity = EntitySnapshot::parse(0x2233_0000, &raw).unwrap();
    let identity = LocalPlayerIdentity {
        ptr: 0x1111_0000,
        target_id: 0xBF,
        self_char_id: 0xBF,
        name: "MyChar".to_string(),
        aliases: vec!["MyChar".to_string()],
    };

    assert!(is_local_player_entity(
        &identity,
        entity,
        &["MyChar".to_string()]
    ));
    assert_eq!(
        resolve_entity_monster_level_for_identity(
            &identity,
            entity,
            &["MyChar".to_string()],
            Some(SPRITE_TYPE_MONSTER),
            false
        )
        .resolved_level,
        None
    );
}

#[test]
fn rejects_local_player_world_avatar_by_alias_name_when_primary_name_differs() {
    let mut raw = sample_entity(80);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&200_123_456u32.to_le_bytes());
    let entity = EntitySnapshot::parse(0x2233_0000, &raw).unwrap();
    let identity = LocalPlayerIdentity {
        ptr: 0x1111_0000,
        target_id: 0xBF,
        self_char_id: 0xBF,
        name: "LoginName".to_string(),
        aliases: vec!["衣衫衣衫".to_string()],
    };
    let entity_names = vec!["SomeHeapName".to_string(), "衣衫衣衫".to_string()];

    assert!(is_local_player_entity(&identity, entity, &entity_names));
    assert_eq!(
        resolve_entity_monster_level_for_identity(
            &identity,
            entity,
            &entity_names,
            Some(SPRITE_TYPE_MONSTER),
            false
        )
        .resolved_level,
        None
    );
}

#[test]
fn accepts_live_high_object_id_monsters_with_valid_level() {
    let mut raw = sample_entity(8);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&200_031_075u32.to_le_bytes());
    raw[ENTITY_SPRITE_OFFSET as usize..ENTITY_SPRITE_OFFSET as usize + 2]
        .copy_from_slice(&3864u16.to_le_bytes());
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();

    assert_eq!(
        resolve_entity_monster_level(entity, Some(SPRITE_TYPE_MONSTER), false).resolved_level,
        Some(8)
    );
}

#[test]
fn accepts_unknown_sprite_type_world_monster_with_high_object_id_and_valid_level() {
    let mut raw = sample_entity(8);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&200_031_075u32.to_le_bytes());
    raw[ENTITY_KIND_OFFSET as usize] = ENTITY_KIND_WORLD_MONSTER;
    raw[ENTITY_SPRITE_OFFSET as usize..ENTITY_SPRITE_OFFSET as usize + 2]
        .copy_from_slice(&3865u16.to_le_bytes());
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();

    assert_eq!(
        resolve_entity_monster_level(entity, None, false).resolved_level,
        Some(8)
    );
}

#[test]
fn keeps_previously_colored_high_object_id_monster_when_sprite_type_is_temporarily_unknown() {
    let mut raw = sample_entity(10);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&200_031_075u32.to_le_bytes());
    raw[ENTITY_KIND_OFFSET as usize] = 0x01;
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();

    assert_eq!(
        resolve_entity_monster_level(entity, None, true).resolved_level,
        Some(10)
    );
}

#[test]
fn accepts_runtime_level_monsters_without_id_range() {
    let mut raw = sample_entity(8);
    raw[ENTITY_SERVER_ID_OFFSET as usize..ENTITY_SERVER_ID_OFFSET as usize + 4]
        .copy_from_slice(&123_456_789u32.to_le_bytes());
    let entity = EntitySnapshot::parse(0x1234_0000, &raw).unwrap();

    assert_eq!(
        resolve_entity_monster_level(entity, Some(SPRITE_TYPE_MONSTER), false).resolved_level,
        Some(8)
    );
}
#[test]
fn name_render_shellcode_gates_feature_color_by_world_object_id() {
    let sc = build_name_render_shellcode(0x1000_0000, 0x2000_0000);

    assert!(sc
        .windows(3)
        .any(|w| w == [0x8B, 0x50, ENTITY_SERVER_ID_OFFSET as u8]));
    assert!(sc.windows(2).any(|w| w == [0x81, 0xFA]));
}

#[test]
fn name_render_shellcode_gates_feature_color_by_render_marker_table() {
    let sc = build_name_render_shellcode(0x1000_0000, 0x2000_0000);

    assert!(sc.windows(4).any(|w| w == 0x2000_0000u32.to_le_bytes()));
    assert!(sc.windows(2).any(|w| w == [0x39, 0x06])); // cmp [esi],eax
}

// 刻意斷言 const 不變式:這些是編譯期 feature 開關,測試固定其預期值,改錯會被擋下
#[allow(clippy::assertions_on_constants)]
#[test]
fn runtime_render_hooks_are_limited_to_normal_world_name_call_site() {
    assert!(ENABLE_MONSTER_NAME_RENDER_HOOKS);
    assert!(ENABLE_SELECTED_NAME_COLOR_HOOKS);
    assert!(!ENABLE_OVERHEAD_TEXT_COLOR_HOOK);
}

#[test]
fn overhead_text_color_shellcode_gates_by_render_marker_table() {
    let sc = build_overhead_text_color_shellcode(0x1000_0000, 0x2000_0000);

    assert!(sc.windows(3).any(|w| w == [0x8B, 0x55, 0xCC])); // mov edx,[ebp-0x34]
    assert!(sc.windows(4).any(|w| w == 0x2000_0000u32.to_le_bytes()));
    assert!(sc.windows(2).any(|w| w == [0x39, 0x16])); // cmp [esi],edx
    assert!(sc
        .windows(4)
        .any(|w| w == [0x66, 0x8B, 0x4A, ENTITY_COLOR_OFFSET as u8]));
    assert!(sc
        .windows(7)
        .any(|w| w == [0x66, 0x89, 0x88, 0x9C, 0x03, 0x00, 0x00]));
}

#[test]
fn overhead_text_color_reset_shellcode_restores_feature_color_from_text_entity() {
    let sc = build_overhead_text_color_reset_shellcode(0x1000_0000, 0x2000_0000);

    assert!(sc.windows(4).any(|w| w == 0x0095_FB94u32.to_le_bytes()));
    assert!(sc
        .windows(6)
        .any(|w| w == [0x8B, 0x8A, 0x98, 0x03, 0x00, 0x00])); // mov ecx,[edx+0x398]
    assert!(sc.windows(4).any(|w| w == 0x2000_0000u32.to_le_bytes()));
    assert!(sc.windows(2).any(|w| w == [0x39, 0x0E])); // cmp [esi],ecx
    assert!(sc
        .windows(4)
        .any(|w| w == [0x0F, 0xB7, 0x49, ENTITY_COLOR_OFFSET as u8])); // movzx ecx,[ecx+0x30]
    assert!(sc
        .windows(7)
        .any(|w| w == [0x66, 0x89, 0x82, 0x9C, 0x03, 0x00, 0x00])); // mov [edx+0x39C],ax
    assert!(sc
        .windows(4)
        .any(|w| w == (MonsterNameColor::Green.rgb565() as u32).to_le_bytes()));
}

#[test]
fn recognizes_legacy_text_draw_jmp_hook_for_restore() {
    let original = [0x55, 0x8B, 0xEC, 0x6A, 0xFF];
    let legacy_jmp = [0xE9, 0x11, 0x22, 0x33, 0x44];

    assert!(is_legacy_jmp_hook(&legacy_jmp, &original));
    assert!(!is_legacy_jmp_hook(&original, &original));
}

#[test]
fn text_draw_shellcode_gates_feature_color_by_render_marker_table() {
    let sc = build_text_draw_color_fix_shellcode(
        0x1000_0000,
        0x2000_0000,
        &TEXT_DRAW_FN_ORIGINAL_BYTES,
        TEXT_DRAW_FN_FALLTHROUGH_ADDR,
        SELECTED_TEXT_DRAW_RETURNS,
    );

    assert!(sc
        .windows(6)
        .any(|w| w == [0x8B, 0x15, 0x40, 0xF4, 0xAB, 0x00])); // mov edx,[0xABF440]
    assert!(sc.windows(4).any(|w| w == 0x2000_0000u32.to_le_bytes()));
    assert!(sc.windows(2).any(|w| w == [0x39, 0x16])); // cmp [esi],edx
}

#[test]
fn identifies_feature_palette_colors() {
    assert!(is_feature_color(MonsterNameColor::Green.rgb565()));
    assert!(!is_feature_color(DEFAULT_NAME_COLOR));
}

#[test]
fn parses_runtime_sprite_types_from_decrypted_client_memory_text() {
    let raw = b"S12338 0 41211\r\n#94 48 sword orc 102.type(10)\n\0"
        .iter()
        .chain(b"148 48 kent castle guard 102.type(12)\n\0")
        .chain(b"335 32 guard archer 102.type(5)\n\0")
        .chain(b"3864 48=94 orc fighter morph\n\0")
        .copied()
        .collect::<Vec<_>>();

    let records = parse_runtime_sprite_type_records(&raw);
    let types = resolve_runtime_sprite_types(records);

    assert_eq!(types.get(&94), Some(&SPRITE_TYPE_MONSTER));
    assert_eq!(types.get(&3864), Some(&SPRITE_TYPE_MONSTER));
    assert_eq!(types.get(&148), Some(&12));
    assert_eq!(types.get(&335), Some(&5));
}

#[test]
fn direct_sprite_type_zero_overrides_alias_resolution() {
    // \x00 = NUL 記錄分隔符(原 \0336 的 \0,改 \x00 表意明確、位元組完全相同)
    let raw = b"#94 48 sword orc 102.type(10)\n\x00336 32=94 guard archer shadow 102.type(0)\n";

    let records = parse_runtime_sprite_type_records(raw);
    let types = resolve_runtime_sprite_types(records);

    assert_eq!(types.get(&336), Some(&0));
}
