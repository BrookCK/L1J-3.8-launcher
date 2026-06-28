//! Server packet observer hook for low ProcessPacket opcodes used by the bot.
//!
//! The notification hook intentionally sits on the `opcode > 183` branch. This
//! hook sits at `ProcessPacket+0x61`, after the client has decoded the outer
//! opcode and advanced `[ebp+8]` to the payload. It mirrors selected low-opcode
//! packets into a remote ring buffer, then replays the original dispatch.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use once_cell::sync::Lazy;
use windows::Win32::Foundation::HANDLE;

use crate::logger::log_line;
use crate::{memory, process};

pub const LOW_OPCODE_HOOK_ADDR: u32 = 0x0053_9394;
pub const LOW_OPCODE_ORIGINAL_BYTES: [u8; 6] = [0x8B, 0x85, 0x0C, 0x9F, 0xFF, 0xFF];
pub const PROCESS_PACKET_DISPATCH_TABLE: u32 = 0x0054_15B4;
const OPCODE_EBP_OFFSET: i32 = -0x60f4;
const PACKET_PTR_EBP_OFFSET: i8 = 8;

pub const RAW_PACKET_DISPATCH_ADDR: u32 = 0x0054_4A20;
pub const RAW_PACKET_ORIGINAL_BYTES: [u8; 6] = [0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x1C];

pub const SERVER_PACKET_OPCODES: [u8; 7] = [
    crate::bot::packet_events::S_OPCODE_MOVE_OBJECT,
    crate::bot::packet_events::S_OPCODE_ATTACK,
    crate::bot::packet_events::S_OPCODE_RANGESKILLS,
    crate::bot::packet_events::S_OPCODE_PUT_OBJECT,
    crate::bot::packet_events::S_OPCODE_REMOVE_OBJECT,
    crate::bot::packet_events::S_OPCODE_ACTION,
    crate::bot::packet_events::S_OPCODE_HP_METER,
];

pub const SERVER_PACKET_PAYLOAD_MAX: u32 = 80;
pub const SERVER_PACKET_SLOT_SIZE: u32 = 4 + SERVER_PACKET_PAYLOAD_MAX;
pub const RAW_SERVER_PACKET_PAYLOAD_MAX: u32 = 96;
pub const RAW_SERVER_PACKET_SLOT_SIZE: u32 = 4 + RAW_SERVER_PACKET_PAYLOAD_MAX;
pub const MIRROR_ALL_LOW_OPCODES_FOR_DISCOVERY: bool = true;
const SERVER_PACKET_RING_SLOTS: u32 = 32;
const RAW_SERVER_PACKET_RING_SLOTS: u32 = 128;
const CODECAVE_SIZE: usize = 0x5000;
const OFF_TAIL: u32 = 0x200;
const OFF_TOTAL_HITS: u32 = 0x204;
const OFF_RING: u32 = 0x300;
const OFF_RAW_SHELLCODE: u32 = 0x1000;
const OFF_RAW_TAIL: u32 = 0x1200;
const OFF_RAW_TOTAL_HITS: u32 = 0x1204;
const OFF_RAW_RING: u32 = 0x1300;
const SELF_CHAR_ID_ADDR: u32 = 0x00AB_F4B4;
const LOCAL_PLAYER_PTR_ADDR: u32 = 0x00C2_D2B8;
const LOCAL_PLAYER_ID_OFFSET: u32 = 0x0C;
const LOCAL_PLAYER_ALT_ID_OFFSET: u32 = 0x14;

#[derive(Debug, Clone)]
pub struct ServerPacketHookHandle {
    pub cave_addr: u32,
    local_head: u32,
    raw_local_head: u32,
}

static HOOK_STATE: Lazy<Mutex<Option<ServerPacketHookHandle>>> = Lazy::new(|| Mutex::new(None));
static POLL_DIAG: Lazy<Mutex<PollDiag>> = Lazy::new(|| Mutex::new(PollDiag::default()));
static DEBUG_TARGET_ID: Lazy<Mutex<Option<u32>>> = Lazy::new(|| Mutex::new(None));
static PLAYER_ATTACK_TARGET_IDS: Lazy<Mutex<PlayerAttackTargetIds>> =
    Lazy::new(|| Mutex::new(PlayerAttackTargetIds::default()));
static RECENT_REMOVED_OR_DEAD_TARGETS: Lazy<Mutex<HashMap<u32, Instant>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
const RECENT_TARGET_EVENT_TTL: Duration = Duration::from_secs(4);
static RECENT_DAMAGED_TARGETS: Lazy<Mutex<HashMap<u32, Instant>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
const RECENT_TARGET_DAMAGE_TTL: Duration = Duration::from_secs(5);

/// 最近主動攻擊我的怪 id → 過期時間。 餵 V4 planner 用「正在打我 → 排序往前」。
/// key = attacker_id(怪),value = 過期時間。 V4 planner 排序時加權往前。
static RECENT_ATTACKERS: Lazy<Mutex<HashMap<u32, Instant>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
const RECENT_ATTACKER_TTL: Duration = Duration::from_secs(10);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct PlayerAttackTargetIds {
    self_char_id: u32,
    local_target_id: u32,
    local_alt_id: u32,
}

impl PlayerAttackTargetIds {
    fn has_any(self) -> bool {
        self.self_char_id != 0 || self.local_target_id != 0 || self.local_alt_id != 0
    }

    fn contains(self, target_id: u32) -> bool {
        target_id != 0
            && (target_id == self.self_char_id
                || target_id == self.local_target_id
                || target_id == self.local_alt_id)
    }
}

#[derive(Debug, Default)]
struct PollDiag {
    last_log: Option<Instant>,
    last_total_hits: u32,
    last_raw_total_hits: u32,
    drained_since_last: usize,
    drained_low_since_last: usize,
    drained_raw_since_last: usize,
    drained_opcodes_since_last: BTreeMap<u8, usize>,
    raw_opcodes_since_last: BTreeMap<u8, usize>,
    parsed_events_since_last: ParsedEventCounts,
    drained_samples_since_last: BTreeMap<u8, Vec<String>>,
    target_probe_id: Option<u32>,
    target_probe_hits_since_last: BTreeMap<u8, usize>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ParsedEventCounts {
    attack: usize,
    range_skill: usize,
    remove_object: usize,
    action_die: usize,
    put_object_die: usize,
    hp_meter: usize,
    damaged_targets: usize,
    recent_attackers: usize,
}

impl ParsedEventCounts {
    fn add_assign(&mut self, other: ParsedEventCounts) {
        self.attack += other.attack;
        self.range_skill += other.range_skill;
        self.remove_object += other.remove_object;
        self.action_die += other.action_die;
        self.put_object_die += other.put_object_die;
        self.hp_meter += other.hp_meter;
        self.damaged_targets += other.damaged_targets;
        self.recent_attackers += other.recent_attackers;
    }

