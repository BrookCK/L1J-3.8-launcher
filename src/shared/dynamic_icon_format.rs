//! 動態道具圖 — 共用純格式邏輯（lib：bin hook 與 encoder 共用同一份，確保打包格式 == 讀取格式）。
//!
//! - dynamicicons.xml 解析 / 產生（`parse_dynamic_icons` / `to_xml`）
//! - codecave anim 表序列化（`serialize_anim_table`）
//! - Sprite 式 pak 解析 / 打包（`find_entry` / `build_sprite_pak`，plaintext idx）
//!
//! idx 格式對齊 `memory/sprite_idx_format.md`：4-byte LE count + 28-byte entries
//! `[offset:4][filename:20][size:4]`。pak = 串接 body。

use std::collections::HashMap;

// ───────────────────────── AnimEntry / AnimMap ─────────────────────────

/// 一筆動態 icon 設定：哪個 gfxid 的 TBT 要被 PNG 動畫覆蓋。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimEntry {
    /// 目標道具 icon 編號（= items_table index = item gfxid）。
    pub tbt: u16,
    /// 每幀停留毫秒（動畫速度）。
    pub speed_ms: u16,
    /// 播完一輪後維持靜態 TBT 的毫秒數（循環間隔）。
    pub interval_ms: u32,
    /// 依序的 PNG 幀資源 id，長度 1..=99。
    pub frames: Vec<u32>,
}

impl AnimEntry {
    /// 一個完整循環週期（ms）= 動畫時間 + 休息時間。
    pub fn cycle_ms(&self) -> u32 {
        self.frames.len() as u32 * self.speed_ms as u32 + self.interval_ms
    }
}

pub type AnimMap = HashMap<u16, AnimEntry>;

// ───────────────────────── XML 解析 / 產生 ─────────────────────────

/// 解析 dynamicicons.xml。容錯：忽略未知屬性/標籤、空白；驗證每筆 1..=99 幀。
pub fn parse_dynamic_icons(xml: &str) -> Result<AnimMap, String> {
    let mut map = AnimMap::new();
    let mut i = 0usize;
    while let Some(open) = find_from(xml, i, "<item") {
        let tag_end = find_from(xml, open, ">").ok_or("item 標籤未閉合")?;
        let attrs = &xml[open + 5..tag_end];
        let tbt = attr_u32(attrs, "tbt").ok_or("item 缺 tbt")? as u16;
        let speed_ms = attr_u32(attrs, "speed").ok_or("item 缺 speed")? as u16;
        let interval_ms = attr_u32(attrs, "interval").ok_or("item 缺 interval")?;
        let close = find_from(xml, tag_end, "</item>").ok_or("item 未閉合")?;
        let body = &xml[tag_end + 1..close];
        let mut frames = Vec::new();
        let mut j = 0usize;
        while let Some(p) = find_from(body, j, "<png>") {
            let pe = find_from(body, p, "</png>").ok_or("png 未閉合")?;
            let raw = body[p + 5..pe].trim();
            let id: u32 = raw.parse().map_err(|_| format!("png 非數字: {raw}"))?;
            frames.push(id);
            j = pe + 6;
        }
        if frames.is_empty() || frames.len() > 99 {
            return Err(format!("tbt={tbt} 幀數 {} 越界(需 1..=99)", frames.len()));
        }
        map.insert(
            tbt,
            AnimEntry {
                tbt,
                speed_ms,
                interval_ms,
                frames,
            },
        );
        i = close + 7;
    }
    Ok(map)
}

/// 從 AnimMap 產生 dynamicicons.xml（tbt 遞增序，可與 parse_dynamic_icons round-trip）。
pub fn to_xml(map: &AnimMap) -> String {
    let mut entries: Vec<&AnimEntry> = map.values().collect();
    entries.sort_by_key(|e| e.tbt);
    let mut s = String::from("<dynamicicons>\n");
    for e in &entries {
        s.push_str(&format!(
            "  <item tbt=\"{}\" speed=\"{}\" interval=\"{}\">\n",
            e.tbt, e.speed_ms, e.interval_ms
        ));
        for id in &e.frames {
            s.push_str(&format!("    <png>{id}</png>\n"));
        }
        s.push_str("  </item>\n");
    }
    s.push_str("</dynamicicons>\n");
    s
}

fn find_from(hay: &str, from: usize, needle: &str) -> Option<usize> {
    hay.get(from..)?.find(needle).map(|p| p + from)
}

