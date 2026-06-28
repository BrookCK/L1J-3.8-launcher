//! `FUN_0045b270` inline hook + anim 表 codecave。
//!
//! `FUN_0045b270(this=0x9AAA40, gfxid)`（__thiscall, ret 4）是**所有 item icon**的 gfxid→TBT
//! resource 解析器（商店/交易/背包/快捷/擺攤所有面板、快取與懶載入路徑都先呼叫它）。
//! 2026-06-26 實機驗證：原設計的 0x5553D0 只覆蓋預載快取路徑，懶載入 icon（如自訂高 gfxid）
//! 走 FUN_0045b270→0x5554B0/0x5552F0，完全繞過 0x5553D0。
//!
//! hook 後：gfxid 在 anim 表且處於動畫期 → 回傳「該幀的 TBT-raw buffer」(launcher 把 PNG 轉成
//! 遊戲 icon 格式注入)；休息期 / 非目標 → 跑原 prologue 回 0x45B276 解析原生 TBT。
//! gfxid 直接是參數 → 免 buf_map / poll / 反查（原設計一整塊複雜度消除）。全域時鐘 GetTickCount
//! → 全畫面同 gfxid 自動同步。
//!
//! anim record 佈局（serialize_anim_table）：tbt:u16(0), speed:u16(2), interval:u32(4),
//! n_frames:u32(8), frames:[u32;99](12)。frames[] = 注入的 buffer 遊戲位址。

use anyhow::Result;
use windows::core::s;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};

use launcher::dynamic_icon_format::{serialize_anim_table, AnimMap};

use crate::platform::memory;

/// gfxid→resource 解析器（所有 icon 路徑唯一 gate）。
const TARGET: u32 = 0x0045B270;
/// 6-byte prologue（push ebp; mov ebp,esp; sub esp,0x4C）之後的 clean 邊界。
const HOOK_RETN: u32 = 0x0045B276;
/// 實證 prologue。
const EXPECTED_PROLOGUE: [u8; 6] = [0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x4C];
/// anim record byte 大小（lib 同步）。
const REC: u32 = 408;

/// 把序列化的 anim 表（frames[] = 注入 buffer 位址）寫進 game codecave，回 (表位址, 筆數)。
pub fn write_anim_table(h: HANDLE, ptr_map: &AnimMap) -> Result<(u32, u32)> {
    let (count, blob) = serialize_anim_table(ptr_map);
    if blob.is_empty() {
        return Ok((0, 0));
    }
    let addr = memory::alloc_exec(h, blob.len().max(16))?;
    memory::write_code(h, addr, &blob)?;
    Ok((addr, count))
}

/// 驗 FUN_0045b270 prologue = 55 8B EC 83 EC 4C。
fn check_prologue(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 6 || bytes[..6] != EXPECTED_PROLOGUE {
        anyhow::bail!(
            "FUN_0045b270 prologue 不符 55 8B EC 83 EC 4C（實際 {:02X?}）",
            &bytes[..bytes.len().min(6)]
        );
    }
    Ok(())
}

