//! AC/MR(物理防禦/魔法防禦)32-bit 擴展。
//!
//! 依 Phase 0 live RE 位址表安裝 AC/MR 32-bit 擴展。

use anyhow::{Context, Result};
use windows::Win32::Foundation::HANDLE;

use crate::log_line;
use crate::platform::memory::{alloc_exec, read_bytes, write_code};

const S_STATUS_FORMAT_AC_ADDR: u32 = 0x008D_4701; // 0x8D46F4 + original AC field[13]
const S_STATUS_AC_PTR_PUSH: u32 = 0x0052_33D7;

const S_CHARSYNACK_1_FORMAT_AC_ADDR: u32 = 0x008D_72CE; // 0x8D72CC + field[2]
const S_CHARSYNACK_1_OUTPUT_HOOK_ADDR: u32 = 0x0054_3995;
const S_CHARSYNACK_1_OUTPUT_HOOK_RETURN: u32 = 0x0054_39A5;
const S_CHARSYNACK_1_AC_ARG_PUSH: u32 = 0x0054_39B9;

const S_CHARSYNACK_2_FORMAT_AC_ADDR: u32 = 0x008D_72D8; // 0x8D72D4 + field[4]
const S_CHARSYNACK_2_AC_LOCAL_TRUNC: u32 = 0x0054_3A86;

const MR_UPDATE_FORMAT_ADDR: u32 = 0x008D_5065; // 0x8D5064 "ch", field[1]
const MR_UPDATE_PTR_PUSH: u32 = 0x0053_40E3;

const IN_GAME_AC_STATUS_TEXT_READ: u32 = 0x0078_1554;
const IN_GAME_AC_OVERLAY_TEXT_READ: u32 = 0x0079_97A8;
const IN_GAME_AC_STATUS_FORMAT_READ: u32 = 0x0056_65F3;

const CHARSELECT_F1_SETTER_ADDR: u32 = 0x0054_4910;
const CHARSELECT_F2_SETTER_ADDR: u32 = 0x0054_4940;
const CHARSELECT_AC_DISPLAY_HOOK_ADDR: u32 = 0x0076_B637;
const CHARSELECT_AC_DISPLAY_RETURN: u32 = 0x0076_B63C;
const CHARSELECT_AC_GETTER_ADDR: u32 = 0x0076_D810;
const CHARSELECT_AC_GETTER_LEN: usize = 18;

const MR_COMPUTE_READ_EDX: u32 = 0x0073_B273;
const MR_COMPUTE_READ_ECX: u32 = 0x0073_B29E;
const MR_COMPUTE_READ_EAX: u32 = 0x0073_B2BB;
const MR_DISPLAY_CAP_ADDR: u32 = 0x0078_141C;
const MR_SKILL_UI_CAP_ADDR: u32 = 0x0074_0631;

const MAX_HP_ADDR: u32 = 0x00C3_1E90;
const MAX_MP_ADDR: u32 = 0x00C3_1E8C;

const CAVE_TOTAL_SIZE: usize = 0x180;
const CAVE_IN_GAME_AC_OFFSET: u32 = 0x00;
const CAVE_CHARSELECT_AC_OFFSET: u32 = 0x04;
const CAVE_MR_BONUS_OFFSET: u32 = 0x08;
const CAVE_CHARSELECT_F2_OFFSET: u32 = 0x20;
const CAVE_CHARSELECT_AC_DISPLAY_OFFSET: u32 = 0xC0;
const CAVE_CHARSELECT_F1_OFFSET: u32 = 0x100;
const CAVE_CHARSYNACK1_OUTPUT_OFFSET: u32 = 0x140;

const ENABLE_PACKET_FORMAT_EXPANSION: bool = true;

#[derive(Clone, Copy)]
struct AcMrDataLayout {
    in_game_ac: u32,
    charselect_ac: u32,
    mr_bonus: u32,
}

