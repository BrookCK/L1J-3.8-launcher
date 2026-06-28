use super::{
    clean_item_name, format_buff_item, format_command_item, parse_buff_item, parse_fkey,
    parse_suffix_70, strip_qty, strip_state_paren,
};
use crate::aux::runtime::CastTarget;

// ════════════════════════════════════════════════════════════════
// strip_qty:剝物品名 " (數量)" 後綴(數量含千分位逗號)
// ════════════════════════════════════════════════════════════════

#[test]
fn strip_qty_basic_no_count() {
    assert_eq!(strip_qty("變形卷軸"), "變形卷軸");
    assert_eq!(strip_qty("歐西斯弓"), "歐西斯弓");
}

#[test]
fn strip_qty_small_count_strips() {
    assert_eq!(strip_qty("肉 (191)"), "肉");
    assert_eq!(strip_qty("象牙塔變身卷軸 (364)"), "象牙塔變身卷軸");
}

#[test]
fn strip_state_paren_wielded() {
    assert_eq!(strip_state_paren("銀劍 (揮舞)"), "銀劍");
    assert_eq!(strip_state_paren("銀劍(揮舞)"), "銀劍");
}

#[test]
fn strip_state_paren_in_use() {
    assert_eq!(strip_state_paren("胸甲 (使用中)"), "胸甲");
    assert_eq!(strip_state_paren("胸甲(使用中)"), "胸甲");
}

#[test]
fn strip_state_paren_no_state_unchanged() {
    // 描述性括號 + 數量括號都不剝(留給 strip_qty / 不動)
    assert_eq!(
        strip_state_paren("魔法卷軸 (擬似魔法武器)"),
        "魔法卷軸 (擬似魔法武器)"
    );
    assert_eq!(strip_state_paren("金幣 (17,099)"), "金幣 (17,099)");
    assert_eq!(strip_state_paren("純粹的劍"), "純粹的劍");
}

#[test]
fn clean_item_name_combines_qty_and_state() {
    // 比 strip_qty 多一層狀態剝除 — IA/IW=name 過濾用
    assert_eq!(clean_item_name("銀劍 (揮舞)"), "銀劍");
    assert_eq!(clean_item_name("胸甲 (使用中)"), "胸甲");
    // strip_qty 該剝的還是會剝
    assert_eq!(clean_item_name("金幣 (17,099)"), "金幣");
    // 兩者都不適用 → 等同 trim
    assert_eq!(
        clean_item_name("魔法卷軸 (擬似魔法武器)"),
        "魔法卷軸 (擬似魔法武器)"
    );
}

#[test]
fn strip_qty_thousand_with_comma_strips() {
    // 1000+ 的堆疊物會插入千分位,strip_qty 必須容許 inner 含 ','
    assert_eq!(strip_qty("變形卷軸 (1,000)"), "變形卷軸");
    assert_eq!(strip_qty("金幣 (17,099)"), "金幣");
}

#[test]
fn strip_qty_keeps_non_count_parens() {
    // 「精靈水晶(水之元氣)」括號內是中文,不算數量,應保留整串
    assert_eq!(strip_qty("精靈水晶(水之元氣)"), "精靈水晶(水之元氣)");
    // 「魔法書(壞物術)」同上
    assert_eq!(strip_qty("魔法書(壞物術)"), "魔法書(壞物術)");
}

// ════════════════════════════════════════════════════════════════
// Native 格式測試:`name_id_suffix`(3 段底線分隔)
// ════════════════════════════════════════════════════════════════

#[test]
fn item_basic_i() {
    let b = parse_buff_item("肉_-1_I");
    assert_eq!(b.name, "肉");
    assert_eq!(b.id, -1);
    assert_eq!(b.item_type, 'I');
    assert!(matches!(b.cast_target, CastTarget::Item));
}

#[test]
fn magic_m_no_spec() {
    let b = parse_buff_item("保護罩_-1_M");
    assert_eq!(b.name, "保護罩");
    assert_eq!(b.item_type, 'S');
    assert!(matches!(b.cast_target, CastTarget::NoSpec));
}

#[test]
fn magic_mme_self() {
    let b = parse_buff_item("生命之泉_-1_MME");
    assert_eq!(b.name, "生命之泉");
    assert!(matches!(b.cast_target, CastTarget::Self_));
}

