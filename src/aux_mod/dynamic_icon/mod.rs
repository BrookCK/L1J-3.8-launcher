//! 動態道具圖 — 把指定 gfxid 的 item icon 播成 PNG 動畫，播完回原生靜態 TBT。
//!
//! 2026-06-26 實機驗證後改架構（Path C）：
//! - item icon = 原始 TBT 資料（0x560AE0 逐列 RLE 解碼貼 16bpp framebuffer），非 vtable surface。
//! - 唯一 gfxid→resource gate = `FUN_0045b270`（所有面板/快取+懶載入路徑都呼叫它）→ hook 此點 = 全欄位。
//! - launcher 把每張 PNG 幀轉成同格式 TBT-raw 注入遊戲；hook 動畫期回傳幀 buffer，休息期跑原生。
//! - 全域時鐘 GetTickCount → 全畫面同 gfxid 自動同步，免執行緒/計時器。
//!
//! 設計：`docs/superpowers/specs/2026-06-26-dynamic-item-icon-design.md`（v2 修正架構章）

pub mod anim;
pub mod hook;
pub mod pak_register;

use anyhow::{Context, Result};
use std::path::Path;
use windows::Win32::Foundation::HANDLE;

use launcher::dynamic_icon_format::{encode_tbt_raw, AnimEntry, AnimMap};

use crate::platform::memory;

/// 動畫幀 alpha 門檻：< 此值視為透明（不畫）。
const ALPHA_THRESHOLD: u8 = 128;

/// 動態 icon 總安裝：讀 pak → 抽 XML → 每幀 PNG→TBT-raw 注入 → 寫表 → 裝 hook。
///
/// `game_dir`：登入器/遊戲所在目錄（自製 pak 放這）。`pak_basename`：如 "123" → 123.pak/123.idx。
pub fn install(h: HANDLE, game_dir: &Path, pak_basename: &str) -> Result<()> {
    // 1. 讀 pak + idx
    let pak = std::fs::read(game_dir.join(format!("{pak_basename}.pak")))
        .with_context(|| format!("讀 {pak_basename}.pak 失敗"))?;
    let idx = std::fs::read(game_dir.join(format!("{pak_basename}.idx")))
        .with_context(|| format!("讀 {pak_basename}.idx 失敗"))?;

    // 2. 解析 dynamicicons.xml → AnimMap（frames = png id）
    let map = pak_register::extract_anim_map(&pak, &idx).map_err(anyhow::Error::msg)?;
    if map.is_empty() {
        return Ok(()); // 無設定，不裝 hook
    }

    // 3. 每幀 PNG → TBT-raw → 注入遊戲，建 frames = buffer 位址 的新表
    let mut ptr_map = AnimMap::new();
    for (tbt, e) in &map {
        let mut frame_ptrs = Vec::with_capacity(e.frames.len());
        for &png_id in &e.frames {
            let png = pak_register::extract_file(&pak, &idx, &format!("{png_id}.png"))
                .map_err(anyhow::Error::msg)
                .with_context(|| format!("pak 內缺 {png_id}.png"))?;
            let raw =
                png_to_tbt_raw(&png).with_context(|| format!("{png_id}.png 轉 TBT-raw 失敗"))?;
            let addr = memory::alloc_exec(h, raw.len().max(8))?;
            memory::write_code(h, addr, &raw)?;
            frame_ptrs.push(addr);
        }
        ptr_map.insert(
            *tbt,
            AnimEntry {
                tbt: *tbt,
                speed_ms: e.speed_ms,
                interval_ms: e.interval_ms,
                frames: frame_ptrs,
            },
        );
    }

    // 4. 寫表 + 裝 hook
    let (table, count) = hook::write_anim_table(h, &ptr_map)?;
    hook::install_hook(h, table, count)?;
    Ok(())
}

/// PNG bytes → 遊戲 item-icon TBT-raw 格式（透過 lib `encode_tbt_raw`）。
fn png_to_tbt_raw(png_bytes: &[u8]) -> Result<Vec<u8>> {
    let decoder = png::Decoder::new(png_bytes);
    let mut reader = decoder.read_info().context("PNG 解碼")?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).context("PNG 讀幀")?;
    let (w, h) = (info.width, info.height);
    if w == 0 || h == 0 || w > 255 || h > 255 {
        anyhow::bail!("PNG 尺寸 {w}x{h} 越界（需 1..=255，建議 32x32）");
    }
    if info.bit_depth != png::BitDepth::Eight {
        anyhow::bail!("PNG bit depth {:?} 不支援（請輸出 8-bit）", info.bit_depth);
    }
    let data = &buf[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => {
            let mut v = Vec::with_capacity(w as usize * h as usize * 4);
            for px in data.chunks_exact(3) {
                v.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
            v
        }
        other => anyhow::bail!("PNG color type {other:?} 不支援（請輸出 RGBA 或 RGB 8-bit）"),
    };
    encode_tbt_raw(&rgba, w as u16, h as u16, ALPHA_THRESHOLD).map_err(anyhow::Error::msg)
}
