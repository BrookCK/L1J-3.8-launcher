use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use windows::Win32::Foundation::HANDLE;

use super::{selectors::pick_shout_message, AuxSettings};
use crate::log_line;

/// timer_shout 喊話 tick — 獨立 thread 跑(不寄生 timer_2)。
pub(super) fn shout_tick(
    h: HANDLE,
    settings: &Arc<RwLock<AuxSettings>>,
    drink: &Arc<RwLock<Option<Arc<crate::aux::drink_hook::DrinkHandle>>>>,
    last_fire: &mut Option<Instant>,
    next_idx: &mut usize,
) {
    let s = settings.read().clone();
    if !s.shout_enabled || s.shout_messages.is_empty() || s.shout_interval_sec == 0 {
        return;
    }

    if !super::guards::process_in_game_world(h) {
        return;
    }

    let dh = match super::guards::clone_drink_handle(drink) {
        Some(h) => h,
        None => return,
    };

    let interval = Duration::from_secs(s.shout_interval_sec as u64);
    if let Some(t) = last_fire {
        if t.elapsed() < interval {
            return;
        }
    }

    let Some((msg, new_idx)) = pick_shout_message(&s.shout_messages, *next_idx) else {
        return;
    };

    log_line!(
        "[shout] 發送一般對話「{}」(interval={}s, idx={}/{})",
        msg,
        s.shout_interval_sec,
        *next_idx % s.shout_messages.len(),
        s.shout_messages.len()
    );
    if let Err(e) = dh.execute_chat(h, crate::aux::address::CHAT_CHANNEL_NORMAL, &msg) {
        log_line!("[shout] execute_chat 失敗: {e:#}");
    }

    *next_idx = new_idx;
    *last_fire = Some(Instant::now());
}