/// 組 hook shellcode（cave 內絕對位址；data 槽 cave_ret 在 cave+0x200）。
///
/// 進入時 ecx=this, [esp+4]=gfxid。pushad 後掃表；命中且動畫期 → eax=幀ptr; ret 4；
/// 否則 popad → 跑原 prologue → jmp 0x45B276。
pub fn build_hook_shellcode(cave: u32, table: u32, count: u32, gtc: u32) -> Vec<u8> {
    let table_end = table.wrapping_add(count.saturating_mul(REC));
    let cave_ret = cave + 0x200;
    let mut sc: Vec<u8> = Vec::with_capacity(128);
    let push32 = |sc: &mut Vec<u8>, v: u32| sc.extend_from_slice(&v.to_le_bytes());
    let mut fix_pass: Vec<usize> = Vec::new(); // → .pass
    let mut fix_match: Vec<usize> = Vec::new(); // → .match

    // pushad
    sc.push(0x60);
    // mov eax,[esp+0x24]   ; gfxid（pushad 0x20 後，原 [esp+4]→[esp+0x24]）
    sc.extend_from_slice(&[0x8B, 0x44, 0x24, 0x24]);
    // mov edx, table
    sc.push(0xBA);
    push32(&mut sc, table);

    // .scan:
    let scan = sc.len();
    // cmp edx, table_end
    sc.extend_from_slice(&[0x81, 0xFA]);
    push32(&mut sc, table_end);
    // jae .pass
    sc.extend_from_slice(&[0x0F, 0x83]);
    fix_pass.push(sc.len());
    push32(&mut sc, 0);
    // movzx ecx, word [edx]   ; record.tbt（零延伸 32-bit，避免 gfxid>0xFFFF 假命中）
    sc.extend_from_slice(&[0x0F, 0xB7, 0x0A]);
    // cmp eax, ecx
    sc.extend_from_slice(&[0x3B, 0xC1]);
    // je .match
    sc.extend_from_slice(&[0x0F, 0x84]);
    fix_match.push(sc.len());
    push32(&mut sc, 0);
    // add edx, 408
    sc.extend_from_slice(&[0x81, 0xC2, 0x98, 0x01, 0x00, 0x00]);
    // jmp .scan
    sc.push(0xE9);
    let after = sc.len() + 4;
    push32(&mut sc, (scan as i32 - after as i32) as u32);

    // .match:  edx → record
    let m = sc.len();
    for pos in &fix_match {
        let rel = m as i32 - (*pos as i32 + 4);
        sc[*pos..*pos + 4].copy_from_slice(&rel.to_le_bytes());
    }
    // mov ebp, edx   ; record（GetTickCount 為 stdcall，保留 ebp）
    sc.extend_from_slice(&[0x8B, 0xEA]);
    // mov eax, gtc ; call eax
    sc.push(0xB8);
    push32(&mut sc, gtc);
    sc.extend_from_slice(&[0xFF, 0xD0]); // eax = now_ms
                                         // movzx ecx, word [ebp+2]   ; speed
    sc.extend_from_slice(&[0x0F, 0xB7, 0x4D, 0x02]);
    // mov ebx, [ebp+8]          ; n_frames
    sc.extend_from_slice(&[0x8B, 0x5D, 0x08]);
    // imul ebx, ecx             ; anim_dur = n_frames*speed
    sc.extend_from_slice(&[0x0F, 0xAF, 0xD9]);
    // mov edi, [ebp+4]          ; interval
    sc.extend_from_slice(&[0x8B, 0x7D, 0x04]);
    // add edi, ebx              ; cycle = anim_dur + interval
    sc.extend_from_slice(&[0x03, 0xFB]);
    // xor edx,edx ; div edi     ; eax=now/cycle, edx=t=now%cycle
    sc.extend_from_slice(&[0x31, 0xD2, 0xF7, 0xF7]);
    // cmp edx, ebx              ; t vs anim_dur
    sc.extend_from_slice(&[0x3B, 0xD3]);
    // jae .pass                 ; t>=anim_dur → 休息
    sc.extend_from_slice(&[0x0F, 0x83]);
    fix_pass.push(sc.len());
    push32(&mut sc, 0);
    // mov eax, edx              ; eax = t
    sc.extend_from_slice(&[0x8B, 0xC2]);
    // xor edx,edx ; div ecx     ; eax = t/speed = frame_idx
    sc.extend_from_slice(&[0x31, 0xD2, 0xF7, 0xF1]);
    // mov eax, [ebp + eax*4 + 12]  ; frames[frame_idx] = buffer ptr
    sc.extend_from_slice(&[0x8B, 0x44, 0x85, 0x0C]);
    // mov [cave_ret], eax
    sc.push(0xA3);
    push32(&mut sc, cave_ret);
    // popad
    sc.push(0x61);
    // mov eax, [cave_ret]
    sc.push(0xA1);
    push32(&mut sc, cave_ret);
    // ret 4
    sc.extend_from_slice(&[0xC2, 0x04, 0x00]);

    // .pass:
    let p = sc.len();
    for pos in &fix_pass {
        let rel = p as i32 - (*pos as i32 + 4);
        sc[*pos..*pos + 4].copy_from_slice(&rel.to_le_bytes());
    }
    // popad ; push ebp ; mov ebp,esp ; sub esp,0x4C   ; 還原原 prologue
    sc.extend_from_slice(&[0x61, 0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x4C]);
    // jmp HOOK_RETN
    sc.push(0xE9);
    let after = sc.len() + 4;
    let rel = HOOK_RETN as i32 - (cave as i32 + after as i32);
    push32(&mut sc, rel as u32);
    sc
}

/// 安裝 hook。回傳 cave 位址。
pub fn install_hook(h: HANDLE, table: u32, count: u32) -> Result<u32> {
    // 1. 驗 prologue
    let bytes = memory::read_bytes(h, TARGET, 8)?;
    check_prologue(&bytes)?;

    // 2. GetTickCount（kernel32 全進程共享）
    let gtc = unsafe {
        let k32 = GetModuleHandleA(s!("kernel32.dll"))?;
        GetProcAddress(k32, s!("GetTickCount"))
            .ok_or_else(|| anyhow::anyhow!("GetTickCount 解析失敗"))? as usize as u32
    };

    // 3. cave（shellcode + 0x200 data 槽）
    let cave = memory::alloc_exec(h, 0x400)?;
    let sc = build_hook_shellcode(cave, table, count, gtc);
    anyhow::ensure!(
        sc.len() <= 0x200,
        "shellcode {} bytes 越過 data 區 0x200",
        sc.len()
    );
    memory::write_code(h, cave, &sc)?;

    // 4. TARGET 寫 E9 rel32 + 90（6 bytes 覆蓋整個 prologue 55 8B EC 83 EC 4C）
    let mut patch = [0u8; 6];
    patch[0] = 0xE9;
    let rel = cave.wrapping_sub(TARGET + 5) as i32;
    patch[1..5].copy_from_slice(&rel.to_le_bytes());
    patch[5] = 0x90; // nop（補滿被切的 4C）
    memory::write_code(h, TARGET, &patch)?;
    Ok(cave)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shellcode_shape() {
        let sc = build_hook_shellcode(0x10000000, 0x20000000, 3, 0x77001234);
        assert!(sc.len() <= 0x200, "shellcode {} bytes 過長", sc.len());
        assert_eq!(sc[0], 0x60); // pushad
                                 // 結尾 jmp HOOK_RETN
        assert_eq!(sc[sc.len() - 5], 0xE9);
        let rel = i32::from_le_bytes(sc[sc.len() - 4..].try_into().unwrap());
        let after = 0x10000000i32 + sc.len() as i32;
        assert_eq!((after + rel) as u32, HOOK_RETN);
    }

    #[test]
    fn prologue_check() {
        assert!(check_prologue(&[0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x4C, 0x00]).is_ok());
        assert!(check_prologue(&[0x55, 0x8B, 0xEC, 0x6A, 0x00, 0x00]).is_err());
    }
}
