//! 自製 pak 讀取（純記憶體，無 game 操作）。
//!
//! - `extract_file`：用 idx::find_entry 從 pak bytes 抽出指定檔案 bytes。
//! - `extract_anim_map`：抽 dynamicicons.xml 並解析成 AnimMap。
//!
//! Path C（2026-06-26）下不再需要 register_custom_pak：幀 PNG 由 launcher 自己解碼轉
//! TBT-raw 注入，遊戲不必認得這個 pak。pak 只是「幀 PNG + XML」的容器。

use launcher::dynamic_icon_format::{find_entry, parse_dynamic_icons, AnimMap};

const XML_NAME: &str = "dynamicicons.xml";

/// 從 pak bytes + idx bytes 抽出指定檔名的 bytes。
pub fn extract_file<'a>(pak: &'a [u8], idx: &[u8], filename: &str) -> Result<&'a [u8], String> {
    let e = find_entry(idx, filename).ok_or_else(|| format!("pak 內找不到 {filename}"))?;
    let start = e.offset as usize;
    let end = start
        .checked_add(e.size as usize)
        .ok_or_else(|| format!("{filename} entry 範圍溢位"))?;
    pak.get(start..end)
        .ok_or_else(|| format!("{filename} entry 超出 pak 範圍"))
}

/// 抽出並解析 dynamicicons.xml → AnimMap（frames = png id）。
pub fn extract_anim_map(pak: &[u8], idx: &[u8]) -> Result<AnimMap, String> {
    let bytes = extract_file(pak, idx, XML_NAME)?;
    let text = std::str::from_utf8(bytes).map_err(|_| "dynamicicons.xml 非 UTF-8".to_string())?;
    parse_dynamic_icons(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_idx(entries: &[(&str, u32, u32)]) -> Vec<u8> {
        let mut idx = (entries.len() as u32).to_le_bytes().to_vec();
        for (name, off, size) in entries {
            idx.extend_from_slice(&off.to_le_bytes());
            let mut fname = [0u8; 20];
            fname[..name.len()].copy_from_slice(name.as_bytes());
            idx.extend_from_slice(&fname);
            idx.extend_from_slice(&size.to_le_bytes());
        }
        idx
    }

    #[test]
    fn extract_file_and_anim_map() {
        let xml = r#"<dynamicicons><item tbt="80" speed="100" interval="2000"><png>30001</png></item></dynamicicons>"#;
        let off = 64u32;
        let mut pak = vec![0u8; off as usize];
        pak.extend_from_slice(xml.as_bytes());
        let idx = build_idx(&[(XML_NAME, off, xml.len() as u32)]);

        assert_eq!(extract_file(&pak, &idx, XML_NAME).unwrap(), xml.as_bytes());
        let map = extract_anim_map(&pak, &idx).expect("應抽出並解析");
        assert_eq!(map.get(&80).unwrap().frames, vec![30001]);
    }
}
