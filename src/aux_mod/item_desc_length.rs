//! Item tooltip length and long-status packet patches.
//!
//! The native splitter at `0x004AEC90` writes offsets into a small stack buffer.
//! Increasing that buffer directly trips the caller's GS cookie, so this module keeps
//! the native inline buffer small and stores the full split result in heap sidecar
//! records keyed by the caller's `out_breaks` pointer.
//!
//! Rendering is split into stacked UI cells:
//! - native cell: lines 0..9
//! - sidecar cell 1: lines 10..29
//! - sidecar cell 2: lines 30..49
//!
//! Sidecar record layout:
//! ```text
//! record+0x00  out_breaks tag
//! record+0x04  duplicated tooltip text pointer
//! record+0x08  full line count returned by the native helper
//! record+0x0C  inline clamp count returned to the native caller
//! record+0x10  16-bit line offset table followed by copied text
//! ```
//!
//! Hooks are installed from a worker because the target sites are decrypted on execute.
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HANDLE;

use crate::logger::log_line;
use crate::platform::memory;

/// Native tooltip splitter entry (`FUN_004AEC90`).
const SPLITTER: u32 = 0x004A_EC90;
/// Splitter prologue: `push ebp; mov ebp, esp; sub esp, 0x58`.
const SPLITTER_ORIGINAL: [u8; 6] = [0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x58];
/// Frame-height hook inside `FUN_004BF680`.
const FRAME_HEIGHT_HOOK: u32 = 0x004B_FE4B;
const FRAME_HEIGHT_ORIGINAL: [u8; 6] = [0x8B, 0x50, 0x14, 0x6B, 0xD2, 0x0C];
const FRAME_HEIGHT_RETURN: u32 = 0x004B_FE51;
const FRAME_SURFACE_HOOK: u32 = 0x004B_FF45;
const FRAME_SURFACE_ORIGINAL: [u8; 7] = [0x8B, 0x45, 0xBC, 0x50, 0x8B, 0x4D, 0xB8];
const FRAME_SURFACE_RETURN: u32 = 0x004B_FF4C;
const FRAME_BACKGROUND_DRAW: u32 = 0x0055_7530;
const FRAME_RECT_DRAW: u32 = 0x0055_5010;
/// Extra draw hook inside `FUN_004BF680` after native line rendering.
const EXTRA_DRAW_HOOK: u32 = 0x004C_066B;
const EXTRA_DRAW_ORIGINAL: [u8; 5] = [0xE9, 0xCA, 0x01, 0x00, 0x00];
const EXTRA_DRAW_RETURN: u32 = 0x004C_083A;
/// Native line-break helper (`FUN_0042AF10`).
const HELPER: u32 = 0x0042_AF10;
/// Native `strlen` direct-call target.
const STRLEN: u32 = 0x0079_A12C;
/// Native `strdup` IAT slot.
const STRDUP_SLOT: u32 = 0x008C_8664;
/// Raw line draw is kept only as test evidence that the extra hook uses color rendering.
#[cfg(test)]
const RAW_LINE_DRAW: u32 = 0x0046_FEC0;
const COLOR_RENDERER: u32 = 0x0046_E0F0;
const SURFACE_GLOBAL: u32 = 0x009A_84E0;
const ITEM_TEXT_COLOR_TABLE: u32 = 0x00C2_D698;
const TOOLTIP_FRAME_COLOR: u32 = 0x0095_FB54;
const TOOLTIP_FRAME_SHADOW: u32 = 0x0095_FB5C;

/// Native splitter width seed (`[ebp-0x4C] = 0x17`).
const SPLIT_WIDTH: u32 = 0x17;
/// Native inline clamp for non-tooltip callers.
/// Non-tooltip callers remain capped to the legacy inline count.
const MAX_INLINE: u32 = 19;
/// Tooltip UI uses a compact native header cell, then 20-line sidecar cells.
const TOP_CELL_LINES: u32 = 10;
const LINES_PER_UI_CELL: u32 = 20;
const TAIL_CELL_START: u32 = TOP_CELL_LINES + LINES_PER_UI_CELL;
const LINE_HEIGHT: u32 = 0x0C;
const SIDE_CELL_HEIGHT_DELTA: u32 = (LINES_PER_UI_CELL - TOP_CELL_LINES) * LINE_HEIGHT;
const SIDE_CELL_BOTTOM_TRIM: u32 = LINE_HEIGHT;
const SIDE_CELL_STACKED_HEIGHT_DELTA: u32 = SIDE_CELL_HEIGHT_DELTA - SIDE_CELL_BOTTOM_TRIM;
const FRAME_INSET: u8 = 5;
const CELL_JOIN_OVERLAP: u8 = 1;
/// Keep native inline rendering inside one UI cell. Extra lines are drawn by the sidecar hook.
const MAX_INLINE_TOOLTIP: u32 = TOP_CELL_LINES;
/// Tooltip target line count stored in the sidecar/render hook.
const TARGET_TOOLTIP_LINES: u32 = 50;
/// Crash isolation build: keep packet/splitter support, but do not enlarge/draw the tooltip frame.
const ENABLE_EXTENDED_TOOLTIP_RENDER: bool = true;
const SCREEN_WIDTH: u32 = 0x320;
const SCREEN_HEIGHT: u32 = 0x258;
/// Tooltip callers that can safely render the compact native tooltip cell:
///   0x4AF04A = FUN_004AEF30(item+0x18)
///   0x4AF1C3 = FUN_004AF070(this+0x18, hover path)
///   0x45AA02 = FUN_0045A8F0(this+0x20)
const TOOLTIP_RETS: [u32; 3] = [0x004A_F04A, 0x004A_F1C3, 0x0045_AA02];

/// Sidecar slot hash: `(out_breaks >> 2) & RECORD_MASK`.
const RECORD_MASK: u32 = 0x3F;
/// Sidecar slot stride shift.
const RECORD_SHIFT: u32 = 0x0C;
/// One sidecar record stores offsets plus copied text.
const RECORD_STRIDE: usize = 0x1000;
/// 64 sidecar records.
const STATE_SIZE: usize = (RECORD_MASK as usize + 1) * RECORD_STRIDE;

const _: () = assert!(
    (RECORD_STRIDE - 0x10) / 2 > TARGET_TOOLTIP_LINES as usize,
    "sidecar record table must hold target tooltip lines"
);

/// Splitter codecave capacity.
const CAVE_SIZE: usize = 0x200;
const FRAME_CAVE_SIZE: usize = 0x100;
const FRAME_SURFACE_CAVE_SIZE: usize = 0x240;
const EXTRA_CAVE_SIZE: usize = 0x900;
const EXTRA_DRAW_TEMP_SIZE: usize = 0x200;

/// Worker timeout for decrypt-on-execute hook sites.
const WORKER_TIMEOUT_MS: u64 = 600_000;
/// Worker poll interval.
const POLL_MS: u64 = 200;

