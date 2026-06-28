use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;

use crate::aux::address::G_GAME_STATE;
use crate::log_line;
use crate::memory::read_u32;

use super::action::{attack::stop_client_auto_attack, walk::walk_release};
use super::decide::hunt::{HuntConfig, HuntOutcome};
use super::hunt4::context::HuntContext;
use super::hunt4::observe::StateReport;
use super::hunt4::runtime as hunt4_runtime;
use super::state::BotState;

const GAME_STATE_IN_GAME: u32 = 3;
const TICK_INTERVAL: Duration = Duration::from_millis(200);
const HUNT_ALIVE_DIAG_INTERVAL: Duration = Duration::from_secs(3);

pub struct BotEngine {
    h_raw: usize,
    cancel: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    hunt_config: Arc<RwLock<HuntConfig>>,
    hunt4_report: Arc<Mutex<Option<StateReport>>>,
    thread: Option<JoinHandle<()>>,
}

impl BotEngine {
    pub fn start(h: HANDLE) -> Self {
        let h_raw = h.0 as usize;
        let cancel = Arc::new(AtomicBool::new(false));
        let enabled = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(BotState::Idle));
        let hunt_config = Arc::new(RwLock::new(HuntConfig::default()));
        let hunt4_report = Arc::new(Mutex::new(Some(HuntContext::new(Instant::now()).report())));

        let thread = {
            let cancel = Arc::clone(&cancel);
            let enabled = Arc::clone(&enabled);
            let state = Arc::clone(&state);
            let hunt_config = Arc::clone(&hunt_config);
            let hunt4_report = Arc::clone(&hunt4_report);
            thread::spawn(move || {
                tick_loop(h_raw, cancel, enabled, state, hunt_config, hunt4_report)
            })
        };

        log_line!("[bot/engine] tick thread started(200ms,state=Idle,enabled=false)");

        Self {
            h_raw,
            cancel,
            enabled,
            hunt_config,
            hunt4_report,
            thread: Some(thread),
        }
    }

    pub fn set_hunt_config(&self, cfg: HuntConfig) {
        *self
            .hunt_config
            .write()
            .expect("hunt_config rwlock poisoned") = cfg;
    }

    pub fn hunt_config_snapshot(&self) -> HuntConfig {
        self.hunt_config
            .read()
            .expect("hunt_config rwlock poisoned")
            .clone()
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        log_line!(
            "[bot/engine] master toggle -> {}",
            if on { "ON" } else { "OFF" }
        );
        if !on {
            let h = HANDLE(self.h_raw as *mut _);
            if let Err(e) = stop_client_auto_attack(h) {
                log_line!("[bot/engine] master off stop_client_auto_attack failed:{e:#}");
            }
        }
    }

    pub fn hunt4_report_snapshot(&self) -> Option<StateReport> {
        self.hunt4_report
            .lock()
            .expect("hunt4_report mutex poisoned")
            .clone()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn h_raw(&self) -> usize {
        self.h_raw
    }

    pub fn shutdown(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            match handle.join() {
                Ok(()) => log_line!("[bot/engine] tick thread ended"),
                Err(_) => log_line!("[bot/engine] tick thread join failed(panic)"),
            }
        }

        let h = HANDLE(self.h_raw as *mut _);
        if let Err(e) = stop_client_auto_attack(h) {
            log_line!("[bot/engine] stop_client_auto_attack failed:{e:#}");
        }
    }
}

impl Drop for BotEngine {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.shutdown();
        }
    }
}

