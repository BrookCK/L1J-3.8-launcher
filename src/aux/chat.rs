//! 遊戲內 chat 注入 — 兩條路徑
//!
//! 路徑 A:**直接寫 buffer + bump index**(`push_chat_line`,僅供測試)
//!   - chat ring buffer @ 0x00996D00,150 entries × 0x126 bytes
//!   - 寫入索引 @ 0x00980EA0,(index + 1) % 150 自動 wrap
//!   - 不需 codecave / CreateRemoteThread,只 WriteProcessMemory
//!   - **缺陷**:bypass 0x00437500 ChatDispatch → 不觸發 0x00437D30
//!     ChatSideEffect → 玩家如果先前往上捲過,聊天框不再 auto-tail 到底
//!
//! 路徑 B:**CreateRemoteThread 呼叫 ChatDispatch(0x00437500) 並送 channel=-1**
//!   (`push_chat_via_dispatch`,正式啟動通知用)
//!   - channel=-1 路由至 0x004378A0(顯示)+ 0x00437D30(scroll/sound 副作用)
//!   - 同步 wait,啟動只呼叫一次,主執行緒此時尚未渲染聊天 UI,race 風險低
//!   - 用於 LinHelperZ-執行中 啟動字

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateRemoteThread, WaitForSingleObject};

use crate::memory;

const CHAT_DISPATCH_FN: u32 = 0x0043_7500;

/// RGB565 預設色(實測 2026-04-28 對應 \F0~\F4 palette)
pub mod color {
    pub const GREEN: u16 = 0x07E0; // 純綠(R=0,G=63,B=0)
}

/// 路徑 B — CreateRemoteThread 呼叫 ChatDispatch(0x00437500)。
///
/// 簽名(假設):`(char* text, WORD src_id, WORD color, int channel, int p5)` cdecl。
/// `channel = -1` 時函數內部分別呼叫 0x004378A0(顯示)+ 0x00437D30(副作用)。
///
/// 注意:
/// - 配 codecave 後**不釋放**(thread 退出後 ChatSideEffect 可能仍引用 text 字串)
/// - 啟動只呼叫一次,記憶體浪費可忽略
pub fn push_chat_via_dispatch(h: HANDLE, text_bytes: &[u8], src_id: u16, color: u16) -> Result<()> {
    let mut text_with_null: Vec<u8> = text_bytes.to_vec();
    text_with_null.push(0);
    let text_len = text_with_null.len();

    // text + shellcode (預估 ~32 bytes)
    let total = text_len + 64;
    let base = memory::alloc_exec(h, total)?;
    let text_addr = base;
    let sc_addr = base + text_len as u32;

    memory::write_code(h, text_addr, &text_with_null)?;

    // ChatDispatch(text, src_id, color, -1, 0)
    //   68 ?? ?? ?? ??       push 0                 ; p5
    //   6A FF                push -1                ; channel
    //   68 ?? ?? ?? ??       push <color as u32>
    //   68 ?? ?? ?? ??       push <src_id as u32>
    //   68 ?? ?? ?? ??       push <text_addr>
    //   B8 ?? ?? ?? ??       mov eax, 0x00437500
    //   FF D0                call eax
    //   83 C4 14             add esp, 0x14          ; cdecl 5 args cleanup
    //   33 C0                xor eax, eax
    //   C2 04 00             ret 4                  ; stdcall ThreadProc 收尾
    let mut sc: Vec<u8> = Vec::with_capacity(32);
    sc.push(0x68);
    sc.extend_from_slice(&0u32.to_le_bytes());
    sc.push(0x6A);
    sc.push(0xFF);
    sc.push(0x68);
    sc.extend_from_slice(&(color as u32).to_le_bytes());
    sc.push(0x68);
    sc.extend_from_slice(&(src_id as u32).to_le_bytes());
    sc.push(0x68);
    sc.extend_from_slice(&text_addr.to_le_bytes());
    sc.push(0xB8);
    sc.extend_from_slice(&CHAT_DISPATCH_FN.to_le_bytes());
    sc.push(0xFF);
    sc.push(0xD0);
    sc.push(0x83);
    sc.push(0xC4);
    sc.push(0x14);
    sc.push(0x33);
    sc.push(0xC0);
    sc.push(0xC2);
    sc.push(0x04);
    sc.push(0x00);

    memory::write_code(h, sc_addr, &sc)?;

    unsafe {
        let mut tid = 0u32;
        let thread_handle = CreateRemoteThread(
            h,
            None,
            0,
            Some(std::mem::transmute::<
                usize,
                unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
            >(sc_addr as usize)),
            None,
            0,
            Some(&mut tid),
        )
        .context("CreateRemoteThread(ChatDispatch)")?;

        let wait = WaitForSingleObject(thread_handle, 5000);
        let _ = CloseHandle(thread_handle);

        if wait != WAIT_OBJECT_0 {
            anyhow::bail!(
                "ChatDispatch shellcode 等待逾時 (wait={:?}, tid={})",
                wait,
                tid
            );
        }
    }

    Ok(())
}

/// 推 LinHelperZ 啟動訊息(綠字)。
///
/// 顯示文字: `LinHelperZ-執行中`。中文「執行中」以 Big5 hardcoded(B0F5 A6E6 A4A4)。
/// 走路徑 B(ChatDispatch + channel=-1)以保留 auto-tail 行為。
///
/// 色碼用 `\F2`(palette 綠 = 0x87CA 淡綠)前綴內嵌在 text 裡,由 AddChatLine
/// 函數開頭的 prefix parser 處理 — 不依賴 ChatDispatch 第幾個 arg 是 color。
pub fn push_lhx_started(h: HANDLE) -> Result<()> {
    push_chat_via_dispatch(
        h,
        b"\\F2LinHelperZ-\xB0\xF5\xA6\xE6\xA4\xA4",
        0xFFFF,
        color::GREEN,
    )
}