/// Guard against installing this hook twice.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install sidecar splitter plus tooltip render hooks.
pub fn install(h: HANDLE) -> Result<()> {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let outcome = (|| -> Result<(u32, u32, u32, u32, u32, u32)> {
        let state =
            memory::alloc_exec(h, STATE_SIZE).context("[item_desc_length] alloc sidecar state")?;
        let cave =
            memory::alloc_exec(h, CAVE_SIZE).context("[item_desc_length] alloc splitter cave")?;
        let frame_cave = memory::alloc_exec(h, FRAME_CAVE_SIZE)
            .context("[item_desc_length] alloc frame height cave")?;
        let frame_surface_cave = memory::alloc_exec(h, FRAME_SURFACE_CAVE_SIZE)
            .context("[item_desc_length] alloc frame surface guard cave")?;
        let extra_cave = memory::alloc_exec(h, EXTRA_CAVE_SIZE)
            .context("[item_desc_length] alloc extra draw cave")?;
        let temp = memory::alloc_exec(h, EXTRA_DRAW_TEMP_SIZE)
            .context("[item_desc_length] alloc extra draw temp")?;
        let shellcode = build_split_sidecar_cave(cave, state);
        if shellcode.len() > CAVE_SIZE {
            anyhow::bail!(
                "[item_desc_length] cave shellcode too large: {} > {}",
                shellcode.len(),
                CAVE_SIZE
            );
        }
        let frame_shellcode = build_frame_height_cave(frame_cave, state);
        if frame_shellcode.len() > FRAME_CAVE_SIZE {
            anyhow::bail!(
                "[item_desc_length] frame cave shellcode too large: {} > {}",
                frame_shellcode.len(),
                FRAME_CAVE_SIZE
            );
        }
        let frame_surface_shellcode = build_frame_surface_guard_cave(frame_surface_cave, state);
        if frame_surface_shellcode.len() > FRAME_SURFACE_CAVE_SIZE {
            anyhow::bail!(
                "[item_desc_length] frame surface cave shellcode too large: {} > {}",
                frame_surface_shellcode.len(),
                FRAME_SURFACE_CAVE_SIZE
            );
        }
        let extra_shellcode = build_extra_draw_cave(extra_cave, state, temp);
        if extra_shellcode.len() > EXTRA_CAVE_SIZE {
            anyhow::bail!(
                "[item_desc_length] extra draw cave shellcode too large: {} > {}",
                extra_shellcode.len(),
                EXTRA_CAVE_SIZE
            );
        }
        memory::write_code(h, cave, &shellcode)
            .context("[item_desc_length] write splitter cave")?;
        memory::write_code(h, frame_cave, &frame_shellcode)
            .context("[item_desc_length] write frame height cave")?;
        memory::write_code(h, frame_surface_cave, &frame_surface_shellcode)
            .context("[item_desc_length] write frame surface guard cave")?;
        memory::write_code(h, extra_cave, &extra_shellcode)
            .context("[item_desc_length] write extra draw cave")?;
        Ok((
            state,
            cave,
            frame_cave,
            frame_surface_cave,
            extra_cave,
            temp,
        ))
    })();

    let (state, cave, frame_cave, frame_surface_cave, extra_cave, temp) = match outcome {
        Ok(v) => v,
        Err(e) => {
            INSTALLED.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };

    let h_raw = h.0 as usize;
    thread::Builder::new()
        .name("item-desc-length".to_string())
        .spawn(move || {
            let h = HANDLE(h_raw as *mut _);
            apply_loop(h, cave, frame_cave, frame_surface_cave, extra_cave);
        })
        .context("[item_desc_length] spawn apply worker")?;

    log_line!(
        "[item_desc_length] installed: split=0x{cave:08X} frame=0x{frame_cave:08X} frame_guard=0x{frame_surface_cave:08X} extra=0x{extra_cave:08X} temp=0x{temp:08X} state=0x{state:08X}, waiting for render hooks"
    );
    Ok(())
}

/// Worker: waits until decrypt-on-execute sites expose original bytes, then installs jumps.
fn apply_loop(h: HANDLE, cave: u32, frame_cave: u32, frame_surface_cave: u32, extra_cave: u32) {
    let start = Instant::now();
    let timeout = Duration::from_millis(WORKER_TIMEOUT_MS);
    let mut split_done = false;
    let mut frame_done = !ENABLE_EXTENDED_TOOLTIP_RENDER;
    let mut frame_surface_done = !ENABLE_EXTENDED_TOOLTIP_RENDER;
    let mut extra_done = !ENABLE_EXTENDED_TOOLTIP_RENDER;
    let mut last_count = 0usize;

    loop {
        if !split_done {
            match try_patch_splitter(h, cave) {
                SitePatch::Patched | SitePatch::AlreadyPatched => split_done = true,
                SitePatch::NotReady => {}
            }
        }
        if ENABLE_EXTENDED_TOOLTIP_RENDER && !frame_done {
            match try_patch_jmp_site(h, FRAME_HEIGHT_HOOK, &FRAME_HEIGHT_ORIGINAL, frame_cave) {
                SitePatch::Patched | SitePatch::AlreadyPatched => frame_done = true,
                SitePatch::NotReady => {}
            }
        }
        if ENABLE_EXTENDED_TOOLTIP_RENDER && !frame_surface_done {
            match try_patch_jmp_site(
                h,
                FRAME_SURFACE_HOOK,
                &FRAME_SURFACE_ORIGINAL,
                frame_surface_cave,
            ) {
                SitePatch::Patched | SitePatch::AlreadyPatched => frame_surface_done = true,
                SitePatch::NotReady => {}
            }
        }
        if ENABLE_EXTENDED_TOOLTIP_RENDER && !extra_done {
            match try_patch_jmp_site(h, EXTRA_DRAW_HOOK, &EXTRA_DRAW_ORIGINAL, extra_cave) {
                SitePatch::Patched | SitePatch::AlreadyPatched => extra_done = true,
                SitePatch::NotReady => {}
            }
        }

        let count = if ENABLE_EXTENDED_TOOLTIP_RENDER {
            [split_done, frame_done, frame_surface_done, extra_done]
                .into_iter()
                .filter(|done| *done)
                .count()
        } else if split_done {
            1
        } else {
            0
        };
        if count != last_count {
            if ENABLE_EXTENDED_TOOLTIP_RENDER {
                log_line!("[item_desc_length] render hooks routed {count}/4");
            } else {
                log_line!(
                    "[item_desc_length] diagnostic splitter routed {count}/1(render hooks disabled)"
                );
            }
            last_count = count;
        }
        if split_done && frame_done && frame_surface_done && extra_done {
            if ENABLE_EXTENDED_TOOLTIP_RENDER {
                log_line!("[item_desc_length] 50-line stacked tooltip hooks installed");
            } else {
                log_line!(
                    "[item_desc_length] diagnostic splitter installed; render hooks disabled"
                );
            }
            return;
        }
        if start.elapsed() >= timeout {
            if ENABLE_EXTENDED_TOOLTIP_RENDER {
                log_line!("[item_desc_length] worker timeout: render hooks routed {last_count}/4");
            } else {
                log_line!(
                    "[item_desc_length] worker timeout: diagnostic splitter routed {last_count}/1"
                );
            }
            return;
        }
        thread::sleep(Duration::from_millis(POLL_MS));
    }
}

enum SitePatch {
    Patched,
    AlreadyPatched,
    NotReady,
}

/// Patch the splitter prologue to jump into the codecave once it is decrypted.
fn try_patch_splitter(h: HANDLE, cave: u32) -> SitePatch {
    try_patch_jmp_site(h, SPLITTER, &SPLITTER_ORIGINAL, cave)
}

fn try_patch_jmp_site(h: HANDLE, site: u32, original: &[u8], cave: u32) -> SitePatch {
    let Ok(bytes) = memory::read_bytes(h, site, original.len()) else {
        return SitePatch::NotReady;
    };
    let patch = build_jmp_patch_len(site, cave, original.len());
    if bytes == original {
        if memory::write_code(h, site, &patch).is_ok() {
            SitePatch::Patched
        } else {
            SitePatch::NotReady
        }
    } else if bytes == patch || bytes.first() == Some(&0xE9) {
        // Another run may have already installed the jump at this site.
        SitePatch::AlreadyPatched
    } else {
        SitePatch::NotReady
    }
}

/// Build `jmp cave` plus NOP padding for the splitter prologue.
#[cfg(test)]
fn build_jmp_patch(site: u32, cave: u32) -> Vec<u8> {
    build_jmp_patch_len(site, cave, SPLITTER_ORIGINAL.len())
}

fn build_jmp_patch_len(site: u32, cave: u32, len: usize) -> Vec<u8> {
    let rel = (cave as i64 - (site as i64 + 5)) as i32;
    let mut patch = Vec::with_capacity(len);
    patch.push(0xE9);
    patch.extend_from_slice(&rel.to_le_bytes());
    while patch.len() < len {
        patch.push(0x90);
    }
    patch
}

/// Push an `E8 rel32` call.
fn push_call(sc: &mut Vec<u8>, cave: u32, target: u32) {
    sc.push(0xE8);
    let at = sc.len();
    let rel = (target as i64 - (cave as i64 + at as i64 + 4)) as i32;
    sc.extend_from_slice(&rel.to_le_bytes());
}

/// Push a short conditional/unconditional jump and return the rel8 slot.
fn push_rel8(sc: &mut Vec<u8>, opcode: u8) -> usize {
    sc.push(opcode);
    sc.push(0x00);
    sc.len() - 1
}

/// Patch a previously reserved rel8 jump slot.
fn patch_rel8(sc: &mut [u8], at: usize, target: usize) {
    let rel = target as i64 - (at as i64 + 1);
    sc[at] = rel as i8 as u8;
}

fn push_rel32(sc: &mut Vec<u8>, opcode: &[u8]) -> usize {
    sc.extend_from_slice(opcode);
    let at = sc.len();
    sc.extend_from_slice(&[0; 4]);
    at
}

fn patch_rel32(sc: &mut [u8], at: usize, target: usize) {
    let rel = target as i64 - (at as i64 + 4);
    sc[at..at + 4].copy_from_slice(&(rel as i32).to_le_bytes());
}

fn push_jmp(sc: &mut Vec<u8>, cave: u32, target: u32) {
    sc.push(0xE9);
    let at = sc.len();
    let rel = (target as i64 - (cave as i64 + at as i64 + 4)) as i32;
    sc.extend_from_slice(&rel.to_le_bytes());
}

fn push_extra_cell_y(sc: &mut Vec<u8>, cell_index: u8) {
    debug_assert!((1..=2).contains(&cell_index));
    sc.extend_from_slice(&[0x8B, 0x4D, 0xF4]);
    sc.extend_from_slice(&[0x83, 0xE9, FRAME_INSET]);
    if cell_index >= 1 {
        sc.extend_from_slice(&[0x03, 0x4D, 0xBC]);
        sc.extend_from_slice(&[0x83, 0xE9, CELL_JOIN_OVERLAP]);
    }
    if cell_index >= 2 {
        sc.extend_from_slice(&[0x03, 0x4D, 0xBC]);
        sc.extend_from_slice(&[0x81, 0xC1]);
        sc.extend_from_slice(&SIDE_CELL_STACKED_HEIGHT_DELTA.to_le_bytes());
        sc.extend_from_slice(&[0x83, 0xE9, CELL_JOIN_OVERLAP]);
    }
}

fn push_side_cell_height(sc: &mut Vec<u8>, cell_index: u8) {
    debug_assert!((1..=2).contains(&cell_index));
    sc.extend_from_slice(&[0x8B, 0x55, 0xBC]);
    sc.extend_from_slice(&[0x8B, 0x47, 0x08]);
    let count_cap = if cell_index == 1 {
        TAIL_CELL_START
    } else {
        TARGET_TOOLTIP_LINES
    };
    sc.extend_from_slice(&[0x83, 0xF8, count_cap as u8]);
    let count_ready_jle = push_rel8(sc, 0x7E);
    sc.push(0xB8);
    sc.extend_from_slice(&count_cap.to_le_bytes());
    let count_ready = sc.len();
    let zero_delta_at = if cell_index == 1 {
        TOP_CELL_LINES * 2
    } else {
        TAIL_CELL_START + TOP_CELL_LINES
    };
    sc.extend_from_slice(&[0x83, 0xE8, zero_delta_at as u8]);
    sc.extend_from_slice(&[0x6B, 0xC0, LINE_HEIGHT as u8]);
    sc.extend_from_slice(&[0x03, 0xD0]);
    sc.extend_from_slice(&[0x83, 0xEA, SIDE_CELL_BOTTOM_TRIM as u8]);
    sc.extend_from_slice(&[0x83, 0xFA, 0x01]);
    let height_positive_jge = push_rel8(sc, 0x7D);
    sc.push(0xBA);
    sc.extend_from_slice(&1u32.to_le_bytes());
    let height_positive = sc.len();
    patch_rel8(sc, count_ready_jle, count_ready);
    patch_rel8(sc, height_positive_jge, height_positive);
}

fn push_extra_cell_y_and_height(sc: &mut Vec<u8>, cell_index: u8) {
    push_extra_cell_y(sc, cell_index);
    push_side_cell_height(sc, cell_index);
    sc.push(0xB8);
    sc.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
    sc.extend_from_slice(&[0x2B, 0xC1, 0x3B, 0xC2]);
    let height_ready_jge = push_rel8(sc, 0x7D);
    sc.extend_from_slice(&[0x8B, 0xD0]);
    let height_ready = sc.len();
    patch_rel8(sc, height_ready_jge, height_ready);
}

fn push_tooltip_frame_palette(sc: &mut Vec<u8>, palette: u32) {
    sc.extend_from_slice(&[0xFF, 0x35]);
    sc.extend_from_slice(&palette.to_le_bytes());
}

fn push_surface_arg(sc: &mut Vec<u8>) {
    sc.push(0xA1);
    sc.extend_from_slice(&SURFACE_GLOBAL.to_le_bytes());
    sc.push(0x50);
}

fn push_frame_rect_call(sc: &mut Vec<u8>, cave: u32) {
    push_surface_arg(sc);
    push_call(sc, cave, FRAME_RECT_DRAW);
    sc.extend_from_slice(&[0x83, 0xC4, 0x18]);
}

fn push_x_outer(sc: &mut Vec<u8>) {
    sc.extend_from_slice(&[0x8B, 0x45, 0xF0, 0x83, 0xE8, FRAME_INSET, 0x50]);
}

fn push_x_outer_plus(sc: &mut Vec<u8>, plus: u8) {
    sc.extend_from_slice(&[0x8B, 0x45, 0xF0, 0x83, 0xE8, FRAME_INSET]);
    if plus != 0 {
        sc.extend_from_slice(&[0x83, 0xC0, plus]);
    }
    sc.push(0x50);
}

fn push_x_outer_right(sc: &mut Vec<u8>, minus: u8) {
    sc.extend_from_slice(&[0x8B, 0x45, 0xF0, 0x83, 0xE8, FRAME_INSET]);
    sc.extend_from_slice(&[0x03, 0x45, 0xB8]);
    if minus != 0 {
        sc.extend_from_slice(&[0x83, 0xE8, minus]);
    }
    sc.push(0x50);
}

fn push_y_cell(sc: &mut Vec<u8>, plus: u8) {
    if plus == 0 {
        sc.push(0x51);
    } else {
        sc.extend_from_slice(&[0x8B, 0xC1, 0x83, 0xC0, plus, 0x50]);
    }
}

fn push_y_bottom(sc: &mut Vec<u8>, minus: u8) {
    sc.extend_from_slice(&[0x8B, 0xC1, 0x03, 0xC2]);
    if minus != 0 {
        sc.extend_from_slice(&[0x83, 0xE8, minus]);
    }
    sc.push(0x50);
}

fn push_frame_width(sc: &mut Vec<u8>, minus: u8) {
    sc.extend_from_slice(&[0x8B, 0x45, 0xB8]);
    if minus != 0 {
        sc.extend_from_slice(&[0x83, 0xE8, minus]);
    }
    sc.push(0x50);
}

fn push_frame_height(sc: &mut Vec<u8>, minus: u8) {
    sc.extend_from_slice(&[0x8B, 0xC2]);
    if minus != 0 {
        sc.extend_from_slice(&[0x83, 0xE8, minus]);
    }
    sc.push(0x50);
}

fn push_one(sc: &mut Vec<u8>) {
    sc.extend_from_slice(&[0x6A, 0x01]);
}

fn push_extended_surface_y_shift(sc: &mut Vec<u8>, state: u32) {
    // Re-read the selected item and sidecar record here because this hook runs before
    // the native frame draw, while the extra draw hook runs after native x/y are inset.
    sc.extend_from_slice(&[0x8B, 0x45, 0x94]);
    sc.extend_from_slice(&[0x8B, 0x48, 0x4C]);
    sc.extend_from_slice(&[0x8B, 0x55, 0xE0]);
    sc.extend_from_slice(&[0x8B, 0x04, 0x8A]);
    sc.extend_from_slice(&[0x8B, 0x4D, 0x94]);
    sc.extend_from_slice(&[0x8B, 0x51, 0x58]);
    sc.extend_from_slice(&[0x8B, 0x04, 0x82]);
    sc.extend_from_slice(&[0x85, 0xC0]);
    let no_item_je = push_rel32(sc, &[0x0F, 0x84]);
    sc.extend_from_slice(&[0x8D, 0x48, 0x18]);
    sc.extend_from_slice(&[0x8B, 0xD1, 0xC1, 0xEA, 0x02]);
    sc.extend_from_slice(&[0x83, 0xE2, RECORD_MASK as u8]);
    sc.extend_from_slice(&[0xC1, 0xE2, RECORD_SHIFT as u8]);
    sc.push(0xB8);
    sc.extend_from_slice(&state.to_le_bytes());
    sc.extend_from_slice(&[0x03, 0xC2]);
    sc.extend_from_slice(&[0x39, 0x08]);
    let key_mismatch_jne = push_rel32(sc, &[0x0F, 0x85]);
    sc.extend_from_slice(&[0x8B, 0x48, 0x08]);
    sc.extend_from_slice(&[0x83, 0xF9, MAX_INLINE_TOOLTIP as u8]);
    let inline_only_jle = push_rel32(sc, &[0x0F, 0x8E]);

    sc.extend_from_slice(&[0x8B, 0x48, 0x08]);
    sc.extend_from_slice(&[0x83, 0xF9, TAIL_CELL_START as u8]);
    sc.extend_from_slice(&[0x8B, 0x55, 0xBC]);
    sc.extend_from_slice(&[0x8B, 0xC1]);
    let second_count_ready_jle = push_rel8(sc, 0x7E);
    sc.push(0xB8);
    sc.extend_from_slice(&TAIL_CELL_START.to_le_bytes());
    let second_count_ready = sc.len();
    sc.extend_from_slice(&[0x83, 0xE8, (TOP_CELL_LINES * 2) as u8]);
    sc.extend_from_slice(&[0x6B, 0xC0, LINE_HEIGHT as u8]);
    sc.extend_from_slice(&[0x03, 0x45, 0xBC]);
    sc.extend_from_slice(&[0x03, 0xD0]);
    sc.extend_from_slice(&[0x83, 0xEA, SIDE_CELL_BOTTOM_TRIM as u8]);
    sc.extend_from_slice(&[0x83, 0xEA, CELL_JOIN_OVERLAP]);

    sc.extend_from_slice(&[0x83, 0xF9, TAIL_CELL_START as u8]);
    let total_ready_jle = push_rel8(sc, 0x7E);
    sc.extend_from_slice(&[0x8B, 0xC1]);
    sc.extend_from_slice(&[0x83, 0xF8, TARGET_TOOLTIP_LINES as u8]);
    let tail_count_ready_jle = push_rel8(sc, 0x7E);
    sc.push(0xB8);
    sc.extend_from_slice(&TARGET_TOOLTIP_LINES.to_le_bytes());
    let tail_count_ready = sc.len();
    sc.extend_from_slice(&[0x83, 0xE8, (TAIL_CELL_START + TOP_CELL_LINES) as u8]);
    sc.extend_from_slice(&[0x6B, 0xC0, LINE_HEIGHT as u8]);
    sc.extend_from_slice(&[0x03, 0x45, 0xBC]);
    sc.extend_from_slice(&[0x03, 0xD0]);
    sc.extend_from_slice(&[0x83, 0xEA, SIDE_CELL_BOTTOM_TRIM as u8]);
    sc.extend_from_slice(&[0x83, 0xEA, CELL_JOIN_OVERLAP]);
    let total_ready = sc.len();

    sc.extend_from_slice(&[0x8B, 0x4D, 0xF4]);
    sc.extend_from_slice(&[0x03, 0xCA]);
    sc.extend_from_slice(&[0x81, 0xF9]);
    sc.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
    let bottom_ok_jle = push_rel32(sc, &[0x0F, 0x8E]);
    sc.push(0xB9);
    sc.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
    sc.extend_from_slice(&[0x2B, 0xCA, 0x85, 0xC9, 0x7D, 0x02, 0x33, 0xC9]);
    sc.extend_from_slice(&[0x89, 0x4D, 0xF4]);

    let done = sc.len();
    patch_rel32(sc, no_item_je, done);
    patch_rel32(sc, key_mismatch_jne, done);
    patch_rel32(sc, inline_only_jle, done);
    patch_rel8(sc, second_count_ready_jle, second_count_ready);
    patch_rel8(sc, total_ready_jle, total_ready);
    patch_rel8(sc, tail_count_ready_jle, tail_count_ready);
    patch_rel32(sc, bottom_ok_jle, done);
}

fn push_extra_cell_frame(sc: &mut Vec<u8>, cave: u32, cell_index: u8) {
    push_extra_cell_y(sc, cell_index);
    // `FUN_00557530` writes directly through the computed surface pointer.
    // Clamp extra-cell background height so bottom-edge tooltips cannot read past VRAM.
    sc.extend_from_slice(&[0x81, 0xF9]);
    sc.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
    let skip_background_jge = push_rel32(sc, &[0x0F, 0x8D]);
    push_extra_cell_y_and_height(sc, cell_index);
    sc.push(0x52);
    sc.extend_from_slice(&[0xFF, 0x75, 0xB8]);
    sc.push(0x51);
    push_x_outer(sc);
    push_surface_arg(sc);
    push_call(sc, cave, FRAME_BACKGROUND_DRAW);
    sc.extend_from_slice(&[0x83, 0xC4, 0x14]);
    let skip_background = sc.len();
    patch_rel32(sc, skip_background_jge, skip_background);

    push_extra_cell_y(sc, cell_index);
    sc.extend_from_slice(&[0x81, 0xF9]);
    sc.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
    let skip_border_jge = push_rel32(sc, &[0x0F, 0x8D]);

    push_extra_cell_y_and_height(sc, cell_index);
    sc.extend_from_slice(&[0x83, 0xFA, 0x03]);
    let skip_tiny_border_jl = push_rel32(sc, &[0x0F, 0x8C]);

    push_extra_cell_y_and_height(sc, cell_index);
    push_tooltip_frame_palette(sc, TOOLTIP_FRAME_COLOR);
    push_frame_height(sc, 1);
    push_one(sc);
    push_y_cell(sc, 1);
    push_x_outer(sc);
    push_frame_rect_call(sc, cave);

    push_extra_cell_y_and_height(sc, cell_index);
    push_tooltip_frame_palette(sc, TOOLTIP_FRAME_COLOR);
    push_frame_height(sc, 2);
    push_one(sc);
    push_y_cell(sc, 2);
    push_x_outer_plus(sc, 1);
    push_frame_rect_call(sc, cave);

    push_extra_cell_y_and_height(sc, cell_index);
    push_tooltip_frame_palette(sc, TOOLTIP_FRAME_SHADOW);
    push_one(sc);
    push_frame_width(sc, 1);
    push_y_bottom(sc, 1);
    push_x_outer_plus(sc, 1);
    push_frame_rect_call(sc, cave);

    push_extra_cell_y_and_height(sc, cell_index);
    push_tooltip_frame_palette(sc, TOOLTIP_FRAME_SHADOW);
    push_one(sc);
    push_frame_width(sc, 2);
    push_y_bottom(sc, 2);
    push_x_outer_plus(sc, 2);
    push_frame_rect_call(sc, cave);

    push_extra_cell_y_and_height(sc, cell_index);
    push_tooltip_frame_palette(sc, TOOLTIP_FRAME_SHADOW);
    push_frame_height(sc, 1);
    push_one(sc);
    push_y_cell(sc, 0);
    push_x_outer_right(sc, 1);
    push_frame_rect_call(sc, cave);

    push_extra_cell_y_and_height(sc, cell_index);
    push_tooltip_frame_palette(sc, TOOLTIP_FRAME_SHADOW);
    push_frame_height(sc, 2);
    push_one(sc);
    push_y_cell(sc, 1);
    push_x_outer_right(sc, 2);
    push_frame_rect_call(sc, cave);

    let skip_border = sc.len();
    patch_rel32(sc, skip_tiny_border_jl, skip_border);
    patch_rel32(sc, skip_border_jge, skip_border);
}

/// Splitter sidecar cave for `FUN_004AEC90`.
///
/// This mirrors the 3.63 sidecar approach: keep the native stack buffer bounded,
/// duplicate the source text through the native `strdup` slot, and expose the
/// full split table through heap sidecar state for the render hook.
/// Build `cave(char* text, int* out_breaks, int* out_count)`.
fn build_split_sidecar_cave(cave: u32, state: u32) -> Vec<u8> {
    let mut sc = Vec::with_capacity(192);

    // push ebp; mov ebp,esp; sub esp,4(1 local = width @ [ebp-4])
    sc.extend_from_slice(&[0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x04]);
    // push esi; push edi; push ebx
    sc.extend_from_slice(&[0x56, 0x57, 0x53]);
    // mov eax,[ebp+8]; test eax,eax; jne non_null
    sc.extend_from_slice(&[0x8B, 0x45, 0x08, 0x85, 0xC0]);
    let non_null_jne = push_rel8(&mut sc, 0x75);

    // text==0:mov ecx,[ebp+0xC]; mov [ecx],0; xor eax,eax; epilogue
    sc.extend_from_slice(&[0x8B, 0x4D, 0x0C]);
    sc.extend_from_slice(&[0xC7, 0x01, 0x00, 0x00, 0x00, 0x00]);
    sc.extend_from_slice(&[0x33, 0xC0]);
    sc.extend_from_slice(&[0x5B, 0x5F, 0x5E, 0x8B, 0xE5, 0x5D, 0xC3]);

    // non_null:
    let non_null = sc.len();
    // mov dword [ebp-4], SPLIT_WIDTH
    sc.extend_from_slice(&[0xC7, 0x45, 0xFC]);
    sc.extend_from_slice(&SPLIT_WIDTH.to_le_bytes());
    // mov esi,[ebp+0xC](out_breaks)
    sc.extend_from_slice(&[0x8B, 0x75, 0x0C]);
    // mov eax,esi; shr eax,2; and eax,RECORD_MASK; shl eax,RECORD_SHIFT(hash)
    sc.extend_from_slice(&[0x8B, 0xC6, 0xC1, 0xE8, 0x02]);
    sc.extend_from_slice(&[0x83, 0xE0, RECORD_MASK as u8]);
    sc.extend_from_slice(&[0xC1, 0xE0, RECORD_SHIFT as u8]);
    // mov edi,state; add edi,eax(edi = record)
    sc.push(0xBF);
    sc.extend_from_slice(&state.to_le_bytes());
    sc.extend_from_slice(&[0x03, 0xF8]);
    // mov [edi],esi(record[0]=out_breaks tag)
    sc.extend_from_slice(&[0x89, 0x37]);
    // mov eax,[ebp+8]; mov [edi+4],eax; push eax(record[1]=text;push text)
    sc.extend_from_slice(&[0x8B, 0x45, 0x08, 0x89, 0x47, 0x04, 0x50]);
    // call strlen; add esp,4
    push_call(&mut sc, cave, STRLEN);
    sc.extend_from_slice(&[0x83, 0xC4, 0x04]);
    // push 0; push 0
    sc.extend_from_slice(&[0x6A, 0x00, 0x6A, 0x00]);
    // lea ecx,[ebp-4]; push ecx(&width)
    sc.extend_from_slice(&[0x8D, 0x4D, 0xFC, 0x51]);
    // lea edx,[edi+0x10]; push edx(&record_table)
    sc.extend_from_slice(&[0x8D, 0x57, 0x10, 0x52]);
    // push eax(strlen); mov eax,[ebp+8]; push eax(text)
    sc.extend_from_slice(&[0x50, 0x8B, 0x45, 0x08, 0x50]);
    // call helper; add esp,0x18
    push_call(&mut sc, cave, HELPER);
    sc.extend_from_slice(&[0x83, 0xC4, 0x18]);
    // mov [edi+8],eax(record[2]=full count); mov ebx,eax
    sc.extend_from_slice(&[0x89, 0x47, 0x08, 0x8B, 0xD8]);
    // Per-caller cap: default to MAX_INLINE, tooltip callers get MAX_INLINE_TOOLTIP.
    // mov edx,MAX_INLINE
    sc.push(0xBA);
    sc.extend_from_slice(&MAX_INLINE.to_le_bytes());
    // Identify tooltip callers by comparing the return address at [ebp+4].
    let mut set31_jes = Vec::new();
    for ret in TOOLTIP_RETS {
        sc.extend_from_slice(&[0x81, 0x7D, 0x04]);
        sc.extend_from_slice(&ret.to_le_bytes());
        set31_jes.push(push_rel8(&mut sc, 0x74));
    }
    // Non-tooltip callers keep the legacy clamp.
    let skip_set31_jmp = push_rel8(&mut sc, 0xEB);
    // tooltip caller cap: mov edx, MAX_INLINE_TOOLTIP
    let set31 = sc.len();
    sc.push(0xBA);
    sc.extend_from_slice(&MAX_INLINE_TOOLTIP.to_le_bytes());
    // skip_set31:cmp ebx,edx; jbe count_ok; mov ebx,edx
    let skip_set31 = sc.len();
    sc.extend_from_slice(&[0x3B, 0xDA]);
    let count_ok_jbe = push_rel8(&mut sc, 0x76);
    sc.extend_from_slice(&[0x8B, 0xDA]);

    // count_ok:
    let count_ok = sc.len();
    // mov [edi+0xC],ebx(record[3]=clamped)
    sc.extend_from_slice(&[0x89, 0x5F, 0x0C]);
    // mov ecx,[ebp+0x10]; mov [ecx],ebx(*out_count=clamped); xor ecx,ecx
    sc.extend_from_slice(&[0x8B, 0x4D, 0x10, 0x89, 0x19, 0x33, 0xC9]);

    // copy_loop:cmp ecx,ebx; jge copy_done
    let copy_loop = sc.len();
    sc.extend_from_slice(&[0x3B, 0xCB]);
    let copy_done_jge = push_rel8(&mut sc, 0x7D);
    // movsx eax,word [edi+ecx*2+0x12](record_table[1+ecx])
    sc.extend_from_slice(&[0x0F, 0xBF, 0x44, 0x4F, 0x12]);
    // mov edx,[ebp+0xC]; mov [edx+ecx*4],eax; inc ecx
    sc.extend_from_slice(&[0x8B, 0x55, 0x0C, 0x89, 0x04, 0x8A, 0x41]);
    let copy_loop_jmp = push_rel8(&mut sc, 0xEB);

    // copy_done:xor ecx,ecx
    let copy_done = sc.len();
    sc.extend_from_slice(&[0x33, 0xC9]);

    // text_loop:mov edx,[ebp+8]; mov al,[edx+ecx]; test al,al; je ret_text
    let text_loop = sc.len();
    sc.extend_from_slice(&[0x8B, 0x55, 0x08, 0x8A, 0x04, 0x0A, 0x84, 0xC0]);
    let ret_text_je = push_rel8(&mut sc, 0x74);
    // cmp al,0x0A; jne text_next
    sc.extend_from_slice(&[0x3C, 0x0A]);
    let text_next_jne = push_rel8(&mut sc, 0x75);
    // mov byte [edx+ecx],0x20
    sc.extend_from_slice(&[0xC6, 0x04, 0x0A, 0x20]);
    // text_next:inc ecx; jmp text_loop
    let text_next = sc.len();
    sc.push(0x41);
    let text_loop_jmp = push_rel8(&mut sc, 0xEB);

    // ret_text:mov eax,[ebp+8]; push eax; call [STRDUP_SLOT]; add esp,4
    let ret_text = sc.len();
    sc.extend_from_slice(&[0x8B, 0x45, 0x08, 0x50]);
    sc.extend_from_slice(&[0xFF, 0x15]);
    sc.extend_from_slice(&STRDUP_SLOT.to_le_bytes());
    sc.extend_from_slice(&[0x83, 0xC4, 0x04]);
    // mov [edi+4],eax(record[1]=strdup text); epilogue
    sc.extend_from_slice(&[0x89, 0x47, 0x04]);
    sc.extend_from_slice(&[0x5B, 0x5F, 0x5E, 0x8B, 0xE5, 0x5D, 0xC3]);

    patch_rel8(&mut sc, non_null_jne, non_null);
    for je in set31_jes {
        patch_rel8(&mut sc, je, set31);
    }
    patch_rel8(&mut sc, skip_set31_jmp, skip_set31);
    patch_rel8(&mut sc, count_ok_jbe, count_ok);
    patch_rel8(&mut sc, copy_done_jge, copy_done);
    patch_rel8(&mut sc, copy_loop_jmp, copy_loop);
    patch_rel8(&mut sc, ret_text_je, ret_text);
    patch_rel8(&mut sc, text_next_jne, text_next);
    patch_rel8(&mut sc, text_loop_jmp, text_loop);
    sc
}

fn build_frame_height_cave(cave: u32, state: u32) -> Vec<u8> {
    let mut sc = Vec::with_capacity(FRAME_CAVE_SIZE);

    // Preserve the native `mov edx, [eax+0x14]`; `eax` is the item pointer.
    sc.extend_from_slice(&[0x8B, 0x50, 0x14]);
    // Preserve `ecx` while checking the sidecar record for `item+0x18`.
    sc.push(0x51);
    sc.extend_from_slice(&[0x8D, 0x48, 0x18]);
    sc.extend_from_slice(&[0x8B, 0xC1, 0xC1, 0xE8, 0x02]);
    sc.extend_from_slice(&[0x83, 0xE0, RECORD_MASK as u8]);
    sc.extend_from_slice(&[0xC1, 0xE0, RECORD_SHIFT as u8]);
    sc.push(0x05);
    sc.extend_from_slice(&state.to_le_bytes());
    sc.extend_from_slice(&[0x39, 0x08]);
    let key_mismatch_jne = push_rel32(&mut sc, &[0x0F, 0x85]);
    sc.extend_from_slice(&[0x8B, 0x40, 0x08]);
    sc.extend_from_slice(&[0x3B, 0xC2]);
    let inline_only_jle = push_rel32(&mut sc, &[0x0F, 0x8E]);
    sc.push(0x3D);
    sc.extend_from_slice(&MAX_INLINE_TOOLTIP.to_le_bytes());
    let count_ok_jle = push_rel8(&mut sc, 0x7E);
    sc.push(0xB8);
    sc.extend_from_slice(&MAX_INLINE_TOOLTIP.to_le_bytes());

    let count_ok = sc.len();
    sc.extend_from_slice(&[0x8B, 0xD0]);
    let done = sc.len();
    sc.push(0x59);
    sc.extend_from_slice(&[0x6B, 0xD2, 0x0C]);
    push_jmp(&mut sc, cave, FRAME_HEIGHT_RETURN);

    patch_rel32(&mut sc, key_mismatch_jne, done);
    patch_rel32(&mut sc, inline_only_jle, done);
    patch_rel8(&mut sc, count_ok_jle, count_ok);
    sc
}

fn build_frame_surface_guard_cave(cave: u32, state: u32) -> Vec<u8> {
    let mut sc = Vec::with_capacity(FRAME_SURFACE_CAVE_SIZE);

    // Keep the native cell inside the 800x600 surface; extra cells draw their own frames.
    sc.extend_from_slice(&[0x8B, 0x45, 0x94]);
    sc.extend_from_slice(&[0x8B, 0x48, 0x4C]);
    sc.extend_from_slice(&[0x8B, 0x55, 0xE0]);
    sc.extend_from_slice(&[0x8B, 0x04, 0x8A]);
    sc.extend_from_slice(&[0x8B, 0x4D, 0x94]);
    sc.extend_from_slice(&[0x8B, 0x51, 0x58]);
    sc.extend_from_slice(&[0x8B, 0x04, 0x82]);
    sc.extend_from_slice(&[0x85, 0xC0]);
    let no_item_je = push_rel8(&mut sc, 0x74);
    sc.extend_from_slice(&[0x8D, 0x48, 0x18]);
    sc.extend_from_slice(&[0x8B, 0xD1, 0xC1, 0xEA, 0x02]);
    sc.extend_from_slice(&[0x83, 0xE2, RECORD_MASK as u8]);
    sc.extend_from_slice(&[0xC1, 0xE2, RECORD_SHIFT as u8]);
    sc.push(0xB8);
    sc.extend_from_slice(&state.to_le_bytes());
    sc.extend_from_slice(&[0x03, 0xC2]);
    sc.extend_from_slice(&[0x39, 0x08]);
    let key_mismatch_jne = push_rel8(&mut sc, 0x75);
    sc.extend_from_slice(&[0x83, 0x78, 0x08, MAX_INLINE_TOOLTIP as u8]);
    let inline_only_jle = push_rel8(&mut sc, 0x7E);

    let width_ready = sc.len();

    // Clamp x/y to non-negative values before `FUN_00557530` computes the surface pointer.
    sc.extend_from_slice(&[0x83, 0x7D, 0xF0, 0x00]);
    let x_nonneg_jge = push_rel8(&mut sc, 0x7D);
    sc.extend_from_slice(&[0xC7, 0x45, 0xF0]);
    sc.extend_from_slice(&0u32.to_le_bytes());
    let x_nonneg = sc.len();

    sc.extend_from_slice(&[0x83, 0x7D, 0xF4, 0x00]);
    let y_nonneg_jge = push_rel8(&mut sc, 0x7D);
    sc.extend_from_slice(&[0xC7, 0x45, 0xF4]);
    sc.extend_from_slice(&0u32.to_le_bytes());
    let y_nonneg = sc.len();

    // If x/y are already outside the 800x600 surface, reset to the origin instead of
    // letting width/height subtraction underflow.
    sc.extend_from_slice(&[0x81, 0x7D, 0xF0]);
    sc.extend_from_slice(&SCREEN_WIDTH.to_le_bytes());
    let x_inside_jl = push_rel8(&mut sc, 0x7C);
    sc.extend_from_slice(&[0xC7, 0x45, 0xF0]);
    sc.extend_from_slice(&0u32.to_le_bytes());
    let x_inside = sc.len();

    sc.extend_from_slice(&[0x81, 0x7D, 0xF4]);
    sc.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
    let y_inside_jl = push_rel8(&mut sc, 0x7C);
    sc.extend_from_slice(&[0xC7, 0x45, 0xF4]);
    sc.extend_from_slice(&0u32.to_le_bytes());
    let y_inside = sc.len();

    // Clamp width/height to positive values and the physical surface size.
    sc.extend_from_slice(&[0x83, 0x7D, 0xB8, 0x01]);
    let width_positive_jge = push_rel8(&mut sc, 0x7D);
    sc.extend_from_slice(&[0xC7, 0x45, 0xB8]);
    sc.extend_from_slice(&1u32.to_le_bytes());
    let width_positive = sc.len();

    sc.extend_from_slice(&[0x83, 0x7D, 0xBC, 0x01]);
    let height_positive_jge = push_rel8(&mut sc, 0x7D);
    sc.extend_from_slice(&[0xC7, 0x45, 0xBC]);
    sc.extend_from_slice(&1u32.to_le_bytes());
    let height_positive = sc.len();

    sc.extend_from_slice(&[0x81, 0x7D, 0xB8]);
    sc.extend_from_slice(&SCREEN_WIDTH.to_le_bytes());
    let width_max_jle = push_rel8(&mut sc, 0x7E);
    sc.extend_from_slice(&[0xC7, 0x45, 0xB8]);
    sc.extend_from_slice(&SCREEN_WIDTH.to_le_bytes());
    let width_max = sc.len();

    sc.extend_from_slice(&[0x81, 0x7D, 0xBC]);
    sc.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
    let height_max_jle = push_rel8(&mut sc, 0x7E);
    sc.extend_from_slice(&[0xC7, 0x45, 0xBC]);
    sc.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
    let height_max = sc.len();

    // Keep the full multi-cell tooltip visible by shifting x left at the right edge.
    sc.extend_from_slice(&[0x8B, 0x45, 0xF0, 0x03, 0x45, 0xB8]);
    sc.push(0x3D);
    sc.extend_from_slice(&SCREEN_WIDTH.to_le_bytes());
    let right_ok_jle = push_rel8(&mut sc, 0x7E);
    sc.push(0xB8);
    sc.extend_from_slice(&SCREEN_WIDTH.to_le_bytes());
    sc.extend_from_slice(&[0x2B, 0x45, 0xB8, 0x89, 0x45, 0xF0]);
    let right_ok = sc.len();

    push_extended_surface_y_shift(&mut sc, state);

    // Clamp bottom edges to the surface.
    sc.extend_from_slice(&[0x8B, 0x45, 0xF4, 0x03, 0x45, 0xBC]);
    sc.push(0x3D);
    sc.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
    let bottom_ok_jle = push_rel8(&mut sc, 0x7E);
    sc.push(0xB8);
    sc.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
    sc.extend_from_slice(&[0x2B, 0x45, 0xF4, 0x89, 0x45, 0xBC]);
    let bottom_ok = sc.len();

    // Re-check after edge clipping.
    sc.extend_from_slice(&[0x83, 0x7D, 0xB8, 0x01]);
    let width_final_jge = push_rel8(&mut sc, 0x7D);
    sc.extend_from_slice(&[0xC7, 0x45, 0xB8]);
    sc.extend_from_slice(&1u32.to_le_bytes());
    let width_final = sc.len();

    sc.extend_from_slice(&[0x83, 0x7D, 0xBC, 0x01]);
    let height_final_jge = push_rel8(&mut sc, 0x7D);
    sc.extend_from_slice(&[0xC7, 0x45, 0xBC]);
    sc.extend_from_slice(&1u32.to_le_bytes());
    let height_final = sc.len();

    sc.extend_from_slice(&FRAME_SURFACE_ORIGINAL);
    push_jmp(&mut sc, cave, FRAME_SURFACE_RETURN);

    patch_rel8(&mut sc, x_nonneg_jge, x_nonneg);
    patch_rel8(&mut sc, y_nonneg_jge, y_nonneg);
    patch_rel8(&mut sc, x_inside_jl, x_inside);
    patch_rel8(&mut sc, y_inside_jl, y_inside);
    patch_rel8(&mut sc, width_positive_jge, width_positive);
    patch_rel8(&mut sc, height_positive_jge, height_positive);
    patch_rel8(&mut sc, width_max_jle, width_max);
    patch_rel8(&mut sc, height_max_jle, height_max);
    patch_rel8(&mut sc, right_ok_jle, right_ok);
    patch_rel8(&mut sc, bottom_ok_jle, bottom_ok);
    patch_rel8(&mut sc, width_final_jge, width_final);
    patch_rel8(&mut sc, height_final_jge, height_final);
    patch_rel8(&mut sc, no_item_je, width_ready);
    patch_rel8(&mut sc, key_mismatch_jne, width_ready);
    patch_rel8(&mut sc, inline_only_jle, width_ready);
    sc
}

fn build_extra_draw_cave(cave: u32, state: u32, temp: u32) -> Vec<u8> {
    let mut sc = Vec::with_capacity(EXTRA_CAVE_SIZE);

    sc.push(0x60);
    // item = this->items[this->selected_index] using the 3.8 `FUN_004BF680` layout.
    sc.extend_from_slice(&[0x8B, 0x55, 0x94]);
    sc.extend_from_slice(&[0x8B, 0x42, 0x4C]);
    sc.extend_from_slice(&[0x8B, 0x4D, 0xE0]);
    sc.extend_from_slice(&[0x8B, 0x14, 0x81]);
    sc.extend_from_slice(&[0x8B, 0x45, 0x94]);
    sc.extend_from_slice(&[0x8B, 0x48, 0x58]);
    sc.extend_from_slice(&[0x8B, 0x1C, 0x91]);
    sc.extend_from_slice(&[0x85, 0xDB]);
    let null_item_je = push_rel32(&mut sc, &[0x0F, 0x84]);

    // record = state[hash(item+0x18)]; record[0] must match `item+0x18`.
    sc.extend_from_slice(&[0x8D, 0x4B, 0x18]);
    sc.extend_from_slice(&[0x8B, 0xC1, 0xC1, 0xE8, 0x02]);
    sc.extend_from_slice(&[0x83, 0xE0, RECORD_MASK as u8]);
    sc.extend_from_slice(&[0xC1, 0xE0, RECORD_SHIFT as u8]);
    sc.push(0xBF);
    sc.extend_from_slice(&state.to_le_bytes());
    sc.extend_from_slice(&[0x03, 0xF8]);
    sc.extend_from_slice(&[0x39, 0x0F]);
    let key_mismatch_jne = push_rel32(&mut sc, &[0x0F, 0x85]);
    sc.extend_from_slice(&[0x8B, 0x47, 0x08]);
    sc.extend_from_slice(&[0x83, 0xF8, MAX_INLINE_TOOLTIP as u8]);
    let inline_only_jle = push_rel32(&mut sc, &[0x0F, 0x8E]);
    sc.extend_from_slice(&[0x8B, 0x57, 0x04]);
    sc.extend_from_slice(&[0x85, 0xD2]);
    let no_text_je = push_rel32(&mut sc, &[0x0F, 0x84]);
    push_extra_cell_frame(&mut sc, cave, 1);
    sc.extend_from_slice(&[0x83, 0x7F, 0x08, TAIL_CELL_START as u8]);
    let no_third_frame_jle = push_rel32(&mut sc, &[0x0F, 0x8E]);
    push_extra_cell_frame(&mut sc, cave, 2);
    let frames_ready = sc.len();
    sc.push(0xBE);
    sc.extend_from_slice(&MAX_INLINE_TOOLTIP.to_le_bytes());

    let loop_start = sc.len();
    sc.extend_from_slice(&[0x3B, 0x77, 0x08]);
    let count_done_jge = push_rel32(&mut sc, &[0x0F, 0x8D]);
    sc.extend_from_slice(&[0x83, 0xFE, TARGET_TOOLTIP_LINES as u8]);
    let target_done_jge = push_rel32(&mut sc, &[0x0F, 0x8D]);

    // len = record_table[i + 1] - record_table[i], text = record_text + record_table[i].
    sc.extend_from_slice(&[0x0F, 0xBF, 0x44, 0x77, 0x12]);
    sc.extend_from_slice(&[0x0F, 0xBF, 0x4C, 0x77, 0x10]);
    sc.extend_from_slice(&[0x2B, 0xC1]);
    let empty_line_jle = push_rel32(&mut sc, &[0x0F, 0x8E]);
    sc.push(0x3D);
    sc.extend_from_slice(&0xFFu32.to_le_bytes());
    let len_ok_jle = push_rel8(&mut sc, 0x7E);
    sc.push(0xB8);
    sc.extend_from_slice(&0xFFu32.to_le_bytes());
    let len_ok = sc.len();
    sc.extend_from_slice(&[0x8B, 0x57, 0x04, 0x03, 0xD1]);

    // Copy segment into temp buffer and NUL-terminate for `COLOR_RENDERER`.
    sc.extend_from_slice(&[0x56, 0x57, 0x50, 0x52]);
    sc.extend_from_slice(&[0x8B, 0xF2, 0x8B, 0xC8]);
    sc.push(0xBF);
    sc.extend_from_slice(&temp.to_le_bytes());
    sc.extend_from_slice(&[0xF3, 0xA4, 0xC6, 0x07, 0x00]);

    // Normalize uppercase color marker `\F` to lowercase `\f` in the temp buffer.
    sc.push(0xB8);
    sc.extend_from_slice(&temp.to_le_bytes());
    let norm_loop = sc.len();
    sc.extend_from_slice(&[0x8A, 0x08, 0x84, 0xC9]);
    let norm_done_je = push_rel8(&mut sc, 0x74);
    sc.extend_from_slice(&[0x80, 0xF9, 0x5C]);
    let norm_next_jne = push_rel8(&mut sc, 0x75);
    sc.extend_from_slice(&[0x80, 0x78, 0x01, 0x46]);
    let norm_next2_jne = push_rel8(&mut sc, 0x75);
    sc.extend_from_slice(&[0xC6, 0x40, 0x01, 0x66]);
    let norm_next = sc.len();
    sc.push(0x40);
    let norm_loop_jmp = push_rel8(&mut sc, 0xEB);
    let norm_done = sc.len();
    sc.extend_from_slice(&[0x83, 0xC4, 0x08, 0x5F, 0x5E]);

    // y = cell_y + (line in sidecar cell) * 12; frame inset already provides padding.
    sc.extend_from_slice(&[0x8B, 0xCE]);
    sc.extend_from_slice(&[0x83, 0xFE, TAIL_CELL_START as u8]);
    let second_cell_y_jl = push_rel8(&mut sc, 0x7C);
    sc.extend_from_slice(&[0x83, 0xE9, TAIL_CELL_START as u8]);
    sc.extend_from_slice(&[0xB8, 0x02, 0x00, 0x00, 0x00]);
    let y_cell_ready_jmp = push_rel8(&mut sc, 0xEB);
    let second_cell_y = sc.len();
    sc.extend_from_slice(&[0x83, 0xE9, MAX_INLINE_TOOLTIP as u8]);
    sc.extend_from_slice(&[0xB8, 0x01, 0x00, 0x00, 0x00]);
    let y_cell_ready = sc.len();
    sc.extend_from_slice(&[0x6B, 0xC9, 0x0C]);
    sc.extend_from_slice(&[0x03, 0x4D, 0xF4]);
    sc.extend_from_slice(&[0x03, 0x4D, 0xBC]);
    sc.extend_from_slice(&[0x83, 0xE9, CELL_JOIN_OVERLAP]);
    sc.extend_from_slice(&[0x83, 0xF8, 0x02]);
    let text_offset_ready_jl = push_rel8(&mut sc, 0x7C);
    sc.extend_from_slice(&[0x03, 0x4D, 0xBC]);
    sc.extend_from_slice(&[0x81, 0xC1]);
    sc.extend_from_slice(&SIDE_CELL_STACKED_HEIGHT_DELTA.to_le_bytes());
    sc.extend_from_slice(&[0x83, 0xE9, CELL_JOIN_OVERLAP]);
    let text_offset_ready = sc.len();

    // Draw the current extra line in the selected vertical cell.
    sc.extend_from_slice(&[0x8B, 0x55, 0xF4]);
    sc.extend_from_slice(&[0x03, 0x55, 0xBC]);
    sc.extend_from_slice(&[0x83, 0xEA, CELL_JOIN_OVERLAP]);
    sc.extend_from_slice(&[0x03, 0x55, 0xBC]);
    sc.extend_from_slice(&[0x81, 0xC2]);
    sc.extend_from_slice(&SIDE_CELL_STACKED_HEIGHT_DELTA.to_le_bytes());
    sc.extend_from_slice(&[0x83, 0xF8, 0x02]);
    let second_bottom_ready_jl = push_rel8(&mut sc, 0x7C);
    sc.extend_from_slice(&[0x83, 0xEA, CELL_JOIN_OVERLAP]);
    sc.extend_from_slice(&[0x03, 0x55, 0xBC]);
    sc.extend_from_slice(&[0x81, 0xC2]);
    sc.extend_from_slice(&SIDE_CELL_STACKED_HEIGHT_DELTA.to_le_bytes());
    let line_bottom_ready = sc.len();
    sc.extend_from_slice(&[0x81, 0xFA]);
    sc.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
    let line_bottom_screen_ok_jle = push_rel8(&mut sc, 0x7E);
    sc.push(0xBA);
    sc.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
    let line_bottom_screen_ready = sc.len();
    sc.extend_from_slice(&[0x8B, 0xC1, 0x83, 0xC0, 0x0C, 0x3B, 0xC2]);
    let line_bottom_overflow_jg = push_rel32(&mut sc, &[0x0F, 0x8F]);

    // COLOR_RENDERER(surface, temp, x, y, color, 0).
    sc.extend_from_slice(&[0x6A, 0x00]);
    sc.extend_from_slice(&[0x8B, 0x45, 0xC8]);
    sc.extend_from_slice(&[0x0F, 0xBF, 0x04, 0x45]);
    sc.extend_from_slice(&ITEM_TEXT_COLOR_TABLE.to_le_bytes());
    sc.push(0x50);
    sc.push(0x51);
    sc.extend_from_slice(&[0x8B, 0x45, 0xF0]);
    sc.push(0x50);
    sc.push(0xB8);
    sc.extend_from_slice(&temp.to_le_bytes());
    sc.push(0x50);
    sc.push(0xA1);
    sc.extend_from_slice(&SURFACE_GLOBAL.to_le_bytes());
    sc.push(0x50);
    push_call(&mut sc, cave, COLOR_RENDERER);
    sc.extend_from_slice(&[0x83, 0xC4, 0x18]);

    let next_line = sc.len();
    sc.push(0x46);
    let loop_jmp = push_rel32(&mut sc, &[0xE9]);

    let done = sc.len();
    sc.push(0x61);
    push_jmp(&mut sc, cave, EXTRA_DRAW_RETURN);

    patch_rel32(&mut sc, null_item_je, done);
    patch_rel32(&mut sc, key_mismatch_jne, done);
    patch_rel32(&mut sc, inline_only_jle, done);
    patch_rel32(&mut sc, no_text_je, done);
    patch_rel32(&mut sc, no_third_frame_jle, frames_ready);
    patch_rel32(&mut sc, count_done_jge, done);
    patch_rel32(&mut sc, target_done_jge, done);
    patch_rel32(&mut sc, empty_line_jle, next_line);
    patch_rel8(&mut sc, len_ok_jle, len_ok);
    patch_rel8(&mut sc, norm_done_je, norm_done);
    patch_rel8(&mut sc, norm_next_jne, norm_next);
    patch_rel8(&mut sc, norm_next2_jne, norm_next);
    patch_rel8(&mut sc, norm_loop_jmp, norm_loop);
    patch_rel8(&mut sc, second_cell_y_jl, second_cell_y);
    patch_rel8(&mut sc, y_cell_ready_jmp, y_cell_ready);
    patch_rel8(&mut sc, text_offset_ready_jl, text_offset_ready);
    patch_rel8(&mut sc, second_bottom_ready_jl, line_bottom_ready);
    patch_rel8(&mut sc, line_bottom_screen_ok_jle, line_bottom_screen_ready);
    patch_rel32(&mut sc, line_bottom_overflow_jg, done);
    patch_rel32(&mut sc, loop_jmp, loop_start);
    sc
}

// ============ opcode 242 custom mux + long S_ItemStatus ============
//
// The launcher-side custom packet slot is `[F2][sub_opcode][payload...]`.
// Sub-opcode `0x01` routes to a copied `FUN_00528A00` status handler.
// Unknown F2 sub-opcodes return immediately instead of falling into the native
// bail/PostQuitMessage path.
//
// The copied status handler changes the parser format from `dsdc` to `dsdh`,
// replaces the 256-byte stack status buffer with a heap buffer, converts the
// four length reads from byte to word, relocates internal calls, and jumps back
// to the native continuation at `0x00528B18`.
//
// The source block is decrypted on execute, so the worker polls until the known
// prologue, format push, buffer `lea`, length reads, and call opcodes are visible.
/// Packet dispatcher entry (`FUN_00544A20`) patched for custom opcode `0xF2`.
const DISPATCHER: u32 = 0x0054_4A20;
/// Dispatcher prologue: `push ebp; mov ebp, esp; sub esp, 0x1c`.
const DISPATCHER_ORIGINAL: [u8; 6] = [0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x1C];
/// Resume address after the dispatcher prologue.
const DISPATCHER_CONT: u32 = 0x0054_4A26;
/// Custom packet opcode slot.
const CUSTOM_PACKET_OPCODE: u8 = 0xF2;
/// F2 sub-opcode for long item status.
const LONG_STATUS_SUBOPCODE: u8 = 0x01;
/// Legacy status opcode kept for compatibility.
const LEGACY_LONG_STATUS_OPCODE: u8 = 0xF1;

/// Native long-status handler copy source (`FUN_00528A00`).
const STATUS_SRC: u32 = 0x0052_8A00;
/// Copied handler size ending before the downstream continuation.
const STATUS_COPY_LEN: usize = 0x118;
/// Native continuation after the copied handler block.
const STATUS_DOWNSTREAM: u32 = 0x0052_8B18;

/// Offset of the status parser format-string push opcode.
const FMT_PUSH_OFF: usize = 0x2E;
/// Offset of the parser format-string immediate.
const FMT_IMM_OFF: usize = 0x2F;
/// Offset of the stack status-buffer `lea`.
const BUF_LEA_OFF: usize = 0xB8;
/// Byte-length reads converted from `movzx byte` to `movzx word`.
const LEN_B6_OFFS: [usize; 4] = [0xAE, 0xC2, 0xDA, 0xF7];
/// Relative calls inside the copied status handler.
const CALL_OFFS: [usize; 8] = [0x3A, 0x5F, 0x72, 0x88, 0x9E, 0xD1, 0xEE, 0x113];

/// Heap status buffer for 2-byte length status text.
const STATUS_BUF_SIZE: usize = 0x1_0000;
/// `dsdh\0` format string: d, s, d, 2-byte length string.
const DSDH_FORMAT: [u8; 5] = [b'd', b's', b'd', b'h', 0];
/// Copied status handler cave capacity.
const STATUS_CAVE_SIZE: usize = 0x140;
/// Dispatcher router cave capacity.
const HOOK_CAVE_SIZE: usize = 0x80;

/// Guard against installing the custom packet mux twice.
static STATUS_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install opcode 242 custom packet entry. Long status requires server F2/01.
pub fn install_custom_opcode_242(h: HANDLE) -> Result<()> {
    if STATUS_INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let outcome = (|| -> Result<(u32, u32, u32, u32)> {
        let dsdh =
            memory::alloc_exec(h, DSDH_FORMAT.len()).context("[item_desc_length] alloc dsdh")?;
        memory::write_code(h, dsdh, &DSDH_FORMAT).context("[item_desc_length] write dsdh")?;
        let buf = memory::alloc_exec(h, STATUS_BUF_SIZE)
            .context("[item_desc_length] alloc 242 status buffer")?;
        let status_cave = memory::alloc_exec(h, STATUS_CAVE_SIZE)
            .context("[item_desc_length] alloc 242 status cave")?;
        let hook_cave = memory::alloc_exec(h, HOOK_CAVE_SIZE)
            .context("[item_desc_length] alloc dispatcher hook cave")?;
        Ok((dsdh, buf, status_cave, hook_cave))
    })();

    let (dsdh, buf, status_cave, hook_cave) = match outcome {
        Ok(v) => v,
        Err(e) => {
            STATUS_INSTALLED.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };

    let h_raw = h.0 as usize;
    thread::Builder::new()
        .name("item-long-status".to_string())
        .spawn(move || {
            let h = HANDLE(h_raw as *mut _);
            long_status_apply_loop(h, dsdh, buf, status_cave, hook_cave);
        })
        .context("[item_desc_length] spawn 242 worker")?;

    log_line!(
        "[item_desc_length] opcode 242 custom mux scheduled dsdh=0x{dsdh:08X} buf=0x{buf:08X} cave=0x{status_cave:08X} hook=0x{hook_cave:08X} server=F2/01"
    );
    Ok(())
}

/// Wait for the decrypted status handler and dispatcher, then patch both.
fn long_status_apply_loop(h: HANDLE, dsdh: u32, buf: u32, status_cave: u32, hook_cave: u32) {
    let start = Instant::now();
    let timeout = Duration::from_millis(WORKER_TIMEOUT_MS);

    loop {
        match try_patch_long_status(h, dsdh, buf, status_cave, hook_cave) {
            SitePatch::Patched => {
                log_line!(
                    "[item_desc_length] opcode 242 custom mux installed: F2/01 long-status handler with 2-byte length"
                );
                return;
            }
            SitePatch::AlreadyPatched => return,
            SitePatch::NotReady => {}
        }
        if start.elapsed() >= timeout {
            log_line!("[item_desc_length] 242 worker timeout waiting for decrypted status handler");
            return;
        }
        thread::sleep(Duration::from_millis(POLL_MS));
    }
}

/// Patch the copied long-status handler and the dispatcher router.
fn try_patch_long_status(
    h: HANDLE,
    dsdh: u32,
    buf: u32,
    status_cave: u32,
    hook_cave: u32,
) -> SitePatch {
    // Already hooked?
    let Ok(d0) = memory::read_bytes(h, DISPATCHER, 1) else {
        return SitePatch::NotReady;
    };
    if d0.first() == Some(&0xE9) {
        return SitePatch::AlreadyPatched;
    }
    // Read the decrypted source handler and verify the expected byte pattern.
    let Ok(raw) = memory::read_bytes(h, STATUS_SRC, STATUS_COPY_LEN) else {
        return SitePatch::NotReady;
    };
    if !long_status_src_decrypted(&raw) {
        return SitePatch::NotReady;
    }
    // Dispatcher must still expose the original prologue before patching.
    let Ok(disp) = memory::read_bytes(h, DISPATCHER, DISPATCHER_ORIGINAL.len()) else {
        return SitePatch::NotReady;
    };
    if disp != DISPATCHER_ORIGINAL {
        return SitePatch::NotReady;
    }

    // Write the transformed status handler cave.
    let sc = transform_long_status_copy(&raw, status_cave, dsdh, buf);
    if memory::write_code(h, status_cave, &sc).is_err() {
        return SitePatch::NotReady;
    }
    // Write the dispatcher router cave.
    let hc = build_dispatcher_hook_cave(hook_cave, status_cave);
    if memory::write_code(h, hook_cave, &hc).is_err() {
        return SitePatch::NotReady;
    }
    // Install the dispatcher jump only after both caves are ready.
    if memory::write_code(h, DISPATCHER, &build_dispatcher_jmp(hook_cave)).is_err() {
        return SitePatch::NotReady;
    }
    SitePatch::Patched
}

/// Verify the copied `FUN_00528A00` block is decrypted and still matches the known shape.
fn long_status_src_decrypted(raw: &[u8]) -> bool {
    if raw.len() < STATUS_COPY_LEN {
        return false;
    }
    if raw[0] != 0x55 || raw[FMT_PUSH_OFF] != 0x68 || raw[BUF_LEA_OFF] != 0x8D {
        return false;
    }
    if LEN_B6_OFFS.iter().any(|&o| raw[o] != 0xB6) {
        return false;
    }
    if CALL_OFFS.iter().any(|&o| raw[o] != 0xE8) {
        return false;
    }
    true
}

/// Transform the copied status handler into the F2/01 long-status handler.
fn transform_long_status_copy(raw: &[u8], cave: u32, dsdh: u32, buf: u32) -> Vec<u8> {
    let mut c = raw[..STATUS_COPY_LEN].to_vec();
    // Replace the format-string immediate (`dsdc` -> `dsdh`).
    c[FMT_IMM_OFF..FMT_IMM_OFF + 4].copy_from_slice(&dsdh.to_le_bytes());
    // Replace the stack-buffer `lea` with `mov ecx, buf` plus NOP.
    c[BUF_LEA_OFF] = 0xB9;
    c[BUF_LEA_OFF + 1..BUF_LEA_OFF + 5].copy_from_slice(&buf.to_le_bytes());
    c[BUF_LEA_OFF + 5] = 0x90;
    // Convert the four length reads from byte (`B6`) to word (`B7`).
    for &off in &LEN_B6_OFFS {
        c[off] = 0xB7;
    }
    // Relocate the eight `E8 rel32` calls in the copied handler.
    let delta = STATUS_SRC as i64 - cave as i64;
    for &off in &CALL_OFFS {
        let rel = i32::from_le_bytes([c[off + 1], c[off + 2], c[off + 3], c[off + 4]]) as i64;
        c[off + 1..off + 5].copy_from_slice(&((rel + delta) as i32).to_le_bytes());
    }
    // Jump back to the native downstream continuation.
    c.push(0xE9);
    let jrel = (STATUS_DOWNSTREAM as i64 - (cave as i64 + c.len() as i64 + 4)) as i32;
    c.extend_from_slice(&jrel.to_le_bytes());
    c
}

/// Dispatcher router:
/// - `0xF2/0x01`: call `status_cave(packet+1)`.
/// - `0xF2/unknown`: return without falling into the native bail path.
/// - `0xF1`: call `status_cave(packet)` for legacy compatibility.
/// - Other opcodes: replay the original prologue and jump back to the dispatcher.
fn build_dispatcher_hook_cave(cave: u32, status_cave: u32) -> Vec<u8> {
    let mut c = Vec::with_capacity(HOOK_CAVE_SIZE);
    // mov eax,[esp+4] (cdecl packet arg); mov cl,[eax] (opcode).
    c.extend_from_slice(&[0x8B, 0x44, 0x24, 0x04, 0x8A, 0x08]);

    // cmp cl,0xF2; je .custom
    c.extend_from_slice(&[0x80, 0xF9, CUSTOM_PACKET_OPCODE]);
    let custom_jmp = push_rel8(&mut c, 0x74);
    // cmp cl,0xF1; je .legacy
    c.extend_from_slice(&[0x80, 0xF9, LEGACY_LONG_STATUS_OPCODE]);
    let legacy_jmp = push_rel8(&mut c, 0x74);

    // --- .orig: replay prologue + jmp 0x544A26 ---
    c.extend_from_slice(&DISPATCHER_ORIGINAL);
    c.push(0xE9);
    let jrel = (DISPATCHER_CONT as i64 - (cave as i64 + c.len() as i64 + 4)) as i32;
    c.extend_from_slice(&jrel.to_le_bytes());

    // --- .custom: accept F2/01 and drop unknown F2 sub-opcodes ---
    let custom_at = c.len();
    patch_rel8(&mut c, custom_jmp, custom_at);
    // cmp byte ptr [eax+1],0x01; jne .drop
    c.extend_from_slice(&[0x80, 0x78, 0x01, LONG_STATUS_SUBOPCODE]);
    let drop_jmp = push_rel8(&mut c, 0x75);
    // inc eax; push eax(packet+1); call status_cave; add esp,4; ret
    c.push(0x40);
    c.push(0x50);
    push_call(&mut c, cave, status_cave);
    c.extend_from_slice(&[0x83, 0xC4, 0x04, 0xC3]);

    // --- .legacy: push packet; call status_cave; add esp,4; ret ---
    let legacy_at = c.len();
    patch_rel8(&mut c, legacy_jmp, legacy_at);
    c.push(0x50);
    push_call(&mut c, cave, status_cave);
    c.extend_from_slice(&[0x83, 0xC4, 0x04, 0xC3]);

    let drop_at = c.len();
    patch_rel8(&mut c, drop_jmp, drop_at);
    c.push(0xC3);
    debug_assert!(c.len() <= HOOK_CAVE_SIZE);
    c
}

/// Dispatcher patch: `E9 rel32` to `hook_cave` plus NOP padding.
fn build_dispatcher_jmp(hook_cave: u32) -> [u8; 6] {
    let mut p = [0x90u8; 6];
    p[0] = 0xE9;
    let rel = (hook_cave as i64 - (DISPATCHER as i64 + 5)) as i32;
    p[1..5].copy_from_slice(&rel.to_le_bytes());
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_size_is_64_records() {
        assert_eq!(STATE_SIZE, 0x40 * 0x1000);
    }

    #[test]
    fn tooltip_target_keeps_native_inline_clamp_safe() {
        assert_eq!(TARGET_TOOLTIP_LINES, 50);
        assert_eq!(MAX_INLINE_TOOLTIP, TOP_CELL_LINES);
        assert_eq!(TAIL_CELL_START, 30);
        assert!(
            MAX_INLINE_TOOLTIP < TARGET_TOOLTIP_LINES,
            "50-line support must stay outside the native out_breaks buffer"
        );
    }

    #[test]
    fn sidecar_record_can_hold_target_lines() {
        let table_words = (RECORD_STRIDE - 0x10) / 2;
        assert!(
            table_words > TARGET_TOOLTIP_LINES as usize,
            "sidecar line-offset table is too small: {table_words} words"
        );
    }

    #[test]
    fn jmp_patch_redirects_and_pads() {
        let cave = 0x2000_0000u32;
        let patch = build_jmp_patch(SPLITTER, cave);
        // Preserve the prologue length: E9 + rel32 + NOP padding.
        assert_eq!(patch.len(), SPLITTER_ORIGINAL.len());
        assert_eq!(patch[0], 0xE9);
        assert_eq!(patch[5], 0x90);
        let rel = i32::from_le_bytes([patch[1], patch[2], patch[3], patch[4]]);
        let resolved = (SPLITTER as i64 + 5 + rel as i64) as u32;
        assert_eq!(resolved, cave);
    }

    #[test]
    fn cave_fits_capacity() {
        let sc = build_split_sidecar_cave(0x3000_0000, 0x3001_0000);
        assert!(sc.len() <= CAVE_SIZE, "cave {} > {}", sc.len(), CAVE_SIZE);
    }

    #[test]
    fn cave_starts_with_frame_and_ends_with_ret() {
        let sc = build_split_sidecar_cave(0x3000_0000, 0x3001_0000);
        // push ebp; mov ebp,esp; sub esp,4
        assert_eq!(&sc[..6], &[0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x04]);
        // Expected epilogue: pop ebx/edi/esi; mov esp,ebp; pop ebp; ret.
        assert_eq!(
            &sc[sc.len() - 7..],
            &[0x5B, 0x5F, 0x5E, 0x8B, 0xE5, 0x5D, 0xC3]
        );
    }

    #[test]
    fn cave_strlen_and_helper_calls_resolve() {
        let cave = 0x3000_0000u32;
        let sc = build_split_sidecar_cave(cave, 0x3001_0000);
        // Resolve all `E8 rel32` calls and verify the expected native targets.
        let mut e8s = Vec::new();
        let mut i = 0;
        while i + 5 <= sc.len() {
            if sc[i] == 0xE8 {
                let rel = i32::from_le_bytes([sc[i + 1], sc[i + 2], sc[i + 3], sc[i + 4]]);
                let resolved = (cave as i64 + i as i64 + 5 + rel as i64) as u32;
                e8s.push(resolved);
                i += 5;
            } else {
                i += 1;
            }
        }
        assert!(e8s.contains(&STRLEN), "strlen call missing, got {e8s:08X?}");
        assert!(e8s.contains(&HELPER), "helper call missing, got {e8s:08X?}");
    }

    #[test]
    fn cave_has_indirect_strdup() {
        let sc = build_split_sidecar_cave(0x3000_0000, 0x3001_0000);
        // call dword ptr [STRDUP_SLOT] = FF 15 <slot>
        let mut needle = vec![0xFF, 0x15];
        needle.extend_from_slice(&STRDUP_SLOT.to_le_bytes());
        assert!(
            sc.windows(needle.len()).any(|w| w == needle),
            "indirect strdup call [0x{STRDUP_SLOT:08X}] not found"
        );
    }

    #[test]
    fn cave_clamps_per_caller() {
        let sc = build_split_sidecar_cave(0x3000_0000, 0x3001_0000);
        // Default cap: `mov edx, MAX_INLINE` (BA + imm32 LE).
        let mut def = vec![0xBA];
        def.extend_from_slice(&MAX_INLINE.to_le_bytes());
        assert!(
            sc.windows(def.len()).any(|w| w == def),
            "default cap mov edx,{MAX_INLINE} not found"
        );
        // Tooltip callers are detected by `cmp dword [ebp+4], ret`.
        for ret in TOOLTIP_RETS {
            let mut cmp = vec![0x81, 0x7D, 0x04];
            cmp.extend_from_slice(&ret.to_le_bytes());
            assert!(
                sc.windows(cmp.len()).any(|w| w == cmp),
                "caller {ret:#010x} compare not found"
            );
        }
        // tooltip cap:mov edx,MAX_INLINE_TOOLTIP
        let mut tip = vec![0xBA];
        tip.extend_from_slice(&MAX_INLINE_TOOLTIP.to_le_bytes());
        assert!(
            sc.windows(tip.len()).any(|w| w == tip),
            "tooltip cap mov edx,{MAX_INLINE_TOOLTIP} not found"
        );
        let mut target = vec![0xBA];
        target.extend_from_slice(&TARGET_TOOLTIP_LINES.to_le_bytes());
        assert!(
            !sc.windows(target.len()).any(|w| w == target),
            "native inline clamp must not use target {TARGET_TOOLTIP_LINES}"
        );
        // clamp:cmp ebx,edx(3B DA)
        assert!(
            sc.windows(2).any(|w| w == [0x3B, 0xDA]),
            "cmp ebx,edx clamp not found"
        );
    }

    #[test]
    fn render_hook_constants_match_static_evidence() {
        assert_eq!(FRAME_HEIGHT_HOOK, 0x004B_FE4B);
        assert_eq!(FRAME_HEIGHT_ORIGINAL, [0x8B, 0x50, 0x14, 0x6B, 0xD2, 0x0C]);
        assert_eq!(FRAME_HEIGHT_RETURN, 0x004B_FE51);
        assert_eq!(FRAME_SURFACE_HOOK, 0x004B_FF45);
        assert_eq!(
            FRAME_SURFACE_ORIGINAL,
            [0x8B, 0x45, 0xBC, 0x50, 0x8B, 0x4D, 0xB8]
        );
        assert_eq!(FRAME_SURFACE_RETURN, 0x004B_FF4C);
        assert_eq!(EXTRA_DRAW_HOOK, 0x004C_066B);
        assert_eq!(EXTRA_DRAW_ORIGINAL, [0xE9, 0xCA, 0x01, 0x00, 0x00]);
        assert_eq!(EXTRA_DRAW_RETURN, 0x004C_083A);
    }

    #[test]
    fn release_build_enables_extended_tooltip_render() {
        assert!(ENABLE_EXTENDED_TOOLTIP_RENDER);
    }

    #[test]
    fn frame_surface_guard_cave_clips_background_rect() {
        let cave_addr = 0x3600_0000u32;
        let state_addr = 0x3700_0000u32;
        let sc = build_frame_surface_guard_cave(cave_addr, state_addr);

        assert!(sc.len() <= FRAME_SURFACE_CAVE_SIZE);
        assert!(
            !sc.windows(3).any(|w| w == [0x81, 0x45, 0xB8]),
            "stacked tooltip UI must not widen the native frame into one large box"
        );
        assert!(sc.windows(4).any(|w| w == SCREEN_WIDTH.to_le_bytes()));
        assert!(sc.windows(4).any(|w| w == SCREEN_HEIGHT.to_le_bytes()));
        assert!(
            sc.windows(6)
                .any(|w| w == [0x8B, 0x48, 0x08, 0x83, 0xF9, TAIL_CELL_START as u8]),
            "surface guard must derive total height from the actual tooltip line count"
        );
        assert!(
            sc.windows(4)
                .any(|w| w == [0x83, 0xF9, TAIL_CELL_START as u8, 0x7E]),
            "surface guard must add third-cell height only when the tail cell is present"
        );
        assert!(
            sc.windows(3).any(|w| w == [0x6B, 0xC0, LINE_HEIGHT as u8]),
            "surface guard must convert actual line counts into pixel height"
        );
        assert!(
            sc.windows(3).any(|w| w == [0x83, 0xEA, LINE_HEIGHT as u8]),
            "surface guard must trim the native sidecar bottom padding from stacked cells"
        );
        assert!(
            sc.windows(6)
                .any(|w| w == [0x2B, 0xCA, 0x85, 0xC9, 0x7D, 0x02]),
            "surface guard must shift the whole tooltip up and clamp y to zero"
        );
        assert!(sc
            .windows(FRAME_SURFACE_ORIGINAL.len())
            .any(|w| w == FRAME_SURFACE_ORIGINAL));

        let jmp_at = sc
            .iter()
            .rposition(|byte| *byte == 0xE9)
            .expect("return jmp");
        let rel = i32::from_le_bytes(sc[jmp_at + 1..jmp_at + 5].try_into().unwrap());
        let target = (cave_addr as i64 + jmp_at as i64 + 5 + rel as i64) as u32;
        assert_eq!(target, FRAME_SURFACE_RETURN);
    }

    #[test]
    fn frame_height_cave_caps_cell_height_to_top_cell_rows() {
        let cave_addr: u32 = 0x3100_0000;
        let state_addr: u32 = 0x3200_0000;
        let sc = build_frame_height_cave(cave_addr, state_addr);

        assert!(sc.len() <= FRAME_CAVE_SIZE);
        assert!(sc.windows(4).any(|w| w == state_addr.to_le_bytes()));
        assert!(sc.windows(3).any(|w| w == [0x8D, 0x48, 0x18]));
        assert!(sc.windows(3).any(|w| w == [0x8B, 0x40, 0x08]));
        assert!(sc
            .windows(4)
            .any(|w| { w == (MAX_INLINE_TOOLTIP as u32).to_le_bytes() }));
        assert!(!sc
            .windows(4)
            .any(|w| { w == (TARGET_TOOLTIP_LINES as u32).to_le_bytes() }));

        let jmp_at = sc
            .iter()
            .rposition(|byte| *byte == 0xE9)
            .expect("return jmp");
        let rel = i32::from_le_bytes(sc[jmp_at + 1..jmp_at + 5].try_into().unwrap());
        let target = (cave_addr as i64 + jmp_at as i64 + 5 + rel as i64) as u32;
        assert_eq!(target, FRAME_HEIGHT_RETURN);
    }

    #[test]
    fn extra_draw_cave_draws_top_10_middle_20_tail_cell() {
        let cave_addr: u32 = 0x3300_0000;
        let state_addr: u32 = 0x3400_0000;
        let temp_addr: u32 = 0x3500_0000;
        let sc = build_extra_draw_cave(cave_addr, state_addr, temp_addr);
        const EXPECTED_TOP_LINES: u8 = 10;
        const EXPECTED_TAIL_START: u8 = 30;
        const EXPECTED_SIDE_HEIGHT_DELTA: u32 = 108;

        assert!(sc.len() <= EXTRA_CAVE_SIZE);
        assert!(sc.windows(4).any(|w| w == state_addr.to_le_bytes()));
        assert!(sc.windows(4).any(|w| w == temp_addr.to_le_bytes()));
        assert_eq!(MAX_INLINE_TOOLTIP, EXPECTED_TOP_LINES as u32);
        assert!(sc
            .windows(5)
            .any(|w| { w == [0xBE, EXPECTED_TOP_LINES, 0x00, 0x00, 0x00,] }));
        assert!(sc
            .windows(3)
            .any(|w| w == [0x83, 0xFE, TARGET_TOOLTIP_LINES as u8]));
        assert!(sc.windows(3).any(|w| w == [0x83, 0xE9, EXPECTED_TOP_LINES]));
        assert!(sc
            .windows(3)
            .any(|w| w == [0x83, 0xFE, EXPECTED_TAIL_START]));
        assert!(sc
            .windows(3)
            .any(|w| w == [0x83, 0xE9, EXPECTED_TAIL_START]));
        assert!(!sc.windows(5).any(|w| w == [0x05, 0x90, 0x00, 0x00, 0x00]));
        assert!(!sc.windows(5).any(|w| w == [0x05, 0x20, 0x01, 0x00, 0x00]));
        assert!(sc.windows(3).any(|w| w == [0x03, 0x4D, 0xBC]));
        assert!(
            sc.windows(3).any(|w| w == [0x83, 0xE9, 0x01]),
            "stacked tooltip cells must overlap by one pixel instead of leaving a gap"
        );
        assert!(
            sc.windows(4).any(|w| w == [0x8B, 0x4D, 0xF4, 0x83])
                && sc.windows(3).any(|w| w == [0xE9, FRAME_INSET, 0x03]),
            "sidecar frame y must subtract the native 5px text inset"
        );
        assert!(
            sc.windows(6)
                .any(|w| w == [0x8B, 0x45, 0xF0, 0x83, 0xE8, FRAME_INSET]),
            "sidecar frame x must subtract the native 5px text inset"
        );
        assert!(
            sc.windows(6)
                .any(|w| w == [0x8B, 0x47, 0x08, 0x83, 0xF8, TAIL_CELL_START as u8]),
            "sidecar frame height must shrink to the actual remaining line count"
        );
        assert!(
            sc.windows(6)
                .any(|w| w == [0x8B, 0x47, 0x08, 0x83, 0xF8, TARGET_TOOLTIP_LINES as u8]),
            "tail frame height must also shrink to the actual remaining line count"
        );
        assert!(
            sc.windows(3).any(|w| w == [0x6B, 0xC0, LINE_HEIGHT as u8]),
            "sidecar frame height must convert actual line counts into pixel height"
        );
        assert!(
            sc.windows(3).any(|w| w == [0x83, 0xEA, LINE_HEIGHT as u8]),
            "sidecar frame height must not reserve an extra blank line at the bottom"
        );
        let mut screen_height_cmp = vec![0x81, 0xF9];
        screen_height_cmp.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
        assert!(
            sc.windows(screen_height_cmp.len())
                .any(|w| w == screen_height_cmp),
            "extra cell background must skip cells starting below the screen"
        );
        let mut background_height_clamp = vec![0xB8];
        background_height_clamp.extend_from_slice(&SCREEN_HEIGHT.to_le_bytes());
        background_height_clamp.extend_from_slice(&[0x2B, 0xC1, 0x3B, 0xC2]);
        assert!(
            sc.windows(background_height_clamp.len())
                .any(|w| w == background_height_clamp),
            "extra cell background height must be clamped to the screen bottom"
        );
        let mut stacked_bottom = vec![
            0x8B, 0x55, 0xF4, // mov edx,[ebp-0x0c](base_y)
            0x03, 0x55, 0xBC, // add edx,[ebp-0x44](top height)
            0x83, 0xEA, 0x01, // sub edx,1(join overlap)
            0x03, 0x55, 0xBC, // add edx,[ebp-0x44]
            0x81, 0xC2, // add edx,side-height delta
        ];
        stacked_bottom.extend_from_slice(&EXPECTED_SIDE_HEIGHT_DELTA.to_le_bytes());
        assert!(
            sc.windows(stacked_bottom.len())
                .any(|w| w == stacked_bottom),
            "extra line bottom clip must use the same stacked cell geometry as the frames"
        );
        assert!(
            !sc.windows(4).any(|w| w == [0x83, 0x7B, 0x10, 0x00]),
            "sidecar text cells must not inherit the native icon/header padding"
        );
        assert!(
            !sc.windows(3).any(|w| w == [0x83, 0xC1, 0x01]),
            "sidecar text cells must not burn a whole blank line before drawing"
        );
        assert!(
            sc.windows(4)
                .any(|w| w == TOOLTIP_FRAME_SHADOW.to_le_bytes()),
            "sidecar frame must use the native shadow palette too"
        );
        assert!(sc.windows(4).any(|w| w == [0x0F, 0xBF, 0x44, 0x77]));
        assert!(sc.windows(2).any(|w| w == [0xF3, 0xA4]));
        let call_targets = sc
            .iter()
            .enumerate()
            .filter_map(|(idx, byte)| {
                if *byte != 0xE8 || idx + 5 > sc.len() {
                    return None;
                }
                let rel = i32::from_le_bytes(sc[idx + 1..idx + 5].try_into().unwrap());
                Some((cave_addr as i64 + idx as i64 + 5 + rel as i64) as u32)
            })
            .collect::<Vec<_>>();
        assert!(call_targets.contains(&COLOR_RENDERER));
        assert!(
            call_targets
                .iter()
                .filter(|target| **target == FRAME_BACKGROUND_DRAW)
                .count()
                >= 2,
            "second and third cells must each draw their own background"
        );
        assert!(
            call_targets
                .iter()
                .filter(|target| **target == FRAME_RECT_DRAW)
                .count()
                == 12,
            "sidecar cells must skip their own top border so shared seams are drawn once"
        );
        assert!(!sc.windows(4).any(|w| w == RAW_LINE_DRAW.to_le_bytes()));

        let jmp_at = sc
            .iter()
            .rposition(|byte| *byte == 0xE9)
            .expect("return jmp");
        let rel = i32::from_le_bytes(sc[jmp_at + 1..jmp_at + 5].try_into().unwrap());
        let target = (cave_addr as i64 + jmp_at as i64 + 5 + rel as i64) as u32;
        assert_eq!(target, EXTRA_DRAW_RETURN);
    }

    #[test]
    fn dsdh_format_is_2byte_len() {
        assert_eq!(DSDH_FORMAT.len(), 5);
        assert_eq!(&DSDH_FORMAT[..4], b"dsdh");
        assert_eq!(DSDH_FORMAT[4], 0);
    }

    #[test]
    fn transform_applies_three_changes_and_jmp() {
        // Fake raw 0x118-byte handler with the expected format, buffer, length, and call opcodes.
        let mut raw = vec![0u8; STATUS_COPY_LEN];
        raw[0] = 0x55;
        raw[FMT_PUSH_OFF] = 0x68; // push imm32
        raw[BUF_LEA_OFF] = 0x8D; // lea
        for &o in &LEN_B6_OFFS {
            raw[o] = 0xB6;
        }
        for &o in &CALL_OFFS {
            raw[o] = 0xE8; // rel32 = 0 before relocation.
        }
        let cave = 0x0600_0000u32;
        let dsdh = 0x0601_0000u32;
        let buf = 0x0602_0000u32;
        let c = transform_long_status_copy(&raw, cave, dsdh, buf);

        // Format immediate now points to `dsdh`.
        assert_eq!(
            u32::from_le_bytes([
                c[FMT_IMM_OFF],
                c[FMT_IMM_OFF + 1],
                c[FMT_IMM_OFF + 2],
                c[FMT_IMM_OFF + 3]
            ]),
            dsdh
        );
        // Buffer `lea` becomes `mov ecx, buf` + NOP.
        assert_eq!(c[BUF_LEA_OFF], 0xB9);
        assert_eq!(
            u32::from_le_bytes([
                c[BUF_LEA_OFF + 1],
                c[BUF_LEA_OFF + 2],
                c[BUF_LEA_OFF + 3],
                c[BUF_LEA_OFF + 4]
            ]),
            buf
        );
        assert_eq!(c[BUF_LEA_OFF + 5], 0x90);
        // Four byte-length reads become word-length reads.
        for &o in &LEN_B6_OFFS {
            assert_eq!(c[o], 0xB7, "len read @0x{o:X} not B7");
        }
        // Call rel32 values are relocated by `STATUS_SRC - cave`.
        let delta = STATUS_SRC as i64 - cave as i64;
        for &o in &CALL_OFFS {
            let rel = i32::from_le_bytes([c[o + 1], c[o + 2], c[o + 3], c[o + 4]]) as i64;
            assert_eq!(rel, delta, "call @0x{o:X} reloc wrong");
        }
        // Appended jump returns to the native downstream continuation.
        assert_eq!(c.len(), STATUS_COPY_LEN + 5);
        assert_eq!(c[STATUS_COPY_LEN], 0xE9);
        let jrel = i32::from_le_bytes([
            c[STATUS_COPY_LEN + 1],
            c[STATUS_COPY_LEN + 2],
            c[STATUS_COPY_LEN + 3],
            c[STATUS_COPY_LEN + 4],
        ]);
        let resolved = (cave as i64 + STATUS_COPY_LEN as i64 + 5 + jrel as i64) as u32;
        assert_eq!(resolved, STATUS_DOWNSTREAM);
    }

    fn rel8_target(rel_at: usize, rel: u8) -> usize {
        (rel_at as isize + 1 + rel as i8 as isize) as usize
    }

    fn rel32_target(cave: u32, code: &[u8], op_at: usize) -> u32 {
        let rel = i32::from_le_bytes(code[op_at + 1..op_at + 5].try_into().unwrap());
        (cave as i64 + op_at as i64 + 5 + rel as i64) as u32
    }

    fn find_bytes(code: &[u8], needle: &[u8]) -> usize {
        code.windows(needle.len())
            .position(|window| window == needle)
            .expect("needle not found")
    }

    #[test]
    fn dispatcher_hook_routes_custom_f2_subopcode_and_keeps_f1_legacy() {
        let hook = 0x0600_0000u32;
        let status_cave = 0x0605_0000u32;
        let c = build_dispatcher_hook_cave(hook, status_cave);
        assert!(c.len() <= HOOK_CAVE_SIZE);
        // mov eax,[esp+4]; mov cl,[eax]
        assert_eq!(&c[..4], &[0x8B, 0x44, 0x24, 0x04]);
        assert_eq!(&c[4..6], &[0x8A, 0x08]);

        // F2 is now a custom-packet mux: [F2][sub_opcode][payload...].
        let f2_cmp = find_bytes(&c, &[0x80, 0xF9, 0xF2, 0x74]);
        let custom_at = rel8_target(f2_cmp + 4, c[f2_cmp + 4]);
        assert_eq!(
            &c[custom_at..custom_at + 4],
            &[0x80, 0x78, 0x01, 0x01],
            "F2 must compare byte ptr [eax+1] with subopcode 0x01"
        );
        assert_eq!(c[custom_at + 4], 0x75, "unknown F2 subopcode must branch");
        let drop_at = rel8_target(custom_at + 5, c[custom_at + 5]);
        assert_eq!(c[drop_at], 0xC3, "unknown F2 subopcode must be dropped");

        // subopcode 0x01 reuses the copied S_ItemStatus handler with packet+1,
        // so the handler still sees one leading opcode byte before its payload.
        let f2_handler = custom_at + 6;
        assert_eq!(c[f2_handler], 0x40, "F2/01 must advance eax by one byte");
        assert_eq!(c[f2_handler + 1], 0x50);
        assert_eq!(c[f2_handler + 2], 0xE8);
        assert_eq!(
            rel32_target(hook, &c, f2_handler + 2),
            status_cave,
            "F2/01 status_cave rel wrong"
        );
        assert_eq!(
            &c[f2_handler + 7..f2_handler + 11],
            &[0x83, 0xC4, 0x04, 0xC3]
        );

        // F1 remains a direct legacy long-status opcode for old server builds.
        let f1_cmp = find_bytes(&c, &[0x80, 0xF9, 0xF1, 0x74]);
        let legacy_at = rel8_target(f1_cmp + 4, c[f1_cmp + 4]);
        assert_eq!(c[legacy_at], 0x50, "F1 legacy must pass original packet");
        assert_eq!(c[legacy_at + 1], 0xE8);
        assert_eq!(
            rel32_target(hook, &c, legacy_at + 1),
            status_cave,
            "F1 legacy status_cave rel wrong"
        );
        assert_eq!(&c[legacy_at + 6..legacy_at + 10], &[0x83, 0xC4, 0x04, 0xC3]);

        let orig_at = find_bytes(&c, &DISPATCHER_ORIGINAL);
        assert_eq!(c[orig_at + DISPATCHER_ORIGINAL.len()], 0xE9);
        assert_eq!(
            rel32_target(hook, &c, orig_at + DISPATCHER_ORIGINAL.len()),
            DISPATCHER_CONT
        );
    }

    #[test]
    fn dispatcher_jmp_redirects_to_hook() {
        let hook = 0x0600_0000u32;
        let p = build_dispatcher_jmp(hook);
        assert_eq!(p.len(), DISPATCHER_ORIGINAL.len());
        assert_eq!(p[0], 0xE9);
        assert_eq!(p[5], 0x90);
        let rel = i32::from_le_bytes([p[1], p[2], p[3], p[4]]);
        let resolved = (DISPATCHER as i64 + 5 + rel as i64) as u32;
        assert_eq!(resolved, hook);
    }
}