#[test]
fn magic_mt_hover() {
    let b = parse_buff_item("寒冰術_-1_MT");
    assert!(matches!(b.cast_target, CastTarget::HoverTarget));
}

#[test]
fn magic_mia_in_use() {
    let b = parse_buff_item("淨化_-1_MIA");
    assert!(matches!(b.cast_target, CastTarget::OnInUseItem(None)));
}

#[test]
fn magic_miw_wielded() {
    let b = parse_buff_item("祝福_-1_MIW");
    assert!(matches!(b.cast_target, CastTarget::OnWieldedItem(None)));
}

/// `/MI` 第 2 段是 target name,不是數字
#[test]
fn magic_mi_named_item() {
    let b = parse_buff_item("提煉魔石_紅魔石_MI");
    assert_eq!(b.name, "提煉魔石");
    match &b.cast_target {
        CastTarget::OnNamedItem(n) => assert_eq!(n, "紅魔石"),
        other => panic!("expected OnNamedItem, got {other:?}"),
    }
}

/// 使用者 UI 直接輸入的 slash 格式 — `<技能>/MIA` `<技能>/MIW` `<技能>/MI=name`
/// 走 legacy 分支(`parse_buff_item_legacy`)。
/// 之前 legacy 分支沒接 MIA/MIW,fall through 變成 USE_ITEM 找不到「暗影之牙」物品。
#[test]
fn slash_mia_in_use() {
    let b = parse_buff_item("暗影之牙/MIA");
    assert_eq!(b.name, "暗影之牙");
    assert_eq!(b.item_type, 'S', "/MIA 必須是技能系");
    assert!(matches!(b.cast_target, CastTarget::OnInUseItem(None)));
}

#[test]
fn slash_miw_wielded() {
    let b = parse_buff_item("暗影之牙/MIW");
    assert_eq!(b.name, "暗影之牙");
    assert_eq!(b.item_type, 'S', "/MIW 必須是技能系");
    assert!(matches!(b.cast_target, CastTarget::OnWieldedItem(None)));
}

#[test]
fn slash_mi_named_item() {
    let b = parse_buff_item("提煉魔石/MI=紅魔石");
    assert_eq!(b.name, "提煉魔石");
    assert_eq!(b.item_type, 'S');
    match &b.cast_target {
        CastTarget::OnNamedItem(n) => assert_eq!(n, "紅魔石"),
        other => panic!("expected OnNamedItem, got {other:?}"),
    }
}

#[test]
fn item_id_drop() {
    let b = parse_buff_item("廢物_-1_ID");
    assert!(matches!(b.cast_target, CastTarget::DropItem));
}

/// 仍未實作的 suffix(IBM/IP/IT)在 3.8 不支援,fall through 成 Item —
/// 不會 crash 也不會誤送 packet。
///
/// 已支援的對映:
/// - IIA/IIW/II → 物品系 OnInUseItem/OnWieldedItem/OnNamedItem
/// - IME → 物品系 SelfItem(`/IME`,自施卷軸)
///
/// (見 parse_suffix_70 / dispatch_use_on_item)
#[test]
fn deprecated_suffix_falls_through_to_item() {
    for raw in ["法術書_-1_IBM", "祝福卷軸_某玩家_IP"] {
        let b = parse_buff_item(raw);
        assert!(
            matches!(b.cast_target, CastTarget::Item),
            "deprecated suffix {raw:?} 應 fall through 成 Item, got {:?}",
            b.cast_target
        );
    }
}

#[test]
fn debug_info() {
    let b = parse_buff_item("DEBUG_-1_INFO");
    assert!(matches!(b.cast_target, CastTarget::Info));
}

#[test]
fn key_macro() {
    let b = parse_buff_item("召喚術_-1_KEY=F1");
    assert_eq!(b.item_type, 'K');
    assert!(matches!(b.cast_target, CastTarget::Key(1)));
    let b2 = parse_buff_item("龍息術_-1_DKEY=F12");
    assert!(matches!(b2.cast_target, CastTarget::DelayKey(12)));
}

#[test]
fn unknown_suffix_treated_as_item() {
    let b = parse_buff_item("未知_-1_XYZ");
    assert!(matches!(b.cast_target, CastTarget::Item));
}