fn attr_u32(attrs: &str, key: &str) -> Option<u32> {
    let pat = format!("{key}=\"");
    let s = find_from(attrs, 0, &pat)? + pat.len();
    let e = find_from(attrs, s, "\"")?;
    attrs[s..e].trim().parse().ok()
}

// ───────────────────────── codecave anim 表 ─────────────────────────

/// 每筆 anim 記錄的固定 byte 大小（codecave 表）。
/// 佈局：tbt:u16(0), speed:u16(2), interval:u32(4), n_frames:u32(8), frames:[u32;99](12..)
pub const ANIM_REC_SIZE: usize = 2 + 2 + 4 + 4 + 99 * 4; // = 408

/// 序列化 anim_map → (筆數, 連續 bytes)。tbt 遞增排序，與 buf_map / shellcode 同序。
pub fn serialize_anim_table(map: &AnimMap) -> (u32, Vec<u8>) {
    let mut entries: Vec<&AnimEntry> = map.values().collect();
    entries.sort_by_key(|e| e.tbt);
    let mut blob = Vec::with_capacity(entries.len() * ANIM_REC_SIZE);
    for e in &entries {
        blob.extend_from_slice(&e.tbt.to_le_bytes());
        blob.extend_from_slice(&e.speed_ms.to_le_bytes());
        blob.extend_from_slice(&e.interval_ms.to_le_bytes());
        blob.extend_from_slice(&(e.frames.len() as u32).to_le_bytes());
        for slot in 0..99 {
            let id = e.frames.get(slot).copied().unwrap_or(0);
            blob.extend_from_slice(&id.to_le_bytes());
        }
    }
    (entries.len() as u32, blob)
}

// ───────────────────────── Sprite 式 pak 解析 / 打包 ─────────────────────────

const HEADER: usize = 4;
const ENTRY: usize = 28;
const NAME_OFF: usize = 4;
const NAME_LEN: usize = 20;
const SIZE_OFF: usize = 24;

/// 一筆 idx entry。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdxEntry {
    pub offset: u32,
    pub size: u32,
}

/// 在 idx bytes 中找指定檔名的 entry（大小寫不敏感，null-padded ASCII）。
pub fn find_entry(idx: &[u8], filename: &str) -> Option<IdxEntry> {
    if idx.len() < HEADER {
        return None;
    }
    let count = u32::from_le_bytes(idx[0..4].try_into().ok()?) as usize;
    let body = &idx[HEADER..];
    if body.len() < count.checked_mul(ENTRY)? {
        return None;
    }
    let want = filename.to_ascii_lowercase();
    for k in 0..count {
        let e = &body[k * ENTRY..k * ENTRY + ENTRY];
        let raw = &e[NAME_OFF..NAME_OFF + NAME_LEN];
        let nul = raw.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        let name = std::str::from_utf8(&raw[..nul]).ok()?.to_ascii_lowercase();
        if name == want {
            let offset = u32::from_le_bytes(e[0..4].try_into().ok()?);
            let size = u32::from_le_bytes(e[SIZE_OFF..SIZE_OFF + 4].try_into().ok()?);
            return Some(IdxEntry { offset, size });
        }
    }
    None
}

/// 把 (檔名, body) 清單打包成 Sprite 式 (pak, idx) plaintext 二進位。
/// 與 `find_entry` / launcher `register_custom_pak` 讀的格式完全一致。
/// 檔名須 ≤ 20 bytes ASCII。
pub fn build_sprite_pak(files: &[(String, Vec<u8>)]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut pak: Vec<u8> = Vec::new();
    let mut idx: Vec<u8> = Vec::with_capacity(HEADER + files.len() * ENTRY);
    idx.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for (name, body) in files {
        if name.len() > NAME_LEN {
            return Err(format!("檔名 \"{name}\" 超過 {NAME_LEN} bytes"));
        }
        let offset = pak.len() as u32;
        idx.extend_from_slice(&offset.to_le_bytes());
        let mut fname = [0u8; NAME_LEN];
        fname[..name.len()].copy_from_slice(name.as_bytes());
        idx.extend_from_slice(&fname);
        idx.extend_from_slice(&(body.len() as u32).to_le_bytes());
        pak.extend_from_slice(body);
    }
    Ok((pak, idx))
}

// ───────────────────────── TBT-raw icon 編碼（PNG→遊戲 icon 格式）─────────────────────────