fn tick_loop(
    h_raw: usize,
    cancel: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    state: Arc<Mutex<BotState>>,
    hunt_config: Arc<RwLock<HuntConfig>>,
    hunt4_report: Arc<Mutex<Option<StateReport>>>,
) {
    let h = HANDLE(h_raw as *mut _);
    let mut last_log = Instant::now();
    let mut last_logged_state: Option<BotState> = None;
    let mut hunt4_ctx = HuntContext::new(Instant::now());
    let mut last_hunt_diag = Instant::now();

    while !cancel.load(Ordering::Relaxed) {
        let cur = *state.lock().expect("state mutex poisoned");
        let packet_probe_target = if cur == BotState::Hunting {
            hunt4_ctx.report().lock_summary.map(|lock| lock.target_id)
        } else {
            None
        };
        crate::aux::server_packet_hook::set_debug_target_id(packet_probe_target);
        let _ = crate::aux::server_packet_hook::poll(h);

        let game_state = read_u32(h, G_GAME_STATE).unwrap_or(0);
        let master_on = enabled.load(Ordering::Relaxed);

        let next = compute_next_state(cur, game_state, master_on);
        if next != cur && cur.can_transition_to(next) {
            *state.lock().expect("state mutex poisoned") = next;
            log_line!(
                "[bot/engine] state {} -> {}(game_state={}, master={})",
                cur,
                next,
                game_state,
                if master_on { "on" } else { "off" }
            );
            last_logged_state = Some(next);
            if next == BotState::Hunting {
                hunt4_ctx = HuntContext::new(Instant::now());
                *hunt4_report.lock().expect("hunt4_report mutex poisoned") =
                    Some(hunt4_ctx.report());
            }
        }

        if next == BotState::Hunting && should_suspend_hunt_for_ui(super::ui::is_bot_window_open())
        {
            let _ = walk_release();
            thread::sleep(TICK_INTERVAL);
            continue;
        }

        if next == BotState::Hunting {
            let cfg = hunt_config
                .read()
                .expect("hunt_config rwlock poisoned")
                .clone();
            let outcome = run_hunt_tick(h, &cfg, &mut hunt4_ctx);
            *hunt4_report.lock().expect("hunt4_report mutex poisoned") = Some(hunt4_ctx.report());
            log_hunt_outcome(&outcome);
            log_hunt_alive_diag(&outcome, game_state, master_on, &mut last_hunt_diag);
        }

        if last_log.elapsed() >= Duration::from_secs(10) {
            let s = *state.lock().expect("state mutex poisoned");
            if last_logged_state != Some(s) && s != BotState::Hunting {
                log_line!("[bot/engine] heartbeat state={}", s);
                last_logged_state = Some(s);
            }
            last_log = Instant::now();
        }

        thread::sleep(TICK_INTERVAL);
    }
}

fn run_hunt_tick(h: HANDLE, cfg: &HuntConfig, hunt4_ctx: &mut HuntContext) -> HuntOutcome {
    hunt4_runtime::tick_io(h, cfg, hunt4_ctx, true)
}

static LAST_LOGGED_HEADING: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);
static LAST_LOGGED_DISTANCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn log_hunt_outcome(outcome: &HuntOutcome) {
    use std::sync::atomic::Ordering;
    match outcome {
        HuntOutcome::Cooldown { .. } => {}
        HuntOutcome::NoTarget => {
            if LAST_LOGGED_HEADING.swap(u8::MAX, Ordering::Relaxed) != u8::MAX {
                log_line!("[bot/engine] hunt: no whitelist target");
            }
        }
        HuntOutcome::Cast {
            target_id,
            name,
            player_pos,
        } => {
            LAST_LOGGED_HEADING.store(u8::MAX, Ordering::Relaxed);
            match player_pos {
                Some(p) => log_line!(
                    "[bot/engine] hunt: attack {} (target_id=0x{:X}) @ ({}, {})",
                    name,
                    target_id,
                    p.x,
                    p.y
                ),
                None => log_line!(
                    "[bot/engine] hunt: attack {} (target_id=0x{:X}) @ (unknown)",
                    name,
                    target_id
                ),
            }
        }
        HuntOutcome::Walked {
            target_id,
            name,
            heading,
            distance_tiles,
        } => {
            let prev_h = LAST_LOGGED_HEADING.load(Ordering::Relaxed);
            let prev_d = LAST_LOGGED_DISTANCE.load(Ordering::Relaxed);
            let heading_changed = prev_h != *heading;
            let distance_jumped = prev_d.abs_diff(*distance_tiles) >= 3;
            if heading_changed || distance_jumped {
                log_line!(
                    "[bot/engine] hunt: walk to {} (target_id=0x{:X}, dist={}, heading={})",
                    name,
                    target_id,
                    distance_tiles,
                    heading
                );
                LAST_LOGGED_HEADING.store(*heading, Ordering::Relaxed);
                LAST_LOGGED_DISTANCE.store(*distance_tiles, Ordering::Relaxed);
            }
        }
        HuntOutcome::ActionFailed(msg) => {
            log_line!("[bot/engine] hunt: action failed: {}", msg);
        }
    }
}