/// 安裝 AC/MR 32-bit 擴展修補。
pub fn install_ac_mr_patches(h: HANDLE, _pid: u32) -> Result<()> {
    log_line!("\n--- AC/MR 32-bit 擴展 ---");
    if !ENABLE_PACKET_FORMAT_EXPANSION {
        return install_h_sized_safe_patches(h);
    }

    let cave = alloc_exec(h, CAVE_TOTAL_SIZE).context("分配 AC/MR codecave 失敗")?;
    let layout = AcMrDataLayout {
        in_game_ac: cave + CAVE_IN_GAME_AC_OFFSET,
        charselect_ac: cave + CAVE_CHARSELECT_AC_OFFSET,
        mr_bonus: cave + CAVE_MR_BONUS_OFFSET,
    };
    write_code(h, cave, &[0; 12]).context("初始化 AC/MR data cave 失敗")?;
    log_line!(
        "[AC/MR] codecave @ 0x{cave:08X}, data in_game_ac=0x{:08X} charselect_ac=0x{:08X} mr=0x{:08X}",
        layout.in_game_ac,
        layout.charselect_ac,
        layout.mr_bonus
    );

    let f2_code = cave + CAVE_CHARSELECT_F2_OFFSET;
    let f2 = build_charselect_f2_setter(layout);
    write_code(h, f2_code, &f2).context("寫入 AC/MR charselect F2 setter codecave 失敗")?;

    let f1_code = cave + CAVE_CHARSELECT_F1_OFFSET;
    let f1 = build_charselect_f1_setter(layout);
    write_code(h, f1_code, &f1).context("write AC/MR charselect F1 setter codecave failed")?;

    let ac_display_hook = cave + CAVE_CHARSELECT_AC_DISPLAY_OFFSET;
    let display_hook = build_charselect_ac_display_hook(layout, ac_display_hook);
    write_code(h, ac_display_hook, &display_hook)
        .context("寫入 AC/MR charselect display codecave 失敗")?;

    let charsynack1_output_hook = cave + CAVE_CHARSYNACK1_OUTPUT_OFFSET;
    let output_hook = build_charsynack1_output_hook(layout, charsynack1_output_hook);
    write_code(h, charsynack1_output_hook, &output_hook)
        .context("write AC/MR charselect F1 output codecave failed")?;

    let mut patched = 0usize;

    patched += patch_byte(
        h,
        S_STATUS_FORMAT_AC_ADDR,
        b'c',
        b'd',
        "S_STATUS AC format c->d",
    )?;
    patched += patch_push_ptr(
        h,
        S_STATUS_AC_PTR_PUSH,
        0x00C3_1E7B,
        layout.in_game_ac,
        "S_STATUS AC output ptr",
    )?;
    patched += patch_abs_read(
        h,
        IN_GAME_AC_STATUS_TEXT_READ,
        &[0x0F, 0xBE, 0x05, 0x7B, 0x1E, 0xC3, 0x00],
        &[0xA1],
        layout.in_game_ac,
        2,
        "status AC text read",
    )?;
    patched += patch_abs_read(
        h,
        IN_GAME_AC_OVERLAY_TEXT_READ,
        &[0x0F, 0xBE, 0x0D, 0x7B, 0x1E, 0xC3, 0x00],
        &[0x8B, 0x0D],
        layout.in_game_ac,
        1,
        "overlay AC text read",
    )?;
    patched += patch_abs_read(
        h,
        IN_GAME_AC_STATUS_FORMAT_READ,
        &[0x0F, 0xB6, 0x15, 0x7B, 0x1E, 0xC3, 0x00],
        &[0x8B, 0x15],
        layout.in_game_ac,
        1,
        "status format AC read",
    )?;

    patched += patch_byte(
        h,
        S_CHARSYNACK_1_FORMAT_AC_ADDR,
        b'c',
        b'd',
        "S_CHARSYNACK_1 AC format c->d",
    )?;
    patched += patch_jmp_span_exact(
        h,
        S_CHARSYNACK_1_OUTPUT_HOOK_ADDR,
        &[
            0x8D, 0x45, 0xFE, 0x50, 0x8D, 0x4D, 0xF3, 0x51, 0x8D, 0x55, 0xF4, 0x52, 0x8D, 0x45,
            0xF8, 0x50,
        ],
        charsynack1_output_hook,
        "S_CHARSYNACK_1 AC output ptr hook",
    )?;
    patched += patch_charsynack1_ac_arg_push(h, layout)?;
    patched += patch_jmp_allow_existing(
        h,
        CHARSELECT_F1_SETTER_ADDR,
        f1_code,
        "charselect F1 setter trampoline",
    )?;

    patched += patch_byte(
        h,
        S_CHARSYNACK_2_FORMAT_AC_ADDR,
        b'h',
        b'd',
        "S_CHARSYNACK_2 AC format h->d",
    )?;
    patched += patch_exact(
        h,
        S_CHARSYNACK_2_AC_LOCAL_TRUNC,
        &[0x0F, 0xB7, 0x45, 0xE4],
        &[0x8B, 0x45, 0xE4, 0x90],
        "S_CHARSYNACK_2 AC local movzx->mov",
    )?;
    patched += patch_jmp_allow_existing(
        h,
        CHARSELECT_F2_SETTER_ADDR,
        f2_code,
        "charselect F2 setter trampoline",
    )?;
    patched += patch_jmp_exact(
        h,
        CHARSELECT_AC_DISPLAY_HOOK_ADDR,
        &[0x0F, 0xBF, 0x48, 0x0E, 0x51],
        ac_display_hook,
        "charselect AC display hook",
    )?;
    patched += patch_charselect_ac_getter(h, layout)?;

    patched += patch_byte(
        h,
        MR_UPDATE_FORMAT_ADDR,
        b'h',
        b'd',
        "MR update format h->d",
    )?;
    patched += patch_push_ptr(
        h,
        MR_UPDATE_PTR_PUSH,
        0x00C3_1EAC,
        layout.mr_bonus,
        "MR update output ptr",
    )?;
    patched += patch_abs_read(
        h,
        MR_COMPUTE_READ_EDX,
        &[0x0F, 0xBF, 0x15, 0xAC, 0x1E, 0xC3, 0x00],
        &[0x8B, 0x15],
        layout.mr_bonus,
        1,
        "MR compute read edx",
    )?;
    patched += patch_abs_read(
        h,
        MR_COMPUTE_READ_ECX,
        &[0x0F, 0xBF, 0x0D, 0xAC, 0x1E, 0xC3, 0x00],
        &[0x8B, 0x0D],
        layout.mr_bonus,
        1,
        "MR compute read ecx",
    )?;
    patched += patch_abs_read(
        h,
        MR_COMPUTE_READ_EAX,
        &[0x0F, 0xBF, 0x05, 0xAC, 0x1E, 0xC3, 0x00],
        &[0xA1],
        layout.mr_bonus,
        2,
        "MR compute read eax",
    )?;
    patched += patch_exact(
        h,
        MR_DISPLAY_CAP_ADDR,
        &[
            0x81, 0x7D, 0xFC, 0xFA, 0x00, 0x00, 0x00, 0x7E, 0x07, 0xC7, 0x45, 0xFC, 0xFA, 0x00,
            0x00, 0x00,
        ],
        &[0x90; 16],
        "MR display cap remove",
    )?;
    patched += patch_exact(
        h,
        MR_SKILL_UI_CAP_ADDR,
        &[
            0x81, 0x7D, 0xF8, 0xFA, 0x00, 0x00, 0x00, 0x7E, 0x07, 0xC7, 0x45, 0xF8, 0xFA, 0x00,
            0x00, 0x00,
        ],
        &[0x90; 16],
        "MR skill UI cap remove",
    )?;

    log_line!("[AC/MR] 完成: {patched} 個 patch/site 已套用或已存在");
    Ok(())
}

