//! 靜態修補模組 — 等待解密 + 原子修補（防閃退核心） — v1.0.0 第一版
//!
//! 流程：輪詢等待 packer 解密完成 → 暫停所有執行緒 → 一次寫入兩個 patch → 恢復
//! 這是修復 Python launcher.py 登入畫面閃退的關鍵改進。

use crate::logger::log_line;
use crate::platform::{memory, process};
use anyhow::{bail, Context, Result};
use launcher::server_list::{
    MAX_IMG_LIMIT_VALUE, MAX_INVENTORY_LIMIT_VALUE, MIN_IMG_LIMIT_VALUE, MIN_INVENTORY_LIMIT_VALUE,
};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HANDLE;

// 解密偵測
const DECRYPT_ADDR: u32 = 0x004E204E;
const DECRYPT_EXPECTED: u32 = 0x0097850F; // JNZ +0x97（原始指令，解密後出現）

// ConditionalPatch：JNZ → NOP+JMP（繞過保護檢查）
const CONDITIONAL_PATCH_VAL: u32 = 0x0097E990; // 90 E9 97 00

// PatchCode_Point1
const PATCHCODE1_ADDR: u32 = 0x00722761;
const PATCHCODE1_VAL: u32 = 0x859001B0;
const TEXT_LOCALE_CODEPAGE_ADDR: u32 = 0x00968618;
const TEXT_LOCALE_SIMPLIFIED_CODEPAGE: u32 = 0x000003A8;
const FORCE_TEXT_LOCALE_TIMEOUT_MS: u64 = 8_000;
const FORCE_TEXT_LOCALE_POLL_MS: u64 = 50;
const SIMPLIFIED_STATUS_TOOLTIP_ENCODING_ADDR: u32 = 0x005126ED;
const SIMPLIFIED_STATUS_TOOLTIP_ENCODING_ORIGINAL: &[u8] = &[0x0F, 0x84, 0x4F, 0x01, 0x00, 0x00];
const SIMPLIFIED_STATUS_TOOLTIP_ENCODING_PATCHED: &[u8] = &[0x90; 6];
const SIMPLIFIED_STATUS_TOOLTIP_ENCODING_TIMEOUT_MS: u64 = 8_000;
const SIMPLIFIED_STATUS_TOOLTIP_ENCODING_POLL_MS: u64 = 50;

const TEXT_SCAN_START: u32 = 0x00401000;
const TEXT_SCAN_END: u32 = 0x00830000;
const DECRYPT_WAIT_TIMEOUT_MS: u64 = 120_000;
const DECRYPT_POLL_INTERVAL_MS: u64 = 50;

pub fn spawn_force_simplified_text_locale_worker(h: HANDLE) {
    let h_raw = h.0 as usize;
    std::thread::spawn(move || {
        let h = HANDLE(h_raw as *mut _);
        let start = Instant::now();
        let timeout = Duration::from_millis(FORCE_TEXT_LOCALE_TIMEOUT_MS);
        let target = TEXT_LOCALE_SIMPLIFIED_CODEPAGE.to_le_bytes();
        let mut first_value = None;
        let mut last_value = None;
        let mut wrote = false;
        let mut last_err = None;

        while start.elapsed() < timeout {
            match memory::read_u32(h, TEXT_LOCALE_CODEPAGE_ADDR) {
                Ok(current) => {
                    first_value.get_or_insert(current);
                    last_value = Some(current);
                    if current != TEXT_LOCALE_SIMPLIFIED_CODEPAGE {
                        match memory::write_code(h, TEXT_LOCALE_CODEPAGE_ADDR, &target) {
                            Ok(()) => wrote = true,
                            Err(e) => last_err = Some(format!("{e:#}")),
                        }
                    } else {
                        wrote = true;
                    }
                }
                Err(e) => last_err = Some(format!("{e:#}")),
            }
            std::thread::sleep(Duration::from_millis(FORCE_TEXT_LOCALE_POLL_MS));
        }

        if wrote {
            let first = first_value.unwrap_or(0);
            let last = last_value.unwrap_or(0);
            log_line!(
                "[patch-text] forced text locale @ 0x{TEXT_LOCALE_CODEPAGE_ADDR:08X}: first=0x{first:08X} last=0x{last:08X} target=0x{TEXT_LOCALE_SIMPLIFIED_CODEPAGE:08X}"
            );
        } else if let Some(err) = last_err {
            log_line!("[patch-text] forced text locale failed: {err}");
        } else {
            log_line!("[patch-text] forced text locale failed: no readable sample");
        }
    });
}

