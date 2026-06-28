use std::sync::Arc;

use parking_lot::RwLock;
use windows::Win32::Foundation::HANDLE;

pub(super) mod selectors;

use super::AuxSettings;
use crate::log_line;

/// 狀態頁 tick — 自動吃肉 + 解毒 + 磨刀石 + 變身
///
/// 為什麼分區 tick 而非每功能一個 thread:這四個動作共享 game_state guard +
/// DrinkHandle,合在一條 polling thread 跑省 thread context switch。
/// `cooldowns` 用 `&'static str` key(每個 feature 一條 cooldown,5 秒節流)。
pub(super) fn status_tick(
    h: HANDLE,
    settings: &Arc<RwLock<AuxSettings>>,
    drink: &Arc<RwLock<Option<Arc<crate::aux::drink_hook::DrinkHandle>>>>,
    spell_book: &Arc<RwLock<Option<crate::aux::spell_book::SpellBook>>>,
    cooldowns: &mut std::collections::HashMap<&'static str, std::time::Instant>,
) {
    let s = settings.read().clone();
    if !selectors::status_work_enabled(&s) {
        return; // 四個功能全關
    }

    // game_state guard(同 buff_tick / drink_tick)
    if !super::guards::process_in_game_world(h) {
        return;
    }

    let dh = match super::guards::clone_drink_handle(drink) {
        Some(h) => h,
        None => return,
    };

    // SpellBook lazy build — buff_tick 在 buff 未啟用時 early-return 不會建表,
    // 但 status_tick(解毒術等技能類動作)需要靠它查 packed。在這裡也建一次,
    // 確保即使 user 沒勾任何 buff 也能跑技能解毒。換角後 ensure_fresh 偵測 stale 並重建。
    if s.status_antidote_enabled {
        let _ = crate::aux::spell_book::ensure_fresh(h, spell_book, "status");
    }

    let now = std::time::Instant::now();
    // 解毒 / 卡毒走標準 5s(server RTT + 動畫時間)
    const COOLDOWN_POISON: std::time::Duration = std::time::Duration::from_secs(5);
    // 吃肉走 1s(USE_ITEM 即時動作不阻擋,要快點補滿)
    const COOLDOWN_EAT: std::time::Duration = std::time::Duration::from_secs(1);

    // 1. 自動吃肉 — 飽食度沒滿就吃(raw < FOOD_MAX 即觸發,1s cooldown 防 packet 連發)
    //
    // raw 是 0..225 的 byte,225 = 100%。沒設可調門檻 — 開了就要吃滿。
    if s.status_eat_meat {
        let due = selectors::cooldown_due(cooldowns.get("eat_meat").copied(), now, COOLDOWN_EAT);
        if due {
            if let Some(addr) = crate::aux::address::FOOD_LEVEL {
                if let Ok(b) = crate::platform::memory::read_bytes(h, addr, 1) {
                    let raw = b.first().copied().unwrap_or(0xFF) as u32;
                    let max = crate::aux::address::FOOD_LEVEL_DIVISOR;
                    if selectors::eat_meat_needed(raw, max) {
                        cooldowns.insert("eat_meat", now);
                        if let Ok(items) = crate::aux::inventory::list_items(h) {
                            let found = items.iter().find(|it| {
                                crate::aux::lhx_window::strip_qty(&it.name_lossy()) == "肉"
                            });
                            if let Some(it) = found {
                                let pct = raw * 100 / max;
                                log_line!(
                                    "[status] 自動吃肉:飽食度 {}%({}/{})→ 用 entry=0x{:08X}",
                                    pct,
                                    raw,
                                    max,
                                    it.entry_addr
                                );
                                if let Err(e) = dh.execute_use_item(h, it.entry_addr) {
                                    log_line!("[status] 吃肉 execute 失敗: {e:#}");
                                }
                            } else {
                                log_line!("[status] 飽食度未滿但背包沒「肉」");
                            }
                        }
                    }
                }
            }
        }
    }

    // 2. 解毒 / 卡毒 — 偵測來源:poison_hook 讀 `player+0x20` bit 5
    //
    // 為什麼不用 byte_table:3.8 PoisonHandler 對毒呼叫 apply_buff(state_id, type=0),
    // type=0 路徑不寫 byte_table 只設 timer,所以 byte 永遠是 0,中毒偵測必須換來源。
    // poison_hook 改成直接讀 player struct 的 status bit(2026-05-02 真實怪物毒驗證),
    // 無 hook、無 patch,反偵測零風險。
    if s.status_antidote_enabled && !s.status_antidote_item.is_empty() {
        let poisoned = crate::aux::poison_hook::is_damage_poisoned(h);
        if selectors::antidote_action_enabled(&s, poisoned) {
            let due =
                selectors::cooldown_due(cooldowns.get("antidote").copied(), now, COOLDOWN_POISON);
            if due {
                cooldowns.insert("antidote", now);
                fire_status_action(h, &s.status_antidote_item, &dh, spell_book, "antidote");
            }
        }
    }

    // 3. 自動磨刀石 — 偵測揮舞中武器的 description 含「損壞度」就磨
    //
    // 機制(2026-05-02 spy log + caller RE 驗證):
    //   - 揮舞中武器 entry: list_items() 找 name 含「(揮舞)」的 item
    //   - description string: item_entry+0xA8(3.8 偏移)
    //   - 觸發條件:description 含「損壞度」(Big5 B7 6C C3 61 AB D7)
    //   - 動作:**直接送 II packet**(opcode 0xA4, source=磨刀石.item_param,
    //     target=揮舞武器.item_param)。
    //     為什麼不 call 遊戲 0x00410570 wrapper:該函數從 RemoteThread 進入時 ECX
    //     結構未初始化,server 收到不完整 packet 會踢線。
    //
    // 1 秒 cooldown — tick 是 500ms,實際上每 1~1.5 秒磨一次。
    if s.status_whetstone {
        const COOLDOWN_WHETSTONE: std::time::Duration = std::time::Duration::from_secs(1);
        let due =
            selectors::cooldown_due(cooldowns.get("whetstone").copied(), now, COOLDOWN_WHETSTONE);
        if due {
            if let Err(e) = whetstone_tick(h, &dh) {
                // log_line! 一次就好,避免每秒噴 — 用 cooldown 強制節流
                cooldowns.insert("whetstone", now);
                log_line!("[status][whetstone] {e:#}");
            } else {
                cooldowns.insert("whetstone", now);
            }
        }
    }

    // 4. 自動變身(普通變身藥水模式)
    //
    // 目前僅實作模式 1(普通 USE_ITEM):選單選一個變身物品(e.g. 狼人變身藥水),
    // 偵測到「沒在變身」就 USE_ITEM。
    //
    // 模式 2(變形卷軸_選項_IP):需要 RE 3.8 的 IP packet opcode + packet 結構,
    // 暫不實作 — `status_transform_cond` 欄位先保留 UI 但不影響行為。
    //
    // state byte:`buff_table[39]` 為變身 flag,進場後有變身 spr_id 時 server 會把
    // 該 byte 設為 1;偵測 byte == 0 才觸發 USE_ITEM。
    //
    // 5 秒 cooldown — 對齊 server 變身動畫 + RTT;每次只送一顆。
    if selectors::transform_action_enabled(&s) {
        const COOLDOWN_TRANSFORM: std::time::Duration = std::time::Duration::from_secs(5);
        let due =
            selectors::cooldown_due(cooldowns.get("transform").copied(), now, COOLDOWN_TRANSFORM);
        if due {
            cooldowns.insert("transform", now);
            if let Err(e) =
                transform_tick(h, &s.status_transform_item, &s.status_transform_cond, &dh)
            {
                log_line!("[status][transform] {e:#}");
            }
        }
    }
}