fn install_h_sized_safe_patches(h: HANDLE) -> Result<()> {
    log_line!(
        "[AC/MR] h-sized safe mode: keep packet layouts unchanged; patch display-only limits"
    );
    let patched = patch_exact(
        h,
        MR_DISPLAY_CAP_ADDR,
        &[
            0x81, 0x7D, 0xFC, 0xFA, 0x00, 0x00, 0x00, 0x7E, 0x07, 0xC7, 0x45, 0xFC, 0xFA, 0x00,
            0x00, 0x00,
        ],
        &[0x90; 16],
        "MR display cap remove",
    )?;
    log_line!("[AC/MR] safe mode complete: {patched} display patch/site");
    Ok(())
}

fn build_jmp5(from: u32, to: u32) -> [u8; 5] {
    let rel = to.wrapping_sub(from + 5) as i32;
    let mut patch = [0u8; 5];
    patch[0] = 0xE9;
    patch[1..5].copy_from_slice(&rel.to_le_bytes());
    patch
}

fn append_jmp(code: &mut Vec<u8>, from_next_ip: u32, to: u32) {
    code.push(0xE9);
    let rel = to.wrapping_sub(from_next_ip + 5) as i32;
    code.extend_from_slice(&rel.to_le_bytes());
}

fn build_charselect_f1_setter(layout: AcMrDataLayout) -> Vec<u8> {
    let mut c = Vec::with_capacity(64);
    c.extend_from_slice(&[0x55, 0x8B, 0xEC, 0x51]); // push ebp; mov ebp,esp; push ecx
    c.extend_from_slice(&[0x89, 0x4D, 0xFC]); // mov [ebp-4],ecx
    c.extend_from_slice(&[0x8B, 0x45, 0xFC]); // mov eax,[ebp-4] (this)

    c.extend_from_slice(&[0x8B, 0x4D, 0x08]); // maxHP dword
    c.extend_from_slice(&[0x89, 0x0D]);
    c.extend_from_slice(&MAX_HP_ADDR.to_le_bytes());
    c.extend_from_slice(&[0x66, 0x89, 0x48, 0x0A]); // preserve legacy word

    c.extend_from_slice(&[0x8B, 0x4D, 0x0C]); // maxMP dword
    c.extend_from_slice(&[0x89, 0x0D]);
    c.extend_from_slice(&MAX_MP_ADDR.to_le_bytes());
    c.extend_from_slice(&[0x66, 0x89, 0x48, 0x0C]); // preserve legacy word

    c.extend_from_slice(&[0x8B, 0x4D, 0x10]); // AC dword
    c.extend_from_slice(&[0x89, 0x0D]);
    c.extend_from_slice(&layout.charselect_ac.to_le_bytes());
    c.extend_from_slice(&[0x66, 0x8B, 0x4D, 0x10]);
    c.extend_from_slice(&[0x66, 0x89, 0x48, 0x0E]); // preserve legacy word

    c.extend_from_slice(&[0x8B, 0xE5, 0x5D, 0xC2, 0x0C, 0x00]);
    c
}

