//! Runtime patch for server-supplied NPC HTML bodies.
//!
//! The stock client path reads `htmlName` from the S_HYPERTEXT payload and
//! eventually calls the local-file loader at 0x4943B0.  This hook keeps that
//! path unchanged unless `htmlName` begins with `@`.  For `@...` names, the
//! second string field in the packet is captured into a private codecave
//! buffer and the HTML loader wrapper calls the existing in-memory parser
//! at 0x4944E0 instead of looking up `lineage/html/*.html`.

use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use once_cell::sync::Lazy;
use windows::Win32::Foundation::HANDLE;

use crate::logger::log_line;
use crate::platform::{memory, process};

pub const HYPERTEXT_PARSE_HOOK_ADDR: u32 = 0x0052_7A58;
pub const HYPERTEXT_PARSE_ORIGINAL_BYTES: [u8; 10] =
    [0x8D, 0x4D, 0xEC, 0x51, 0x8B, 0x15, 0xB8, 0x8E, 0x9A, 0x00];
pub const HYPERTEXT_PARSE_RESUME_ADDR: u32 = 0x0052_7A62;
pub const HYPERTEXT_PARSE_AFTER_ADDR: u32 = 0x0052_7A92;

pub const PACKET_DESERIALIZE_ADDR: u32 = 0x0052_2110;
pub const HYPERTEXT_DSSH_FORMAT_ADDR: u32 = 0x008D_4A70;
pub const HTML_LOCAL_FILE_LOAD_ADDR: u32 = 0x0049_43B0;
pub const HTML_PARSE_FROM_STRING_ADDR: u32 = 0x0049_44E0;

pub const HTML_LOCAL_FILE_CALLS: [u32; 6] = [
    0x0049_E1F7,
    0x0049_E20D,
    0x0049_E418,
    0x005E_F97B,
    0x005E_FB78,
    0x0064_3A5F,
];

pub const CODECAVE_SIZE: usize = 0x9000;
pub const OFF_WRAPPER: u32 = 0x0000;
pub const OFF_PARSE_HOOK: u32 = 0x0100;
pub const OFF_BODY_PTR: u32 = 0x0200;
pub const OFF_BODY_BUF: u32 = 0x0300;
pub const BODY_BUF_SIZE: u32 = 0x7000;

#[derive(Debug, Clone)]
pub struct DynamicDialogHookHandle {
    pub patched_call_sites: Vec<u32>,
}

static HOOK_STATE: Lazy<Mutex<Option<DynamicDialogHookHandle>>> = Lazy::new(|| Mutex::new(None));

