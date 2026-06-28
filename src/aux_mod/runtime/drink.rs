use std::sync::Arc;

use parking_lot::RwLock;
use windows::Win32::Foundation::HANDLE;

pub(super) mod selectors;

use super::{
    selectors::{auto_drink_item_request, mp_when_safe_triggered, AutoDrinkItemRequest},
    AuxSettings,
};
use crate::log_line;

/// thin wrapper — 委派給 [`crate::aux::spell_book::ensure_fresh`],統一換角偵測。
///
/// 保留名字是為了相容 `drink_tick` / `mp-safe` 既有 caller 不用全部改名。
fn ensure_spell_book_ready(
    h: HANDLE,
    spell_book: &Arc<RwLock<Option<crate::aux::spell_book::SpellBook>>>,
    tag: &str,
) -> bool {
    crate::aux::spell_book::ensure_fresh(h, spell_book, tag)
}

fn execute_auto_drink_item(
    dh: &crate::aux::drink_hook::DrinkHandle,
    h: HANDLE,
    item: &crate::aux::inventory::Item,
) -> anyhow::Result<()> {
    match auto_drink_item_request(item) {
        AutoDrinkItemRequest::DirectUsePacket { item_param } => {
            dh.execute_use_item_packet(h, item_param)
        }
    }
}