pub fn spawn_simplified_status_tooltip_encoding_worker(h: HANDLE) {
    let h_raw = h.0 as usize;
    std::thread::spawn(move || {
        let h = HANDLE(h_raw as *mut _);
        let start = Instant::now();
        let timeout = Duration::from_millis(SIMPLIFIED_STATUS_TOOLTIP_ENCODING_TIMEOUT_MS);
        let mut last_locale = None;
        let mut last_sample = None;
        let mut last_err = None;

        while start.elapsed() < timeout {
            match memory::read_u32(h, TEXT_LOCALE_CODEPAGE_ADDR) {
                Ok(locale) => {
                    last_locale = Some(locale);
                    if locale != TEXT_LOCALE_SIMPLIFIED_CODEPAGE {
                        std::thread::sleep(Duration::from_millis(
                            SIMPLIFIED_STATUS_TOOLTIP_ENCODING_POLL_MS,
                        ));
                        continue;
                    }
                }
                Err(e) => {
                    last_err = Some(format!("{e:#}"));
                    std::thread::sleep(Duration::from_millis(
                        SIMPLIFIED_STATUS_TOOLTIP_ENCODING_POLL_MS,
                    ));
                    continue;
                }
            }

            match memory::read_bytes(
                h,
                SIMPLIFIED_STATUS_TOOLTIP_ENCODING_ADDR,
                SIMPLIFIED_STATUS_TOOLTIP_ENCODING_ORIGINAL.len(),
            ) {
                Ok(current) if current == SIMPLIFIED_STATUS_TOOLTIP_ENCODING_PATCHED => {
                    log_line!(
                        "[patch-text] simplified status tooltip encoding already patched @ 0x{SIMPLIFIED_STATUS_TOOLTIP_ENCODING_ADDR:08X}"
                    );
                    return;
                }
                Ok(current) if current == SIMPLIFIED_STATUS_TOOLTIP_ENCODING_ORIGINAL => {
                    match memory::write_code(
                        h,
                        SIMPLIFIED_STATUS_TOOLTIP_ENCODING_ADDR,
                        SIMPLIFIED_STATUS_TOOLTIP_ENCODING_PATCHED,
                    ) {
                        Ok(()) => {
                            log_line!(
                                "[patch-text] simplified status tooltip encoding patched @ 0x{SIMPLIFIED_STATUS_TOOLTIP_ENCODING_ADDR:08X}"
                            );
                            return;
                        }
                        Err(e) => last_err = Some(format!("{e:#}")),
                    }
                }
                Ok(current) => last_sample = Some(hex_bytes(&current)),
                Err(e) => last_err = Some(format!("{e:#}")),
            }
            std::thread::sleep(Duration::from_millis(
                SIMPLIFIED_STATUS_TOOLTIP_ENCODING_POLL_MS,
            ));
        }

        if let Some(locale) = last_locale {
            if locale != TEXT_LOCALE_SIMPLIFIED_CODEPAGE {
                log_line!(
                    "[patch-text] simplified status tooltip encoding skipped: locale=0x{locale:08X}"
                );
                return;
            }
        }

        if let Some(sample) = last_sample {
            log_line!(
                "[patch-text] simplified status tooltip encoding not patched @ 0x{SIMPLIFIED_STATUS_TOOLTIP_ENCODING_ADDR:08X}: current={sample}"
            );
        } else if let Some(err) = last_err {
            log_line!("[patch-text] simplified status tooltip encoding failed: {err}");
        } else {
            log_line!("[patch-text] simplified status tooltip encoding failed: no readable sample");
        }
    });
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

const MOVE_STATE_OBFUSCATION_PATTERN: &[u8] = &[
    0x0F, 0xBE, 0x42, 0x14, 0x83, 0xF8, 0x08, 0x74, 0x21, 0x8B, 0x0D, 0xB8, 0xD2, 0xC2, 0x00,
];
const MOVE_PACKET_ENCRYPTION_PATTERN: &[u8] = &[
    0x0F, 0xBE, 0x15, 0xE1, 0xAE, 0x9A, 0x00, 0x83, 0xFA, 0x03, 0x75, 0x22, 0xA1, 0xB8, 0xD2, 0xC2,
    0x00, 0x0F, 0xBE, 0x48, 0x15, 0x83, 0xF1, 0x49,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BytePatchSpec {
    pub name: &'static str,
    pub pattern: &'static [u8],
    pub patch_offset: u32,
    pub original: u8,
    pub patched: u8,
}

pub(crate) fn move_packet_no_encrypt_patch_specs() -> [BytePatchSpec; 2] {
    [
        BytePatchSpec {
            name: "move state obfuscation",
            pattern: MOVE_STATE_OBFUSCATION_PATTERN,
            patch_offset: 7,
            original: 0x74,
            patched: 0xEB,
        },
        BytePatchSpec {
            name: "move packet encryption",
            pattern: MOVE_PACKET_ENCRYPTION_PATTERN,
            patch_offset: 10,
            original: 0x75,
            patched: 0xEB,
        },
    ]
}

/// 等待 packer 解密完成，然後原子修補兩個關鍵位址
///
/// 修復閃退的核心邏輯：
/// 1. 輪詢 0x4E204E 直到解密完成
/// 2. 立即暫停所有遊戲執行緒
/// 3. 寫入 ConditionalPatch + PatchCode_Point1
/// 4. 恢復執行緒
pub fn wait_and_patch(h: HANDLE, pid: u32) -> Result<()> {
    log_line!("[等待] 程式碼解密中...");

    // 輪詢等待解密（最多 120 秒，間隔 10ms）
    let mut decrypted = false;
    let mut already_patched = false;
    let wait_start = std::time::Instant::now();
    let mut first_readable_logged = false;
    let mut last_val = 0u32;
    let poll_count = DECRYPT_WAIT_TIMEOUT_MS / DECRYPT_POLL_INTERVAL_MS;
    let log_every = (10_000 / DECRYPT_POLL_INTERVAL_MS).max(1);
    let unreadable_log_every = (5_000 / DECRYPT_POLL_INTERVAL_MS).max(1);
    for i in 0..poll_count {
        match memory::read_u32(h, DECRYPT_ADDR) {
            Ok(val) if val == DECRYPT_EXPECTED => {
                log_line!(
                    "[patch-time] decrypt marker ready after {:.3}s (0x{DECRYPT_ADDR:08X}=0x{val:08X})",
                    wait_start.elapsed().as_secs_f64()
                );
                log_line!("[OK] 程式碼已解密（耗時 {:.1}s）", i as f64 * 0.01);
                decrypted = true;
                break;
            }
            Ok(val) if val == CONDITIONAL_PATCH_VAL => {
                log_line!(
                    "[patch-time] decrypt marker already patched after {:.3}s (0x{DECRYPT_ADDR:08X}=0x{val:08X})",
                    wait_start.elapsed().as_secs_f64()
                );
                decrypted = true;
                already_patched = true;
                break;
            }
            Ok(val) => {
                last_val = val;
                if !first_readable_logged {
                    first_readable_logged = true;
                    log_line!(
                        "[patch-time] decrypt marker readable after {:.3}s (0x{DECRYPT_ADDR:08X}=0x{val:08X})",
                        wait_start.elapsed().as_secs_f64()
                    );
                } else if i > 0 && i % log_every == 0 {
                    log_line!(
                        "[patch-time] waiting decrypt marker {:.3}s (last=0x{last_val:08X})",
                        wait_start.elapsed().as_secs_f64()
                    );
                }
            }
            Err(_) => {
                // 進程可能還在初始化，繼續等待
                if i % unreadable_log_every == 0 && i > 0 {
                    log_line!("[等待] 讀取中... ({:.1}s)", i as f64 * 0.01);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(DECRYPT_POLL_INTERVAL_MS));
    }

    if !decrypted {
        log_line!("[patch-time] wait_and_patch timed out after 120s");
        log_line!("[patch-time] last decrypt marker value 0x{last_val:08X}");
        bail!("等待解密逾時（120s）");
    }

    // === ConditionalPatch：立即寫入（不暫停，避免競態條件） ===
    let patch1 = CONDITIONAL_PATCH_VAL.to_le_bytes();
    if already_patched {
        log_line!("[OK] ConditionalPatch @ 0x{DECRYPT_ADDR:08X}: already patched by StartupHook");
    } else {
        memory::write_code(h, DECRYPT_ADDR, &patch1)?;
        log_line!("[OK] ConditionalPatch @ 0x{DECRYPT_ADDR:08X}: JNZ → NOP+JMP（立即寫入）");
    }

    // === PatchCode_Point1：暫停 → 寫入 → 恢復（原子修補） ===
    log_line!("[修補] 暫停遊戲執行緒...");
    let threads = process::suspend_threads(pid)?;
    log_line!("[OK] 已暫停 {} 個執行緒", threads.len());

    let patch2 = PATCHCODE1_VAL.to_le_bytes();
    match memory::write_code(h, PATCHCODE1_ADDR, &patch2) {
        Ok(()) => {
            log_line!("[OK] PatchCode_Point1 @ 0x{PATCHCODE1_ADDR:08X}: 0x{PATCHCODE1_VAL:08X}")
        }
        Err(e) => {
            process::resume_threads(threads);
            bail!("PatchCode_Point1 寫入失敗: {e}");
        }
    }

    // 驗證修補結果
    let v1 = memory::read_u32(h, DECRYPT_ADDR)?;
    let v2 = memory::read_u32(h, PATCHCODE1_ADDR)?;
    if v1 != CONDITIONAL_PATCH_VAL || v2 != PATCHCODE1_VAL {
        process::resume_threads(threads);
        bail!(
            "修補驗證失敗: 0x{DECRYPT_ADDR:08X}=0x{v1:08X}(預期 0x{CONDITIONAL_PATCH_VAL:08X}), \
             0x{PATCHCODE1_ADDR:08X}=0x{v2:08X}(預期 0x{PATCHCODE1_VAL:08X})"
        );
    }

    process::resume_threads(threads);
    log_line!("[OK] 修補完成，遊戲執行緒已恢復");

    Ok(())
}

/// 修補 AC（反外掛）偵測 — 繞過 CRC 校驗結果檢查
///
/// 遊戲客戶端有內建的記憶體完整性檢查：
/// 1. CRC 函數（0x4A33B0）遍歷所有精靈的 action table + frame_data，計算 hash
/// 2. AC 函數比較 hash 與初始化時的儲存值
/// 3. 不匹配 → MessageBox("ERROR") + ExitProcess
///
/// 有兩個獨立的 AC 檢查：
/// - 檢查 1：CRC 比較 → jz 跳過（動態 hash vs 全域變數）
/// - 檢查 2：固定 hash 比較 → jz 跳過（hash vs 硬編碼常數 0x5967）
///
/// 修補方式：將 JZ（匹配時跳過偵測）改為 JMP（永遠跳過），單字節修改
pub fn patch_ac_check(h: HANDLE) -> Result<()> {
    log_line!("\n--- AC 偵測繞過 ---");

    // === AC 檢查 1：CRC 比較 ===
    // 原始碼模式：
    //   mov [ebp-4], eax        ; 89 45 FC
    //   mov eax, [ebp-4]        ; 8B 45 FC
    //   cmp eax, [stored_crc]   ; 3B 05 ?? ?? ?? ??
    //   jz  skip_detection      ; 74 3C          ← 改為 EB 3C (jmp)
    //   cmp [gameState], 3      ; 83 3D ?? ?? ?? ?? 03
    //   jne skip_detection      ; 75 33
    let pattern1: Vec<Option<u8>> = vec![
        Some(0x89),
        Some(0x45),
        Some(0xFC),
        Some(0x8B),
        Some(0x45),
        Some(0xFC),
        Some(0x3B),
        Some(0x05),
        None,
        None,
        None,
        None,
        Some(0x74),
        Some(0x3C),
        Some(0x83),
        Some(0x3D),
        None,
        None,
        None,
        None,
        Some(0x03),
        Some(0x75),
        Some(0x33),
    ];

    match memory::scan_pattern(h, 0x401000, 0x830000, &pattern1)? {
        Some(addr) => {
            let jz_addr = addr + 12;
            memory::write_code(h, jz_addr, &[0xEB])?;
            log_line!("[OK] AC 檢查 1（CRC 比較）已繞過 @ 0x{jz_addr:08X}");
        }
        None => {
            log_line!("[警告] 找不到 AC 檢查 1 模式");
        }
    }

    // === AC 檢查 2：固定 hash 比較 ===
    // 原始碼模式：
    //   call hash_func          ; E8 ?? ?? ?? ??
    //   add esp, 8              ; 83 C4 08
    //   cmp eax, 0x5967         ; 3D 67 59 00 00
    //   jz  skip_detection      ; 74 2A          ← 改為 EB 2A (jmp)
    let pattern2: Vec<Option<u8>> = vec![
        Some(0x83),
        Some(0xC4),
        Some(0x08),
        Some(0x3D),
        Some(0x67),
        Some(0x59),
        Some(0x00),
        Some(0x00),
        Some(0x74),
        Some(0x2A),
    ];

    match memory::scan_pattern(h, 0x401000, 0x830000, &pattern2)? {
        Some(addr) => {
            let jz_addr = addr + 8;
            memory::write_code(h, jz_addr, &[0xEB])?;
            log_line!("[OK] AC 檢查 2（固定 hash）已繞過 @ 0x{jz_addr:08X}");
        }
        None => {
            log_line!("[警告] 找不到 AC 檢查 2 模式");
        }
    }

    Ok(())
}

/// 修補 MSVCR90.dll 的 _invoke_watson，防止 CRT 無效參數崩潰
///
/// 遊戲使用 VC++ 2008 CRT，某些函數收到無效參數時會呼叫 _invoke_watson
/// 直接終止進程（exit code 0xC0000417）。
/// 修補方式：將 _invoke_watson 替換為 `ret`（__cdecl，呼叫者清理堆疊）
pub fn patch_crt_watson(h: HANDLE, pid: u32) -> Result<()> {
    log_line!("\n--- CRT 無效參數修補 ---");

    // 等待 MSVCR90.dll 載入
    let mut crt_base = None;
    for i in 0..100 {
        match process::find_module(pid, "msvcr90.dll")? {
            Some(base) => {
                crt_base = Some(base);
                break;
            }
            None => {
                if i == 0 {
                    log_line!("[等待] MSVCR90.dll...");
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    let crt_base = match crt_base {
        Some(b) => b,
        None => {
            log_line!("[警告] MSVCR90.dll 未載入，跳過 CRT 修補");
            return Ok(());
        }
    };
    log_line!("[OK] MSVCR90.dll 基址: 0x{crt_base:08X}");

    // 找 _invoke_watson 匯出函數
    let watson_addr = match process::find_export(h, crt_base, "_invoke_watson")? {
        Some(addr) => addr,
        None => {
            log_line!("[警告] 找不到 _invoke_watson，跳過");
            return Ok(());
        }
    };
    log_line!("[OK] _invoke_watson 地址: 0x{watson_addr:08X}");

    // 讀取原始指令確認
    let orig = memory::read_bytes(h, watson_addr, 8)?;
    log_line!(
        "[INFO] 原始碼: {}",
        orig.iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    );

    if orig[0] == 0xC3 {
        log_line!("[跳過] _invoke_watson 已被修補");
        return Ok(());
    }

    // 寫入 ret（0xC3）— __cdecl 呼叫者清理堆疊
    memory::write_code(h, watson_addr, &[0xC3])?;
    log_line!("[OK] _invoke_watson → ret（CRT 無效參數不再終止進程）");

    Ok(())
}

/// img 圖檔讀取上限突破
///
/// 遊戲 Surf 資源系統有兩層限制：
/// 1. 資源範圍上限 7000 — push 7000; call alloc_range
/// 2. Surf 陣列邊界 6295 — array[id] 的硬編碼邊界 + malloc 大小
///
/// 注意：push 7000/8000 用於 IE FEATURE_BROWSER_EMULATION 的不可動。
pub fn patch_img_limit(h: HANDLE, new_limit: u32) -> Result<()> {
    let new_limit = new_limit.clamp(MIN_IMG_LIMIT_VALUE, MAX_IMG_LIMIT_VALUE);
    let new_bytes = new_limit.to_le_bytes();
    let new_alloc = (new_limit * 4).to_le_bytes(); // 陣列大小 = limit * 4
    let mut count = 0;

    const OLD_LIMIT: u32 = 6295; // 0x1897
    const OLD_ALLOC: u32 = 6295 * 4; // 25180 = 0x625C
    let old_limit_bytes = OLD_LIMIT.to_le_bytes();
    let old_alloc_bytes = OLD_ALLOC.to_le_bytes();

    // ── 第一層: 資源範圍 push 7000 → push 50000 ──
    // AOB: 6A 00 68 ?? ?? ?? ?? 68 58 1B 00 00 E8
    let pat_range: Vec<Option<u8>> = vec![
        Some(0x6A),
        Some(0x00),
        Some(0x68),
        None,
        None,
        None,
        None,
        Some(0x68),
        Some(0x58),
        Some(0x1B),
        Some(0x00),
        Some(0x00),
        Some(0xE8),
    ];
    let hits_range = memory::scan_pattern_all(h, 0x00401000, 0x00800000, &pat_range)?;
    for &hit in &hits_range {
        memory::write_code(h, hit + 8, &new_bytes)?;
        log_line!(
            "[ImgLimit] 資源範圍 @ 0x{:08X}: push 7000 → {}",
            hit + 7,
            new_limit
        );
        count += 1;
    }

    // ── 第二層: Surf 陣列邊界 6295 → 50000 ──
    // 掃描所有 dword 6295 (0x00001897)，檢查前導 byte 判斷是否為 cmp 指令
    let scan_start: u32 = 0x00401000;
    let scan_end: u32 = 0x00800000;
    let pat_6295: Vec<Option<u8>> = old_limit_bytes.iter().map(|&b| Some(b)).collect();
    let hits_6295 = memory::scan_pattern_all(h, scan_start, scan_end, &pat_6295)?;
    for &hit in &hits_6295 {
        // 讀取前面 6 bytes 判斷指令類型
        // 指令格式:
        //   81 7D/79/7B/7E/7F XX [imm32]  = cmp [reg+disp8], imm32  → 81 在 hit-3
        //   81 BD XX XX XX XX [imm32]      = cmp [ebp+disp32], imm32 → 81 在 hit-6
        let prefix = memory::read_bytes(h, hit.saturating_sub(6), 6)?;
        let plen = prefix.len();
        let is_cmp = if plen >= 3 {
            // 81 XX YY [imm32]: 81 在 hit-3, ModRM 在 hit-2, disp8 在 hit-1
            let opc = prefix[plen - 3]; // hit-3
            let modrm = prefix[plen - 2]; // hit-2
            (opc == 0x81 && matches!(modrm, 0x79 | 0x7B | 0x7D | 0x7E | 0x7F))
            // 81 BD [disp32] [imm32]: 81 在 hit-6, BD 在 hit-5
            || (plen >= 6 && prefix[plen - 6] == 0x81 && prefix[plen - 5] == 0xBD)
        } else {
            false
        };

        if is_cmp {
            memory::write_code(h, hit, &new_bytes)?;
            log_line!(
                "[ImgLimit] 邊界檢查 @ 0x{:08X}: cmp 6295 → {}",
                hit,
                new_limit
            );
            count += 1;
        }
    }

    // ── 陣列分配: push 25180 (6295*4) → push 200000 (50000*4) ──
    // AOB: 68 5C 62 00 00
    let pat_alloc: Vec<Option<u8>> = vec![
        Some(0x68),
        Some(old_alloc_bytes[0]),
        Some(old_alloc_bytes[1]),
        Some(old_alloc_bytes[2]),
        Some(old_alloc_bytes[3]),
    ];
    let hits_alloc = memory::scan_pattern_all(h, scan_start, scan_end, &pat_alloc)?;
    for &hit in &hits_alloc {
        memory::write_code(h, hit + 1, &new_alloc)?;
        log_line!(
            "[ImgLimit] 陣列分配 @ 0x{:08X}: push {} → {}",
            hit,
            OLD_ALLOC,
            new_limit * 4
        );
        count += 1;
    }

    if count == 0 {
        log_line!("[ImgLimit] 警告：未找到任何 img 限制位置");
    } else {
        log_line!(
            "[OK] img 圖檔上限突破: {} 處修補（目標 {}）",
            count,
            new_limit
        );
    }
    Ok(())
}

/// PNG 圖檔上限突破 — 擴大 PngSurfManager 預配陣列
///
/// 遊戲啟動時 SurfManager::Init @ 0x0075A9B0 會做一次性預配:
///   1. malloc 0x1870 bytes (= 1564 * 4) 當指標陣列
///   2. for (i=0; i < 0x61C; i++) array[i] = new PngSurf(i)
///   3. 卸載時 for (i=0; i < 0x61C; i++) delete array[i]  // cleanup loop
///
/// 1564 個 slot 對「使用者後續會大量加 PNG」太少。把 3 個常數同步擴大:
///   • push 0x1870  (array malloc 6256 bytes)         → push (limit*4)
///   • cmp [ebp-0x10], 0x61C  (init loop 上限)         → cmp ..., limit
///   • cmp [ebp-4],    0x61C  (cleanup loop 上限)      → cmp ..., limit
///
/// 因為 init loop 在遊戲 startup 早期跑(CRT _initterm),patch 必須在
/// ResumeThread / 解密門檻通過之後、init loop 執行之前下,跟 ConditionalPatch 同窗口。
/// DirectDraw 離屏 surface 像素格式 patch — 解打字閃爍 + 輸入法顯示異常
///
/// 背景：surface 建立函式 0x00448310 只填 DDSD_CAPS|HEIGHT|WIDTH（dwFlags=0x07），
/// 未指定 DDSD_PIXELFORMAT。XP/Win7 真 DDraw 下「不指定 = 跟 primary 一致」剛好對上
/// 遊戲軟體 blitter 寫死的 16bpp；但 Win11 原生 ddraw 模擬層下，未指定格式的
/// system-memory surface 實際 layout 由模擬層決定，不保證等於遊戲假設的格式 →
/// 軟體繪入的文字/IME 區域顯示異常 + 閃爍。
///
/// 修法（對齊朋友 ALW 的 sub_4101E0：離屏 surface 強制 explicit 16bpp）+ 可調診斷:
///   ⚠ 原生 Win11 ddraw（無 dgVoodoo2 包裝）對「explicit 16bpp system-memory surface vs
///   32bit primary」很挑 — CreateSurface 或之後 primary->Blt(present @ vtable+0x14) 可能失敗
///   → 全黑。故三個變數全部用 env 控制,方便定位本機可跑的組合:
///     LOGIN38_DISABLE_SURFACE_PF=1（或 disable_surface_pf.flag）→ 整個跳過（baseline 未 patch,
///       畫面花但可見）
///     LOGIN38_SURFACE_PF_FORMAT = auto(預設,cave 讀 selector 0x9A235C 動態選) | 555 | 565
///     LOGIN38_SURFACE_PF_PIN    = none(預設) | 555(釘 selector=0) | 565(釘 selector=1)
///   套用點:
///   1. dwFlags 0x07 → 0x1007（加 DDSD_PIXELFORMAT）@ 0x0044833C
///   2. detour 0x00448340 → cave 填 desc.ddpfPixelFormat（[ebp-0x40] 起,16bpp,遮罩依 FORMAT）
///   3. （可選）pin selector [0x9A235C]：0x00448D35 / 0x00448D52 立即數寫 0(555) 或 1(565)
///
/// 遊戲原生美術 = 555（Ghidra 0x0043FB10 證實 555→565 只是轉換層）→ ALW 用 555;但本機能否
/// 用 explicit 16bpp 取決於 ddraw 層,故先用 env 量測。失敗只回 Err 由 caller log warning。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SurfacePfFormat {
    Auto,
    Rgb555,
    Rgb565,
}

fn surface_pf_marker(name: &str) -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(name).exists()))
        .unwrap_or(false)
}

fn surface_pf_disabled() -> bool {
    let env_off = std::env::var("LOGIN38_DISABLE_SURFACE_PF")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false);
    env_off || surface_pf_marker("disable_surface_pf.flag")
}

fn surface_pf_format_from_env() -> SurfacePfFormat {
    match std::env::var("LOGIN38_SURFACE_PF_FORMAT")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("555") | Some("rgb555") => SurfacePfFormat::Rgb555,
        Some("565") | Some("rgb565") => SurfacePfFormat::Rgb565,
        Some("auto") => SurfacePfFormat::Auto,
        // 預設 Auto（讀 selector,跟著 blitter）。555 在原生 Win11 會全黑（present Blt
        // 16bpp→32bit primary 失敗），須配 ALW 假主表面拷貝層才可用,故預設不強制 555。
        _ => SurfacePfFormat::Auto,
    }
}

/// None = 不釘 selector;Some(0) = 釘 555;Some(1) = 釘 565
fn surface_pf_pin_from_env() -> Option<u8> {
    match std::env::var("LOGIN38_SURFACE_PF_PIN")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("555") | Some("0") => Some(0),
        Some("565") | Some("1") => Some(1),
        Some("none") | Some("off") => None,
        // 預設不釘 selector（None）。強制 555 才需要,而 555 預設已關（見上）。
        _ => None,
    }
}
pub fn patch_surface_pixel_format(h: HANDLE, pid: u32) -> Result<()> {
    log_line!("\n--- DirectDraw surface pixel format patch（解打字閃爍 + IME）---");

    if surface_pf_disabled() {
        log_line!("[surface-pf] DISABLED by env/marker（baseline 未 patch 模式），跳過");
        return Ok(());
    }
    let fmt = surface_pf_format_from_env();
    let pin = surface_pf_pin_from_env();
    log_line!("[surface-pf] config: format={fmt:?}, selector_pin={pin:?}");

    // dwFlags imm32 位於 0x0044833C（指令 C7 85 7C FF FF FF <imm32> @ 0x00448336）
    const FLAGS_IMM_ADDR: u32 = 0x0044833C;
    // mov [ebp-0x20], 0x840（建表面 caps）— 被 detour 覆蓋的 7 bytes
    const DETOUR_ADDR: u32 = 0x00448340;
    // detour 之後接回的指令位址
    const JMP_BACK: u32 = 0x00448347;
    // selector pin:把 565 / 32bit-fallback 兩分支的 `mov byte[0x9A235C],1` 立即數釘成 0(555)
    // 立即數位元組位址 = 指令起點 +6（指令 C6 05 5C 23 9A 00 <imm8>）
    const SELECTOR_PIN_ADDRS: [u32; 2] = [0x00448D35, 0x00448D52];

    const FLAGS_ORIG: [u8; 4] = [0x07, 0x00, 0x00, 0x00];
    const DETOUR_ORIG: [u8; 7] = [0xC7, 0x45, 0xE0, 0x40, 0x08, 0x00, 0x00];

    // --- 寫前驗證：已 patch / 位元組不符都安全退出 ---
    let flags_now = memory::read_bytes(h, FLAGS_IMM_ADDR, 4)?;
    if flags_now.len() == 4 && flags_now[0] == 0x07 && flags_now[1] == 0x10 {
        log_line!("[surface-pf] 已 patch（dwFlags=0x1007），跳過");
        return Ok(());
    }
    if flags_now[..] != FLAGS_ORIG {
        bail!("dwFlags @ 0x{FLAGS_IMM_ADDR:08X} 位元組不符: {flags_now:02X?}（預期 07 00 00 00），跳過");
    }
    let detour_now = memory::read_bytes(h, DETOUR_ADDR, 7)?;
    if detour_now[..] != DETOUR_ORIG {
        bail!("detour 點 @ 0x{DETOUR_ADDR:08X} 位元組不符: {detour_now:02X?}（預期 C7 45 E0 40 08 00 00），跳過");
    }

    // --- 分配 cave + 寫 shellcode（獨立記憶體，先寫好再裝 detour）---
    let cave = memory::alloc_exec(h, 128)?;
    let shellcode = build_surface_pf_shellcode(cave, JMP_BACK, fmt);
    memory::write_code(h, cave, &shellcode)?;
    log_line!(
        "[surface-pf] codecave @ 0x{cave:08X}（shellcode {} bytes, format={fmt:?}）",
        shellcode.len()
    );

    // --- detour：E9 rel32 + 2×NOP，正好覆蓋 7 bytes ---
    let mut detour = [0x90u8; 7];
    detour[0] = 0xE9;
    let rel = cave.wrapping_sub(DETOUR_ADDR + 5) as i32;
    detour[1..5].copy_from_slice(&rel.to_le_bytes());

    // --- 暫停 → 寫 detour + flag → 恢復（原子，避免執行緒正好跑在被改的指令上）---
    let threads = process::suspend_threads(pid)?;
    let write_result = (|| -> Result<()> {
        memory::write_code(h, DETOUR_ADDR, &detour)?;
        memory::write_code(h, FLAGS_IMM_ADDR, &[0x07, 0x10, 0x00, 0x00])?;
        Ok(())
    })();
    process::resume_threads(threads);
    write_result.context("寫入 surface pixel format detour/flag 失敗")?;

    // --- 可選 selector pin（best-effort,單點不符只 log 不中斷）---
    let mut pinned = 0;
    if let Some(sel_val) = pin {
        for addr in SELECTOR_PIN_ADDRS {
            match memory::read_bytes(h, addr, 1) {
                Ok(b) if matches!(b.first(), Some(&0x00) | Some(&0x01)) => {
                    if let Err(e) = memory::write_code(h, addr, &[sel_val]) {
                        log_line!("[surface-pf] WARN selector pin @ 0x{addr:08X} 寫入失敗: {e:#}");
                    } else {
                        pinned += 1;
                    }
                }
                Ok(b) => log_line!(
                    "[surface-pf] WARN selector @ 0x{addr:08X} 位元組不符: {b:02X?}（預期 01/00），跳過"
                ),
                Err(e) => log_line!("[surface-pf] WARN selector @ 0x{addr:08X} 讀取失敗: {e:#}"),
            }
        }
    }

    log_line!(
        "[surface-pf] OK — dwFlags→0x1007, format={fmt:?}, selector_pin={pin:?}（{pinned}/2 寫入）"
    );
    Ok(())
}

/// 組裝 cave shellcode：補回被 detour 覆蓋的指令 + 填 DDPIXELFORMAT。
///
/// 進入時 ebp 為 0x00448310 的 frame，desc 在 [ebp-0x88]，
/// desc.ddpfPixelFormat（offset +0x48）落在 [ebp-0x40] 起。
/// fmt:Rgb555/Rgb565 寫死遮罩;Auto 讀 selector [0x9A235C] 動態選（surface 跟著 blitter）。
fn build_surface_pf_shellcode(cave: u32, jmp_back: u32, fmt: SurfacePfFormat) -> Vec<u8> {
    // mov [ebp-disp], imm32 各 7 bytes
    const R555: [u8; 7] = [0xC7, 0x45, 0xD0, 0x00, 0x7C, 0x00, 0x00]; // pf.dwRBitMask = 0x7C00
    const G555: [u8; 7] = [0xC7, 0x45, 0xD4, 0xE0, 0x03, 0x00, 0x00]; // pf.dwGBitMask = 0x03E0
    const R565: [u8; 7] = [0xC7, 0x45, 0xD0, 0x00, 0xF8, 0x00, 0x00]; // pf.dwRBitMask = 0xF800
    const G565: [u8; 7] = [0xC7, 0x45, 0xD4, 0xE0, 0x07, 0x00, 0x00]; // pf.dwGBitMask = 0x07E0

    let mut sc: Vec<u8> = Vec::with_capacity(96);

    // 共用欄位（順序與原 WIP 一致,保證 Auto 分支 jz/jmp 偏移不變）
    sc.extend_from_slice(&[0xC7, 0x45, 0xE0, 0x40, 0x08, 0x00, 0x00]); // [ebp-0x20]=0x840 caps（補回）
    sc.extend_from_slice(&[0xC7, 0x45, 0xC0, 0x20, 0x00, 0x00, 0x00]); // pf.dwSize = 0x20
    sc.extend_from_slice(&[0xC7, 0x45, 0xC4, 0x40, 0x00, 0x00, 0x00]); // pf.dwFlags = DDPF_RGB
    sc.extend_from_slice(&[0xC7, 0x45, 0xCC, 0x10, 0x00, 0x00, 0x00]); // pf.dwRGBBitCount = 16
    sc.extend_from_slice(&[0xC7, 0x45, 0xD8, 0x1F, 0x00, 0x00, 0x00]); // pf.dwBBitMask = 0x1F（555/565 共用）

    match fmt {
        SurfacePfFormat::Rgb555 => {
            sc.extend_from_slice(&R555);
            sc.extend_from_slice(&G555);
        }
        SurfacePfFormat::Rgb565 => {
            sc.extend_from_slice(&R565);
            sc.extend_from_slice(&G565);
        }
        SurfacePfFormat::Auto => {
            // mov al, [0x9A235C]; test al,al; jz +0x10 → .rgb555
            sc.push(0xA0);
            sc.extend_from_slice(&0x009A_235Cu32.to_le_bytes());
            sc.extend_from_slice(&[0x84, 0xC0, 0x74, 0x10]);
            // .rgb565
            sc.extend_from_slice(&R565);
            sc.extend_from_slice(&G565);
            sc.extend_from_slice(&[0xEB, 0x0E]); // jmp +0x0E → .done
                                                 // .rgb555
            sc.extend_from_slice(&R555);
            sc.extend_from_slice(&G555);
        }
    }

    // jmp jmp_back
    sc.push(0xE9);
    let rel = jmp_back.wrapping_sub(cave + sc.len() as u32 + 4) as i32;
    sc.extend_from_slice(&rel.to_le_bytes());

    sc
}

/// 輸入框（LUnicodeEdit）背景修補 — 接管離屏背景擷取，改從真實螢幕擷取框後方畫面
///
/// 根因（Ghidra + live 反組譯雙重驗證）：
/// LUnicodeEdit 開框時呼叫「背景擷取函式」(live FUN_0059f5d0) 建立一張框大小的離屏點陣圖
/// (this+0x2F4)，再把「框正後方的遊戲畫面」BitBlt 進去當背景，paint 時整張拉伸鋪滿、文字透明畫上。
/// 擷取來源是 `graphics->vtable[0x44]` 取得的 DDraw 螢幕 surface DC，源座標 = 視埠偏移
/// [0xABFA60/64] + 框座標，且只在模式閘門 [0x9AB5E8]==3 時才 BitBlt。
/// 全螢幕時來源 DC 有效 → 框顯示正常真實背景；但 Win11 原生 DDraw-on-D3D9 視窗化下，
/// 對該 surface GetDC 拿不到已渲染畫面（emulation 給空/錯位 buffer）→ 黑底或顯示遊戲別處。
///
/// 解法（從遊戲端，讓真實背景正常顯示）：
/// 在離屏圖建好、SelectObject(memDC,bmp) 之後那一刀（live `cmp [gate],3; jne; push SRCCOPY`，
/// AOB `83 3D ?? ?? ?? ?? 03 75 ?? 68 20 00 CC 00`）下 codecave detour，
/// 用 GetWindowRect 取框「真實螢幕座標」（不靠全螢幕導向的視埠偏移）→ GetDC(NULL) 取真實螢幕 DC →
/// BitBlt 把框後方「已由 DWM 合成的真實遊戲畫面」拷進 memDC → ReleaseDC → jmp 函式 tail。
/// 離屏圖拿到正確真實背景，paint 照原樣輸出 → 視窗化下框背景正常。一處 codecave、開框時跑一次。
///
/// 注意：執行中為 LC111 build，0x59xxxx 區比靜態 bin 位移 +0x390，故用 runtime AOB 定位，
/// 並斷言唯一匹配；位元組不符或多匹配都安全 bail，不中斷啟動鏈。
pub fn patch_input_box_offscreen(h: HANDLE, pid: u32) -> Result<()> {
    log_line!("\n--- 輸入框背景 patch（接管離屏擷取，視窗化顯示真實背景）---");

    // --- runtime AOB 定位擷取函式內的擷取點（cmp [gate],3; jne; push SRCCOPY）---
    // 唯一屬於背景擷取函式（paint 函式的對應點用 movzx/test，不會誤中）
    let pat: [Option<u8>; 14] = [
        Some(0x83),
        Some(0x3D),
        None,
        None,
        None,
        None,
        Some(0x03), // cmp dword [gate],3
        Some(0x75),
        None, // jne disp8
        Some(0x68),
        Some(0x20),
        Some(0x00),
        Some(0xCC),
        Some(0x00), // push 0x00CC0020 (SRCCOPY)
    ];
    let hits = memory::scan_pattern_all(h, 0x00590000, 0x005B0000, &pat)?;
    let site = match hits.as_slice() {
        [one] => *one,
        [] => bail!("找不到輸入框擷取點 AOB（83 3D ?? ?? ?? ?? 03 75 ?? 68 20 00 CC 00），跳過"),
        many => bail!("輸入框擷取點 AOB 多匹配 {many:02X?}（預期唯一），保守跳過"),
    };

    // --- 冪等：detour 已存在則跳過 ---
    let now = memory::read_bytes(h, site, 7)?;
    if now.first() == Some(&0xE9) {
        log_line!("[input-box] 已 patch（detour @ 0x{site:08X}），跳過");
        return Ok(());
    }
    if now.len() != 7 || now[0] != 0x83 || now[1] != 0x3D || now[6] != 0x03 {
        bail!("擷取點 @ 0x{site:08X} 位元組不符: {now:02X?}（預期 83 3D .. 03），跳過");
    }

    // tail = jne 的目標（原本 gate!=3 時跳去的清理段）= site + 7(cmp) + 2(jne) + disp8
    let disp8 = memory::read_bytes(h, site + 8, 1)?[0] as i8 as i32;
    let tail = (site as i64 + 9 + disp8 as i64) as u32;
    log_line!("[input-box] 擷取點 @ 0x{site:08X}, tail @ 0x{tail:08X}");

    // --- 解析 GDI/USER32 API 絕對位址（系統 DLL 同 base，launcher 端取得即遊戲端可用）---
    let api = InputBoxApis::resolve()?;
    log_line!(
        "[input-box] API: GetWindowRect=0x{:08X} GetDC=0x{:08X} BitBlt=0x{:08X} ReleaseDC=0x{:08X}",
        api.get_window_rect,
        api.get_dc,
        api.bit_blt,
        api.release_dc
    );

    // --- 分配 cave + 寫 shellcode（先寫好再裝 detour）---
    let cave = memory::alloc_exec(h, 256)?;
    let sc = build_input_box_shellcode(cave, tail, &api);
    memory::write_code(h, cave, &sc)?;
    log_line!(
        "[input-box] codecave @ 0x{cave:08X}（shellcode {} bytes）",
        sc.len()
    );

    // --- detour：E9 rel32 + 2×NOP，正好覆蓋 7 bytes 的 cmp ---
    let mut detour = [0x90u8; 7];
    detour[0] = 0xE9;
    let rel = cave.wrapping_sub(site + 5) as i32;
    detour[1..5].copy_from_slice(&rel.to_le_bytes());

    // 暫停 → 寫 detour → 恢復（原子，避免執行緒正好跑在被改指令上）
    let threads = process::suspend_threads(pid)?;
    let r = memory::write_code(h, site, &detour);
    process::resume_threads(threads);
    r.context("寫入 input-box detour 失敗")?;

    log_line!("[input-box] OK — 離屏背景改為真實螢幕擷取 @ 0x{site:08X}（cave 0x{cave:08X}）");
    Ok(())
}

/// 輸入框背景 codecave 需要的 4 個 API 絕對位址
struct InputBoxApis {
    get_window_rect: u32,
    get_dc: u32,
    bit_blt: u32,
    release_dc: u32,
}

impl InputBoxApis {
    fn resolve() -> Result<Self> {
        Ok(Self {
            get_window_rect: resolve_api_addr("user32.dll", b"GetWindowRect\0")?,
            get_dc: resolve_api_addr("user32.dll", b"GetDC\0")?,
            release_dc: resolve_api_addr("user32.dll", b"ReleaseDC\0")?,
            bit_blt: resolve_api_addr("gdi32.dll", b"BitBlt\0")?,
        })
    }
}

/// 取得系統 DLL 匯出函式的絕對位址（launcher 進程 = 遊戲進程，系統 DLL 同 base）
fn resolve_api_addr(dll: &str, name: &[u8]) -> Result<u32> {
    use windows::core::PCSTR;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress, LoadLibraryA};
    let dll_c = std::ffi::CString::new(dll).context("dll 名稱含 NUL")?;
    unsafe {
        let module = match GetModuleHandleA(PCSTR(dll_c.as_ptr() as *const u8)) {
            Ok(m) => m,
            Err(_) => LoadLibraryA(PCSTR(dll_c.as_ptr() as *const u8))
                .with_context(|| format!("LoadLibraryA({dll}) 失敗"))?,
        };
        let proc = GetProcAddress(module, PCSTR(name.as_ptr())).ok_or_else(|| {
            anyhow::anyhow!(
                "GetProcAddress({dll}!{:?}) 回傳 NULL",
                String::from_utf8_lossy(name)
            )
        })?;
        Ok(proc as usize as u32)
    }
}

