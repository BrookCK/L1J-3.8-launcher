use crate::app::launch_config::load_aux_config;
use crate::aux;
use crate::logger::log_line;
use crate::platform::memory;
use anyhow::Result;
use parking_lot::RwLock;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

const LHX_USE_ITEM_ADDR: u32 = 0x004B3EE0;

pub(crate) fn should_start_lhx_aux(aux_cfg: &launcher::server_list::AuxConfig) -> bool {
    aux_cfg.lhx_aux_enabled
}

pub(crate) fn player_state_ready_for_lhx(state: &aux::player_state::PlayerState) -> bool {
    state.max_hp > 0 && state.max_mp > 0
}

fn read_lhx_profile_name(h_process: HANDLE) -> String {
    for _ in 0..50 {
        if let Some(name) = aux::profile::read_player_name(h_process) {
            log_line!("[stage2] LHX aux profile={name}");
            return name;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    log_line!("[stage2] LHX aux profile fallback=default");
    "default".to_string()
}

fn initialize_lhx_runtime_handles(h_process: HANDLE, control: &aux::runtime::AuxControl) {
    match memory::read_bytes(h_process, LHX_USE_ITEM_ADDR, 6) {
        Ok(bytes) => log_line!(
            "[stage2] LHX USE_ITEM ready @ 0x{LHX_USE_ITEM_ADDR:08X}: {}",
            bytes
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Err(e) => log_line!(
            "[stage2] LHX USE_ITEM prologue read failed @ 0x{LHX_USE_ITEM_ADDR:08X}: {e:#}"
        ),
    }

    *control.drink.write() = Some(Arc::new(aux::drink_hook::DrinkHandle::new(
        LHX_USE_ITEM_ADDR,
    )));
    log_line!("[stage2] LHX DrinkHandle installed");
}

struct LhxActiveSession {
    profile_name: String,
    control: Arc<aux::runtime::AuxControl>,
    window: aux::lhx_window::WindowControl,
    handles: Vec<std::thread::JoinHandle<()>>,
}

impl LhxActiveSession {
    fn shutdown(self) {
        let h_raw = self.window.game_handle.load(Ordering::Relaxed);

        self.control.shutdown();
        self.window
            .visible
            .store(aux::lhx_window::VISIBLE_CLOSE, Ordering::Relaxed);
        for handle in self.handles {
            let _ = handle.join();
        }
        let _ = self.window.thread.join();

        if h_raw != 0 {
            let h = HANDLE(h_raw as *mut _);
            aux::lhx_window::restore_all_misc_patches(h);
        }

        aux::profile::save(&self.profile_name, &self.control.settings.read().clone());
    }
}

fn start_lhx_session(h_process: HANDLE, pid: u32) -> Result<LhxActiveSession> {
    let profile_name = read_lhx_profile_name(h_process);
    let settings = Arc::new(RwLock::new(aux::profile::load(&profile_name)));
    let control = Arc::new(aux::runtime::AuxControl::from_shared(settings.clone()));
    initialize_lhx_runtime_handles(h_process, &control);
    let window = aux::lhx_window::spawn_window_thread(
        settings,
        h_process,
        Arc::downgrade(&control.timer_resets),
    );
    window
        .visible
        .store(aux::lhx_window::VISIBLE_SHOWN, Ordering::Relaxed);
    let scheduler = aux::runtime::AuxScheduler::new(h_process, pid, control.clone());
    let handles = scheduler.spawn_all();
    if let Err(e) = aux::chat::push_lhx_started(h_process) {
        log_line!("[stage2] push_lhx_started failed: {e:#}");
    }
    log_line!("[stage2] LHX aux started by Home");
    Ok(LhxActiveSession {
        profile_name,
        control,
        window,
        handles,
    })
}

fn spawn_lhx_home_toggle_thread(h_process: HANDLE, pid: u32) -> std::thread::JoinHandle<()> {
    #[link(name = "user32")]
    extern "system" {
        fn GetAsyncKeyState(vkey: i32) -> i16;
    }

    const VK_HOME: i32 = 0x24;
    let h_raw = h_process.0 as usize;
    std::thread::spawn(move || {
        let h_process = HANDLE(h_raw as *mut _);
        let mut last_state = unsafe { GetAsyncKeyState(VK_HOME) } as u16 & 0x8000 != 0;
        let mut last_in_world = false;
        let mut session: Option<LhxActiveSession> = None;
        log_line!("[stage2] LHX Home listener started; aux is idle until Home");
        loop {
            if unsafe { WaitForSingleObject(h_process, 0) } == WAIT_OBJECT_0 {
                if let Some(active) = session.take() {
                    active.shutdown();
                }
                log_line!("[stage2] LHX Home listener stopped: game exited");
                return;
            }

            let cur_in_world = memory::read_u32(h_process, aux::address::G_GAME_STATE)
                .map(|s| s == 3)
                .unwrap_or(false);

            let pressed = unsafe { GetAsyncKeyState(VK_HOME) } as u16 & 0x8000 != 0;
            let rising_edge = pressed && !last_state;
            let target_focused = unsafe {
                let fg = GetForegroundWindow();
                if fg.0.is_null() {
                    false
                } else {
                    let mut fg_pid: u32 = 0;
                    GetWindowThreadProcessId(fg, Some(&mut fg_pid));
                    fg_pid == pid
                }
            };
            if rising_edge && target_focused {
                if let Some(active) = &session {
                    let current = active.window.visible.load(Ordering::Relaxed);
                    let next = if current == aux::lhx_window::VISIBLE_SHOWN {
                        aux::lhx_window::VISIBLE_HIDDEN
                    } else {
                        aux::lhx_window::VISIBLE_SHOWN
                    };
                    active.window.visible.store(next, Ordering::Relaxed);
                    log_line!(
                        "[stage2] LHX Home toggle -> {}",
                        if next == aux::lhx_window::VISIBLE_SHOWN {
                            "show"
                        } else {
                            "hide"
                        }
                    );
                } else if !cur_in_world {
                    log_line!("[stage2] LHX Home ignored: player is not in game world");
                } else {
                    match aux::player_state::read_player_state(h_process) {
                        Ok(state) if player_state_ready_for_lhx(&state) => {
                            match start_lhx_session(h_process, pid) {
                                Ok(active) => session = Some(active),
                                Err(e) => log_line!("[stage2] LHX Home start failed: {e:#}"),
                            }
                        }
                        Ok(_) => log_line!("[stage2] LHX Home ignored: player state not ready"),
                        Err(e) => {
                            log_line!("[stage2] LHX Home ignored: player state read failed: {e:#}")
                        }
                    }
                }
            }

            if last_in_world && !cur_in_world {
                if let Some(active) = session.take() {
                    active.shutdown();
                    log_line!("[stage2] LHX aux stopped after leaving game world");
                }
            }
            last_in_world = cur_in_world;
            last_state = pressed;
            std::thread::sleep(Duration::from_millis(50));
        }
    })
}

pub(crate) fn run_lhx_aux_until_game_exit(h_process: HANDLE, pid: u32) -> Result<bool> {
    let aux_cfg = load_aux_config();
    if !should_start_lhx_aux(&aux_cfg) {
        log_line!("[stage2] LHX aux disabled by config");
        return Ok(false);
    }

    let home_toggle = spawn_lhx_home_toggle_thread(h_process, pid);
    log_line!("[stage2] LHX aux enabled; waiting for Home or game exit");
    unsafe {
        WaitForSingleObject(h_process, INFINITE);
    }
    let _ = home_toggle.join();
    log_line!("[stage2] LHX aux shutdown complete");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lhx_aux_switch_controls_stage2_keepalive_aux() {
        // Build the struct directly so unspecified fields stay at defaults and clippy stays quiet.
        let mut aux = launcher::server_list::AuxConfig {
            lhx_aux_enabled: false,
            ..Default::default()
        };
        assert!(!should_start_lhx_aux(&aux));

        aux.lhx_aux_enabled = true;
        assert!(should_start_lhx_aux(&aux));
    }

    #[test]
    fn lhx_aux_waits_for_real_player_state() {
        assert!(!player_state_ready_for_lhx(
            &crate::aux::player_state::PlayerState::default()
        ));

        let state = crate::aux::player_state::PlayerState {
            max_hp: 100,
            max_mp: 30,
            ..Default::default()
        };
        assert!(player_state_ready_for_lhx(&state));
    }
}