    fn is_empty(&self) -> bool {
        self.attack == 0
            && self.range_skill == 0
            && self.remove_object == 0
            && self.action_die == 0
            && self.put_object_die == 0
            && self.hp_meter == 0
            && self.damaged_targets == 0
            && self.recent_attackers == 0
    }
}

pub fn build_server_packet_shellcode(cave: u32) -> Vec<u8> {
    let tail_addr = cave + OFF_TAIL;
    let total_hits_addr = cave + OFF_TOTAL_HITS;
    let ring_addr = cave + OFF_RING;
    let mut sc = Vec::with_capacity(220);
    let mut miss_jmp_fixup = None;

    if !MIRROR_ALL_LOW_OPCODES_FOR_DISCOVERY {
        let mut je_fixups = Vec::new();
        for opcode in SERVER_PACKET_OPCODES {
            sc.extend_from_slice(&[0x81, 0xBD]);
            sc.extend_from_slice(&OPCODE_EBP_OFFSET.to_le_bytes());
            sc.extend_from_slice(&(opcode as u32).to_le_bytes());
            sc.extend_from_slice(&[0x0F, 0x84]);
            je_fixups.push(sc.len());
            sc.extend_from_slice(&[0, 0, 0, 0]);
        }

        sc.push(0xE9);
        miss_jmp_fixup = Some(sc.len());
        sc.extend_from_slice(&[0, 0, 0, 0]);

        let push_off = sc.len();
        for fixup in je_fixups {
            let disp = push_off as i32 - (fixup as i32 + 4);
            sc[fixup..fixup + 4].copy_from_slice(&disp.to_le_bytes());
        }
    }

    sc.extend_from_slice(&[0xF0, 0xFF, 0x05]);
    sc.extend_from_slice(&total_hits_addr.to_le_bytes());
    sc.push(0x60);
    sc.push(0x9C);

    sc.extend_from_slice(&[0x8B, 0x0D]);
    sc.extend_from_slice(&tail_addr.to_le_bytes());
    sc.extend_from_slice(&[0xFF, 0x05]);
    sc.extend_from_slice(&tail_addr.to_le_bytes());
    sc.extend_from_slice(&[0x83, 0xE1, (SERVER_PACKET_RING_SLOTS - 1) as u8]);
    sc.extend_from_slice(&[0x6B, 0xC9, SERVER_PACKET_SLOT_SIZE as u8]);
    sc.extend_from_slice(&[0x81, 0xC1]);
    sc.extend_from_slice(&ring_addr.to_le_bytes());

    sc.extend_from_slice(&[0x8A, 0x85]);
    sc.extend_from_slice(&OPCODE_EBP_OFFSET.to_le_bytes());
    sc.extend_from_slice(&[0x88, 0x01]);
    sc.extend_from_slice(&[0xC6, 0x41, 0x01, 0x01]);
    sc.extend_from_slice(&[0x8B, 0x75, PACKET_PTR_EBP_OFFSET as u8]);
    sc.extend_from_slice(&[0x8D, 0x79, 0x04]);
    sc.push(0xB9);
    sc.extend_from_slice(&SERVER_PACKET_PAYLOAD_MAX.to_le_bytes());
    sc.extend_from_slice(&[0xF3, 0xA4]);

    sc.push(0x9D);
    sc.push(0x61);

    let replay_off = sc.len();
    if let Some(miss_jmp_fixup) = miss_jmp_fixup {
        sc[miss_jmp_fixup..miss_jmp_fixup + 4]
            .copy_from_slice(&(replay_off as i32 - (miss_jmp_fixup as i32 + 4)).to_le_bytes());
    }
    sc.extend_from_slice(&LOW_OPCODE_ORIGINAL_BYTES);
    sc.extend_from_slice(&[0xFF, 0x24, 0x85]);
    sc.extend_from_slice(&PROCESS_PACKET_DISPATCH_TABLE.to_le_bytes());

    sc
}

pub fn build_raw_server_packet_shellcode(cave: u32) -> Vec<u8> {
    let raw_shellcode_addr = cave + OFF_RAW_SHELLCODE;
    let tail_addr = cave + OFF_RAW_TAIL;
    let total_hits_addr = cave + OFF_RAW_TOTAL_HITS;
    let ring_addr = cave + OFF_RAW_RING;
    let mut sc = Vec::with_capacity(180);

    sc.extend_from_slice(&[0xF0, 0xFF, 0x05]);
    sc.extend_from_slice(&total_hits_addr.to_le_bytes());
    sc.push(0x60);
    sc.push(0x9C);

    sc.extend_from_slice(&[0x8B, 0x0D]);
    sc.extend_from_slice(&tail_addr.to_le_bytes());
    sc.extend_from_slice(&[0xFF, 0x05]);
    sc.extend_from_slice(&tail_addr.to_le_bytes());
    sc.extend_from_slice(&[0x83, 0xE1, (RAW_SERVER_PACKET_RING_SLOTS - 1) as u8]);
    sc.extend_from_slice(&[0x6B, 0xC9, RAW_SERVER_PACKET_SLOT_SIZE as u8]);
    sc.extend_from_slice(&[0x81, 0xC1]);
    sc.extend_from_slice(&ring_addr.to_le_bytes());

    sc.extend_from_slice(&[0x8B, 0x74, 0x24, 0x28]);
    sc.extend_from_slice(&[0x85, 0xF6]);
    sc.extend_from_slice(&[0x74, 0x14]);

    sc.extend_from_slice(&[0x8A, 0x06]);
    sc.extend_from_slice(&[0x88, 0x01]);
    sc.extend_from_slice(&[0xC6, 0x41, 0x01, 0x01]);
    sc.extend_from_slice(&[0x8D, 0x79, 0x04]);
    sc.push(0xB9);
    sc.extend_from_slice(&RAW_SERVER_PACKET_PAYLOAD_MAX.to_le_bytes());
    sc.extend_from_slice(&[0xF3, 0xA4]);
    sc.extend_from_slice(&[0xEB, 0x07]);

    sc.extend_from_slice(&[0xC6, 0x01, 0x00]);
    sc.extend_from_slice(&[0xC6, 0x41, 0x01, 0x00]);

    sc.push(0x9D);
    sc.push(0x61);
    sc.extend_from_slice(&RAW_PACKET_ORIGINAL_BYTES);
    sc.push(0xE9);
    let next_ip = raw_shellcode_addr + sc.len() as u32 + 4;
    let return_addr = RAW_PACKET_DISPATCH_ADDR + RAW_PACKET_ORIGINAL_BYTES.len() as u32;
    sc.extend_from_slice(&(return_addr.wrapping_sub(next_ip) as i32).to_le_bytes());

    sc
}

pub fn install(h: HANDLE, pid: u32) -> Result<ServerPacketHookHandle> {
    if HOOK_STATE.lock().ok().and_then(|s| s.clone()).is_some() {
        log_line!("[server-packet-hook] already installed");
        return Ok(HOOK_STATE
            .lock()
            .expect("SERVER_PACKET_HOOK_STATE poisoned")
            .as_ref()
            .expect("state checked")
            .clone());
    }

    let live = memory::read_bytes(h, LOW_OPCODE_HOOK_ADDR, LOW_OPCODE_ORIGINAL_BYTES.len())
        .context("read server packet hook site failed")?;
    if live != LOW_OPCODE_ORIGINAL_BYTES {
        bail!(
            "[server-packet-hook] hook site 0x{LOW_OPCODE_HOOK_ADDR:08X} bytes mismatch: expected {:02X?}, got {:02X?}",
            LOW_OPCODE_ORIGINAL_BYTES,
            live
        );
    }
    let raw_live = memory::read_bytes(h, RAW_PACKET_DISPATCH_ADDR, RAW_PACKET_ORIGINAL_BYTES.len())
        .context("read raw server packet hook site failed")?;
    if raw_live != RAW_PACKET_ORIGINAL_BYTES {
        bail!(
            "[server-packet-hook] raw hook site 0x{RAW_PACKET_DISPATCH_ADDR:08X} bytes mismatch: expected {:02X?}, got {:02X?}",
            RAW_PACKET_ORIGINAL_BYTES,
            raw_live
        );
    }

    let cave = memory::alloc_exec(h, CODECAVE_SIZE).context("alloc server packet hook cave")?;
    memory::write_code(h, cave, &vec![0u8; CODECAVE_SIZE])
        .context("zero server packet hook cave")?;
    let sc = build_server_packet_shellcode(cave);
    if sc.len() > OFF_TAIL as usize {
        bail!("server packet shellcode too large: {} bytes", sc.len());
    }
    memory::write_code(h, cave, &sc).context("write server packet hook shellcode")?;
    let raw_sc = build_raw_server_packet_shellcode(cave);
    if OFF_RAW_SHELLCODE as usize + raw_sc.len() > OFF_RAW_TAIL as usize {
        bail!(
            "raw server packet shellcode too large: {} bytes",
            raw_sc.len()
        );
    }
    memory::write_code(h, cave + OFF_RAW_SHELLCODE, &raw_sc)
        .context("write raw server packet hook shellcode")?;

    let mut hook = [0x90u8; LOW_OPCODE_ORIGINAL_BYTES.len()];
    hook[0] = 0xE9;
    let rel = cave.wrapping_sub(LOW_OPCODE_HOOK_ADDR + 5) as i32;
    hook[1..5].copy_from_slice(&rel.to_le_bytes());
    let raw_shellcode_addr = cave + OFF_RAW_SHELLCODE;
    let mut raw_hook = [0x90u8; RAW_PACKET_ORIGINAL_BYTES.len()];
    raw_hook[0] = 0xE9;
    let raw_rel = raw_shellcode_addr.wrapping_sub(RAW_PACKET_DISPATCH_ADDR + 5) as i32;
    raw_hook[1..5].copy_from_slice(&raw_rel.to_le_bytes());

    let threads = process::suspend_threads(pid)?;
    let res = (|| -> Result<()> {
        memory::write_code(h, LOW_OPCODE_HOOK_ADDR, &hook)
            .context("write low server packet hook bytes")?;
        memory::write_code(h, RAW_PACKET_DISPATCH_ADDR, &raw_hook)
            .context("write raw server packet hook bytes")?;
        Ok(())
    })();
    process::resume_threads(threads);
    res?;

    let handle = ServerPacketHookHandle {
        cave_addr: cave,
        local_head: 0,
        raw_local_head: 0,
    };
    *HOOK_STATE
        .lock()
        .expect("SERVER_PACKET_HOOK_STATE poisoned") = Some(handle.clone());
    log_line!(
        "[server-packet-hook] installed @ 0x{LOW_OPCODE_HOOK_ADDR:08X} + raw @ 0x{RAW_PACKET_DISPATCH_ADDR:08X} -> cave 0x{cave:08X} (low={} bytes raw={} bytes, mode={})",
        sc.len(),
        raw_sc.len(),
        if MIRROR_ALL_LOW_OPCODES_FOR_DISCOVERY {
            "all-low-opcode-discovery"
        } else {
            "selected-opcodes"
        }
    );
    log_line!("[server-packet-hook] parser opcodes={SERVER_PACKET_OPCODES:?}");
    Ok(handle)
}

pub fn drain(h: HANDLE) -> Vec<Vec<u8>> {
    let Some((cave_addr, mut local_head)) = HOOK_STATE
        .lock()
        .ok()
        .and_then(|mut s| s.as_mut().map(|h| (h.cave_addr, h.local_head)))
    else {
        return Vec::new();
    };

    let tail_bytes = match memory::read_bytes(h, cave_addr + OFF_TAIL, 4) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    let tail = u32::from_le_bytes([tail_bytes[0], tail_bytes[1], tail_bytes[2], tail_bytes[3]]);
    if tail == local_head {
        return Vec::new();
    }

    let lag = tail.wrapping_sub(local_head);
    if lag > SERVER_PACKET_RING_SLOTS {
        log_line!(
            "[server-packet-hook] ring overrun: lag={} > {}",
            lag,
            SERVER_PACKET_RING_SLOTS
        );
        local_head = tail.wrapping_sub(SERVER_PACKET_RING_SLOTS);
    }

    let mut out = Vec::new();
    while local_head != tail {
        let slot_idx = local_head & (SERVER_PACKET_RING_SLOTS - 1);
        let slot_addr = cave_addr + OFF_RING + slot_idx * SERVER_PACKET_SLOT_SIZE;
        match memory::read_bytes(h, slot_addr, SERVER_PACKET_SLOT_SIZE as usize) {
            Ok(slot) => {
                if let Some(packet) = decode_server_packet_slot(&slot) {
                    out.push(packet);
                }
            }
            Err(_) => break,
        }
        local_head = local_head.wrapping_add(1);
    }

    if let Ok(mut state) = HOOK_STATE.lock() {
        if let Some(handle) = state.as_mut() {
            handle.local_head = local_head;
        }
    }

    out
}

pub fn drain_raw(h: HANDLE) -> Vec<Vec<u8>> {
    let Some((cave_addr, mut local_head)) = HOOK_STATE
        .lock()
        .ok()
        .and_then(|mut s| s.as_mut().map(|h| (h.cave_addr, h.raw_local_head)))
    else {
        return Vec::new();
    };

    let tail_bytes = match memory::read_bytes(h, cave_addr + OFF_RAW_TAIL, 4) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    let tail = u32::from_le_bytes([tail_bytes[0], tail_bytes[1], tail_bytes[2], tail_bytes[3]]);
    if tail == local_head {
        return Vec::new();
    }

    let lag = tail.wrapping_sub(local_head);
    if lag > RAW_SERVER_PACKET_RING_SLOTS {
        log_line!(
            "[server-packet-hook] raw ring overrun: lag={} > {}",
            lag,
            RAW_SERVER_PACKET_RING_SLOTS
        );
        local_head = tail.wrapping_sub(RAW_SERVER_PACKET_RING_SLOTS);
    }

    let mut out = Vec::new();
    while local_head != tail {
        let slot_idx = local_head & (RAW_SERVER_PACKET_RING_SLOTS - 1);
        let slot_addr = cave_addr + OFF_RAW_RING + slot_idx * RAW_SERVER_PACKET_SLOT_SIZE;
        match memory::read_bytes(h, slot_addr, RAW_SERVER_PACKET_SLOT_SIZE as usize) {
            Ok(slot) => {
                if let Some(packet) = decode_raw_server_packet_slot(&slot) {
                    out.push(packet);
                }
            }
            Err(_) => break,
        }
        local_head = local_head.wrapping_add(1);
    }

    if let Ok(mut state) = HOOK_STATE.lock() {
        if let Some(handle) = state.as_mut() {
            handle.raw_local_head = local_head;
        }
    }

    out
}

pub fn poll(h: HANDLE) -> usize {
    refresh_player_attack_target_ids(h);
    let low_packets = drain(h);
    let raw_packets = drain_raw(h);
    let drained_low = low_packets.len();
    let drained_raw = raw_packets.len();
    let raw_drained_opcodes = opcode_counts(&raw_packets);
    let mut packets = Vec::with_capacity(drained_low + drained_raw);
    packets.extend(low_packets);
    packets.extend(raw_packets);
    let parsed_events = record_server_packet_events(&packets);
    let drained = packets.len();
    let drained_opcodes = opcode_counts(&packets);
    let drained_samples = opcode_samples(&packets);
    let target_probe_hits =
        current_debug_target_id().map(|target_id| (target_id, target_id_hits(&packets, target_id)));
    record_poll_diag(
        h,
        drained,
        drained_low,
        drained_raw,
        drained_opcodes,
        raw_drained_opcodes,
        parsed_events,
        drained_samples,
        target_probe_hits,
    );
    drained
}

pub fn set_debug_target_id(target_id: Option<u32>) {
    let Ok(mut slot) = DEBUG_TARGET_ID.lock() else {
        return;
    };
    *slot = target_id.filter(|id| *id != 0);
}

pub fn target_recently_removed_or_dead(target_id: u32) -> bool {
    target_recently_removed_or_dead_at(target_id, Instant::now())
}

pub fn target_recently_damaged(target_id: u32) -> bool {
    target_recently_damaged_at(target_id, Instant::now())
}

fn target_recently_removed_or_dead_at(target_id: u32, now: Instant) -> bool {
    let Ok(mut targets) = RECENT_REMOVED_OR_DEAD_TARGETS.lock() else {
        return false;
    };
    targets.retain(|_, expires_at| *expires_at > now);
    targets.contains_key(&target_id)
}

fn target_recently_damaged_at(target_id: u32, now: Instant) -> bool {
    let Ok(mut targets) = RECENT_DAMAGED_TARGETS.lock() else {
        return false;
    };
    targets.retain(|_, expires_at| *expires_at > now);
    targets.contains_key(&target_id)
}

fn record_server_packet_events(packets: &[Vec<u8>]) -> ParsedEventCounts {
    record_server_packet_events_at(packets, Instant::now())
}

fn record_server_packet_events_at(packets: &[Vec<u8>], now: Instant) -> ParsedEventCounts {
    record_server_packet_events_at_with_player_ids(packets, now, current_player_attack_target_ids())
}

fn record_server_packet_events_at_with_player_ids(
    packets: &[Vec<u8>],
    now: Instant,
    player_ids: PlayerAttackTargetIds,
) -> ParsedEventCounts {
    let mut counts = ParsedEventCounts::default();
    for packet in packets {
        let Some(event) = crate::bot::packet_events::parse_server_packet(packet) else {
            continue;
        };
        match event {
            crate::bot::packet_events::BotPacketEvent::RemoveObject { object_id } => {
                counts.remove_object += 1;
                record_removed_or_dead_target(object_id, now);
            }
            crate::bot::packet_events::BotPacketEvent::Action { object_id, action }
                if action == crate::bot::packet_events::ACTION_DIE =>
            {
                counts.action_die += 1;
                record_removed_or_dead_target(object_id, now);
            }
            crate::bot::packet_events::BotPacketEvent::PutObject {
                object_id, status, ..
            } if status == crate::bot::packet_events::ACTION_DIE => {
                counts.put_object_die += 1;
                record_removed_or_dead_target(object_id, now);
            }
            crate::bot::packet_events::BotPacketEvent::Attack {
                attacker_id,
                target_id,
                damage,
                ..
            } => {
                counts.attack += 1;
                if record_attack_event(attacker_id, target_id, now, player_ids) {
                    counts.recent_attackers += 1;
                }
                if damage > 0 {
                    counts.damaged_targets += 1;
                    record_damaged_target(target_id, now);
                }
            }
            crate::bot::packet_events::BotPacketEvent::RangeSkill {
                caster_id, targets, ..
            } => {
                counts.range_skill += 1;
                let mut attacker_recorded = false;
                for target in targets {
                    if target.hit && target.damage > 0 {
                        if !attacker_recorded
                            && record_attack_event(caster_id, target.object_id, now, player_ids)
                        {
                            counts.recent_attackers += 1;
                            attacker_recorded = true;
                        }
                        counts.damaged_targets += 1;
                        record_damaged_target(target.object_id, now);
                    }
                }
            }
            crate::bot::packet_events::BotPacketEvent::HpMeter {
                object_id, clear, ..
            } => {
                counts.hp_meter += 1;
                if !clear {
                    record_damaged_target(object_id, now);
                }
            }
            _ => {}
        }
    }
    counts
}

fn record_attack_event(
    attacker_id: u32,
    target_id: u32,
    now: Instant,
    player_ids: PlayerAttackTargetIds,
) -> bool {
    if !should_record_recent_attacker(target_id, player_ids) {
        return false;
    }
    let Ok(mut attackers) = RECENT_ATTACKERS.lock() else {
        return false;
    };
    attackers.retain(|_, expires_at| *expires_at > now);
    attackers.insert(attacker_id, now + RECENT_ATTACKER_TTL);
    true
}

fn should_record_recent_attacker(target_id: u32, player_ids: PlayerAttackTargetIds) -> bool {
    !player_ids.has_any() || player_ids.contains(target_id)
}

fn refresh_player_attack_target_ids(h: HANDLE) {
    let self_char_id = memory::read_u32(h, SELF_CHAR_ID_ADDR).unwrap_or(0);
    let local_player_ptr = memory::read_u32(h, LOCAL_PLAYER_PTR_ADDR).unwrap_or(0);
    let local_target_id = if local_player_ptr != 0 {
        memory::read_u32(h, local_player_ptr + LOCAL_PLAYER_ID_OFFSET).unwrap_or(0)
    } else {
        0
    };
    let local_alt_id = if local_player_ptr != 0 {
        memory::read_u32(h, local_player_ptr + LOCAL_PLAYER_ALT_ID_OFFSET).unwrap_or(0)
    } else {
        0
    };

    let Ok(mut slot) = PLAYER_ATTACK_TARGET_IDS.lock() else {
        return;
    };
    *slot = PlayerAttackTargetIds {
        self_char_id,
        local_target_id,
        local_alt_id,
    };
}

fn current_player_attack_target_ids() -> PlayerAttackTargetIds {
    PLAYER_ATTACK_TARGET_IDS
        .lock()
        .map(|slot| *slot)
        .unwrap_or_default()
}

/// 撈最近 TTL 內所有 attacker_id(怪),用於 V4 planner 排序加權。
///
/// 注意:**不對 target=player 做 filter**。 server 廣播的 Attack 包含所有附近戰鬥
/// (其他玩家被打、自己人打怪),所以 caller 應該再用 `snapshot.valid_targets`
/// 自然交集 — 只有同時在 snapshot 內的 attacker 才會影響排序。 這樣比讓 hook 知道
/// 玩家 id 簡單,且不會誤殺(其他玩家正在打的怪 = 也算潛在威脅,優先處理也合理)。
pub fn recent_attackers(now: Instant) -> std::collections::HashSet<u32> {
    let Ok(mut attackers) = RECENT_ATTACKERS.lock() else {
        return std::collections::HashSet::new();
    };
    attackers.retain(|_, expires_at| *expires_at > now);
    attackers.keys().copied().collect()
}

fn record_removed_or_dead_target(target_id: u32, now: Instant) {
    let Ok(mut targets) = RECENT_REMOVED_OR_DEAD_TARGETS.lock() else {
        return;
    };
    targets.insert(target_id, now + RECENT_TARGET_EVENT_TTL);
}

fn record_damaged_target(target_id: u32, now: Instant) {
    let Ok(mut targets) = RECENT_DAMAGED_TARGETS.lock() else {
        return;
    };
    targets.retain(|_, expires_at| *expires_at > now);
    targets.insert(target_id, now + RECENT_TARGET_DAMAGE_TTL);
}

fn record_poll_diag(
    h: HANDLE,
    drained: usize,
    drained_low: usize,
    drained_raw: usize,
    drained_opcodes: BTreeMap<u8, usize>,
    raw_drained_opcodes: BTreeMap<u8, usize>,
    parsed_events: ParsedEventCounts,
    drained_samples: BTreeMap<u8, Vec<String>>,
    target_probe_hits: Option<(u32, BTreeMap<u8, usize>)>,
) {
    let Some(cave_addr) = HOOK_STATE
        .lock()
        .ok()
        .and_then(|state| state.as_ref().map(|handle| handle.cave_addr))
    else {
        return;
    };
    let Some(total_hits) = read_total_hits(h, cave_addr) else {
        return;
    };
    let raw_total_hits = read_raw_total_hits(h, cave_addr).unwrap_or(0);

    let Ok(mut diag) = POLL_DIAG.lock() else {
        return;
    };
    diag.drained_since_last += drained;
    diag.drained_low_since_last += drained_low;
    diag.drained_raw_since_last += drained_raw;
    for (opcode, count) in drained_opcodes {
        *diag.drained_opcodes_since_last.entry(opcode).or_default() += count;
    }
    for (opcode, count) in raw_drained_opcodes {
        *diag.raw_opcodes_since_last.entry(opcode).or_default() += count;
    }
    diag.parsed_events_since_last.add_assign(parsed_events);
    for (opcode, samples) in drained_samples {
        let entry = diag.drained_samples_since_last.entry(opcode).or_default();
        for sample in samples {
            if entry.len() >= 3 {
                break;
            }
            if !entry.contains(&sample) {
                entry.push(sample);
            }
        }
    }
    let current_probe_id = target_probe_hits.as_ref().map(|(target_id, _)| *target_id);
    if diag.target_probe_id != current_probe_id {
        diag.target_probe_id = current_probe_id;
        diag.target_probe_hits_since_last.clear();
    }
    if let Some((_, hits)) = target_probe_hits {
        for (opcode, count) in hits {
            *diag.target_probe_hits_since_last.entry(opcode).or_default() += count;
        }
    }

    let now = Instant::now();
    let Some(last_log) = diag.last_log else {
        diag.last_log = Some(now);
        diag.last_total_hits = total_hits;
        diag.last_raw_total_hits = raw_total_hits;
        return;
    };
    if now.duration_since(last_log) < Duration::from_secs(5) {
        return;
    }

    let hit_delta = total_hits.wrapping_sub(diag.last_total_hits);
    let raw_hit_delta = raw_total_hits.wrapping_sub(diag.last_raw_total_hits);
    log_line!(
        "{}",
        format_probe_diag(
            total_hits,
            hit_delta,
            raw_total_hits,
            raw_hit_delta,
            diag.drained_since_last,
            diag.drained_low_since_last,
            diag.drained_raw_since_last,
            &diag.drained_opcodes_since_last,
            &diag.raw_opcodes_since_last,
            &diag.parsed_events_since_last,
            &diag.drained_samples_since_last,
            diag.target_probe_id,
            &diag.target_probe_hits_since_last
        )
    );
    diag.last_log = Some(now);
    diag.last_total_hits = total_hits;
    diag.last_raw_total_hits = raw_total_hits;
    diag.drained_since_last = 0;
    diag.drained_low_since_last = 0;
    diag.drained_raw_since_last = 0;
    diag.drained_opcodes_since_last.clear();
    diag.raw_opcodes_since_last.clear();
    diag.parsed_events_since_last = ParsedEventCounts::default();
    diag.drained_samples_since_last.clear();
    diag.target_probe_hits_since_last.clear();
}

fn read_total_hits(h: HANDLE, cave_addr: u32) -> Option<u32> {
    let bytes = memory::read_bytes(h, cave_addr + OFF_TOTAL_HITS, 4).ok()?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_raw_total_hits(h: HANDLE, cave_addr: u32) -> Option<u32> {
    let bytes = memory::read_bytes(h, cave_addr + OFF_RAW_TOTAL_HITS, 4).ok()?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn format_probe_diag(
    total_hits: u32,
    hit_delta: u32,
    raw_total_hits: u32,
    raw_hit_delta: u32,
    drained: usize,
    drained_low: usize,
    drained_raw: usize,
    drained_opcodes: &BTreeMap<u8, usize>,
    raw_opcodes: &BTreeMap<u8, usize>,
    parsed_events: &ParsedEventCounts,
    drained_samples: &BTreeMap<u8, Vec<String>>,
    target_probe_id: Option<u32>,
    target_probe_hits: &BTreeMap<u8, usize>,
) -> String {
    let total_text = if raw_total_hits > 0 || raw_hit_delta > 0 || drained_raw > 0 {
        format!("total={total_hits}/{raw_total_hits} delta={hit_delta}/{raw_hit_delta}")
    } else {
        format!("total={total_hits} delta={hit_delta}")
    };
    let source_text = if drained_raw > 0 || drained_low != drained {
        format!(" low={drained_low} raw={drained_raw}")
    } else {
        String::new()
    };
    let opcode_text = if drained_opcodes.is_empty() {
        String::new()
    } else {
        let entries = drained_opcodes
            .iter()
            .take(12)
            .map(|(opcode, count)| format!("{opcode}:{count}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(" opcodes=[{entries}]")
    };
    let raw_opcode_text = if raw_opcodes.is_empty() {
        String::new()
    } else {
        let entries = raw_opcodes
            .iter()
            .take(12)
            .map(|(opcode, count)| format!("{opcode}:{count}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(" raw_opcodes=[{entries}]")
    };
    let event_text = format_event_counts(parsed_events, drained);
    let sample_text = if drained_samples.is_empty() {
        String::new()
    } else {
        let entries = drained_samples
            .iter()
            .take(4)
            .map(|(opcode, samples)| format!("{opcode}:{}", samples.join("|")))
            .collect::<Vec<_>>()
            .join(", ");
        format!(" samples=[{entries}]")
    };
    let target_probe_text = format_target_probe(target_probe_id, target_probe_hits);
    format!(
        "[server-packet-hook] probe: {total_text} | drained={drained}{source_text}{opcode_text}{raw_opcode_text}{event_text}{sample_text}{target_probe_text} (5s)"
    )
}

fn format_target_probe(target_id: Option<u32>, hits: &BTreeMap<u8, usize>) -> String {
    let Some(target_id) = target_id else {
        return String::new();
    };
    if hits.is_empty() {
        return format!(" target_probe=0x{target_id:08X}:none");
    }
    let entries = hits
        .iter()
        .take(12)
        .map(|(opcode, count)| format!("{opcode}:{count}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(" target_probe=0x{target_id:08X} hits=[{entries}]")
}

fn format_event_counts(counts: &ParsedEventCounts, drained: usize) -> String {
    if counts.is_empty() {
        return if drained > 0 {
            " events=[none]".to_string()
        } else {
            String::new()
        };
    }

    let mut entries = Vec::new();
    push_count(&mut entries, "Attack", counts.attack);
    push_count(&mut entries, "RangeSkill", counts.range_skill);
    push_count(&mut entries, "Remove", counts.remove_object);
    push_count(&mut entries, "ActionDie", counts.action_die);
    push_count(&mut entries, "PutDie", counts.put_object_die);
    push_count(&mut entries, "HpMeter", counts.hp_meter);
    push_count(&mut entries, "Damage", counts.damaged_targets);
    push_count(&mut entries, "Attacker", counts.recent_attackers);
    format!(" events=[{}]", entries.join(", "))
}

fn push_count(entries: &mut Vec<String>, label: &str, count: usize) {
    if count > 0 {
        entries.push(format!("{label}:{count}"));
    }
}

pub fn decode_server_packet_slot(slot: &[u8]) -> Option<Vec<u8>> {
    if slot.len() < SERVER_PACKET_SLOT_SIZE as usize || slot[1] != 1 {
        return None;
    }
    let opcode = slot[0];
    let mut packet = Vec::with_capacity(1 + SERVER_PACKET_PAYLOAD_MAX as usize);
    packet.push(opcode);
    packet.extend_from_slice(&slot[4..4 + SERVER_PACKET_PAYLOAD_MAX as usize]);
    Some(packet)
}

pub fn decode_raw_server_packet_slot(slot: &[u8]) -> Option<Vec<u8>> {
    if slot.len() < RAW_SERVER_PACKET_SLOT_SIZE as usize || slot[1] != 1 {
        return None;
    }
    let opcode = slot[0];
    let mut packet = slot[4..4 + RAW_SERVER_PACKET_PAYLOAD_MAX as usize].to_vec();
    if packet.first().copied() != Some(opcode) {
        packet.insert(0, opcode);
    }
    Some(packet)
}

fn opcode_counts(packets: &[Vec<u8>]) -> BTreeMap<u8, usize> {
    let mut counts = BTreeMap::new();
    for packet in packets {
        if let Some(opcode) = packet.first() {
            *counts.entry(*opcode).or_default() += 1;
        }
    }
    counts
}

fn opcode_samples(packets: &[Vec<u8>]) -> BTreeMap<u8, Vec<String>> {
    let mut samples = BTreeMap::new();
    for packet in packets {
        let Some(opcode) = packet.first().copied() else {
            continue;
        };
        let entry: &mut Vec<String> = samples.entry(opcode).or_default();
        if entry.len() >= 3 {
            continue;
        }
        let sample = hex_prefix(packet, 24);
        if !entry.contains(&sample) {
            entry.push(sample);
        }
    }
    samples
}

fn current_debug_target_id() -> Option<u32> {
    DEBUG_TARGET_ID.lock().ok().and_then(|slot| *slot)
}

fn target_id_hits(packets: &[Vec<u8>], target_id: u32) -> BTreeMap<u8, usize> {
    let needle = target_id.to_le_bytes();
    let mut hits = BTreeMap::new();
    for packet in packets {
        let Some(opcode) = packet.first().copied() else {
            continue;
        };
        if packet
            .windows(needle.len())
            .any(|window| window == needle.as_slice())
        {
            *hits.entry(opcode).or_default() += 1;
        }
    }
    hits
}

fn hex_prefix(bytes: &[u8], max_len: usize) -> String {
    bytes
        .iter()
        .take(max_len)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CAVE: u32 = 0x1000_0000;

    #[test]
    fn low_opcode_hook_targets_dispatch_entry_without_changing_dispatch_table() {
        assert_eq!(LOW_OPCODE_HOOK_ADDR, 0x0053_9394);
        assert_eq!(
            LOW_OPCODE_ORIGINAL_BYTES,
            [0x8B, 0x85, 0x0C, 0x9F, 0xFF, 0xFF]
        );
        assert_eq!(RAW_PACKET_DISPATCH_ADDR, 0x0054_4A20);
        assert_eq!(
            RAW_PACKET_ORIGINAL_BYTES,
            [0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x1C]
        );
        assert_eq!(PROCESS_PACKET_DISPATCH_TABLE, 0x0054_15B4);
        assert_eq!(
            SERVER_PACKET_OPCODES,
            [
                crate::bot::packet_events::S_OPCODE_MOVE_OBJECT,
                crate::bot::packet_events::S_OPCODE_ATTACK,
                crate::bot::packet_events::S_OPCODE_RANGESKILLS,
                crate::bot::packet_events::S_OPCODE_PUT_OBJECT,
                crate::bot::packet_events::S_OPCODE_REMOVE_OBJECT,
                crate::bot::packet_events::S_OPCODE_ACTION,
                crate::bot::packet_events::S_OPCODE_HP_METER,
            ]
        );
    }

    #[test]
    fn raw_shellcode_replays_packet_dispatch_prologue() {
        let sc = build_raw_server_packet_shellcode(TEST_CAVE);

        assert!(
            sc.windows(RAW_PACKET_ORIGINAL_BYTES.len())
                .any(|window| window == RAW_PACKET_ORIGINAL_BYTES),
            "raw observer must replay packet dispatch prologue"
        );
        assert!(
            sc.windows(4)
                .any(|window| window == [0x8B, 0x74, 0x24, 0x28]),
            "raw observer must read packet pointer from original arg1"
        );
    }

    #[test]
    fn codecave_layout_contains_raw_packet_ring() {
        let low_ring_end = OFF_RING + SERVER_PACKET_RING_SLOTS * SERVER_PACKET_SLOT_SIZE;
        let raw_ring_end =
            OFF_RAW_RING + RAW_SERVER_PACKET_RING_SLOTS * RAW_SERVER_PACKET_SLOT_SIZE;

        assert!(low_ring_end < OFF_RAW_SHELLCODE);
        assert!(raw_ring_end <= CODECAVE_SIZE as u32);
    }

    #[test]
    fn shellcode_mirrors_all_low_opcodes_in_discovery_mode() {
        let sc = build_server_packet_shellcode(TEST_CAVE);

        assert!(MIRROR_ALL_LOW_OPCODES_FOR_DISCOVERY);

        assert!(
            !SERVER_PACKET_OPCODES.iter().any(|&opcode| sc
                .windows(cmp_opcode_pattern(opcode).len())
                .any(|window| window == cmp_opcode_pattern(opcode))),
            "discovery mode must not filter before mirroring"
        );
    }

    #[test]
    fn shellcode_replays_original_dispatch_after_observing() {
        let sc = build_server_packet_shellcode(TEST_CAVE);

        assert!(
            sc.windows(LOW_OPCODE_ORIGINAL_BYTES.len())
                .any(|window| window == LOW_OPCODE_ORIGINAL_BYTES),
            "must replay mov eax, [ebp-0x60f4]"
        );
        assert!(
            sc.windows(7)
                .any(|window| window == [0xFF, 0x24, 0x85, 0xB4, 0x15, 0x54, 0x00]),
            "must tail jmp through original ProcessPacket dispatch table"
        );
    }

    #[test]
    fn shellcode_writes_slot_opcode_from_dispatch_opcode_slot() {
        let sc = build_server_packet_shellcode(TEST_CAVE);
        let mut expected = vec![0x8A, 0x85];
        expected.extend_from_slice(&OPCODE_EBP_OFFSET.to_le_bytes());
        expected.extend_from_slice(&[0x88, 0x01]);

        assert!(
            sc.windows(expected.len())
                .any(|window| window == expected.as_slice()),
            "mirrored packet opcode must come from [ebp-0x60f4], the same ProcessPacket opcode slot used by dispatch"
        );
        assert!(
            !sc.windows(3).any(|window| window == [0x8A, 0x45, 0xF3]),
            "low opcode observer must not reuse the high-hook scratch byte [ebp-0x0d] as the packet opcode"
        );
    }

    #[test]
    fn slot_decode_reconstructs_opcode_prefixed_server_packet() {
        let mut slot = vec![0u8; SERVER_PACKET_SLOT_SIZE as usize];
        slot[0] = crate::bot::packet_events::S_OPCODE_ACTION;
        slot[1] = 1;
        slot[4..8].copy_from_slice(&0x0102_0304u32.to_le_bytes());
        slot[8] = crate::bot::packet_events::ACTION_DIE;

        let packet = decode_server_packet_slot(&slot).expect("packet");

        assert_eq!(packet[0], crate::bot::packet_events::S_OPCODE_ACTION);
        assert_eq!(&packet[1..6], &slot[4..9]);
    }

    #[test]
    fn slot_decode_keeps_unknown_low_opcode_for_discovery() {
        let mut slot = vec![0u8; SERVER_PACKET_SLOT_SIZE as usize];
        slot[0] = 77;
        slot[1] = 1;
        slot[4] = 0xAB;

        let packet = decode_server_packet_slot(&slot).expect("packet");

        assert_eq!(packet[0], 77);
        assert_eq!(packet[1], 0xAB);
    }

    #[test]
    fn raw_slot_decode_keeps_original_opcode_prefixed_packet() {
        let mut slot = vec![0u8; RAW_SERVER_PACKET_SLOT_SIZE as usize];
        slot[0] = crate::bot::packet_events::S_OPCODE_ATTACK;
        slot[1] = 1;
        slot[4] = crate::bot::packet_events::S_OPCODE_ATTACK;
        slot[5] = 0xCC;

        let packet = decode_raw_server_packet_slot(&slot).expect("packet");

        assert_eq!(packet[0], crate::bot::packet_events::S_OPCODE_ATTACK);
        assert_eq!(packet[1], 0xCC);
    }

    #[test]
    fn target_packet_events_mark_targets_recently_removed_or_dead() {
        let now = Instant::now();
        let action_target_id: u32 = 0x0BAD_0001;
        let remove_target_id: u32 = 0x0BAD_0002;
        let mut action_packet = vec![crate::bot::packet_events::S_OPCODE_ACTION];
        action_packet.extend_from_slice(&action_target_id.to_le_bytes());
        action_packet.push(crate::bot::packet_events::ACTION_DIE);
        let mut remove_packet = vec![crate::bot::packet_events::S_OPCODE_REMOVE_OBJECT];
        remove_packet.extend_from_slice(&remove_target_id.to_le_bytes());

        let counts = record_server_packet_events_at(&[action_packet, remove_packet], now);

        assert_eq!(counts.action_die, 1);
        assert_eq!(counts.remove_object, 1);
        assert!(target_recently_removed_or_dead_at(
            action_target_id,
            now + Duration::from_secs(3)
        ));
        assert!(target_recently_removed_or_dead_at(
            remove_target_id,
            now + Duration::from_secs(3)
        ));
        assert!(!target_recently_removed_or_dead_at(
            action_target_id,
            now + Duration::from_secs(5)
        ));
    }

    #[test]
    fn attack_damage_packet_marks_target_recently_damaged() {
        let now = Instant::now();
        let target_id: u32 = 0x0BAD_1001;
        let mut attack_packet = vec![crate::bot::packet_events::S_OPCODE_ATTACK, 0];
        attack_packet.extend_from_slice(&0x0BAD_2001u32.to_le_bytes());
        attack_packet.extend_from_slice(&target_id.to_le_bytes());
        attack_packet.extend_from_slice(&33u16.to_le_bytes());
        attack_packet.push(0);

        let counts = record_server_packet_events_at(&[attack_packet], now);

        assert_eq!(counts.attack, 1);
        assert_eq!(counts.damaged_targets, 1);
        assert_eq!(counts.recent_attackers, 1);
        assert!(target_recently_damaged_at(
            target_id,
            now + Duration::from_secs(3)
        ));
        assert!(!target_recently_damaged_at(
            target_id,
            now + Duration::from_secs(7)
        ));
    }

    #[test]
    fn attack_against_non_player_does_not_mark_recent_attacker_when_identity_known() {
        let now = Instant::now();
        let attacker_id: u32 = 0x0BAD_7001;
        let target_id: u32 = 0x0BAD_7002;
        let player_ids = PlayerAttackTargetIds {
            self_char_id: 0xD3,
            local_target_id: 0xBF,
            local_alt_id: 0,
        };
        let mut attack_packet = vec![crate::bot::packet_events::S_OPCODE_ATTACK, 0];
        attack_packet.extend_from_slice(&attacker_id.to_le_bytes());
        attack_packet.extend_from_slice(&target_id.to_le_bytes());
        attack_packet.extend_from_slice(&7u16.to_le_bytes());
        attack_packet.push(0);

        let counts =
            record_server_packet_events_at_with_player_ids(&[attack_packet], now, player_ids);

        assert_eq!(counts.attack, 1);
        assert_eq!(counts.damaged_targets, 1);
        assert_eq!(counts.recent_attackers, 0);
    }

    #[test]
    fn attack_against_player_marks_recent_attacker_when_identity_known() {
        let now = Instant::now();
        let attacker_id: u32 = 0x0BAD_7101;
        let self_char_id: u32 = 0xD3;
        let player_ids = PlayerAttackTargetIds {
            self_char_id,
            local_target_id: 0xBF,
            local_alt_id: 0,
        };
        let mut attack_packet = vec![crate::bot::packet_events::S_OPCODE_ATTACK, 0];
        attack_packet.extend_from_slice(&attacker_id.to_le_bytes());
        attack_packet.extend_from_slice(&self_char_id.to_le_bytes());
        attack_packet.extend_from_slice(&5u16.to_le_bytes());
        attack_packet.push(0);

        let counts =
            record_server_packet_events_at_with_player_ids(&[attack_packet], now, player_ids);

        assert_eq!(counts.attack, 1);
        assert_eq!(counts.recent_attackers, 1);
    }

    #[test]
    fn hp_meter_packet_marks_target_progress() {
        let now = Instant::now();
        let target_id: u32 = 0x0BAD_7201;
        let mut packet = vec![crate::bot::packet_events::S_OPCODE_HP_METER];
        packet.extend_from_slice(&target_id.to_le_bytes());
        packet.extend_from_slice(&80u16.to_le_bytes());

        let counts = record_server_packet_events_at(&[packet], now);

        assert_eq!(counts.hp_meter, 1);
        assert_eq!(counts.damaged_targets, 0);
    }

    #[test]
    fn range_skill_damage_packet_marks_caster_as_recent_attacker() {
        let now = Instant::now();
        let caster_id: u32 = 0x0BAD_3001;
        let target_id: u32 = 0x0BAD_4001;
        let mut packet = vec![crate::bot::packet_events::S_OPCODE_RANGESKILLS, 18];
        packet.extend_from_slice(&caster_id.to_le_bytes());
        packet.extend_from_slice(&32768u16.to_le_bytes());
        packet.extend_from_slice(&32769u16.to_le_bytes());
        packet.push(3);
        packet.extend_from_slice(&55u32.to_le_bytes());
        packet.extend_from_slice(&777u16.to_le_bytes());
        packet.push(8);
        packet.extend_from_slice(&0u16.to_le_bytes());
        packet.extend_from_slice(&1u16.to_le_bytes());
        packet.extend_from_slice(&target_id.to_le_bytes());
        packet.extend_from_slice(&0x20u16.to_le_bytes());
        packet.extend_from_slice(&123u32.to_le_bytes());

        let counts = record_server_packet_events_at(&[packet], now);

        assert_eq!(counts.range_skill, 1);
        assert_eq!(counts.damaged_targets, 1);
        assert_eq!(counts.recent_attackers, 1);
        assert!(
            recent_attackers(now + Duration::from_secs(3)).contains(&caster_id),
            "range skill caster must be prioritized as a recent attacker"
        );
    }

    #[test]
    fn range_skill_against_non_player_does_not_mark_recent_attacker_when_identity_known() {
        let now = Instant::now();
        let caster_id: u32 = 0x0BAD_7301;
        let monster_target_id: u32 = 0x0BAD_7302;
        let player_ids = PlayerAttackTargetIds {
            self_char_id: 0xD3,
            local_target_id: 0xBF,
            local_alt_id: 0,
        };
        let mut packet = vec![crate::bot::packet_events::S_OPCODE_RANGESKILLS, 18];
        packet.extend_from_slice(&caster_id.to_le_bytes());
        packet.extend_from_slice(&32768u16.to_le_bytes());
        packet.extend_from_slice(&32769u16.to_le_bytes());
        packet.push(3);
        packet.extend_from_slice(&55u32.to_le_bytes());
        packet.extend_from_slice(&777u16.to_le_bytes());
        packet.push(8);
        packet.extend_from_slice(&0u16.to_le_bytes());
        packet.extend_from_slice(&1u16.to_le_bytes());
        packet.extend_from_slice(&monster_target_id.to_le_bytes());
        packet.extend_from_slice(&0x20u16.to_le_bytes());
        packet.extend_from_slice(&123u32.to_le_bytes());

        let counts = record_server_packet_events_at_with_player_ids(&[packet], now, player_ids);

        assert_eq!(counts.range_skill, 1);
        assert_eq!(counts.damaged_targets, 1);
        assert_eq!(counts.recent_attackers, 0);
    }

    #[test]
    fn zero_damage_attack_does_not_mark_target_recently_damaged() {
        let now = Instant::now();
        let target_id: u32 = 0x0BAD_1002;
        let mut attack_packet = vec![crate::bot::packet_events::S_OPCODE_ATTACK, 0];
        attack_packet.extend_from_slice(&0x0BAD_2002u32.to_le_bytes());
        attack_packet.extend_from_slice(&target_id.to_le_bytes());
        attack_packet.extend_from_slice(&0u16.to_le_bytes());
        attack_packet.push(0);

        let counts = record_server_packet_events_at(&[attack_packet], now);

        assert_eq!(counts.attack, 1);
        assert_eq!(counts.damaged_targets, 0);
        assert_eq!(counts.recent_attackers, 1);
        assert!(!target_recently_damaged_at(target_id, now));
    }

    #[test]
    fn probe_diag_reports_hits_and_drained_deltas() {
        assert_eq!(
            format_probe_diag(
                42,
                7,
                0,
                0,
                6,
                6,
                0,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &ParsedEventCounts::default(),
                &BTreeMap::new(),
                None,
                &BTreeMap::new()
            ),
            "[server-packet-hook] probe: total=42 delta=7 | drained=6 events=[none] (5s)"
        );
    }

    #[test]
    fn probe_diag_includes_drained_opcode_counts_when_available() {
        let mut counts = BTreeMap::new();
        counts.insert(crate::bot::packet_events::S_OPCODE_ATTACK, 2);
        counts.insert(77, 1);
        let mut events = ParsedEventCounts::default();
        events.attack = 2;
        events.damaged_targets = 2;
        events.recent_attackers = 2;

        assert_eq!(
            format_probe_diag(
                42,
                7,
                0,
                0,
                3,
                3,
                0,
                &counts,
                &BTreeMap::new(),
                &events,
                &BTreeMap::new(),
                None,
                &BTreeMap::new()
            ),
            "[server-packet-hook] probe: total=42 delta=7 | drained=3 opcodes=[30:2, 77:1] events=[Attack:2, Damage:2, Attacker:2] (5s)"
        );
    }

    #[test]
    fn probe_diag_includes_payload_samples_when_available() {
        let mut counts = BTreeMap::new();
        counts.insert(153, 2);
        let mut samples = BTreeMap::new();
        samples.insert(153, vec!["99AABBCC".to_string(), "99112233".to_string()]);

        assert_eq!(
            format_probe_diag(
                42,
                7,
                0,
                0,
                2,
                2,
                0,
                &counts,
                &BTreeMap::new(),
                &ParsedEventCounts::default(),
                &samples,
                None,
                &BTreeMap::new()
            ),
            "[server-packet-hook] probe: total=42 delta=7 | drained=2 opcodes=[153:2] events=[none] samples=[153:99AABBCC|99112233] (5s)"
        );
    }

    #[test]
    fn target_id_hits_finds_little_endian_id_in_unknown_opcode() {
        let target_id = 0x0BEC_05E9u32;
        let mut matching_packet = vec![153, 0xAA, 0xBB];
        matching_packet.extend_from_slice(&target_id.to_le_bytes());
        matching_packet.push(0xCC);
        let packets = vec![
            matching_packet,
            vec![153, 0xE9, 0x05, 0x00, 0x00],
            vec![30, 0x00, 0x00],
        ];

        let hits = target_id_hits(&packets, target_id);

        assert_eq!(hits.get(&153), Some(&1));
        assert_eq!(hits.get(&30), None);
    }

    #[test]
    fn probe_diag_includes_target_probe_hits_when_available() {
        let mut hits = BTreeMap::new();
        hits.insert(153, 2);

        assert_eq!(
            format_probe_diag(
                42,
                7,
                0,
                0,
                2,
                2,
                0,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &ParsedEventCounts::default(),
                &BTreeMap::new(),
                Some(0x0BEC_05E9),
                &hits
            ),
            "[server-packet-hook] probe: total=42 delta=7 | drained=2 events=[none] target_probe=0x0BEC05E9 hits=[153:2] (5s)"
        );
    }

    #[test]
    fn probe_diag_includes_target_probe_miss_when_target_is_configured() {
        assert_eq!(
            format_probe_diag(
                42,
                7,
                0,
                0,
                2,
                2,
                0,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &ParsedEventCounts::default(),
                &BTreeMap::new(),
                Some(0x0BEC_05E9),
                &BTreeMap::new()
            ),
            "[server-packet-hook] probe: total=42 delta=7 | drained=2 events=[none] target_probe=0x0BEC05E9:none (5s)"
        );
    }

    fn cmp_opcode_pattern(opcode: u8) -> Vec<u8> {
        let mut pattern = vec![0x81, 0xBD];
        pattern.extend_from_slice(&(-0x60f4i32).to_le_bytes());
        pattern.extend_from_slice(&(opcode as u32).to_le_bytes());
        pattern
    }
}