/// 組裝輸入框背景 codecave shellcode。
///
/// 進入時 ebp 為擷取函式 frame：[ebp-0x10]=this, [ebp-4]=memDC（已 SelectObject 離屏 bmp）。
/// this+0x2D8=hwnd, 框大小 W=[this+0x94]-[this+0x8C], H=[this+0x90]-[this+0x88]。
/// 邏輯：GetWindowRect(hwnd,&rc) → GetDC(NULL) → BitBlt(memDC,0,0,W,H, scrDC, rc.left,rc.top, SRCCOPY)
///       → ReleaseDC(NULL,scrDC) → jmp tail。esi/ebx/edi 為 callee-saved，跨 API call 保留。
fn build_input_box_shellcode(cave: u32, tail: u32, api: &InputBoxApis) -> Vec<u8> {
    let rect_addr = cave + 0xC0; // RECT 暫存（16 bytes）
    let scrdc_addr = cave + 0xD0; // screenDC 暫存（4 bytes）
    let mut sc: Vec<u8> = Vec::with_capacity(160);

    sc.push(0x60); // pushad

    // esi = this
    sc.extend_from_slice(&[0x8B, 0x75, 0xF0]); // mov esi,[ebp-0x10]
                                               // ebx = W = [esi+0x94]-[esi+0x8C]
    sc.extend_from_slice(&[0x8B, 0x9E, 0x94, 0x00, 0x00, 0x00]); // mov ebx,[esi+0x94]
    sc.extend_from_slice(&[0x2B, 0x9E, 0x8C, 0x00, 0x00, 0x00]); // sub ebx,[esi+0x8C]
                                                                 // edi = H = [esi+0x90]-[esi+0x88]
    sc.extend_from_slice(&[0x8B, 0xBE, 0x90, 0x00, 0x00, 0x00]); // mov edi,[esi+0x90]
    sc.extend_from_slice(&[0x2B, 0xBE, 0x88, 0x00, 0x00, 0x00]); // sub edi,[esi+0x88]

    // GetWindowRect(hwnd, &rc)
    sc.push(0x68);
    sc.extend_from_slice(&rect_addr.to_le_bytes()); // push &rc
    sc.extend_from_slice(&[0xFF, 0xB6, 0xD8, 0x02, 0x00, 0x00]); // push [esi+0x2D8] (hwnd)
    sc.push(0xB8);
    sc.extend_from_slice(&api.get_window_rect.to_le_bytes());
    sc.extend_from_slice(&[0xFF, 0xD0]); // call eax

    // scrDC = GetDC(NULL)
    sc.extend_from_slice(&[0x6A, 0x00]); // push 0
    sc.push(0xB8);
    sc.extend_from_slice(&api.get_dc.to_le_bytes());
    sc.extend_from_slice(&[0xFF, 0xD0]); // call eax
    sc.push(0xA3);
    sc.extend_from_slice(&scrdc_addr.to_le_bytes()); // mov [scrdc],eax

    // BitBlt(memDC,0,0,W,H, scrDC, rc.left, rc.top, SRCCOPY)
    sc.push(0x68);
    sc.extend_from_slice(&0x00CC_0020u32.to_le_bytes()); // push SRCCOPY
    sc.extend_from_slice(&[0xFF, 0x35]);
    sc.extend_from_slice(&(rect_addr + 4).to_le_bytes()); // push [rc.top]
    sc.extend_from_slice(&[0xFF, 0x35]);
    sc.extend_from_slice(&rect_addr.to_le_bytes()); // push [rc.left]
    sc.extend_from_slice(&[0xFF, 0x35]);
    sc.extend_from_slice(&scrdc_addr.to_le_bytes()); // push [scrDC]
    sc.push(0x57); // push edi (H)
    sc.push(0x53); // push ebx (W)
    sc.extend_from_slice(&[0x6A, 0x00, 0x6A, 0x00]); // push 0; push 0
    sc.extend_from_slice(&[0xFF, 0x75, 0xFC]); // push [ebp-4] (memDC)
    sc.push(0xB8);
    sc.extend_from_slice(&api.bit_blt.to_le_bytes());
    sc.extend_from_slice(&[0xFF, 0xD0]); // call eax

    // ReleaseDC(NULL, scrDC)
    sc.extend_from_slice(&[0xFF, 0x35]);
    sc.extend_from_slice(&scrdc_addr.to_le_bytes()); // push [scrDC]
    sc.extend_from_slice(&[0x6A, 0x00]); // push 0
    sc.push(0xB8);
    sc.extend_from_slice(&api.release_dc.to_le_bytes());
    sc.extend_from_slice(&[0xFF, 0xD0]); // call eax

    sc.push(0x61); // popad

    // jmp tail
    sc.push(0xE9);
    let rel = tail.wrapping_sub(cave + sc.len() as u32 + 4) as i32;
    sc.extend_from_slice(&rel.to_le_bytes());

    sc
}