/// 自動變身單次 tick — 兩種模式自動分流:
///
/// - **模式 1**(普通變身藥水):`option_string` 是空 → USE_ITEM(走 use_item_addr 函數)
/// - **模式 2**(變形卷軸 IP):`option_string` 非空(像 "death 80")→ 自組 II packet
///   `SendPacketData("cds", 0xA4, scroll.item_param, option_ptr)`(對齊 spy #139)
///
/// 共用觸發條件:`buff_table[39] == 0` 表示沒在變身。
fn transform_tick(
    h: HANDLE,
    item_str: &str,
    option_string: &str,
    dh: &Arc<crate::aux::drink_hook::DrinkHandle>,
) -> anyhow::Result<()> {
    // 解析後綴(剝掉 _xxx_I 之類)
    let bi = crate::aux::lhx_window::parse_buff_item(item_str);
    if bi.item_type != 'I' {
        // 技能路徑(_M / _MME 等)變身物品系統不接 — silent skip
        return Ok(());
    }

    // 讀 buff_table[39] — 變身狀態 flag(進場後變身時 server 寫 1)
    const TRANSFORM_STATE_ID: u32 = 39;
    let byte_a = crate::platform::memory::read_bytes(
        h,
        crate::aux::address::G_HASTE_BUFF_TABLE + TRANSFORM_STATE_ID,
        1,
    )?;
    let byte_b = crate::platform::memory::read_bytes(h, 0x00ABF6B8 + TRANSFORM_STATE_ID, 1)?;
    if byte_a.first().copied().unwrap_or(0) != 0 || byte_b.first().copied().unwrap_or(0) != 0 {
        return Ok(()); // 已變身,不重觸發
    }

    // 找背包同名物品
    let items = crate::aux::inventory::list_items(h)?;
    let needle = crate::aux::lhx_window::strip_qty(&bi.name);
    let it = items
        .iter()
        .find(|it| crate::aux::lhx_window::strip_qty(&it.name_lossy()) == needle);
    let Some(it) = it else {
        anyhow::bail!("變身物品「{}」不在背包(背包 {} 件)", bi.name, items.len());
    };

    let cond_raw = option_string.trim();
    if cond_raw.is_empty() {
        // 模式 1:普通變身藥水 — USE_ITEM
        log_line!(
            "[status][transform] state[{}]=0 → 用「{}」(entry=0x{:08X})",
            TRANSFORM_STATE_ID,
            bi.name,
            it.entry_addr
        );
        dh.execute_use_item(h, it.entry_addr)?;
    } else {
        // 模式 2:變形卷軸 IP packet — 帶 option string
        // INI 整行格式 `<中文>_<英文 option>_<spr_id>`,執行時抽英文進封包;
        // 玩家手填純英文(像 "re werewolf")也支援(extract_* 找不到合法格式回原值)。
        let opt = crate::aux::lhx_window::extract_polymorph_option(cond_raw);
        log_line!(
            "[status][transform] state[{}]=0 → 用「{}」+ option={:?} (param=0x{:08X})",
            TRANSFORM_STATE_ID,
            bi.name,
            opt,
            it.item_param
        );
        dh.execute_transform_scroll(h, it.item_param, opt)?;
    }
    Ok(())
}

