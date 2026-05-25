//! 物理攻擊 — bootstrap-once 模式:寫 5 個 CLICK_HANDLER globals + 一次 thiscall
//! `click_attack`(0x5A3770),讓 client 自己用 `Function A` scheduler chain 連打。
//!
//! ## 為什麼不是每 tick fire packet 或 PostMessage drag
//!
//! 玩家手動點怪砍是 client 進程內 ns 級函式呼叫;bot 從外部每 tick 200ms fire PostMessage
//! WM_LBUTTONDOWN/MOUSEMOVE/LBUTTONUP 會讓 WM queue 累積、click state 卡按住、
//! `CLICK_HANDLER` 全域被反覆覆寫,動畫 state machine 跑爛。
//!
//! Raw `C_ATTACK` packet 也不對 — 沒有 client 端 CD 同步,server 雖認可但 client 看起來
//! 像「沒打到怪」,且不會跟玩家手動行為一致。
//!
//! ## 解法:bootstrap-once
//!
//! 1. **lock 第一次到位 / 切到新 target 時**呼一次 [`bootstrap_click_attack`]:
//!    - `WriteProcessMemory` 寫 `ATTACK_TARGET_PTR / X / Y / MODE_FLAG / AUTO_ATTACK_FLAG`
//!    - `CreateRemoteThread` 跑一段 shellcode 做 `__thiscall click_attack(this, target,
//!      x, y, 0)` 一次 — `0x5A3770` 內部會送第一發 packet + 把 `Function A` 排進 scheduler
//! 2. **後續 tick** bot 什麼都不做(`HuntCommand::Idle`),由 client 自己:
//!    - `Function A`(`0x5A3010`)每 scheduler tick 觸發 → 檢查 CD `[0xC2D27C]` → 還沒到
//!      就 ret;到了 → 走 `Function B` 同分支送下一發 packet。
//!    - target 死(target+0x14==8)/ `[0xAC450C]==0` → gate 失敗 → `Function A` 不再
//!      重排自己 → 鏈自然斷。
//! 3. **lock 切換 / shutdown** → bot 寫新 globals + 再 bootstrap 或 [`stop_client_auto_attack`]
//!    寫 `[0xAC450C]=0` 切鏈。
//!
//! ## Trade-offs
//!
//! - CD 完全由 client 算(武器 / haste / debuff 自動套),bot 不追蹤
//! - 沒 PostMessage drag 動畫副作用(WM queue 不堵)
//! - melee / ranged 由 client 看 `target+0x14` byte 自動分派(劍 → 0xE5,弓 → 0x7B)
//! - RemoteThread 開銷 ~1-2ms,但 bootstrap 頻率 = target 切換頻率(數秒一次),可忽略