pub fn patch_png_limit(h: HANDLE, new_limit: u32) -> Result<()> {
    const OLD_LIMIT: u32 = 0x61C; // 1564 — 寫死在 SurfManager::Init 的 cmp 立即數
    const OLD_ALLOC: u32 = 0x1870; // 6256 = 1564 * 4

    let new_limit_bytes = new_limit.to_le_bytes();
    let new_alloc_bytes = (new_limit.saturating_mul(4)).to_le_bytes();
    let old_limit_bytes = OLD_LIMIT.to_le_bytes();
    let old_alloc_bytes = OLD_ALLOC.to_le_bytes();
    let mut count = 0;

    // ── (1) array malloc: push 0x1870 → push (limit*4) ─────────────────
    // AOB 唯一性:在整個 .text 中只會在 SurfManager::Init 出現
    //   68 70 18 00 00          push 0x1870
    //   E8 ?? ?? ?? ??          call malloc
    //   83 C4 04                add esp, 4
    //   89 45 ??                mov [ebp-X], eax  (X=0x14 in observed binary)
    let pat_alloc: Vec<Option<u8>> = vec![
        Some(0x68),
        Some(old_alloc_bytes[0]),
        Some(old_alloc_bytes[1]),
        Some(old_alloc_bytes[2]),
        Some(old_alloc_bytes[3]),
        Some(0xE8),
        None,
        None,
        None,
        None,
        Some(0x83),
        Some(0xC4),
        Some(0x04),
    ];
    match memory::scan_pattern(h, TEXT_SCAN_START, TEXT_SCAN_END, &pat_alloc)? {
        Some(addr) => {
            memory::write_code(h, addr + 1, &new_alloc_bytes)?;
            log_line!(
                "[PngLimit] array malloc @ 0x{:08X}: push {} → push {}",
                addr,
                OLD_ALLOC,
                new_limit.saturating_mul(4)
            );
            count += 1;
        }
        None => {
            log_line!("[PngLimit] 警告: 找不到 array malloc 模式 (push 0x1870 + call malloc)");
        }
    }

    // ── (2) init loop: cmp [ebp-0x10], 0x61C → cmp ..., new_limit ──────
    // AOB:
    //   89 55 F0                mov [ebp-0x10], edx
    //   81 7D F0 1C 06 00 00    cmp [ebp-0x10], 0x61C   ← 立即數在 +6
    //   7D ??                   jge ...
    let pat_init: Vec<Option<u8>> = vec![
        Some(0x89),
        Some(0x55),
        Some(0xF0),
        Some(0x81),
        Some(0x7D),
        Some(0xF0),
        Some(old_limit_bytes[0]),
        Some(old_limit_bytes[1]),
        Some(old_limit_bytes[2]),
        Some(old_limit_bytes[3]),
        Some(0x7D),
    ];
    match memory::scan_pattern(h, TEXT_SCAN_START, TEXT_SCAN_END, &pat_init)? {
        Some(addr) => {
            memory::write_code(h, addr + 6, &new_limit_bytes)?;
            log_line!(
                "[PngLimit] init loop @ 0x{:08X}: cmp 0x{:X} → 0x{:X}",
                addr + 3,
                OLD_LIMIT,
                new_limit
            );
            count += 1;
        }
        None => {
            log_line!("[PngLimit] 警告: 找不到 init loop 模式");
        }
    }

    // ── (3) cleanup loop: cmp [ebp-4], 0x61C → cmp ..., new_limit ──────
    // AOB:
    //   89 4D FC                mov [ebp-4], ecx
    //   81 7D FC 1C 06 00 00    cmp [ebp-4], 0x61C       ← 立即數在 +6
    //   7D ??                   jge ...
    let pat_cleanup: Vec<Option<u8>> = vec![
        Some(0x89),
        Some(0x4D),
        Some(0xFC),
        Some(0x81),
        Some(0x7D),
        Some(0xFC),
        Some(old_limit_bytes[0]),
        Some(old_limit_bytes[1]),
        Some(old_limit_bytes[2]),
        Some(old_limit_bytes[3]),
        Some(0x7D),
    ];
    match memory::scan_pattern(h, TEXT_SCAN_START, TEXT_SCAN_END, &pat_cleanup)? {
        Some(addr) => {
            memory::write_code(h, addr + 6, &new_limit_bytes)?;
            log_line!(
                "[PngLimit] cleanup loop @ 0x{:08X}: cmp 0x{:X} → 0x{:X}",
                addr + 3,
                OLD_LIMIT,
                new_limit
            );
            count += 1;
        }
        None => {
            log_line!("[PngLimit] 警告: 找不到 cleanup loop 模式");
        }
    }

    if count == 0 {
        log_line!("[PngLimit] 警告：未套用任何 PNG 上限 patch (3/3 模式都失敗)");
    } else if count < 3 {
        log_line!(
            "[PngLimit] 部分套用: {}/3 (其餘已套用過或 pattern 找不到)",
            count
        );
    } else {
        log_line!(
            "[OK] PNG 圖檔上限突破: 3/3 處修補,新上限 {} (記憶體成本約 {} KB)",
            new_limit,
            (new_limit.saturating_mul(36)) / 1024
        );
    }
    Ok(())
}