fn build_charsynack1_output_hook(layout: AcMrDataLayout, cave_addr: u32) -> Vec<u8> {
    let mut c = Vec::with_capacity(32);
    c.extend_from_slice(&[0x8D, 0x45, 0xFE, 0x50]); // lea eax,[ebp-2]; push eax
    c.push(0x68); // push charselect_ac as field[2] output pointer
    c.extend_from_slice(&layout.charselect_ac.to_le_bytes());
    c.extend_from_slice(&[0x8D, 0x55, 0xF4, 0x52]); // lea edx,[ebp-0x0c]; push edx
    c.extend_from_slice(&[0x8D, 0x45, 0xF8, 0x50]); // lea eax,[ebp-0x08]; push eax
    let jmp_addr = cave_addr + c.len() as u32;
    append_jmp(&mut c, jmp_addr, S_CHARSYNACK_1_OUTPUT_HOOK_RETURN);
    c
}

fn build_charselect_f2_setter(layout: AcMrDataLayout) -> Vec<u8> {
    let mut c = Vec::with_capacity(128);
    c.extend_from_slice(&[0x55, 0x8B, 0xEC, 0x51]); // push ebp; mov ebp,esp; push ecx
    c.extend_from_slice(&[0x89, 0x4D, 0xFC]); // mov [ebp-4],ecx
    c.extend_from_slice(&[0x8B, 0x45, 0xFC]); // mov eax,[ebp-4] (this)

    c.extend_from_slice(&[0x8A, 0x4D, 0x08, 0x88, 0x48, 0x10]); // arg1 -> +0x10
    c.extend_from_slice(&[0x8A, 0x4D, 0x0C, 0x88, 0x48, 0x11]); // arg2 -> +0x11

    c.extend_from_slice(&[0x8B, 0x4D, 0x10]); // maxHP dword
    c.extend_from_slice(&[0x89, 0x0D]);
    c.extend_from_slice(&MAX_HP_ADDR.to_le_bytes());
    c.extend_from_slice(&[0x66, 0x89, 0x48, 0x0A]); // preserve legacy word

    c.extend_from_slice(&[0x8B, 0x4D, 0x14]); // maxMP dword
    c.extend_from_slice(&[0x89, 0x0D]);
    c.extend_from_slice(&MAX_MP_ADDR.to_le_bytes());
    c.extend_from_slice(&[0x66, 0x89, 0x48, 0x0C]); // preserve legacy word

    c.extend_from_slice(&[0x8B, 0x4D, 0x18]); // AC dword
    c.extend_from_slice(&[0x89, 0x0D]);
    c.extend_from_slice(&layout.charselect_ac.to_le_bytes());
    c.extend_from_slice(&[0x66, 0x8B, 0x4D, 0x18]);
    c.extend_from_slice(&[0x66, 0x89, 0x48, 0x0E]); // preserve legacy word

    for &(disp, off) in &[
        (0x1Cu8, 0x14u8),
        (0x20, 0x18),
        (0x24, 0x1C),
        (0x28, 0x20),
        (0x2C, 0x24),
        (0x30, 0x28),
    ] {
        c.extend_from_slice(&[0x0F, 0xBE, 0x4D, disp]);
        c.extend_from_slice(&[0x89, 0x48, off]);
    }

    c.extend_from_slice(&[0x8B, 0xE5, 0x5D, 0xC2, 0x2C, 0x00]);
    c
}