/// round-trip: parse → format 必須回到 canonical 寫法
///
/// Legacy 格式(`<id>_<name>[/<suffix>]`)對應 user INI;
/// Native 格式留給 legacy 不支援的後綴(MT / MIA / MIW / MI / ID / INFO)。
#[test]
fn round_trip_canonical_format() {
    let cases = [
        // legacy(常用)— 輸入跟輸出都是 legacy
        ("-1_肉", "-1_肉"),
        ("-1_保護罩/M", "-1_保護罩/M"),
        ("-1_生命之泉/ME", "-1_生命之泉/ME"),
        ("-1_召喚術/KEY=F1", "-1_召喚術/KEY=F1"),
        ("-1_龍息術/DKEY=F12", "-1_龍息術/DKEY=F12"),
        // Native 來源也要轉回 legacy(因為 parse 後 enum 一樣)
        ("肉_-1_I", "-1_肉"),
        ("保護罩_-1_M", "-1_保護罩/M"),
        ("生命之泉_-1_MME", "-1_生命之泉/ME"),
        // Legacy 不支援的後綴 — Native round-trip(技能系)
        ("寒冰術_-1_MT", "寒冰術_-1_MT"),
        ("淨化_-1_MIA", "淨化_-1_MIA"),
        ("祝福_-1_MIW", "祝福_-1_MIW"),
        ("提煉魔石_紅魔石_MI", "提煉魔石_紅魔石_MI"),
        ("廢物_-1_ID", "廢物_-1_ID"),
        ("DEBUG_-1_INFO", "DEBUG_-1_INFO"),
        // 物品系對既有物品施放 — Native IIA/IIW/II 轉成 legacy 短後綴
        ("淨化卷軸_-1_IIA", "-1_淨化卷軸/IA"),
        ("祝福卷軸_-1_IIW", "-1_祝福卷軸/IW"),
        ("變身卷軸_狼人_II", "-1_變身卷軸/I=狼人"),
        // Native IIA/IIW + 第 2 段名字 → legacy /IA=name /IW=name
        ("淨化卷軸_胸甲_IIA", "-1_淨化卷軸/IA=胸甲"),
        ("祝福卷軸_神聖戰鎚_IIW", "-1_祝福卷軸/IW=神聖戰鎚"),
        // legacy 短後綴 round-trip 自身
        ("-1_淨化卷軸/IA", "-1_淨化卷軸/IA"),
        ("-1_祝福卷軸/IW", "-1_祝福卷軸/IW"),
        ("0_變身卷軸/I=狼人", "0_變身卷軸/I=狼人"),
        ("3_淨化卷軸/IA=胸甲", "3_淨化卷軸/IA=胸甲"),
        ("0_祝福卷軸/IW=神聖戰鎚", "0_祝福卷軸/IW=神聖戰鎚"),
        // /IT 對 hover entity(物品系)— 重用 HoverTarget enum,靠 item_type 分流
        ("0_復活卷軸/IT", "0_復活卷軸/IT"),
        ("復活卷軸_-1_IT", "-1_復活卷軸/IT"),
        // /IME 對自己施放卷軸 — legacy + Native 都轉回 legacy /IME
        ("0_魔法卷軸 (初級治癒術)/IME", "0_魔法卷軸 (初級治癒術)/IME"),
        (
            "魔法卷軸 (初級治癒術)_-1_IME",
            "-1_魔法卷軸 (初級治癒術)/IME",
        ),
    ];
    for (raw, expected) in cases {
        let b = parse_buff_item(raw);
        let formatted = format_buff_item(&b);
        assert_eq!(formatted, expected, "round-trip failed for {raw:?}");
    }
}

// ════════════════════════════════════════════════════════════════
// 舊格式 migration 測試:有 '/' 偵測為舊格式,自動轉成新 CastTarget
// ════════════════════════════════════════════════════════════════

#[test]
fn legacy_slash_m_migrates_to_no_spec() {
    let b = parse_buff_item("0_強力加速術/M");
    assert_eq!(b.name, "強力加速術");
    assert!(matches!(b.cast_target, CastTarget::NoSpec));
}

#[test]
fn legacy_slash_me_migrates_to_self() {
    let b = parse_buff_item("0_加速術/ME");
    assert!(matches!(b.cast_target, CastTarget::Self_));
}

