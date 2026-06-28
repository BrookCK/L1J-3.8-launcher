//! 物品說明色碼 — 讓物品說明支援 `\f1`~`\f9` 與 `\F1`~`\F9`(大小寫同義,9 色)。
//!
//! ## 根因
//!
//! 物品說明的每一行,native 是經「raw line draw」`0x0046FEC0` 直接畫字 —— 完全不 parse
//! 跳脫碼,所以 `\f` / `\F` 會「字面顯示」且白色,不隱藏不上色(使用者實測:大小寫都不認)。
//!
//! ## 解法(移植 3.63,單一 patch)
//!
//! 把說明每行「改路由」去 native 的色碼 render `0x0046E0F0`(= 3.63 `0x0046B980`),它會
//! parse `\f`+digit、**strip 掉碼 + 上色**(char-indexed palette `0x0095FA78`,`\f0`~`\f9`
//! → `0x0095FB38`~`0x0095FB5C`,9 個真實色)。
//!
//! 做法:`scan_callers(0x0046FEC0)` 找到的 37 個 `CALL 0x0046FEC0` callsite,把每個的呼叫
//! 目標改成一個 codecave —— cave 先把該行 copy 到暫存(clamp 255),做 `\F`→`\f` 正規化
//! (只改暫存,零全域副作用),再以 `flag=0` 呼叫色碼 render。
//!
//! ## 關鍵:flag=0 繞過 render 的 flag-skip bug
//!
//! `0x0046E0F0` 小寫分支有個 bug:當 `[ebp+0x1C]`(arg6 flag)非 0 時,digit 0-9 會被
//! `jle` 跳過設色(只 strip)。cave 呼叫時 `push 0` 當 arg6 → flag=0 → 一律設色,bug 被繞過。
//!
//! `\F` 在暫存被正規化成 `\f` → 也走小寫 char-indexed 10 色路徑 → 大小寫同義,且不必動 render。
//!
//! launch-and-leave:install 配置 cave 後開一次性背景 worker 改寫 callsite(decrypt-on-execute
//! 需輪詢),完成或逾時即結束;launcher 進程結束時 worker 隨之消滅,無需 uninstall。

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HANDLE;

use crate::logger::log_line;
use crate::platform::memory;

/// 3.8 raw line draw(= 3.63 `0x0046D750`)。37 個 callsite 原本呼叫它。
const RAW_LINE_DRAW: u32 = 0x0046_FEC0;
/// 3.8 色碼 render(= 3.63 `0x0046B980`)。會 parse `\f`+digit → strip 碼 + 上色。
const COLOR_RENDERER: u32 = 0x0046_E0F0;

/// E8 rel32 call 長度。
const CALL_LEN: usize = 5;

/// `scan_callers(0x0046FEC0)` 在主模組掃到的 37 個 `CALL 0x0046FEC0` 說明畫行 callsite。
const CALL_SITES: [u32; 37] = [
    0x0045_B52E,
    0x0045_E04B,
    0x0045_E1D9,
    0x0045_E3C4,
    0x0046_187E,
    0x0046_19E3,
    0x0046_1BFF,
    0x004A_9A21,
    0x004A_E25F,
    0x004C_0212,
    0x004C_03F1,
    0x004C_0616,
    0x004C_0663,
    0x004C_0F1D,
    0x004C_1015,
    0x004C_114C,
    0x004C_B302,
    0x0051_27CC,
    0x0057_FD1D,
    0x0059_2833,
    0x0059_5123,
    0x0059_63FA,
    0x0059_6524,
    0x0059_6693,
    0x005B_4F9F,
    0x005B_4FD1,
    0x005B_6928,
    0x005B_6A2A,
    0x005B_6B6B,
    0x005C_35F4,
    0x005C_45BA,
    0x005C_46D9,
    0x005C_4840,
    0x0073_B5D4,
    0x0079_7481,
    0x0079_75E2,
    0x0079_7777,
];

/// codecave 容量(cave shellcode 約 80 bytes,留裕度)。
const CAVE_SIZE: usize = 0x100;
/// 每行說明暫存 buffer(clamp 255,留裕度)。
const TEMP_BUFFER_SIZE: usize = 0x200;
/// 每行最大複製長度(cave 內 clamp)。
const LINE_CLAMP: u32 = 0xFF;