use anyhow::{anyhow, Context, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{VirtualFreeEx, MEM_RELEASE};
use windows::Win32::System::Threading::{CreateRemoteThread, WaitForSingleObject, INFINITE};

use crate::aux::address::{
    ATTACK_MODE_FLAG, ATTACK_TARGET_PTR, ATTACK_TARGET_X, ATTACK_TARGET_Y, AUTO_ATTACK_FLAG,
    CLICK_ATTACK_FUNC, CLICK_ATTACK_NEXT_TICK, LOCAL_PLAYER_PTR,
};
use crate::log_line;
use crate::memory::{alloc_exec, read_u32, write_code};

/// Bootstrap shellcode 大小(實測 30 bytes,32 給對齊餘裕)
const SHELLCODE_REGION_SIZE: usize = 32;

/// shellcode 實際 bytes 數量(`build_bootstrap_shellcode` 產出)
const SHELLCODE_BYTES: usize = 30;

/// 對指定 target 起一次 client-side auto-attack chain。
///
/// **lock 第一次到位 / 切換 target 時**用 `fresh_target=true` 呼叫,bot 會額外把
/// `CLICK_ATTACK_NEXT_TICK` 清 0 強制 Function B 第一發立刻 fire(否則上一隻怪殘留的
/// CD 會讓 Function B `jb skip` 直接 ret,chain 永遠啟動不了)。 同 target 的週期性
/// 重 fire(Option A safety net)用 `fresh_target=false`,讓 Function B 內建 CD gate
/// 守住 weapon interval,避免 server 偵測 speed-hack。
///
/// ## 流程
///
/// 1. (可選)`fresh_target=true` → 寫 `CLICK_ATTACK_NEXT_TICK=0` 強制 CD pass
/// 2. 寫 5 個 globals(`ATTACK_TARGET_PTR / X / Y / MODE_FLAG=0 / AUTO_ATTACK_FLAG=1`)
/// 3. 讀 `LOCAL_PLAYER_PTR` 拿 thiscall 的 `this`
/// 4. 組 30 bytes shellcode + `CreateRemoteThread` 執行 + `WaitForSingleObject` + 釋放記憶體
///
/// ## 錯誤情境
///
/// - `LOCAL_PLAYER_PTR == 0` — 玩家還沒進場,bail(caller 應該避開這情境)
/// - VirtualAllocEx / WriteProcessMemory / CreateRemoteThread 失敗 — anyhow 帶錯誤路徑回傳
pub fn bootstrap_click_attack(
    h: HANDLE,
    target_entity_ptr: u32,
    target_raw_x: u32,
    target_y: u32,
    fresh_target: bool,
) -> Result<()> {
    if fresh_target {
        // 清掉上一隻怪殘留的 CD;Function B 的 `cmp ecx, [0xC2D27C]; jb skip` 因此必定 pass,
        // 第一發 packet 立刻送出 + enqueue Function A。 Function B 隨後會把這格更新成
        // `now + weapon_interval`,所以這個寫入只影響「第一發是否立刻 fire」,不影響後續 CD。
        write_code(h, CLICK_ATTACK_NEXT_TICK, &0u32.to_le_bytes())
            .context("寫 CLICK_ATTACK_NEXT_TICK=0 (fresh target CD reset)")?;
    }
    write_code(h, ATTACK_TARGET_PTR, &target_entity_ptr.to_le_bytes())
        .context("寫 ATTACK_TARGET_PTR")?;
    write_code(h, ATTACK_TARGET_X, &target_raw_x.to_le_bytes()).context("寫 ATTACK_TARGET_X")?;
    write_code(h, ATTACK_TARGET_Y, &target_y.to_le_bytes()).context("寫 ATTACK_TARGET_Y")?;
    write_code(h, ATTACK_MODE_FLAG, &0u32.to_le_bytes()).context("寫 ATTACK_MODE_FLAG=0")?;
    write_code(h, AUTO_ATTACK_FLAG, &[1u8]).context("寫 AUTO_ATTACK_FLAG=1")?;

    let this_ptr = read_u32(h, LOCAL_PLAYER_PTR).context("讀 LOCAL_PLAYER_PTR")?;
    if this_ptr == 0 {
        return Err(anyhow!(
            "LOCAL_PLAYER_PTR=0 (玩家未進場),無法 bootstrap click_attack"
        ));
    }

    let shellcode = build_bootstrap_shellcode(this_ptr, target_entity_ptr, target_raw_x, target_y);
    let cave =
        alloc_exec(h, SHELLCODE_REGION_SIZE).context("alloc_exec for bootstrap shellcode")?;
    let write_result = write_code(h, cave, &shellcode).context("寫 bootstrap shellcode");
    if let Err(e) = write_result {
        unsafe {
            let _ = VirtualFreeEx(h, cave as *mut _, 0, MEM_RELEASE);
        }
        return Err(e);
    }

    let mut tid = 0u32;
    let thread_result = unsafe {
        CreateRemoteThread(
            h,
            None,
            0,
            Some(std::mem::transmute::<
                usize,
                unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
            >(cave as usize)),
            None,
            0,
            Some(&mut tid),
        )
    };
    let thread = match thread_result {
        Ok(handle) => handle,
        Err(e) => {
            unsafe {
                let _ = VirtualFreeEx(h, cave as *mut _, 0, MEM_RELEASE);
            }
            return Err(anyhow!("CreateRemoteThread for bootstrap: {e:#}"));
        }
    };

    unsafe {
        let _ = WaitForSingleObject(thread, INFINITE);
        let _ = CloseHandle(thread);
        let _ = VirtualFreeEx(h, cave as *mut _, 0, MEM_RELEASE);
    }

    log_line!(
        "[bot/attack] bootstrap_click_attack target_ptr=0x{:08X} x=0x{:08X} y={} this=0x{:08X} tid={} fresh={}",
        target_entity_ptr,
        target_raw_x,
        target_y,
        this_ptr,
        tid,
        fresh_target
    );
    Ok(())
}

/// 切斷 client-side auto-attack scheduler chain — 寫 `[0xAC450C]=0`。
///
/// `Function A`(auto-repeat)gate 之一是 `[AUTO_ATTACK_FLAG] != 0`,寫 0 後下個 tick
/// 它就不會重排自己 → 鏈斷,但**不是中斷正在處理的那一發**(client side 該 packet
/// 已經送出)。
///
/// bot shutdown / master toggle off / target 死前主動切換時呼叫。 重複呼叫安全。
pub fn stop_client_auto_attack(h: HANDLE) -> Result<()> {
    write_code(h, AUTO_ATTACK_FLAG, &[0u8]).context("寫 AUTO_ATTACK_FLAG=0(切斷 auto-attack chain)")
}

/// 讀 `[CLICK_ATTACK_NEXT_TICK]`(`[0xC2D27C]`)— 客戶端記錄的「下次允許攻擊 GetTickCount」。
///
/// 與本機 `GetTickCount` 比較:
/// - 回 `0` → CD 已過,bot 可以馬上 fire 下一發 bootstrap(Function B 一定 pass CD gate)
/// - 回 `>0` → CD 尚未過,還剩 N 毫秒;bot 該回 `HuntOutcome::Cooldown` 不要 fire
///
/// **為什麼用本機 `GetTickCount`**:`GetTickCount` 回的是系統 uptime,跨 process 共用
/// 同一份 kernel 時鐘,bot process 跟 game process 讀到的數字相同。 不需要透過 RPC 拿
/// game 端的 tick,直接 `windows::Win32::System::SystemInformation::GetTickCount()` 即可。
///
/// **錯誤情境**:`ReadProcessMemory` 失敗(進程剛 detach 等)→ 視為 CD=0(讓 caller 嘗試
/// fire,bootstrap 內部會再次處理錯誤),不阻斷 bot tick。
#[cfg(test)]
pub fn click_attack_cd_remaining_ms(h: HANDLE) -> u32 {
    use windows::Win32::System::SystemInformation::GetTickCount;
    let next_tick = match crate::memory::read_u32(h, CLICK_ATTACK_NEXT_TICK) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    let now = unsafe { GetTickCount() };
    next_tick.saturating_sub(now)
}

/// 組裝 thiscall click_attack(target, x, y, 0) shellcode(30 bytes)。
///
/// 對應 asm:
/// ```asm
///   push 0                ; 6A 00                              ; 2B (mode=0 normal click)
///   push y                ; 68 [imm32]                         ; 5B
///   push x                ; 68 [imm32]                         ; 5B
///   push target_ptr       ; 68 [imm32]                         ; 5B
///   mov ecx, this_ptr     ; B9 [imm32]                         ; 5B
///   mov eax, 0x5A3770     ; B8 [imm32]                         ; 5B
///   call eax              ; FF D0                              ; 2B
///   ret                   ; C3                                 ; 1B  (callee 自清 0x10)
/// ```
fn build_bootstrap_shellcode(
    this_ptr: u32,
    target_ptr: u32,
    target_x: u32,
    target_y: u32,
) -> Vec<u8> {
    let mut sc: Vec<u8> = Vec::with_capacity(SHELLCODE_REGION_SIZE);
    sc.extend_from_slice(&[0x6A, 0x00]); // push 0 (mode)
    sc.push(0x68);
    sc.extend_from_slice(&target_y.to_le_bytes()); // push y
    sc.push(0x68);
    sc.extend_from_slice(&target_x.to_le_bytes()); // push x
    sc.push(0x68);
    sc.extend_from_slice(&target_ptr.to_le_bytes()); // push target_ptr
    sc.push(0xB9);
    sc.extend_from_slice(&this_ptr.to_le_bytes()); // mov ecx, this_ptr
    sc.push(0xB8);
    sc.extend_from_slice(&CLICK_ATTACK_FUNC.to_le_bytes()); // mov eax, CLICK_ATTACK_FUNC
    sc.extend_from_slice(&[0xFF, 0xD0]); // call eax (ret 0x10 自清)
    sc.push(0xC3); // ret
    debug_assert_eq!(
        sc.len(),
        SHELLCODE_BYTES,
        "bootstrap shellcode 大小應為 {SHELLCODE_BYTES} bytes,實得 {}",
        sc.len()
    );
    sc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shellcode_is_expected_size() {
        let sc = build_bootstrap_shellcode(0xC0FFEE00, 0xDEADBEEF, 0x11112222, 0x33334444);
        assert_eq!(sc.len(), SHELLCODE_BYTES);
    }

    #[test]
    fn shellcode_pushes_args_in_reverse_order_for_cdecl_stack_layout() {
        // thiscall caller pushes args right-to-left: mode → y → x → target_ptr
        let sc = build_bootstrap_shellcode(0xC0FFEE00, 0xDEADBEEF, 0x11112222, 0x33334444);
        // push 0 (mode)
        assert_eq!(&sc[0..2], &[0x6A, 0x00]);
        // push y = 0x33334444
        assert_eq!(sc[2], 0x68);
        assert_eq!(&sc[3..7], &0x3333_4444u32.to_le_bytes());
        // push x = 0x11112222
        assert_eq!(sc[7], 0x68);
        assert_eq!(&sc[8..12], &0x1111_2222u32.to_le_bytes());
        // push target_ptr = 0xDEADBEEF
        assert_eq!(sc[12], 0x68);
        assert_eq!(&sc[13..17], &0xDEAD_BEEFu32.to_le_bytes());
    }

    #[test]
    fn shellcode_loads_local_player_into_ecx() {
        let sc = build_bootstrap_shellcode(0xC0FFEE00, 0xDEADBEEF, 0x11112222, 0x33334444);
        // mov ecx, 0xC0FFEE00
        assert_eq!(sc[17], 0xB9);
        assert_eq!(&sc[18..22], &0xC0FF_EE00u32.to_le_bytes());
    }

    #[test]
    fn shellcode_calls_click_attack_func_then_returns() {
        let sc = build_bootstrap_shellcode(0xC0FFEE00, 0xDEADBEEF, 0x11112222, 0x33334444);
        // mov eax, CLICK_ATTACK_FUNC
        assert_eq!(sc[22], 0xB8);
        assert_eq!(&sc[23..27], &CLICK_ATTACK_FUNC.to_le_bytes());
        // call eax (FF D0) + ret (C3)
        assert_eq!(&sc[27..29], &[0xFF, 0xD0]);
        assert_eq!(sc[29], 0xC3);
    }

    #[test]
    fn shellcode_size_constant_covers_actual_bytes() {
        let sc = build_bootstrap_shellcode(0, 0, 0, 0);
        assert!(sc.len() <= SHELLCODE_REGION_SIZE);
    }
}