/// [AllAntidote] section 沒 id 前綴 — `解毒術/ME` 直接 name/suffix
#[test]
fn legacy_no_id_prefix_with_me_suffix() {
    let b = parse_buff_item("解毒術/ME");
    assert_eq!(b.name, "解毒術");
    assert_eq!(b.id, -1);
    assert_eq!(b.item_type, 'S');
    assert!(matches!(b.cast_target, CastTarget::Self_));
}

/// [AllAntidote] section 純物品 — 無 / 也無 _,直接是 item name
#[test]
fn legacy_plain_item_name() {
    let b = parse_buff_item("解毒藥水");
    assert_eq!(b.name, "解毒藥水");
    assert_eq!(b.item_type, 'I');
    assert!(matches!(b.cast_target, CastTarget::Item));
}

#[test]
fn legacy_no_suffix_migrates_to_item() {
    let b = parse_buff_item("0_自我加速藥水");
    assert_eq!(b.id, 0);
    assert_eq!(b.name, "自我加速藥水");
    assert!(matches!(b.cast_target, CastTarget::Item));
}

#[test]
fn legacy_slash_m_eq_migrates_to_named_item() {
    // 舊式擴充字尾 /M=name → 等價 native OnNamedItem(對 name 物品施法)
    let b = parse_buff_item("5_寒冰術/M=紅魔石");
    match &b.cast_target {
        CastTarget::OnNamedItem(n) => assert_eq!(n, "紅魔石"),
        other => panic!("expected OnNamedItem after migration, got {other:?}"),
    }
}

#[test]
fn legacy_key_macro_preserved() {
    let b = parse_buff_item("11_召喚術/KEY=F1");
    assert!(matches!(b.cast_target, CastTarget::Key(1)));
}

/// `/IME` legacy 後綴 — 自施卷軸,item_type='I',cast_target=SelfItem
#[test]
fn legacy_slash_ime_for_self_item() {
    let b = parse_buff_item("0_魔法卷軸 (初級治癒術)/IME");
    assert_eq!(b.id, 0);
    assert_eq!(b.name, "魔法卷軸 (初級治癒術)");
    assert_eq!(b.item_type, 'I');
    assert!(matches!(b.cast_target, CastTarget::SelfItem));
}

/// Native `_IME` 後綴 — 同上,parse 結果應該完全一樣
#[test]
fn seven_native_ime_for_self_item() {
    let b = parse_buff_item("魔法卷軸 (高級治癒術)_-1_IME");
    assert_eq!(b.id, -1);
    assert_eq!(b.name, "魔法卷軸 (高級治癒術)");
    assert_eq!(b.item_type, 'I');
    assert!(matches!(b.cast_target, CastTarget::SelfItem));
}

/// SelfItem format → `/IME`(永遠輸出 legacy 短後綴,跟 /IT /IA /IW 同慣例)
#[test]
fn format_self_item_emits_ime() {
    let b = crate::aux::runtime::BuffItem {
        id: 5,
        name: "復活卷軸".to_string(),
        item_type: 'I',
        cast_target: CastTarget::SelfItem,
    };
    assert_eq!(format_buff_item(&b), "5_復活卷軸/IME");
}

#[test]
fn legacy_slash_ia_for_item_on_in_use() {
    // /IA = 對使用中防具施放卷軸(物品系,找第一件)
    let b = parse_buff_item("3_淨化卷軸/IA");
    assert_eq!(b.id, 3);
    assert_eq!(b.name, "淨化卷軸");
    assert_eq!(b.item_type, 'I');
    assert!(matches!(b.cast_target, CastTarget::OnInUseItem(None)));
}

#[test]
fn legacy_slash_ia_eq_for_named_in_use_item() {
    // /IA=<裝備名> = 對指定名稱的使用中裝備
    let b = parse_buff_item("3_淨化卷軸/IA=胸甲");
    assert_eq!(b.name, "淨化卷軸");
    assert_eq!(b.item_type, 'I');
    match &b.cast_target {
        CastTarget::OnInUseItem(Some(n)) => assert_eq!(n, "胸甲"),
        other => panic!("/IA=胸甲 應 OnInUseItem(Some('胸甲')), got {other:?}"),
    }
}