/// 把 RGBA8 像素編碼成遊戲 item-icon 的 TBT-raw 格式（0x560AE0 解碼器逆向，2026-06-26 實機驗證）。
///
/// 格式：`[x_off:u8][y_off:u8][width:u8][row_count:u8]` + 每列
/// `[seg_count:u8]`，每段 `[skip:u8(=透明px×2)][len:u8(不透明px)][len×u16 RGB565 LE]`。
/// alpha < `alpha_threshold` 視為透明（不存，以 skip 跳過）；全透明列 seg_count=0。
/// w/h 須 ≤ 255（item icon 一般 32×32）。
pub fn encode_tbt_raw(rgba: &[u8], w: u16, h: u16, alpha_threshold: u8) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 || w > 255 || h > 255 {
        return Err(format!("尺寸越界 {w}x{h}（需 1..=255）"));
    }
    let expect = w as usize * h as usize * 4;
    if rgba.len() != expect {
        return Err(format!("RGBA 長度 {} != {w}x{h}x4={expect}", rgba.len()));
    }
    let w = w as usize;
    // x_off=0, y_off=0, width, row_count
    let mut out = vec![0u8, 0u8, w as u8, h as u8];
    for y in 0..h as usize {
        let mut segs: Vec<u8> = Vec::new();
        let mut seg_count = 0u8;
        let mut x = 0usize;
        while x < w {
            // 透明跑
            let skip_start = x;
            while x < w && rgba[(y * w + x) * 4 + 3] < alpha_threshold {
                x += 1;
            }
            let skip = x - skip_start;
            if x >= w {
                break; // 尾端透明不編碼
            }
            // 不透明跑
            let run_start = x;
            while x < w && rgba[(y * w + x) * 4 + 3] >= alpha_threshold {
                x += 1;
            }
            let run = x - run_start;
            let skip_byte = skip
                .checked_mul(2)
                .filter(|v| *v <= 255)
                .ok_or_else(|| format!("透明跨度 {skip} 太大（skip×2 須 ≤255）"))?;
            if run > 255 {
                return Err(format!("不透明跨度 {run} 太大（須 ≤255）"));
            }
            segs.push(skip_byte as u8);
            segs.push(run as u8);
            for px in run_start..x {
                let i = (y * w + px) * 4;
                let (r, g, b) = (rgba[i] as u16, rgba[i + 1] as u16, rgba[i + 2] as u16);
                let v: u16 = ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3);
                segs.extend_from_slice(&v.to_le_bytes());
            }
            seg_count = seg_count.checked_add(1).ok_or("段數溢位（>255）")?;
        }
        out.push(seg_count);
        out.extend_from_slice(&segs);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 測試用：鏡像 0x560AE0 解碼器，把 TBT-raw 還原成 row×col 的 Option<u16>（None=透明）。
    fn decode_tbt_raw(data: &[u8]) -> (u8, u8, Vec<Vec<Option<u16>>>) {
        let (x_off, y_off, w, rows) = (data[0], data[1], data[2] as usize, data[3] as usize);
        let mut grid = Vec::new();
        let mut i = 4usize;
        for _ in 0..rows {
            let mut row = vec![None; w];
            let seg_count = data[i];
            i += 1;
            let mut col = 0usize;
            for _ in 0..seg_count {
                let skip = (data[i] >> 1) as usize;
                i += 1;
                let len = data[i] as usize;
                i += 1;
                col += skip;
                for k in 0..len {
                    let v = u16::from_le_bytes([data[i], data[i + 1]]);
                    i += 2;
                    row[col + k] = Some(v);
                }
                col += len;
            }
            grid.push(row);
        }
        (x_off, y_off, grid)
    }

    fn rgb565(r: u8, g: u8, b: u8) -> u16 {
        ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)
    }

    #[test]
    fn tbt_raw_round_trips() {
        // 4×2：列0 = [透明, 紅, 綠, 透明]，列1 = [藍, 藍, 透明, 白]
        let mut rgba = vec![0u8; 4 * 2 * 4];
        let mut put = |idx: usize, r: u8, g: u8, b: u8, a: u8| {
            rgba[idx * 4] = r;
            rgba[idx * 4 + 1] = g;
            rgba[idx * 4 + 2] = b;
            rgba[idx * 4 + 3] = a;
        };
        put(1, 0xff, 0, 0, 0xff);
        put(2, 0, 0xff, 0, 0xff);
        put(4, 0, 0, 0xff, 0xff);
        put(5, 0, 0, 0xff, 0xff);
        put(7, 0xff, 0xff, 0xff, 0xff);
        let enc = encode_tbt_raw(&rgba, 4, 2, 128).unwrap();
        let (xo, yo, grid) = decode_tbt_raw(&enc);
        assert_eq!((xo, yo), (0, 0));
        assert_eq!(
            grid[0],
            vec![
                None,
                Some(rgb565(0xff, 0, 0)),
                Some(rgb565(0, 0xff, 0)),
                None
            ]
        );
        assert_eq!(
            grid[1],
            vec![
                Some(rgb565(0, 0, 0xff)),
                Some(rgb565(0, 0, 0xff)),
                None,
                Some(rgb565(0xff, 0xff, 0xff))
            ]
        );
    }

    #[test]
    fn tbt_raw_header_and_full_row() {
        // 32×32 全不透明 → header [0,0,32,32]，每列 seg_count=1, skip=0, len=32
        let rgba = vec![0xffu8; 32 * 32 * 4];
        let enc = encode_tbt_raw(&rgba, 32, 32, 128).unwrap();
        assert_eq!(&enc[0..4], &[0, 0, 32, 32]);
        // 第一列：enc[4]=seg_count=1, enc[5]=skip=0, enc[6]=len=32
        assert_eq!(enc[4], 1);
        assert_eq!(enc[5], 0);
        assert_eq!(enc[6], 32);
        let (_, _, grid) = decode_tbt_raw(&enc);
        assert_eq!(grid.len(), 32);
        assert!(grid.iter().all(|r| r.iter().all(|p| p.is_some())));
    }

    #[test]
    fn tbt_raw_all_transparent_row() {
        // 全透明 2×1 → 每列 seg_count=0
        let rgba = vec![0u8; 2 * 1 * 4];
        let enc = encode_tbt_raw(&rgba, 2, 1, 128).unwrap();
        assert_eq!(&enc[0..4], &[0, 0, 2, 1]);
        assert_eq!(enc[4], 0); // seg_count=0
        let (_, _, grid) = decode_tbt_raw(&enc);
        assert_eq!(grid[0], vec![None, None]);
    }

    #[test]
    fn cycle_ms_is_anim_plus_rest() {
        let e = AnimEntry {
            tbt: 80,
            speed_ms: 100,
            interval_ms: 2000,
            frames: vec![1, 2, 3],
        };
        assert_eq!(e.cycle_ms(), 2300);
    }

    const SAMPLE: &str = r#"