/// 背包物品上限顯示：180 → 255
///
/// 背包底部顯示 "173 / 180"，其中 180 是寫死在格式字串 `"%d / 180"` 中的 ASCII 文字。
/// 修改為 `"%d / 255"` 讓顯示正確反映伺服器端的 255 上限。
pub fn patch_inventory_limit(h: HANDLE, new_limit: u32) -> Result<()> {
    let new_limit = new_limit.clamp(MIN_INVENTORY_LIMIT_VALUE, MAX_INVENTORY_LIMIT_VALUE);
    // 格式字串 "%d / 180\0" → "%d / 255\0"（靜默修補）
    let pat: Vec<Option<u8>> = vec![
        Some(0x25),
        Some(0x64),
        Some(0x20),
        Some(0x2F),
        Some(0x20),
        Some(0x31),
        Some(0x38),
        Some(0x30),
        Some(0x00),
    ];
    if let Some(addr) = memory::scan_pattern(h, 0x800000, 0xA00000, &pat)? {
        let mut bytes = [0u8; 3];
        let digits = new_limit.to_string();
        bytes[..digits.len()].copy_from_slice(digits.as_bytes());
        memory::write_code(h, addr + 5, &bytes)?;
        log_line!("[InventoryLimit] 顯示上限 180 → {new_limit}");
    }
    Ok(())
}