#[test]
fn legacy_slash_iw_for_item_on_wielded() {
    // /IW = 對揮舞武器施放卷軸(物品系,找第一件)
    let b = parse_buff_item("0_祝福卷軸/IW");
    assert_eq!(b.id, 0);
    assert_eq!(b.name, "祝福卷軸");
    assert_eq!(b.item_type, 'I');
    assert!(matches!(b.cast_target, CastTarget::OnWieldedItem(None)));
}

#[test]
fn legacy_slash_it_for_item_on_hover_entity() {
    // /IT = 對鼠標 hover entity(物品系)— enum 重用 HoverTarget,靠 item_type 區分
    let b = parse_buff_item("0_復活卷軸/IT");
    assert_eq!(b.id, 0);
    assert_eq!(b.name, "復活卷軸");
    assert_eq!(b.item_type, 'I');
    assert!(matches!(b.cast_target, CastTarget::HoverTarget));
}

#[test]
fn legacy_slash_iw_eq_for_named_wielded_item() {
    // /IW=<武器名> = 對指定名稱的揮舞武器
    let b = parse_buff_item("0_祝福卷軸/IW=神聖戰鎚");
    assert_eq!(b.item_type, 'I');
    match &b.cast_target {
        CastTarget::OnWieldedItem(Some(n)) => assert_eq!(n, "神聖戰鎚"),
        other => panic!("/IW=神聖戰鎚 應 OnWieldedItem(Some('神聖戰鎚')), got {other:?}"),
    }
}

#[test]
fn legacy_slash_i_eq_for_named_item() {
    // /I=name = 對指定名稱物品施放卷軸(物品系)
    let b = parse_buff_item("0_變身卷軸/I=狼人");
    assert_eq!(b.id, 0);
    assert_eq!(b.name, "變身卷軸");
    assert_eq!(b.item_type, 'I');
    match &b.cast_target {
        CastTarget::OnNamedItem(n) => assert_eq!(n, "狼人"),
        other => panic!("/I=狼人 應 OnNamedItem('狼人'), got {other:?}"),
    }
}

#[test]
fn legacy_slash_it_eq_for_named_entity() {
    // /IT=<entity名> = 全自動對指定名玩家/召喚物施放(物品系)
    let b = parse_buff_item("0_治癒卷軸/IT=阿狗");
    assert_eq!(b.item_type, 'I');
    match &b.cast_target {
        CastTarget::OnNamedEntity(n) => assert_eq!(n, "阿狗"),
        other => panic!("/IT=阿狗 應 OnNamedEntity('阿狗'), got {other:?}"),
    }
    // Native 風格(name 在第 2 段)同樣會被 parse_suffix_70 認到
    let b2 = parse_buff_item("治癒卷軸_召喚物A_IT");
    assert_eq!(b2.item_type, 'I');
    match &b2.cast_target {
        CastTarget::OnNamedEntity(n) => assert_eq!(n, "召喚物A"),
        other => panic!("治癒卷軸_召喚物A_IT 應 OnNamedEntity, got {other:?}"),
    }
}

#[test]
fn it_no_name_still_hover_target() {
    // 沒帶 = 的 /IT 維持半自動 USE_ITEM 快捷鍵(HoverTarget)
    let b = parse_buff_item("0_復活卷軸/IT");
    assert!(matches!(b.cast_target, CastTarget::HoverTarget));
}

// ════════════════════════════════════════════════════════════════
// parse_suffix_70 直接測試
// ════════════════════════════════════════════════════════════════