fn build_charselect_ac_display_hook(layout: AcMrDataLayout, cave_addr: u32) -> Vec<u8> {
    let mut c = Vec::with_capacity(16);
    c.extend_from_slice(&[0x8B, 0x0D]); // mov ecx,[charselect_ac]
    c.extend_from_slice(&layout.charselect_ac.to_le_bytes());
    c.push(0x51); // push ecx (original overwritten byte)
    let jmp_addr = cave_addr + c.len() as u32;
    append_jmp(&mut c, jmp_addr, CHARSELECT_AC_DISPLAY_RETURN);
    c
}

fn patch_byte(h: HANDLE, addr: u32, expected: u8, replacement: u8, name: &str) -> Result<usize> {
    let current = read_bytes(h, addr, 1)?;
    if current[0] == replacement {
        log_line!("[AC/MR] {name} 已修補,跳過");
        return Ok(1);
    }
    if current[0] != expected {
        log_line!(
            "[AC/MR] 警告: {name} @ 0x{addr:08X} 不符,expected {:02X}, got {:02X},skip",
            expected,
            current[0]
        );
        return Ok(0);
    }
    write_code(h, addr, &[replacement])?;
    log_line!("[AC/MR] {name} @ 0x{addr:08X}");
    Ok(1)
}

fn patch_exact(
    h: HANDLE,
    addr: u32,
    expected: &[u8],
    replacement: &[u8],
    name: &str,
) -> Result<usize> {
    let current = read_bytes(h, addr, expected.len())?;
    if current == replacement {
        log_line!("[AC/MR] {name} 已修補,跳過");
        return Ok(1);
    }
    if current != expected {
        log_line!(
            "[AC/MR] 警告: {name} @ 0x{addr:08X} bytes 不符: {:02X?},skip",
            current
        );
        return Ok(0);
    }
    write_code(h, addr, replacement)?;
    log_line!("[AC/MR] {name} @ 0x{addr:08X}");
    Ok(1)
}

fn patch_push_ptr(h: HANDLE, addr: u32, old_ptr: u32, new_ptr: u32, name: &str) -> Result<usize> {
    let mut expected = vec![0x68];
    expected.extend_from_slice(&old_ptr.to_le_bytes());
    let mut replacement = vec![0x68];
    replacement.extend_from_slice(&new_ptr.to_le_bytes());
    patch_exact(h, addr, &expected, &replacement, name)
}

