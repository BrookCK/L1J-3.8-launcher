//! l38ddraw.dll 注入 — in-process DDraw present 接管(ALW 路線,取代 dgVoodoo / ddraw 代理)
//!
//! ## 為什麼
//! Win11 視窗化下原生 `primary->Blt` 每幀蓋過輸入框 child → 黑底 + 打字閃爍 + IME 選字不顯。
//! 解法:注入自家 `l38ddraw.dll`,在遊戲 process 內 hook present 函式 `FUN_0055d460`,
//! 改走我們在遊戲 HWND 上自建的 DXGI swapchain(BitBlt-model + GDI_COMPATIBLE)→ DWM 統一
//! 合成,不黑不閃,子視窗 + IME 自然疊上。surface 仍是遊戲原生(CPU),GPU 收益在 present。
//!
//! ## 交付
//! dll bytes `include_bytes!` 內嵌進 launcher → 啟動時寫到 `%LOCALAPPDATA%\Lineage38Launcher\`
//! → 遊戲視窗可見後(ddraw 已就緒)用本模組 `inject_dll`(LoadLibraryW)注入。
//! dll 內 worker thread 輪詢等 ddraw init,再 AOB 定位 present 函式安裝 inline hook。
//!
//! 開發迭代:設 `LOGIN38_DDRAW_DLL=<路徑>` 可直接注入指定 dll(略過內嵌寫檔),
//! 方便改 dll 後免重編 launcher。設 `LOGIN38_DDRAW_INPROC=0` 整個關閉。

use anyhow::{anyhow, bail, Context, Result};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    CreateRemoteThread, GetExitCodeThread, WaitForSingleObject,
};

use crate::logger::log_line;

const INJECT_WAIT_MS: u32 = 10_000;

/// 內嵌的 l38ddraw.dll(由 `ddraw_inproc/build.bat` 編出)
const DDRAW_DLL_BYTES: &[u8] = include_bytes!("../../ddraw_inproc/build/l38ddraw.dll");

/// 是否被使用者關閉(env / marker)
pub fn disabled_by_env() -> bool {
    std::env::var_os("LOGIN38_DDRAW_INPROC")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

/// 注入 l38ddraw.dll 到遊戲 process。應在遊戲主視窗可見(ddraw 已 init)後呼叫。
pub fn install(h_process: HANDLE) -> Result<()> {
    if disabled_by_env() {
        log_line!("[ddraw] in-process present 接管 已由 env 關閉");
        return Ok(());
    }

    let dll_path = resolve_dll_path()?;
    inject_dll(h_process, &dll_path)
        .with_context(|| format!("l38ddraw 注入失敗: {}", dll_path.display()))?;
    log_line!("[ddraw] l38ddraw.dll 已注入: {}", dll_path.display());
    Ok(())
}

/// 把 dll LoadLibraryW 注入目標 process(VirtualAllocEx + WriteProcessMemory + CreateRemoteThread)
fn inject_dll(h_process: HANDLE, dll_path: &Path) -> Result<()> {
    let load_lib = load_library_w_addr()?;
    let path_w: Vec<u16> = dll_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let path_bytes = path_w.len() * 2;

    let remote = unsafe {
        VirtualAllocEx(
            h_process,
            None,
            path_bytes,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
    };
    if remote.is_null() {
        bail!("VirtualAllocEx({path_bytes}) 失敗");
    }

    let result = (|| -> Result<()> {
        let mut written = 0usize;
        unsafe {
            WriteProcessMemory(
                h_process,
                remote,
                path_w.as_ptr().cast(),
                path_bytes,
                Some(&mut written),
            )?;
        }
        if written != path_bytes {
            bail!("WriteProcessMemory 寫了 {written}/{path_bytes}");
        }

        let mut tid = 0u32;
        let thread = unsafe {
            CreateRemoteThread(
                h_process,
                None,
                0,
                Some(std::mem::transmute::<
                    usize,
                    unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
                >(load_lib)),
                Some(remote),
                0,
                Some(&mut tid),
            )
        }?;

        let wait = unsafe { WaitForSingleObject(thread, INJECT_WAIT_MS) };
        let mut hmod = 0u32;
        let _ = unsafe { GetExitCodeThread(thread, &mut hmod) };
        unsafe {
            let _ = CloseHandle(thread);
        }

        if wait == WAIT_TIMEOUT {
            bail!("LoadLibraryW timeout {INJECT_WAIT_MS}ms tid={tid}");
        }
        if wait != WAIT_OBJECT_0 {
            bail!("LoadLibraryW wait 失敗: {wait:?} tid={tid}");
        }
        if hmod == 0 {
            bail!("LoadLibraryW 回 NULL(dll 載入失敗)tid={tid}");
        }
        log_line!("[ddraw] LoadLibraryW OK tid={tid} hmod=0x{hmod:08X}");
        Ok(())
    })();

    unsafe {
        let _ = VirtualFreeEx(h_process, remote, 0, MEM_RELEASE);
    }
    result
}

fn load_library_w_addr() -> Result<usize> {
    let kernel32: Vec<u16> = std::ffi::OsStr::new("kernel32.dll")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let module = GetModuleHandleW(PCWSTR(kernel32.as_ptr()))
            .context("GetModuleHandleW(kernel32.dll)")?;
        let proc = GetProcAddress(module, PCSTR(c"LoadLibraryW".as_ptr() as *const u8))
            .ok_or_else(|| anyhow!("GetProcAddress(LoadLibraryW) 回 NULL"))?;
        Ok(proc as usize)
    }
}

/// 決定要注入的 dll 路徑:
///  1. `LOGIN38_DDRAW_DLL` env 指定且存在 → 直接用(開發迭代)
///  2. 否則把內嵌 bytes 寫到 `%LOCALAPPDATA%\Lineage38Launcher\l38ddraw.dll`
fn resolve_dll_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("LOGIN38_DDRAW_DLL") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
        log_line!(
            "[ddraw] LOGIN38_DDRAW_DLL 指定路徑不存在,改用內嵌: {}",
            p.display()
        );
    }

    let dir = local_app_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("建立目錄失敗: {}", dir.display()))?;
    let path = dir.join("l38ddraw.dll");

    // 已存在且內容相同就不重寫(避免 game 還掛著 dll 時覆寫失敗)
    let need_write = match std::fs::read(&path) {
        Ok(cur) => cur != DDRAW_DLL_BYTES,
        Err(_) => true,
    };
    if need_write {
        if let Err(e) = std::fs::write(&path, DDRAW_DLL_BYTES) {
            // 舊 dll 可能被前一個遊戲 process 鎖住;改寫帶 pid 的備援檔
            log_line!("[ddraw] 寫 {} 失敗({e:#}),改用備援檔名", path.display());
            let alt = dir.join(format!("l38ddraw_{}.dll", std::process::id()));
            std::fs::write(&alt, DDRAW_DLL_BYTES)
                .with_context(|| format!("寫備援 dll 失敗: {}", alt.display()))?;
            return Ok(alt);
        }
    }
    Ok(path)
}

fn local_app_dir() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or_else(|| anyhow!("找不到 %LOCALAPPDATA%"))?;
    Ok(PathBuf::from(base).join("Lineage38Launcher"))
}