#[test]
fn suffix_70_direct() {
    assert!(matches!(parse_suffix_70("M", "-1").1, CastTarget::NoSpec));
    assert!(matches!(parse_suffix_70("MME", "-1").1, CastTarget::Self_));
    assert!(matches!(
        parse_suffix_70("MT", "-1").1,
        CastTarget::HoverTarget
    ));
    assert!(matches!(
        parse_suffix_70("MIA", "-1").1,
        CastTarget::OnInUseItem(None)
    ));
    assert!(matches!(
        parse_suffix_70("MIW", "-1").1,
        CastTarget::OnWieldedItem(None)
    ));
    assert!(matches!(parse_suffix_70("I", "-1").1, CastTarget::Item));
    assert!(matches!(
        parse_suffix_70("ID", "-1").1,
        CastTarget::DropItem
    ));
    assert!(matches!(parse_suffix_70("INFO", "-1").1, CastTarget::Info));
    // MI 把 id_or_target 作為 name 帶入
    match parse_suffix_70("MI", "紅魔石").1 {
        CastTarget::OnNamedItem(n) => assert_eq!(n, "紅魔石"),
        other => panic!("MI 應 OnNamedItem, got {other:?}"),
    }
    // 物品系新後綴 — 對既有物品施放(II packet),預設 None(找第一件)
    assert!(matches!(
        parse_suffix_70("IA", "-1").1,
        CastTarget::OnInUseItem(None)
    ));
    assert!(matches!(
        parse_suffix_70("IW", "-1").1,
        CastTarget::OnWieldedItem(None)
    ));
    // /IT 物品系對 hover entity — 重用 HoverTarget 但 item_type='I'
    let (it_type_t, ct_t) = parse_suffix_70("IT", "-1");
    assert_eq!(it_type_t, 'I');
    assert!(matches!(ct_t, CastTarget::HoverTarget));
    // Native 長後綴寫法 IIA/IIW 與短後綴 IA/IW 同義
    assert!(matches!(
        parse_suffix_70("IIA", "-1").1,
        CastTarget::OnInUseItem(None)
    ));
    assert!(matches!(
        parse_suffix_70("IIW", "-1").1,
        CastTarget::OnWieldedItem(None)
    ));
    // Native IIA/IIW 第 2 段為非數字 → 帶 Some(name) 過濾
    match parse_suffix_70("IIA", "胸甲").1 {
        CastTarget::OnInUseItem(Some(n)) => assert_eq!(n, "胸甲"),
        other => panic!("IIA + 胸甲 應 OnInUseItem(Some('胸甲')), got {other:?}"),
    }
    match parse_suffix_70("IIW", "戰鎚").1 {
        CastTarget::OnWieldedItem(Some(n)) => assert_eq!(n, "戰鎚"),
        other => panic!("IIW + 戰鎚 應 OnWieldedItem(Some('戰鎚')), got {other:?}"),
    }
    // Native II 把第二段 name 帶入 — item_type='I'
    let (it_type, ct) = parse_suffix_70("II", "腐心毒之劍");
    assert_eq!(it_type, 'I');
    match ct {
        CastTarget::OnNamedItem(n) => assert_eq!(n, "腐心毒之劍"),
        other => panic!("II 應 OnNamedItem('I'), got {other:?}"),
    }
    // IA / IW 應該回 item_type = 'I'(走 dispatch_item 路徑)
    assert_eq!(parse_suffix_70("IA", "-1").0, 'I');
    assert_eq!(parse_suffix_70("IW", "-1").0, 'I');
    // 仍未實作的 suffix → fall through 成 Item(IME 已實作走 SelfItem)
    for dep in ["IBM", "IP"] {
        assert!(
            matches!(parse_suffix_70(dep, "-1").1, CastTarget::Item),
            "deprecated suffix {dep:?} 應 fall through 成 Item"
        );
    }
    // IME 走 SelfItem(`/IME`,自施卷軸)
    assert!(matches!(
        parse_suffix_70("IME", "-1").1,
        CastTarget::SelfItem
    ));
}

#[test]
fn command_format_hides_legacy_id_prefix_for_fkeys() {
    let cases = [
        ("0_BlessScroll", "BlessScroll"),
        ("0_BlessScroll/I", "BlessScroll"),
        ("0_BlessScroll/IW=HolySword", "BlessScroll/IW=HolySword"),
        ("0_Haste/ME", "Haste/ME"),
        ("Skill_7_MIA", "Skill/MIA"),
        ("11_Summon/KEY=F1", "Summon/KEY=F1"),
    ];
    for (raw, expected) in cases {
        let b = parse_buff_item(raw);
        assert_eq!(
            format_command_item(&b),
            expected,
            "command format failed for {raw:?}"
        );
    }
}

#[test]
fn fkey_bounds() {
    assert_eq!(parse_fkey("F1"), Some(1));
    assert_eq!(parse_fkey("F12"), Some(12));
    assert_eq!(parse_fkey("f5"), Some(5));
    assert_eq!(parse_fkey("F0"), None);
    assert_eq!(parse_fkey("F13"), None);
    assert_eq!(parse_fkey("X1"), None);
}