fn patch_abs_read(
    h: HANDLE,
    addr: u32,
    expected: &[u8],
    opcode_prefix: &[u8],
    target_addr: u32,
    nop_count: usize,
    name: &str,
) -> Result<usize> {
    let mut replacement = Vec::with_capacity(expected.len());
    replacement.extend_from_slice(opcode_prefix);
    replacement.extend_from_slice(&target_addr.to_le_bytes());
    replacement.extend(std::iter::repeat(0x90).take(nop_count));
    patch_exact(h, addr, expected, &replacement, name)
}

fn patch_jmp_allow_existing(h: HANDLE, addr: u32, target: u32, name: &str) -> Result<usize> {
    let current = read_bytes(h, addr, 5)?;
    let patch = build_jmp5(addr, target);
    if current == patch {
        log_line!("[AC/MR] {name} 已指向目標,跳過");
        return Ok(1);
    }
    if current[0] != 0x55 && current[0] != 0xE9 {
        log_line!(
            "[AC/MR] 警告: {name} @ 0x{addr:08X} 首 byte 不符: {:02X},skip",
            current[0]
        );
        return Ok(0);
    }
    write_code(h, addr, &patch)?;
    log_line!("[AC/MR] {name} @ 0x{addr:08X} -> 0x{target:08X}");
    Ok(1)
}

fn patch_jmp_exact(
    h: HANDLE,
    addr: u32,
    expected: &[u8],
    target: u32,
    name: &str,
) -> Result<usize> {
    let current = read_bytes(h, addr, expected.len())?;
    let patch = build_jmp5(addr, target);
    if current.first() == Some(&0xE9) {
        log_line!("[AC/MR] {name} 已是 JMP,跳過");
        return Ok(1);
    }
    if current != expected {
        log_line!(
            "[AC/MR] 警告: {name} @ 0x{addr:08X} bytes 不符: {:02X?},skip",
            current
        );
        return Ok(0);
    }
    write_code(h, addr, &patch)?;
    log_line!("[AC/MR] {name} @ 0x{addr:08X} -> 0x{target:08X}");
    Ok(1)
}

fn patch_jmp_span_exact(
    h: HANDLE,
    addr: u32,
    expected: &[u8],
    target: u32,
    name: &str,
) -> Result<usize> {
    let current = read_bytes(h, addr, expected.len())?;
    if current.first() == Some(&0xE9) {
        log_line!("[AC/MR] {name} 撌脫 JMP,頝喲?");
        return Ok(1);
    }
    if current != expected {
        log_line!(
            "[AC/MR] 霅血?: {name} @ 0x{addr:08X} bytes 銝泵: {:02X?},skip",
            current
        );
        return Ok(0);
    }
    let mut patch = build_jmp5(addr, target).to_vec();
    while patch.len() < expected.len() {
        patch.push(0x90);
    }
    write_code(h, addr, &patch)?;
    log_line!("[AC/MR] {name} @ 0x{addr:08X} -> 0x{target:08X}");
    Ok(1)
}

fn patch_charsynack1_ac_arg_push(h: HANDLE, layout: AcMrDataLayout) -> Result<usize> {
    let expected = [
        0x66, 0x0F, 0xBE, 0x55, 0xF3, // movsx dx,byte [ebp-0x0d]
        0x0F, 0xB7, 0xC2, // movzx eax,dx
        0x50, // push eax
    ];
    let mut replacement = vec![0xA1]; // mov eax,[charselect_ac]
    replacement.extend_from_slice(&layout.charselect_ac.to_le_bytes());
    replacement.push(0x50); // push eax
    while replacement.len() < expected.len() {
        replacement.push(0x90);
    }
    patch_exact(
        h,
        S_CHARSYNACK_1_AC_ARG_PUSH,
        &expected,
        &replacement,
        "S_CHARSYNACK_1 AC arg push",
    )
}