/// 自動磨刀石單次 tick — 找揮舞中武器、檢查損壞度、call 遊戲 0x00410570。
///
/// 失敗回傳 Err(原因);成功(已 fire 或無需 fire)回傳 Ok(())。
fn whetstone_tick(h: HANDLE, dh: &Arc<crate::aux::drink_hook::DrinkHandle>) -> anyhow::Result<()> {
    let items = crate::aux::inventory::list_items(h)?;

    // 找揮舞中的武器(name 含「(揮舞)」)
    let weapon = items.iter().find(|it| it.name_lossy().contains("(揮舞)"));
    let Some(weapon) = weapon else {
        return Ok(()); // 沒揮舞武器,不報錯
    };

    // 讀 description string @ entry+0xA8(3.8 偏移)
    let desc_ptr = crate::platform::memory::read_u32(h, weapon.entry_addr + 0xA8)?;
    if desc_ptr < 0x0010_0000 {
        return Ok(()); // description 還沒填好(剛裝備時可能空)
    }
    let desc_raw = crate::platform::memory::read_bytes(h, desc_ptr, 256).unwrap_or_default();
    let end = desc_raw
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(desc_raw.len());
    let desc = &desc_raw[..end];
    // Big5「損壞度」= B7 6C C3 61 AB D7
    const DURABILITY_TAG: &[u8] = b"\xB7\x6C\xC3\x61\xAB\xD7";
    let needs_repair = desc
        .windows(DURABILITY_TAG.len())
        .any(|w| w == DURABILITY_TAG);
    if !needs_repair {
        return Ok(()); // 武器無 durability 或還沒掉血
    }

    // 找一顆磨刀石(name 完全等於「磨刀石」,strip 數量後比對)
    let whetstone = items
        .iter()
        .find(|it| crate::aux::lhx_window::strip_qty(&it.name_lossy()) == "磨刀石");
    let Some(stone) = whetstone else {
        anyhow::bail!("武器 {:?} 有損壞度,但背包沒磨刀石", weapon.name_lossy());
    };

    log_line!(
        "[status][whetstone] 磨 {}(0x{:08X}) ← {}(0x{:08X})",
        weapon.name_lossy(),
        weapon.item_param,
        stone.name_lossy(),
        stone.item_param
    );
    dh.execute_use_on_wielded(h, stone.item_param, weapon.item_param)?;
    Ok(())
}