pub fn patch_move_packet_no_encrypt(h: HANDLE) -> Result<()> {
    let mut patched_count = 0;

    for spec in move_packet_no_encrypt_patch_specs() {
        let pattern: Vec<Option<u8>> = spec.pattern.iter().copied().map(Some).collect();
        let base = memory::scan_pattern(h, TEXT_SCAN_START, TEXT_SCAN_END, &pattern)?
            .with_context(|| format!("move packet no-encrypt pattern not found: {}", spec.name))?;
        let patch_addr = base + spec.patch_offset;
        let current = memory::read_bytes(h, patch_addr, 1)?
            .into_iter()
            .next()
            .context("read move packet no-encrypt patch byte failed")?;

        if current == spec.patched {
            log_line!(
                "[MoveNoEncrypt] {} already patched @ 0x{patch_addr:08X}",
                spec.name
            );
            continue;
        }
        if current != spec.original {
            bail!(
                "move packet no-encrypt target mismatch: {} @ 0x{patch_addr:08X}, current=0x{current:02X}, expected=0x{:02X}",
                spec.name,
                spec.original
            );
        }

        memory::write_code(h, patch_addr, &[spec.patched])?;
        log_line!(
            "[MoveNoEncrypt] {} @ 0x{patch_addr:08X}: {:02X} -> {:02X}",
            spec.name,
            spec.original,
            spec.patched
        );
        patched_count += 1;
    }

    log_line!("[OK] 移動封包不加密 patch 已套用：{} 處", patched_count);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains_seq(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    fn assert_jmp_back(sc: &[u8], cave: u32, jmp_back: u32) {
        let tail = &sc[sc.len() - 5..];
        assert_eq!(tail[0], 0xE9);
        let rel = i32::from_le_bytes(tail[1..5].try_into().unwrap());
        let next_ip = cave.wrapping_add(sc.len() as u32);
        assert_eq!(next_ip.wrapping_add(rel as u32), jmp_back);
    }

    #[test]
    fn surface_pf_shellcode_rgb555_is_hardcoded_no_autodetect() {
        let (cave, jmp_back) = (0x10000000u32, 0x00448347u32);
        let sc = build_surface_pf_shellcode(cave, jmp_back, SurfacePfFormat::Rgb555);

        // 7 個 mov imm32（各 7）+ E9 rel32（5）
        assert_eq!(sc.len(), 7 * 7 + 5);
        assert!(contains_seq(
            &sc,
            &[0xC7, 0x45, 0xE0, 0x40, 0x08, 0x00, 0x00]
        )); // caps 0x840
        assert!(contains_seq(
            &sc,
            &[0xC7, 0x45, 0xCC, 0x10, 0x00, 0x00, 0x00]
        )); // bitcount 16
        assert!(contains_seq(
            &sc,
            &[0xC7, 0x45, 0xD0, 0x00, 0x7C, 0x00, 0x00]
        )); // R 0x7C00 (555)
        assert!(contains_seq(
            &sc,
            &[0xC7, 0x45, 0xD4, 0xE0, 0x03, 0x00, 0x00]
        )); // G 0x03E0 (555)
        assert!(!contains_seq(
            &sc,
            &[0xC7, 0x45, 0xD0, 0x00, 0xF8, 0x00, 0x00]
        )); // 無 565 R
        let mov_block = &sc[..7 * 7];
        assert!(!mov_block.contains(&0xA0)); // 無 mov al,[0x9A235C]
        assert!(!mov_block.contains(&0x74)); // 無 jz 分支
        assert_jmp_back(&sc, cave, jmp_back);
    }

    #[test]
    fn surface_pf_shellcode_rgb565_is_hardcoded() {
        let (cave, jmp_back) = (0x10000000u32, 0x00448347u32);
        let sc = build_surface_pf_shellcode(cave, jmp_back, SurfacePfFormat::Rgb565);

        assert_eq!(sc.len(), 7 * 7 + 5);
        assert!(contains_seq(
            &sc,
            &[0xC7, 0x45, 0xD0, 0x00, 0xF8, 0x00, 0x00]
        )); // R 0xF800 (565)
        assert!(contains_seq(
            &sc,
            &[0xC7, 0x45, 0xD4, 0xE0, 0x07, 0x00, 0x00]
        )); // G 0x07E0 (565)
        assert!(!contains_seq(
            &sc,
            &[0xC7, 0x45, 0xD0, 0x00, 0x7C, 0x00, 0x00]
        )); // 無 555 R
        assert!(!sc[..7 * 7].contains(&0xA0)); // 無 selector 讀取
        assert_jmp_back(&sc, cave, jmp_back);
    }

    #[test]
    fn surface_pf_shellcode_auto_reads_selector_and_has_both_masks() {
        let (cave, jmp_back) = (0x10000000u32, 0x00448347u32);
        let sc = build_surface_pf_shellcode(cave, jmp_back, SurfacePfFormat::Auto);

        // 共用 5 mov(35) + mov al,[imm32](5) + test+jz(4) + 565 R/G(14) + jmp(2) + 555 R/G(14) + E9 rel32(5)
        assert_eq!(sc.len(), 35 + 5 + 4 + 14 + 2 + 14 + 5);
        // 讀 selector:A0 5C 23 9A 00 = mov al,[0x009A235C]
        assert!(contains_seq(&sc, &[0xA0, 0x5C, 0x23, 0x9A, 0x00]));
        // 兩種遮罩都在（動態選）
        assert!(contains_seq(
            &sc,
            &[0xC7, 0x45, 0xD0, 0x00, 0x7C, 0x00, 0x00]
        )); // 555 R
        assert!(contains_seq(
            &sc,
            &[0xC7, 0x45, 0xD0, 0x00, 0xF8, 0x00, 0x00]
        )); // 565 R
        assert_jmp_back(&sc, cave, jmp_back);
    }

    #[test]
    fn move_packet_no_encrypt_patches_two_runtime_branches() {
        let specs = move_packet_no_encrypt_patch_specs();

        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].name, "move state obfuscation");
        assert_eq!(specs[0].patch_offset, 7);
        assert_eq!(specs[0].original, 0x74);
        assert_eq!(specs[0].patched, 0xEB);
        assert_eq!(specs[1].name, "move packet encryption");
        assert_eq!(specs[1].patch_offset, 10);
        assert_eq!(specs[1].original, 0x75);
        assert_eq!(specs[1].patched, 0xEB);
    }

    #[test]
    fn force_simplified_text_locale_targets_codepage_global() {
        assert_eq!(TEXT_LOCALE_CODEPAGE_ADDR, 0x00968618);
        assert_eq!(TEXT_LOCALE_SIMPLIFIED_CODEPAGE, 0x000003A8);
    }

    #[test]
    fn simplified_status_tooltip_encoding_patch_targets_known_branch() {
        assert_eq!(SIMPLIFIED_STATUS_TOOLTIP_ENCODING_ADDR, 0x005126ED);
        assert_eq!(
            SIMPLIFIED_STATUS_TOOLTIP_ENCODING_ORIGINAL,
            &[0x0F, 0x84, 0x4F, 0x01, 0x00, 0x00]
        );
        assert_eq!(SIMPLIFIED_STATUS_TOOLTIP_ENCODING_PATCHED, &[0x90; 6]);
    }
}