pub fn install(h: HANDLE, pid: u32) -> Result<DynamicDialogHookHandle> {
    if let Some(handle) = HOOK_STATE.lock().ok().and_then(|s| s.clone()) {
        log_line!("[dynamic-dialog] already installed");
        return Ok(handle);
    }

    let parse_live = memory::read_bytes(
        h,
        HYPERTEXT_PARSE_HOOK_ADDR,
        HYPERTEXT_PARSE_ORIGINAL_BYTES.len(),
    )
    .context("read dynamic dialog parse hook site")?;
    if parse_live != HYPERTEXT_PARSE_ORIGINAL_BYTES {
        bail!(
            "[dynamic-dialog] parse hook site 0x{HYPERTEXT_PARSE_HOOK_ADDR:08X} bytes mismatch: expected {:02X?}, got {:02X?}",
            HYPERTEXT_PARSE_ORIGINAL_BYTES,
            parse_live
        );
    }

    let cave = memory::alloc_exec(h, CODECAVE_SIZE).context("alloc dynamic dialog cave")?;
    memory::write_code(h, cave, &vec![0u8; CODECAVE_SIZE]).context("zero dynamic dialog cave")?;

    let wrapper_addr = cave + OFF_WRAPPER;
    let parse_hook_addr = cave + OFF_PARSE_HOOK;
    let body_ptr_addr = cave + OFF_BODY_PTR;
    let body_buf_addr = cave + OFF_BODY_BUF;

    let wrapper = build_loader_wrapper_shellcode(body_ptr_addr, wrapper_addr);
    let parse_hook = build_parse_hook_shellcode(body_ptr_addr, body_buf_addr, parse_hook_addr);
    if wrapper.len() > OFF_PARSE_HOOK as usize {
        bail!("dynamic dialog wrapper too large: {} bytes", wrapper.len());
    }
    if parse_hook.len() > (OFF_BODY_PTR - OFF_PARSE_HOOK) as usize {
        bail!(
            "dynamic dialog parse hook too large: {} bytes",
            parse_hook.len()
        );
    }
    if OFF_BODY_BUF + BODY_BUF_SIZE > CODECAVE_SIZE as u32 {
        bail!("dynamic dialog body buffer exceeds codecave");
    }

    memory::write_code(h, wrapper_addr, &wrapper).context("write dynamic dialog wrapper")?;
    memory::write_code(h, parse_hook_addr, &parse_hook).context("write dynamic dialog parser")?;

    let mut patched_call_sites = Vec::new();
    let threads = process::suspend_threads(pid)?;
    let res = (|| -> Result<()> {
        patch_parse_hook(h, parse_hook_addr)?;
        for call_site in HTML_LOCAL_FILE_CALLS {
            match patch_call_to_wrapper(h, call_site, wrapper_addr) {
                Ok(()) => patched_call_sites.push(call_site),
                Err(e) => log_line!("[dynamic-dialog] skip loader call 0x{call_site:08X}: {e:#}"),
            }
        }
        if patched_call_sites.is_empty() {
            bail!("no HTML loader call sites were patched");
        }
        Ok(())
    })();
    process::resume_threads(threads);
    res?;

    let handle = DynamicDialogHookHandle { patched_call_sites };
    if let Ok(mut state) = HOOK_STATE.lock() {
        *state = Some(handle.clone());
    }
    log_line!(
        "[dynamic-dialog] installed parse hook @ 0x{HYPERTEXT_PARSE_HOOK_ADDR:08X}, wrapper=0x{wrapper_addr:08X}, body_buf=0x{body_buf_addr:08X} ({} loader calls)",
        handle.patched_call_sites.len()
    );
    Ok(handle)
}

fn patch_parse_hook(h: HANDLE, target: u32) -> Result<()> {
    let mut hook = [0x90u8; HYPERTEXT_PARSE_ORIGINAL_BYTES.len()];
    hook[0] = 0xE9;
    let rel = target.wrapping_sub(HYPERTEXT_PARSE_HOOK_ADDR + 5) as i32;
    hook[1..5].copy_from_slice(&rel.to_le_bytes());
    memory::write_code(h, HYPERTEXT_PARSE_HOOK_ADDR, &hook)
        .context("write dynamic dialog parse hook")
}

fn patch_call_to_wrapper(h: HANDLE, call_site: u32, wrapper_addr: u32) -> Result<()> {
    let live = memory::read_bytes(h, call_site, 5).context("read HTML loader call")?;
    if live.first().copied() != Some(0xE8) {
        bail!("not a direct call: {:02X?}", live);
    }
    let old_rel = i32::from_le_bytes([live[1], live[2], live[3], live[4]]);
    let old_target = call_site.wrapping_add(5).wrapping_add(old_rel as u32);
    if old_target != HTML_LOCAL_FILE_LOAD_ADDR {
        bail!("call target is 0x{old_target:08X}, expected 0x{HTML_LOCAL_FILE_LOAD_ADDR:08X}");
    }

    let mut patched = [0u8; 5];
    patched[0] = 0xE8;
    let rel = wrapper_addr.wrapping_sub(call_site + 5) as i32;
    patched[1..5].copy_from_slice(&rel.to_le_bytes());
    memory::write_code(h, call_site, &patched).context("patch HTML loader call")
}