<dynamicicons>
  <item tbt="1524" speed="80" interval="2000">
    <png>30001</png>
    <png>30002</png>
  </item>
</dynamicicons>
"#;

    #[test]
    fn parse_and_validate() {
        let map = parse_dynamic_icons(SAMPLE).expect("應解析成功");
        assert_eq!(map.get(&1524).unwrap().frames, vec![30001, 30002]);
        assert!(parse_dynamic_icons(
            r#"<dynamicicons><item tbt="1" speed="5" interval="1"></item></dynamicicons>"#
        )
        .is_err());
    }

    #[test]
    fn xml_round_trips() {
        let map = parse_dynamic_icons(SAMPLE).unwrap();
        let xml = to_xml(&map);
        let map2 = parse_dynamic_icons(&xml).expect("產生的 XML 應可再解析");
        assert_eq!(map, map2);
    }

    #[test]
    fn serialize_table_layout() {
        let mut map = AnimMap::new();
        map.insert(
            80,
            AnimEntry {
                tbt: 80,
                speed_ms: 120,
                interval_ms: 1500,
                frames: vec![30101, 30102],
            },
        );
        let (count, blob) = serialize_anim_table(&map);
        assert_eq!(count, 1);
        assert_eq!(blob.len(), 408);
        assert_eq!(u16::from_le_bytes([blob[0], blob[1]]), 80);
        assert_eq!(
            u32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]),
            2
        );
        assert_eq!(
            u32::from_le_bytes([blob[12], blob[13], blob[14], blob[15]]),
            30101
        );
    }

    #[test]
    fn pack_then_find_round_trips() {
        let files = vec![
            ("dynamicicons.xml".to_string(), b"<xml/>".to_vec()),
            ("30001.png".to_string(), vec![1, 2, 3, 4, 5]),
        ];
        let (pak, idx) = build_sprite_pak(&files).unwrap();
        let e = find_entry(&idx, "30001.png").expect("應找到");
        assert_eq!(e.size, 5);
        assert_eq!(
            &pak[e.offset as usize..e.offset as usize + e.size as usize],
            &[1, 2, 3, 4, 5]
        );
        let x = find_entry(&idx, "dynamicicons.xml").unwrap();
        assert_eq!(
            &pak[x.offset as usize..x.offset as usize + x.size as usize],
            b"<xml/>"
        );
    }

    #[test]
    fn pack_rejects_long_filename() {
        let files = vec![("this_name_is_way_too_long_over_20.png".to_string(), vec![0])];
        assert!(build_sprite_pak(&files).is_err());
    }
}
