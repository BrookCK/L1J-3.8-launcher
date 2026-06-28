use std::sync::Arc;

use parking_lot::RwLock;
use windows::Win32::Foundation::HANDLE;

use super::{selectors::pick_delete_action, AuxSettings};
use crate::log_line;

pub(super) fn is_dissolve_solvent_name(name: &str) -> bool {
    let name = crate::aux::lhx_window::strip_qty(name);
    name.starts_with("溶解劑") || name.starts_with("溶解剂")
}

/// timer_delete 刪物 tick — 獨立 thread 跑(不寄生 timer_2)。
pub(super) fn delete_tick(
    h: HANDLE,
    settings: &Arc<RwLock<AuxSettings>>,
    drink: &Arc<RwLock<Option<Arc<crate::aux::drink_hook::DrinkHandle>>>>,
) {
    let s = settings.read().clone();
    if !s.delete_enabled || (s.delete_list.is_empty() && s.dissolve_list.is_empty()) {
        return;
    }

    if !super::guards::process_in_game_world(h) {
        return;
    }

    let dh = match super::guards::clone_drink_handle(drink) {
        Some(h) => h,
        None => return,
    };

    let items = match crate::aux::inventory::list_items(h) {
        Ok(v) => v,
        Err(_) => return,
    };
    if items.is_empty() {
        return;
    }

    let inv_names: Vec<String> = items.iter().map(|it| it.name_lossy()).collect();
    let Some((mode, name)) = pick_delete_action(&s.delete_list, &s.dissolve_list, &inv_names)
    else {
        return;
    };

    let target = match items.iter().find(|it| it.name_lossy() == name) {
        Some(it) => it,
        None => return,
    };

    match mode {
        "delete" => {
            log_line!(
                "[delete] 刪除「{}」(param=0x{:08X}, count={})",
                target.name_lossy(),
                target.item_param,
                target.count
            );
            if let Err(e) = dh.execute_delete(h, target.item_param, target.count) {
                log_line!("[delete] execute_delete 失敗: {e:#}");
            }
        }
        "dissolve" => {
            let solvent = items
                .iter()
                .find(|it| is_dissolve_solvent_name(&it.name_lossy()));
            let Some(sv) = solvent else {
                log_line!(
                    "[delete] 想溶解「{}」但背包沒「溶解劑/溶解剂」",
                    target.name_lossy()
                );
                return;
            };
            log_line!(
                "[delete] 溶解「{}」(溶解劑 param=0x{:08X} → 0x{:08X})",
                target.name_lossy(),
                sv.item_param,
                target.item_param
            );
            if let Err(e) = dh.execute_use_on_wielded(h, sv.item_param, target.item_param) {
                log_line!("[delete] 溶解 execute 失敗: {e:#}");
            }
        }
        _ => unreachable!(),
    }
}