pub fn build_loader_wrapper_shellcode(body_ptr_addr: u32, wrapper_addr: u32) -> Vec<u8> {
    let mut sc = Vec::with_capacity(64);

    // mov eax, [esp+4]        ; first stdcall arg = htmlName
    sc.extend_from_slice(&[0x8B, 0x44, 0x24, 0x04]);
    // test eax, eax
    sc.extend_from_slice(&[0x85, 0xC0]);
    // je .local
    sc.extend_from_slice(&[0x74, 0x17]);
    // cmp byte ptr [eax], '@'
    sc.extend_from_slice(&[0x80, 0x38, b'@']);
    // jne .local
    sc.extend_from_slice(&[0x75, 0x12]);
    // mov eax, [body_ptr_addr]
    sc.push(0xA1);
    sc.extend_from_slice(&body_ptr_addr.to_le_bytes());
    // test eax, eax
    sc.extend_from_slice(&[0x85, 0xC0]);
    // je .local
    sc.extend_from_slice(&[0x74, 0x09]);
    // mov [esp+4], eax        ; replace htmlName with html body
    sc.extend_from_slice(&[0x89, 0x44, 0x24, 0x04]);
    // jmp HTML_PARSE_FROM_STRING_ADDR
    sc.push(0xE9);
    let inline_jmp_disp_off = sc.len();
    sc.extend_from_slice(&[0, 0, 0, 0]);
    // .local: jmp HTML_LOCAL_FILE_LOAD_ADDR
    let local_off = sc.len();
    sc.push(0xE9);
    let local_jmp_disp_off = sc.len();
    sc.extend_from_slice(&[0, 0, 0, 0]);

    let inline_next = wrapper_addr + inline_jmp_disp_off as u32 + 4;
    let local_next = wrapper_addr + local_jmp_disp_off as u32 + 4;
    let inline_rel = HTML_PARSE_FROM_STRING_ADDR.wrapping_sub(inline_next) as i32;
    let local_rel = HTML_LOCAL_FILE_LOAD_ADDR.wrapping_sub(local_next) as i32;
    sc[inline_jmp_disp_off..inline_jmp_disp_off + 4].copy_from_slice(&inline_rel.to_le_bytes());
    sc[local_jmp_disp_off..local_jmp_disp_off + 4].copy_from_slice(&local_rel.to_le_bytes());

    debug_assert_eq!(local_off, 0x1F);
    sc
}

pub fn build_parse_hook_shellcode(
    body_ptr_addr: u32,
    body_buf_addr: u32,
    parse_hook_addr: u32,
) -> Vec<u8> {
    let mut sc = Vec::with_capacity(128);

    // mov esi, [ebp+8]        ; packet pointer after the stock +1 skip
    sc.extend_from_slice(&[0x8B, 0x75, 0x08]);
    // cmp byte ptr [esi+4], '@' ; htmlName starts after objId
    sc.extend_from_slice(&[0x80, 0x7E, 0x04, b'@']);
    // je .dynamic
    sc.extend_from_slice(&[0x74, 0x0F]);

    // Legacy replay of overwritten bytes:
    // lea ecx, [ebp-0x14]; push ecx; mov edx, [0x009A8EB8]
    sc.extend_from_slice(&HYPERTEXT_PARSE_ORIGINAL_BYTES);
    // jmp 0x00527A62
    sc.push(0xE9);
    let legacy_jmp_disp_off = sc.len();
    sc.extend_from_slice(&[0, 0, 0, 0]);

    // .dynamic:
    // mov dword ptr [body_ptr_addr], body_buf_addr
    sc.extend_from_slice(&[0xC7, 0x05]);
    sc.extend_from_slice(&body_ptr_addr.to_le_bytes());
    sc.extend_from_slice(&body_buf_addr.to_le_bytes());
    // mov byte ptr [body_buf_addr], 0
    sc.extend_from_slice(&[0xC6, 0x05]);
    sc.extend_from_slice(&body_buf_addr.to_le_bytes());
    sc.push(0);

    // push &argc (h)
    sc.extend_from_slice(&[0x8D, 0x4D, 0xEC]);
    sc.push(0x51);
    // push body buffer (second s destination)
    sc.push(0x68);
    sc.extend_from_slice(&body_buf_addr.to_le_bytes());
    // push body buffer max bytes
    sc.push(0x68);
    sc.extend_from_slice(&BODY_BUF_SIZE.to_le_bytes());
    // push &htmlName (first s destination)
    sc.extend_from_slice(&[0x8D, 0x85, 0xDC, 0xFE, 0xFF, 0xFF]);
    sc.push(0x50);
    // push 0x100
    sc.push(0x68);
    sc.extend_from_slice(&0x100u32.to_le_bytes());
    // push &objId (d)
    sc.extend_from_slice(&[0x8D, 0x4D, 0xE4]);
    sc.push(0x51);
    // push "dssh"
    sc.push(0x68);
    sc.extend_from_slice(&HYPERTEXT_DSSH_FORMAT_ADDR.to_le_bytes());
    // push packet pointer
    sc.extend_from_slice(&[0x8B, 0x55, 0x08]);
    sc.push(0x52);
    // call packet deserializer
    sc.push(0xE8);
    let call_disp_off = sc.len();
    sc.extend_from_slice(&[0, 0, 0, 0]);
    // add esp, 0x20
    sc.extend_from_slice(&[0x83, 0xC4, 0x20]);
    // mov [ebp+8], eax
    sc.extend_from_slice(&[0x89, 0x45, 0x08]);
    // jmp post-parse path
    sc.push(0xE9);
    let after_jmp_disp_off = sc.len();
    sc.extend_from_slice(&[0, 0, 0, 0]);

    patch_relative_jmp(
        &mut sc,
        parse_hook_addr,
        legacy_jmp_disp_off - 1,
        HYPERTEXT_PARSE_RESUME_ADDR,
    );
    patch_relative_call(
        &mut sc,
        parse_hook_addr,
        call_disp_off - 1,
        PACKET_DESERIALIZE_ADDR,
    );
    patch_relative_jmp(
        &mut sc,
        parse_hook_addr,
        after_jmp_disp_off - 1,
        HYPERTEXT_PARSE_AFTER_ADDR,
    );
    sc
}