/// timer_2 喝水 tick — 檢查 7 個 row,符合條件 + 對應藥水在背包就 queue。
///
/// 目前只實作 row[0](HP threshold);其他 row + MP/safe-MP 後續補。
pub(super) fn drink_tick(
    h: HANDLE,
    settings: &Arc<RwLock<AuxSettings>>,
    drink: &Arc<RwLock<Option<Arc<crate::aux::drink_hook::DrinkHandle>>>>,
    spell_book: &Arc<RwLock<Option<crate::aux::spell_book::SpellBook>>>,
) {
    let s = settings.read().clone();
    if !selectors::has_drink_work(&s) {
        return; // 沒人勾喝水
    }

    // 必須在遊戲世界內(G_GAME_STATE == 3)才允許喝水。
    // 退選角 / 切換伺服器 / 角色未進場時,inventory 指標和 USE_ITEM 內部 context
    // 可能尚未就緒或已被釋放,呼叫 USE_ITEM 會 crash。
    if !super::guards::process_in_game_world(h) {
        return; // silent skip — 退選角 / 載入中 / 連線斷
    }

    // hook 還沒裝就不能 queue
    let dh = match super::guards::clone_drink_handle(drink) {
        Some(h) => h,
        None => return,
    };

    // 讀玩家狀態
    let state = match crate::aux::player_state::read_player_state(h) {
        Ok(s) if s.max_hp > 0 => s,
        _ => return, // max_hp=0,可能剛進場狀態還沒填,跳過
    };
    let hp_pct = state.hp.saturating_mul(100) / state.max_hp.max(1);

    // 走訪 row(由上而下,優先順序高的先試)
    for row in s.potion_rows.iter() {
        if !selectors::potion_row_triggered(&s, row, &state) {
            continue; // HP 還夠高,輪不到這 row
        }

        // 解析 row 字串(剝掉 /M /ME 等 suffix,或留為一般物品)
        let bi = crate::aux::lhx_window::parse_buff_item(&row.item);

        // A row-local failure should not block later rows or mp_when_safe below.
        match bi.item_type {
            // 物品:走原本 USE_ITEM 路徑
            'I' => {
                let items = match crate::aux::inventory::list_items(h) {
                    Ok(v) => v,
                    Err(e) => {
                        log_line!("[drink] 讀背包失敗(可能剛進場 inventory 還沒 ready): {e:#}");
                        continue;
                    }
                };
                if items.is_empty() {
                    log_line!(
                        "[drink] HP 觸發但背包是空的(item 數=0,inventory pointer 可能失效或 server 還沒下發)。row={:?}",
                        row.item
                    );
                    continue;
                }
                let needle = crate::aux::lhx_window::strip_qty(&bi.name);
                let it = match items
                    .iter()
                    .find(|it| crate::aux::lhx_window::strip_qty(&it.name_lossy()) == needle)
                {
                    Some(it) => it,
                    None => {
                        log_line!(
                            "[drink] HP 觸發但背包找不到目標物品 needle={:?}(背包 {} 件)",
                            needle,
                            items.len()
                        );
                        continue;
                    }
                };
                if s.potion_use_percent {
                    log_line!(
                        "[drink] execute entry=0x{:08X} param=0x{:08X} (HP={}/{} {}% < {}%, item={:?})",
                        it.entry_addr, it.item_param, state.hp, state.max_hp, hp_pct, row.threshold,
                        it.name_lossy()
                    );
                } else {
                    log_line!(
                        "[drink] execute entry=0x{:08X} param=0x{:08X} (HP={}/{} < {}, item={:?})",
                        it.entry_addr,
                        it.item_param,
                        state.hp,
                        state.max_hp,
                        row.threshold,
                        it.name_lossy()
                    );
                }
                let t0 = std::time::Instant::now();
                match execute_auto_drink_item(&dh, h, it) {
                    Ok(()) => log_line!("[drink] execute OK,耗時 {} ms", t0.elapsed().as_millis()),
                    Err(e) => log_line!("[drink] execute 失敗: {e:#}"),
                }
            }
            // 技能:走 spell_book + execute_skill — 對齊 fire_status_action 的 'S' 分支
            'S' => {
                if !ensure_spell_book_ready(h, spell_book, "drink") {
                    continue;
                }
                let packed = match spell_book.read().as_ref().and_then(|b| b.lookup(&bi.name)) {
                    Some(p) => p,
                    None => {
                        log_line!(
                            "[drink] HP 觸發但技能「{}」未學會(spell_book 沒這個),skip",
                            bi.name
                        );
                        continue;
                    }
                };
                let Some(mode) = selectors::drink_skill_target_mode(&bi.cast_target) else {
                    log_line!(
                        "[drink] 技能「{}」cast_target={:?} 喝水流程不支援(只接 /M /ME)",
                        bi.name,
                        bi.cast_target
                    );
                    continue;
                };
                if s.potion_use_percent {
                    log_line!(
                        "[drink] cast skill packed={} (HP={}/{} {}% < {}%, name={:?})",
                        packed,
                        state.hp,
                        state.max_hp,
                        hp_pct,
                        row.threshold,
                        bi.name
                    );
                } else {
                    log_line!(
                        "[drink] cast skill packed={} (HP={}/{} < {}, name={:?})",
                        packed,
                        state.hp,
                        state.max_hp,
                        row.threshold,
                        bi.name
                    );
                }
                if let Err(e) = dh.execute_skill(h, packed, mode) {
                    log_line!("[drink] execute_skill 失敗: {e:#}");
                }
            }
            other => {
                log_line!(
                    "[drink] row {:?} item_type={:?} 不支援(只接物品 / /M / /ME)",
                    row.item,
                    other
                );
                continue;
            }
        }
    }

    if !mp_when_safe_triggered(&s, &state) {
        return;
    }

    let bi = crate::aux::lhx_window::parse_buff_item(&s.mp_when_safe.item);
    match bi.item_type {
        'I' => {
            let items = match crate::aux::inventory::list_items(h) {
                Ok(v) => v,
                Err(e) => {
                    log_line!("[drink/mp-safe] inventory read failed: {e:#}");
                    return;
                }
            };
            let needle = crate::aux::lhx_window::strip_qty(&bi.name);
            let it = match items
                .iter()
                .find(|it| crate::aux::lhx_window::strip_qty(&it.name_lossy()) == needle)
            {
                Some(it) => it,
                None => {
                    log_line!(
                        "[drink/mp-safe] item not found needle={:?}, inventory={}",
                        needle,
                        items.len()
                    );
                    return;
                }
            };
            log_line!(
                "[drink/mp-safe] execute item entry=0x{:08X} param=0x{:08X} (HP={}/{}, MP={}/{}, item={:?})",
                it.entry_addr,
                it.item_param,
                state.hp,
                state.max_hp,
                state.mp,
                state.max_mp,
                it.name_lossy()
            );
            if let Err(e) = execute_auto_drink_item(&dh, h, it) {
                log_line!("[drink/mp-safe] execute item failed: {e:#}");
            }
        }
        'S' => {
            if !ensure_spell_book_ready(h, spell_book, "drink/mp-safe") {
                return;
            }
            let packed = match spell_book.read().as_ref().and_then(|b| b.lookup(&bi.name)) {
                Some(p) => p,
                None => {
                    log_line!(
                        "[drink/mp-safe] spell not found in spell_book: {:?}",
                        bi.name
                    );
                    return;
                }
            };
            let Some(mode) = selectors::drink_skill_target_mode(&bi.cast_target) else {
                log_line!(
                    "[drink/mp-safe] unsupported cast_target for {:?}: {:?}",
                    bi.name,
                    bi.cast_target
                );
                return;
            };
            log_line!(
                "[drink/mp-safe] cast skill packed={} (HP={}/{}, MP={}/{}, name={:?})",
                packed,
                state.hp,
                state.max_hp,
                state.mp,
                state.max_mp,
                bi.name
            );
            if let Err(e) = dh.execute_skill(h, packed, mode) {
                log_line!("[drink/mp-safe] execute_skill failed: {e:#}");
            }
        }
        other => {
            log_line!(
                "[drink/mp-safe] item {:?} item_type={:?} unsupported",
                s.mp_when_safe.item,
                other
            );
        }
    }
}