/// worker 輪詢上限:某些 callsite 是 decrypt-on-execute,要等遊戲第一次畫該面板說明才解密。
const WORKER_TIMEOUT_MS: u64 = 600_000;
/// 輪詢間隔。
const POLL_MS: u64 = 200;

/// 防重複安裝(stage2 只會叫一次,但保險)。
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// 安裝:配置 temp buffer + cave,寫入 cave shellcode,再開一次性背景 worker 把已解密的
/// callsite 改寫成「呼叫 cave」。worker 因 decrypt-on-execute 需長時間輪詢,完成或逾時即結束。
pub fn install(h: HANDLE) -> Result<()> {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let outcome = (|| -> Result<u32> {
        let temp_buffer = memory::alloc_exec(h, TEMP_BUFFER_SIZE)
            .context("[item_desc_color] alloc temp line buffer")?;
        let cave =
            memory::alloc_exec(h, CAVE_SIZE).context("[item_desc_color] alloc line color cave")?;
        let shellcode = build_line_color_cave(cave, temp_buffer);
        if shellcode.len() > CAVE_SIZE {
            anyhow::bail!(
                "[item_desc_color] cave shellcode too large: {} > {}",
                shellcode.len(),
                CAVE_SIZE
            );
        }
        memory::write_code(h, cave, &shellcode)
            .context("[item_desc_color] write line color cave")?;
        Ok(cave)
    })();

    let cave = match outcome {
        Ok(cave) => cave,
        Err(e) => {
            // 配置失敗回復 guard,讓上層可重試。
            INSTALLED.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };

    let h_raw = h.0 as usize;
    thread::Builder::new()
        .name("item-desc-color".to_string())
        .spawn(move || {
            let h = HANDLE(h_raw as *mut _);
            apply_loop(h, cave);
        })
        .context("[item_desc_color] spawn apply worker")?;

    log_line!(
        "[item_desc_color] installed: cave=0x{cave:08X} renderer=0x{COLOR_RENDERER:08X}, polling {} callsites",
        CALL_SITES.len()
    );
    Ok(())
}

/// 背景輪詢:每 tick 嘗試改寫尚未改的 callsite。某些點要等對應面板第一次顯示說明才解密。
/// 全部改寫完成、或逾時即結束。
fn apply_loop(h: HANDLE, cave: u32) {
    let start = Instant::now();
    let timeout = Duration::from_millis(WORKER_TIMEOUT_MS);
    let mut done = vec![false; CALL_SITES.len()];
    let mut last_log = 0usize;

    loop {
        for (i, &site) in CALL_SITES.iter().enumerate() {
            if done[i] {
                continue;
            }
            match try_patch_site(h, site, cave) {
                SitePatch::Patched | SitePatch::AlreadyPatched => done[i] = true,
                SitePatch::NotReady => {}
            }
        }

        let count = done.iter().filter(|d| **d).count();
        if count != last_log {
            log_line!(
                "[item_desc_color] callsites routed {}/{}",
                count,
                CALL_SITES.len()
            );
            last_log = count;
        }
        if count == CALL_SITES.len() {
            log_line!(
                "[item_desc_color] all {} callsites routed",
                CALL_SITES.len()
            );
            return;
        }
        if start.elapsed() >= timeout {
            log_line!(
                "[item_desc_color] worker timeout, routed {}/{} (剩餘為未解密面板,屬正常)",
                last_log,
                CALL_SITES.len()
            );
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

/// 嘗試改寫單一 callsite:只有「目前確實是 `CALL raw_line_draw`」才改寫成 `CALL cave`。
/// 未解密(非 E8 指向 raw draw)或已改寫則不動。
fn try_patch_site(h: HANDLE, site: u32, cave: u32) -> SitePatch {
    let Ok(bytes) = memory::read_bytes(h, site, CALL_LEN) else {
        return SitePatch::NotReady;
    };
    match rel32_call_target(site, &bytes) {
        Some(target) if target == RAW_LINE_DRAW => {
            let patch = build_call_patch(site, cave);
            if memory::write_code(h, site, &patch).is_ok() {
                SitePatch::Patched
            } else {
                SitePatch::NotReady
            }
        }
        Some(target) if target == cave => SitePatch::AlreadyPatched,
        _ => SitePatch::NotReady,
    }
}

/// 由 `E8`/`E9` rel32 指令解出絕對目標位址。非 call/jmp 回 None。
fn rel32_call_target(site: u32, bytes: &[u8]) -> Option<u32> {
    if bytes.len() < CALL_LEN {
        return None;
    }
    if bytes[0] != 0xE8 && bytes[0] != 0xE9 {
        return None;
    }
    let rel = i32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
    Some((site.wrapping_add(CALL_LEN as u32)).wrapping_add(rel as u32))
}

/// 組 `E8 rel32` 把 `site` 的呼叫導向 `target`(保留 call/E8,非 jmp)。
fn build_call_patch(site: u32, target: u32) -> [u8; CALL_LEN] {
    let rel = (target as i64 - (site as i64 + CALL_LEN as i64)) as i32;
    let mut patch = [0u8; CALL_LEN];
    patch[0] = 0xE8;
    patch[1..5].copy_from_slice(&rel.to_le_bytes());
    patch
}

/// cave shellcode:copy 該行(clamp 255)→ `\F`→`\f` 正規化 → 以 flag=0 呼叫色碼 render。
///
/// 移植 3.63 `build_item_tooltip_line_color_cave`,加 `\F`→`\f` 正規化段(只改 temp buffer)。
/// raw line draw 與色碼 render 都是 cdecl 且參數佈局一致(surface,text,...),故 esp-relative
/// 取參直接照搬;正規化段不動堆疊,不影響後續取參偏移。
fn build_line_color_cave(cave: u32, temp_buffer: u32) -> Vec<u8> {
    let mut sc = Vec::with_capacity(96);

    // push esi; push edi —— 進來時 [esp]=ret,[esp+4]=arg1(surface),[esp+8]=arg2(text)...
    sc.extend_from_slice(&[0x56, 0x57]);
    // mov esi,[esp+0x10] —— arg2 = 該行文字指標
    sc.extend_from_slice(&[0x8B, 0x74, 0x24, 0x10]);
    // mov ecx,[esp+0x14] —— arg3 = 長度
    sc.extend_from_slice(&[0x8B, 0x4C, 0x24, 0x14]);
    // cmp ecx,0xFF / jbe +5 / mov ecx,0xFF —— clamp 長度到 255
    sc.extend_from_slice(&[0x81, 0xF9]);
    sc.extend_from_slice(&LINE_CLAMP.to_le_bytes());
    sc.extend_from_slice(&[0x76, 0x05]);
    sc.push(0xB9);
    sc.extend_from_slice(&LINE_CLAMP.to_le_bytes());
    // mov edx,ecx; mov edi,temp_buffer; rep movsb; mov byte [edi],0 —— copy + null-term
    sc.extend_from_slice(&[0x8B, 0xD1]);
    sc.push(0xBF);
    sc.extend_from_slice(&temp_buffer.to_le_bytes());
    sc.extend_from_slice(&[0xF3, 0xA4]);
    sc.extend_from_slice(&[0xC6, 0x07, 0x00]);

    // --- 正規化 \F → \f(只改 temp buffer;不動堆疊)---
    // mov eax,temp_buffer
    sc.push(0xB8);
    sc.extend_from_slice(&temp_buffer.to_le_bytes());
    // norm_loop:
    //   mov cl,[eax]; test cl,cl; je norm_done(+0x12)
    //   cmp cl,0x5C('\'); jne norm_next(+0x0A)
    //   cmp byte[eax+1],0x46('F'); jne norm_next(+4)
    //   mov byte[eax+1],0x66('f')
    // norm_next: inc eax; jmp norm_loop(-0x18)
    sc.extend_from_slice(&[
        0x8A, 0x08, // mov cl,[eax]
        0x84, 0xC9, // test cl,cl
        0x74, 0x12, // je norm_done
        0x80, 0xF9, 0x5C, // cmp cl,0x5C
        0x75, 0x0A, // jne norm_next
        0x80, 0x78, 0x01, 0x46, // cmp byte[eax+1],0x46
        0x75, 0x04, // jne norm_next
        0xC6, 0x40, 0x01, 0x66, // mov byte[eax+1],0x66
        0x40, // inc eax (norm_next)
        0xEB, 0xE8, // jmp norm_loop
    ]);
    // norm_done:

    // push 0 —— arg6 = flag = 0(繞過 render 小寫 flag-skip bug → 一律設色)
    sc.extend_from_slice(&[0x6A, 0x00]);
    // 轉發原本的 x/y/color(原 arg4/arg5/arg6);每次 push 後 esp 下移,故都讀 [esp+0x24]
    for _ in 0..3 {
        sc.extend_from_slice(&[0x8B, 0x44, 0x24, 0x24, 0x50]);
    }
    // mov eax,temp_buffer; push eax —— arg2 = 正規化後的該行
    sc.push(0xB8);
    sc.extend_from_slice(&temp_buffer.to_le_bytes());
    sc.push(0x50);
    // mov eax,[esp+0x20]; push eax —— arg1 = surface
    sc.extend_from_slice(&[0x8B, 0x44, 0x24, 0x20, 0x50]);
    // call COLOR_RENDERER
    sc.push(0xE8);
    let rel_at = sc.len();
    sc.extend_from_slice(&[0; 4]);
    // add esp,0x18(清掉 render 的 6 個參數);pop edi; pop esi; ret(原 6 參由 caller 清,cdecl)
    sc.extend_from_slice(&[0x83, 0xC4, 0x18]);
    sc.extend_from_slice(&[0x5F, 0x5E, 0xC3]);

    let rel = (COLOR_RENDERER as i64 - (cave as i64 + rel_at as i64 + 4)) as i32;
    sc[rel_at..rel_at + 4].copy_from_slice(&rel.to_le_bytes());
    sc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_sites_count_is_37() {
        assert_eq!(CALL_SITES.len(), 37);
    }

    #[test]
    fn call_target_roundtrip() {
        // 模擬一個 callsite 原本呼叫 raw line draw,解出目標應等於 RAW_LINE_DRAW。
        let site = CALL_SITES[0];
        let patch = build_call_patch(site, RAW_LINE_DRAW);
        assert_eq!(patch[0], 0xE8);
        assert_eq!(rel32_call_target(site, &patch), Some(RAW_LINE_DRAW));
    }

    #[test]
    fn call_patch_redirects_to_cave() {
        let site = 0x004C_0212u32;
        let cave = 0x1000_0000u32;
        let patch = build_call_patch(site, cave);
        assert_eq!(rel32_call_target(site, &patch), Some(cave));
    }

    #[test]
    fn rel32_rejects_non_call() {
        // 非 E8/E9(如未解密的雜訊或其他指令)應回 None,worker 視為未就緒。
        let site = 0x004C_0212u32;
        assert_eq!(
            rel32_call_target(site, &[0x90, 0x90, 0x90, 0x90, 0x90]),
            None
        );
    }

    #[test]
    fn cave_fits_and_has_renderer_tail() {
        let cave = 0x2000_0000u32;
        let temp = 0x2000_1000u32;
        let sc = build_line_color_cave(cave, temp);
        assert!(sc.len() <= CAVE_SIZE, "cave {} > {}", sc.len(), CAVE_SIZE);
        // 結尾應為 add esp,0x18 / pop edi / pop esi / ret
        assert_eq!(&sc[sc.len() - 6..], &[0x83, 0xC4, 0x18, 0x5F, 0x5E, 0xC3]);
    }

    #[test]
    fn cave_renderer_rel_resolves_to_renderer() {
        let cave = 0x2000_0000u32;
        let temp = 0x2000_1000u32;
        let sc = build_line_color_cave(cave, temp);
        // render 呼叫固定在結尾前:[E8 rel32] [83 C4 18 5F 5E C3] → E8 在 len-11。
        let e8 = sc.len() - 11;
        assert_eq!(sc[e8], 0xE8, "render call opcode");
        let rel = i32::from_le_bytes([sc[e8 + 1], sc[e8 + 2], sc[e8 + 3], sc[e8 + 4]]);
        let resolved = (cave as i64 + e8 as i64 + 1 + 4 + rel as i64) as u32;
        assert_eq!(resolved, COLOR_RENDERER);
    }
}