fn patch_relative_call(sc: &mut [u8], base: u32, instr_off: usize, target: u32) {
    debug_assert_eq!(sc[instr_off], 0xE8);
    patch_rel32(sc, base, instr_off, target);
}

fn patch_relative_jmp(sc: &mut [u8], base: u32, instr_off: usize, target: u32) {
    debug_assert_eq!(sc[instr_off], 0xE9);
    patch_rel32(sc, base, instr_off, target);
}

fn patch_rel32(sc: &mut [u8], base: u32, instr_off: usize, target: u32) {
    let next_ip = base + instr_off as u32 + 5;
    let rel = target.wrapping_sub(next_ip) as i32;
    sc[instr_off + 1..instr_off + 5].copy_from_slice(&rel.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_switches_at_magic_prefix() {
        let sc = build_loader_wrapper_shellcode(0x1234_5678, 0x1000_0000);
        assert!(sc
            .windows([0x80, 0x38, b'@'].len())
            .any(|w| w == [0x80, 0x38, b'@']));
        assert!(sc
            .windows([0x89, 0x44, 0x24, 0x04].len())
            .any(|w| w == [0x89, 0x44, 0x24, 0x04]));
    }

    #[test]
    fn parse_hook_only_branches_for_dynamic_names() {
        let sc = build_parse_hook_shellcode(0x1111_0000, 0x1111_0300, 0x1000_0100);
        assert!(sc.starts_with(&[0x8B, 0x75, 0x08, 0x80, 0x7E, 0x04, b'@']));
        assert!(sc
            .windows(HYPERTEXT_PARSE_ORIGINAL_BYTES.len())
            .any(|w| w == HYPERTEXT_PARSE_ORIGINAL_BYTES));
        assert!(sc
            .windows(BODY_BUF_SIZE.to_le_bytes().len())
            .any(|w| w == BODY_BUF_SIZE.to_le_bytes()));
    }

    #[test]
    fn call_sites_are_direct_loader_calls() {
        assert_eq!(HTML_LOCAL_FILE_CALLS.len(), 6);
        assert_eq!(HTML_LOCAL_FILE_LOAD_ADDR, 0x0049_43B0);
        assert_eq!(HTML_PARSE_FROM_STRING_ADDR, 0x0049_44E0);
    }
}