/// 解毒 / 卡毒共用觸發 — 解析 INI 字串(可能是物品或 /ME 技能)後 dispatch。
fn fire_status_action(
    h: HANDLE,
    item_str: &str,
    dh: &Arc<crate::aux::drink_hook::DrinkHandle>,
    spell_book: &Arc<RwLock<Option<crate::aux::spell_book::SpellBook>>>,
    feature_tag: &str,
) {
    let bi = crate::aux::lhx_window::parse_buff_item(item_str);
    match bi.item_type {
        'I' => {
            let items = match crate::aux::inventory::list_items(h) {
                Ok(v) => v,
                Err(e) => {
                    log_line!("[status][{feature_tag}] 讀背包失敗: {e:#}");
                    return;
                }
            };
            let needle = crate::aux::lhx_window::strip_qty(&bi.name);
            let found = items
                .iter()
                .find(|it| crate::aux::lhx_window::strip_qty(&it.name_lossy()) == needle);
            match found {
                Some(it) => {
                    log_line!(
                        "[status][{feature_tag}] 偵測到中毒 → 用「{}」(entry=0x{:08X})",
                        bi.name,
                        it.entry_addr
                    );
                    if let Err(e) = dh.execute_use_item(h, it.entry_addr) {
                        log_line!("[status][{feature_tag}] execute_use_item 失敗: {e:#}");
                    }
                }
                None => log_line!("[status][{feature_tag}] 中毒但背包沒「{}」", bi.name),
            }
        }
        'S' => {
            let packed = match spell_book.read().as_ref().and_then(|b| b.lookup(&bi.name)) {
                Some(p) => p,
                None => {
                    log_line!("[status][{feature_tag}] 技能「{}」未學會,skip", bi.name);
                    return;
                }
            };
            let mode = crate::aux::drink_hook::SkillTargetMode::ForceSelfPacket;
            log_line!(
                "[status][{feature_tag}] 偵測到中毒 → 施放「{}」(packed={})",
                bi.name,
                packed
            );
            if let Err(e) = dh.execute_skill(h, packed, mode) {
                log_line!("[status][{feature_tag}] execute_skill 失敗: {e:#}");
            }
        }
        other => log_line!("[status][{feature_tag}] item_type {:?} 不支援", other),
    }
}