fn patch_charselect_ac_getter(h: HANDLE, layout: AcMrDataLayout) -> Result<usize> {
    let expected = [
        0x55, 0x8B, 0xEC, 0x51, 0x89, 0x4D, 0xFC, 0x8B, 0x45, 0xFC, 0x66, 0x8B, 0x40, 0x0E, 0x8B,
        0xE5, 0x5D, 0xC3,
    ];
    let mut replacement = vec![0xA1];
    replacement.extend_from_slice(&layout.charselect_ac.to_le_bytes());
    replacement.push(0xC3);
    while replacement.len() < CHARSELECT_AC_GETTER_LEN {
        replacement.push(0x90);
    }
    patch_exact(
        h,
        CHARSELECT_AC_GETTER_ADDR,
        &expected,
        &replacement,
        "charselect AC getter",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s_status_ac_patch_targets_original_ac_field_not_lawful() {
        assert_eq!(S_STATUS_FORMAT_AC_ADDR, 0x008D_4701);
        assert_eq!(S_STATUS_AC_PTR_PUSH, 0x0052_33D7);
        assert_ne!(S_STATUS_AC_PTR_PUSH, 0x0052_33C3);
    }

    #[test]
    fn charselect_f2_setter_writes_expanded_ac_storage_and_preserves_legacy_slot() {
        let layout = AcMrDataLayout {
            in_game_ac: 0x1111_0000,
            charselect_ac: 0x2222_0000,
            mr_bonus: 0x3333_0000,
        };

        let code = build_charselect_f2_setter(layout);

        assert_eq!(&code[0..4], &[0x55, 0x8B, 0xEC, 0x51]);
        assert!(
            code.windows(9).any(|w| w
                == [
                    0x8B, 0x4D, 0x10, // mov ecx,[ebp+0x10]
                    0x89, 0x0D, 0x90, 0x1E, 0xC3, 0x00, // mov [max_hp],ecx
                ]),
            "setter must keep charselect maxHP dword storage"
        );
        assert!(
            code.windows(9).any(|w| w
                == [
                    0x8B, 0x4D, 0x14, // mov ecx,[ebp+0x14]
                    0x89, 0x0D, 0x8C, 0x1E, 0xC3, 0x00, // mov [max_mp],ecx
                ]),
            "setter must keep charselect maxMP dword storage"
        );
        assert!(
            code.windows(9).any(|w| w
                == [
                    0x8B, 0x4D, 0x18, // mov ecx,[ebp+0x18]
                    0x89, 0x0D, 0x00, 0x00, 0x22, 0x22, // mov [charselect_ac],ecx
                ]),
            "setter must store full dword AC into external storage"
        );
        assert!(
            code.windows(8).any(|w| w
                == [
                    0x66, 0x8B, 0x4D, 0x18, // mov cx,[ebp+0x18]
                    0x66, 0x89, 0x48, 0x0E, // mov [eax+0x0E],cx
                ]),
            "setter must preserve legacy word field for untouched code"
        );
        assert_eq!(&code[code.len() - 3..], &[0xC2, 0x2C, 0x00]);
    }

    #[test]
    fn charselect_f1_path_writes_expanded_ac_storage() {
        let layout = AcMrDataLayout {
            in_game_ac: 0x1111_0000,
            charselect_ac: 0x2222_0000,
            mr_bonus: 0x3333_0000,
        };

        let setter = build_charselect_f1_setter(layout);
        assert!(
            setter.windows(9).any(|w| w
                == [
                    0x8B, 0x4D, 0x10, // mov ecx,[ebp+0x10]
                    0x89, 0x0D, 0x00, 0x00, 0x22, 0x22, // mov [charselect_ac],ecx
                ]),
            "F1 setter must store full dword AC into external storage"
        );
        assert_eq!(&setter[setter.len() - 3..], &[0xC2, 0x0C, 0x00]);

        let hook = build_charsynack1_output_hook(layout, 0x5000_0000);
        assert!(
            hook.windows(5).any(|w| w == [0x68, 0x00, 0x00, 0x22, 0x22]),
            "S_CHARSYNACK_1 output hook must push external AC storage"
        );
    }

    #[test]
    fn jmp5_targets_requested_codecave() {
        let patch = build_jmp5(0x0054_4940, 0x05A2_0040);

        assert_eq!(patch[0], 0xE9);
        let rel = i32::from_le_bytes([patch[1], patch[2], patch[3], patch[4]]);
        assert_eq!(0x0054_4945u32.wrapping_add_signed(rel), 0x05A2_0040);
    }
}
