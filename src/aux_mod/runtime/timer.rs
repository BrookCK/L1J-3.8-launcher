use std::sync::Arc;

use parking_lot::RwLock;
use windows::Win32::Foundation::HANDLE;

use super::{selectors::pick_timer_action, AuxSettings};
use crate::log_line;

/// `timer_tick` 的語義參數集。
pub(super) struct TimerTickCtx<'a> {
    pub(super) h: HANDLE,
    pub(super) settings: &'a Arc<RwLock<AuxSettings>>,
    pub(super) drink: &'a Arc<RwLock<Option<Arc<crate::aux::drink_hook::DrinkHandle>>>>,
    pub(super) spell_book: &'a Arc<RwLock<Option<crate::aux::spell_book::SpellBook>>>,
    pub(super) spell_db: &'a Arc<RwLock<Option<crate::aux::spell_db::SpellDb>>>,
    pub(super) resets: &'a Arc<[std::sync::atomic::AtomicU64; 6]>,
    pub(super) last_fire: &'a mut [Option<std::time::Instant>; 6],
    pub(super) last_seen: &'a mut [u64; 6],
}

/// timer_timer 定時 tick — 獨立 thread 跑(不寄生 timer_2)。
pub(super) fn timer_tick(ctx: TimerTickCtx) {
    use std::sync::atomic::Ordering::Relaxed;

    let TimerTickCtx {
        h,
        settings,
        drink,
        spell_book,
        spell_db,
        resets,
        last_fire,
        last_seen,
    } = ctx;

    let s = settings.read().clone();
    if !s.timer_master_enabled {
        return;
    }
    if !super::guards::process_in_game_world(h) {
        return;
    }
    let dh = match super::guards::clone_drink_handle(drink) {
        Some(h) => h,
        None => return,
    };

    let now = std::time::Instant::now();

    for i in 0..6 {
        let cur = resets[i].load(Relaxed);
        if cur != last_seen[i] {
            last_fire[i] = Some(now);
            last_seen[i] = cur;
            log_line!("[timer] row {} 收到重計信號,從現在開始計時", i);
        }
    }

    let Some(idx) = pick_timer_action(&s.timer_rows, last_fire, s.timer_master_enabled, now) else {
        return;
    };
    let bi = crate::aux::lhx_window::parse_buff_item(&s.timer_rows[idx].command);
    let ctx = crate::aux::buff_dispatch::DispatchCtx {
        h,
        dh: &dh,
        spell_book,
        spell_db,
    };
    log_line!(
        "[timer] row {} 觸發 → 指令「{}」(間隔 {}s)",
        idx,
        s.timer_rows[idx].command,
        s.timer_rows[idx].interval_sec
    );
    let _ = crate::aux::buff_dispatch::execute_buff_item(&ctx, &bi);
    last_fire[idx] = Some(now);
}
