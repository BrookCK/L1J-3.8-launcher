use std::sync::Arc;

use parking_lot::RwLock;
use windows::Win32::Foundation::HANDLE;

pub(in crate::aux::runtime) fn in_game_world(game_state: u32) -> bool {
    game_state == 3
}

pub(in crate::aux::runtime) fn process_in_game_world(h: HANDLE) -> bool {
    let game_state =
        crate::platform::memory::read_u32(h, crate::aux::address::G_GAME_STATE).unwrap_or(0);
    in_game_world(game_state)
}

pub(in crate::aux::runtime) fn clone_drink_handle(
    drink: &Arc<RwLock<Option<Arc<crate::aux::drink_hook::DrinkHandle>>>>,
) -> Option<Arc<crate::aux::drink_hook::DrinkHandle>> {
    drink.read().as_ref().cloned()
}