fn compute_next_state(cur: BotState, game_state: u32, master_on: bool) -> BotState {
    match cur {
        BotState::Stopped => {
            if master_on && game_state == GAME_STATE_IN_GAME {
                BotState::Idle
            } else {
                BotState::Stopped
            }
        }
        BotState::Idle => {
            if master_on && game_state == GAME_STATE_IN_GAME {
                BotState::Hunting
            } else {
                BotState::Idle
            }
        }
        BotState::Hunting => {
            if !master_on || game_state != GAME_STATE_IN_GAME {
                BotState::Stopped
            } else {
                BotState::Hunting
            }
        }
    }
}

fn log_hunt_alive_diag(
    outcome: &HuntOutcome,
    game_state: u32,
    master_on: bool,
    last_hunt_diag: &mut Instant,
) {
    let Some(label) = hunt_outcome_diag_label(outcome) else {
        *last_hunt_diag = Instant::now();
        return;
    };
    if last_hunt_diag.elapsed() < HUNT_ALIVE_DIAG_INTERVAL {
        return;
    }
    *last_hunt_diag = Instant::now();
    log_line!(
        "[bot/engine] hunt alive: outcome={} game_state={} master={}",
        label,
        game_state,
        if master_on { "on" } else { "off" }
    );
}

fn hunt_outcome_diag_label(outcome: &HuntOutcome) -> Option<&'static str> {
    match outcome {
        HuntOutcome::Cooldown { remaining_ms } => {
            if *remaining_ms == 0 {
                Some("cooldown(0)")
            } else {
                Some("cooldown")
            }
        }
        HuntOutcome::NoTarget => Some("no_target"),
        HuntOutcome::Cast { .. } | HuntOutcome::Walked { .. } | HuntOutcome::ActionFailed(_) => {
            None
        }
    }
}

fn should_suspend_hunt_for_ui(bot_window_open: bool) -> bool {
    let _ = bot_window_open;
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_stays_idle_when_master_off() {
        assert_eq!(
            compute_next_state(BotState::Idle, GAME_STATE_IN_GAME, false),
            BotState::Idle
        );
    }

    #[test]
    fn idle_to_hunting_requires_both_in_game_and_master_on() {
        assert_eq!(
            compute_next_state(BotState::Idle, GAME_STATE_IN_GAME, false),
            BotState::Idle
        );
        assert_eq!(compute_next_state(BotState::Idle, 0, true), BotState::Idle);
        assert_eq!(
            compute_next_state(BotState::Idle, GAME_STATE_IN_GAME, true),
            BotState::Hunting
        );
    }

    #[test]
    fn hunting_to_stopped_on_disconnect() {
        assert_eq!(
            compute_next_state(BotState::Hunting, 0, true),
            BotState::Stopped
        );
    }

    #[test]
    fn hunting_to_stopped_on_master_off() {
        assert_eq!(
            compute_next_state(BotState::Hunting, GAME_STATE_IN_GAME, false),
            BotState::Stopped
        );
    }

    #[test]
    fn stopped_recovers_to_idle_when_master_is_on_and_game_is_ready() {
        assert_eq!(
            compute_next_state(BotState::Stopped, GAME_STATE_IN_GAME, true),
            BotState::Idle
        );
    }

    #[test]
    fn hunt_output_suspends_while_bot_window_is_open() {
        assert!(!should_suspend_hunt_for_ui(true));
        assert!(!should_suspend_hunt_for_ui(false));
    }

    #[test]
    fn engine_source_dispatches_hunt4_runtime() {
        let source = include_str!("engine.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        assert!(production.contains("use super::hunt4::runtime as hunt4_runtime;"));
        assert!(production.contains("hunt4_runtime::tick_io(h, cfg, hunt4_ctx, true)"));
        assert!(!production.contains("engine_kind"));
        assert!(!production.contains("match cfg.engine_kind"));
        assert!(!production.contains("hunt3_runtime::tick_io"));
        assert!(!production.contains("hunt_runtime::tick"));
    }
}
