//! Spell Book reader — 從玩家**已學會**的 spell list 建 `name → packed` 對映表
//!
//! 跟 [`crate::aux::spell_db::SpellDb`] 的差別:
//! - `SpellDb` 是「全 client 所有 level 的技能」,name 第一個出現的 packed 通常是 level 1
//! - `SpellBook` 是「玩家身上學的技能」,packed 對應**玩家實際擁有的 level**
//!
//! ## 為什麼需要
//!
//! `ForceSelfPacket` 路徑(體魄強健術 / 通暢氣脈術 等可指定他人的自身 buff)直接組
//! `C_SKILL` packet 送給 server。server 會驗證「玩家是否學會這個 packed」,如果用
//! `SpellDb` 拿到的 level 1 packed,玩家若只學高 level 版本 → server 拒絕 → 永遠循環。
//!
//! 從 spell_book 拿,packed 一定是玩家學的版本,server 必接受。
//!
//! ## 結構(2026-05-01 實機驗證)
//!
//! ```text
//! [SPELL_BOOK_PTR] (= 0x00C31324)
//!   └─→ spell_book object @ heap
//!         +0x00 (4B) = vftable_ptr (= 0x008EF26C)
//!         +0x2C (4B) = spell count (玩家學的技能數)
//!         +0x58 (4B) = spell array ptr (heap)
//!                       └─→ DWORD[count] of entry pointers
//!                             ├─ entry[0]:
//!                             │   +0x00 (4B) = vftable_ptr (= 0x008EF244)
//!                             │   +0x04 (4B) = packed_skill_id ★
//!                             │   +0x0C (4B) = name_ptr (Big5, " (mp/range[/level])" 字尾)
//!                             ├─ entry[1]: 同上
//!                             └─ ...
//! ```
//!
//! 反組譯來源:`spell_book::cast @ 0x73ECE0` 的查表迴圈
//! (0x73ED2C..0x73ED5E):
//! ```asm
//! mov edx, [rcx + 0x58]    ; spell array
//! mov ecx, [rdx + rax*4]   ; entry = array[i]
//! mov edx, [rcx + 4]       ; packed = entry+4
//! cmp edx, [rbp + 8]       ; == 要找的 packed?
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use parking_lot::RwLock;
use windows::Win32::Foundation::HANDLE;

use crate::aux::address;
use crate::aux::spell_db::{decode_spell_name_bytes, parse_range_from_suffix, strip_paren_suffix};
use crate::log_line;
use crate::memory::{read_bytes, read_u32};

/// 名稱欄位最長讀多少 bytes
const NAME_MAX_BYTES: usize = 64;

/// 單個技能 entry 的快取資料 — packed 用於送 C_SKILL,range 用於 bot 判斷攻擊距離。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpellEntry {
    /// `packed_skill_id`(送 packet 用)
    pub packed: u32,
    /// 技能射程(tile)— 從 record name 字尾 `(mp/range[/level])` 第二個數字解析。
    /// 自身 buff(像加速術)= 0;遠距傷害技(烈炎術 = 3 / 光箭 = 8 等)= 對應格數。
    pub range: u32,
    /// `byte_table[packed]` from `0x7404AC` — client `spell_book_cast` dispatcher 用的 handler index。
    ///
    /// 相同 byte 的技能 = 走同一個 jump table handler,可用於分類「buff vs 攻擊」。
    /// 查 client 路徑 `0x73EF8B`: `movzx eax, byte [edx + 0x7404AC]; jmp [eax*4 + 0x7403A8]`。
    pub dispatcher_byte: u8,
}

/// 客戶端 `spell_book_cast` byte_table 起點(`0x7404AC`)— 220 bytes。
const BYTE_TABLE_ADDR: u32 = 0x0074_04AC;
const BYTE_TABLE_LEN: u32 = 0xDC;

/// 「純攻擊類」handler byte whitelist — 走這些 handler 的技能 = 對 target 出 damage / debuff。
///
/// **依據:** 2026-05-17 live attach 把 0xDC bytes byte_table dump 出來,跟 SPELL_DB 對齊
/// 後分組(65 個獨立 handler bytes),逐 group 看技能名分類。 排除掉:
///  - `0x00` 4 個治癒術 (heal)
///  - `0x01` 45 個自身 buff 大 group(日光術/保護罩/絕對屏障/封印禁地/暴風神射/...)
///  - `0x03` 傳送類 (指定傳送 / 集體傳送術 / 世界樹的呼喚)
///  - `0x04` 16 個 weapon/element buff (加速術 / 神聖武器 / 變形術 / 火焰武器 / ...)
///  - `0x07` 武器附魔 (擬似魔法武器 / 鎧甲護持 / 暗影之牙)
///  - `0x0A, 0x0B` 感測 / 修煉 buff
///  - `0x14, 0x17` 召喚 / 造屍
///  - `0x19, 0x1B, 0x1C, 0x1E, 0x1F` 加速 / 隱身 / 創造武器 / 屏障類
///  - `0x20..=0x23` passive (會心一擊 / 精準目標 / 呼喚盟友)
///  - `0x25, 0x27, 0x2B, 0x2C` element release / shield / passive
///  - `0x30, 0x36, 0x39` 覺醒 / 鏡像 / 幻覺類 transformation
///
/// **保留:** 0x02 0x05 0x06 0x08 0x09 0x0C 0x0D 0x0E 0x0F 0x10 0x11 0x12 0x13 0x15 0x16
/// 0x18 0x1A 0x1D 0x24 0x26 0x28 0x29 0x2A 0x2D 0x2E 0x2F 0x31 0x32 0x33 0x34 0x35 0x37
/// 0x38 0x3A 0x3B 0x3C (36 個攻擊/debuff handler bytes)
///
/// **代價:** 個別 edge case 會被誤分類(暴風神射 / 風之神射 / 召喚屬性精靈 在 buff handler 內
/// 雖然語意上像攻擊,但 client dispatch 走 buff path,bot 一律當 buff 排除)。 全表覆蓋的優先級
/// 比少量 outlier 高,要打 outlier 用 raw packet 路徑。
pub const ATTACK_HANDLER_WHITELIST: &[u8] = &[
    0x02, 0x05, 0x06, 0x08, 0x09, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x15, 0x16, 0x18,
    0x1A, 0x1D, 0x24, 0x26, 0x28, 0x29, 0x2A, 0x2D, 0x2E, 0x2F, 0x31, 0x32, 0x33, 0x34, 0x35, 0x37,
    0x38, 0x3A, 0x3B, 0x3C,
];

impl SpellEntry {
    /// `true` 代表此技能走 client 攻擊 handler — 用於 bot UI 過濾。
    pub fn is_attack(&self) -> bool {
        ATTACK_HANDLER_WHITELIST.contains(&self.dispatcher_byte)
    }
}

/// `name → SpellEntry`(玩家學的 level + 技能 metadata)
///
/// `book_ptr` 是 build 當下從 `[SPELL_BOOK_PTR]` 讀到的 spell_book 物件位址。
/// 換角時遊戲會重新分配 spell_book object,這個值就會變,用來偵測 cache stale。
#[derive(Default, Clone, Debug)]
pub struct SpellBook {
    pub(crate) map: HashMap<String, SpellEntry>,
    pub(crate) book_ptr: u32,
}

impl SpellBook {
    /// 從玩家 spell_book 建表 — caller 必須確保已進場(`G_GAME_STATE == 3`)。
    pub fn build(h: HANDLE) -> Result<Self> {
        let book_ptr = read_u32(h, address::SPELL_BOOK_PTR)
            .with_context(|| format!("讀 SPELL_BOOK_PTR @ 0x{:08X}", address::SPELL_BOOK_PTR))?;
        if book_ptr == 0 {
            bail!("SPELL_BOOK_PTR 為 NULL — 玩家可能尚未進場");
        }
        let count = read_u32(h, book_ptr + 0x2C)
            .with_context(|| format!("讀 spell_book.count @ 0x{:08X}", book_ptr + 0x2C))?;
        let array_ptr = read_u32(h, book_ptr + 0x58)
            .with_context(|| format!("讀 spell_book.array @ 0x{:08X}", book_ptr + 0x58))?;
        if count == 0 || array_ptr == 0 {
            bail!("spell_book 空(count={count}, array=0x{array_ptr:08X})");
        }
        // 防呆:count 太誇張代表結構讀錯
        if count > 1024 {
            bail!("spell_book.count={count} 看起來不合理,中斷");
        }

        let array_bytes = read_bytes(h, array_ptr, count as usize * 4)
            .with_context(|| format!("讀 spell array @ 0x{array_ptr:08X}"))?;

        // 一次讀 byte_table (0xDC bytes) — 用於每個 entry 對應 dispatcher_byte。
        // 讀失敗 → 整張 byte_table 都當 0,is_attack() 一律 false,UI 看起來「沒攻擊技能」
        // 比靜默放行 buff 顯眼,容易發現 RE 退化。
        let byte_table =
            read_bytes(h, BYTE_TABLE_ADDR, BYTE_TABLE_LEN as usize).unwrap_or_else(|e| {
                log_line!(
                    "[spell_book] 讀 byte_table @ 0x{:08X} 失敗(攻擊 filter 失效):{e:#}",
                    BYTE_TABLE_ADDR
                );
                vec![0u8; BYTE_TABLE_LEN as usize]
            });

        let mut map: HashMap<String, SpellEntry> = HashMap::new();

        for i in 0..count as usize {
            let off = i * 4;
            let entry_ptr = u32::from_le_bytes([
                array_bytes[off],
                array_bytes[off + 1],
                array_bytes[off + 2],
                array_bytes[off + 3],
            ]);
            if entry_ptr == 0 {
                continue;
            }

            // 讀 entry 前 16 bytes — 取 packed (+0x04) 和 name_ptr (+0x0C)
            let entry_bytes = match read_bytes(h, entry_ptr, 16) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let packed = u32::from_le_bytes([
                entry_bytes[4],
                entry_bytes[5],
                entry_bytes[6],
                entry_bytes[7],
            ]);
            let name_ptr = u32::from_le_bytes([
                entry_bytes[12],
                entry_bytes[13],
                entry_bytes[14],
                entry_bytes[15],
            ]);
            if name_ptr == 0 {
                continue;
            }

            let raw = match read_bytes(h, name_ptr, NAME_MAX_BYTES) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let null_pos = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            if null_pos == 0 {
                continue;
            }
            let full = decode_spell_name_bytes(&raw[..null_pos]);
            let base = strip_paren_suffix(&full);
            if base.is_empty() {
                continue;
            }
            // range 從同一個 full 字串字尾解出來;沒字尾(理論上不會發生,所有技能都有字尾)→ 0
            let range = parse_range_from_suffix(&full).unwrap_or(0);
            // dispatcher_byte = byte_table[packed_id]。 packed 越界(理論上不會發生,
            // table 邊界 = `spell_book_cast` 0x73EF7B `cmp [ebp-0x6C], 0xDB`)→ 0,is_attack 自動 false。
            let dispatcher_byte = byte_table.get(packed as usize).copied().unwrap_or(0);
            let entry = SpellEntry {
                packed,
                range,
                dispatcher_byte,
            };

            // 同名時保留**較大** packed(通常 = 較高 level — 玩家最後學的版本)+ 同步 range
            map.entry(base.to_string())
                .and_modify(|e| {
                    if packed > e.packed {
                        *e = entry;
                    }
                })
                .or_insert(entry);
        }

        Ok(SpellBook { map, book_ptr })
    }

    /// 用 INI 寫的技能名稱查 packed_skill_id
    pub fn lookup(&self, name: &str) -> Option<u32> {
        self.map.get(name).map(|e| e.packed)
    }

    /// 用技能名查射程(tile)。 沒找到 → None;自身 buff = Some(0)。
    pub fn range_of(&self, name: &str) -> Option<u32> {
        self.map.get(name).map(|e| e.range)
    }

    /// 表內技能數
    pub fn unique_names(&self) -> usize {
        self.map.len()
    }

    /// 列出所有名稱(diagnostic 用,lookup miss 時 dump 候選)。
    pub fn names_iter(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(|s| s.as_str())
    }

    /// 列出「**走 client 攻擊 handler**」的技能名 — 給 bot 攻擊技下拉用,
    /// 自動排除自身 buff / heal / 傳送 / 召喚 / passive 等(`is_attack()` 判斷)。
    ///
    /// 失敗 fallback(byte_table 讀失敗 → 全 0)→ iter 空,UI 看起來無攻擊技能,
    /// 比悄悄放行 buff 顯眼,容易發現問題。
    pub fn attack_names_iter(&self) -> impl Iterator<Item = &str> {
        self.map
            .iter()
            .filter(|(_, e)| e.is_attack())
            .map(|(name, _)| name.as_str())
    }

    /// cache 是否對不上當下遊戲狀態(換角後 SPELL_BOOK_PTR 會變)。
    ///
    /// `current_book_ptr == 0` 也視為 stale — 表示遊戲還沒分配,build 也會 fail,
    /// 不如直接 invalidate 讓 caller 走 rebuild 路徑統一處理。
    pub fn is_stale_for(&self, current_book_ptr: u32) -> bool {
        current_book_ptr == 0 || self.book_ptr != current_book_ptr
    }
}

/// 讀目前遊戲全域的 SPELL_BOOK_PTR(= `[0x00C31324]`)。失敗回 0。
pub fn read_current_book_ptr(h: HANDLE) -> u32 {
    read_u32(h, address::SPELL_BOOK_PTR).unwrap_or(0)
}

/// 確保 `spell_book` cache 對應當下遊戲狀態:None / stale 都會 rebuild。
///
/// 回傳 true 代表 cache 現在 fresh 可用;false 代表 build 失敗(玩家未進場 / 結構讀錯)。
///
/// 取代原本 4 個 caller 各自寫的 `is_none() → SpellBook::build` 鏈,新增換角偵測。
/// `tag` 用於 log 來源區分(`hotkey` / `buff` / `status` / `drink` 等)。
pub fn ensure_fresh(h: HANDLE, spell_book: &Arc<RwLock<Option<SpellBook>>>, tag: &str) -> bool {
    let current_ptr = read_current_book_ptr(h);
    if let Some(book) = spell_book.read().as_ref() {
        if !book.is_stale_for(current_ptr) {
            return true;
        }
    }
    match SpellBook::build(h) {
        Ok(book) => {
            log_line!(
                "[{tag}] spell_book 載入完成 — 玩家學了 {} 個技能 (book_ptr=0x{:08X})",
                book.unique_names(),
                book.book_ptr
            );
            *spell_book.write() = Some(book);
            true
        }
        Err(e) => {
            log_line!("[{tag}] spell_book 建表失敗: {e:#}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_book(book_ptr: u32) -> SpellBook {
        let mut map = HashMap::new();
        map.insert(
            "加速術".to_string(),
            SpellEntry {
                packed: 0xDEADBEEF,
                range: 0,
                dispatcher_byte: 0x04, // buff handler
            },
        );
        SpellBook { map, book_ptr }
    }

    #[test]
    fn lookup_and_range_of_split_correctly() {
        let mut map = HashMap::new();
        map.insert(
            "烈炎術".to_string(),
            SpellEntry {
                packed: 0x2E,
                range: 3,
                dispatcher_byte: 0x16, // attack handler
            },
        );
        let book = SpellBook {
            map,
            book_ptr: 0xAA,
        };
        assert_eq!(book.lookup("烈炎術"), Some(0x2E));
        assert_eq!(book.range_of("烈炎術"), Some(3));
        assert_eq!(book.lookup("不存在"), None);
        assert_eq!(book.range_of("不存在"), None);
    }

    #[test]
    fn attack_names_iter_keeps_only_whitelisted_handlers() {
        // 混合 buff/attack 名單,attack_names_iter 應該只回 attack handler 走的技能。
        let mut map = HashMap::new();
        map.insert(
            "光箭".to_string(),
            SpellEntry {
                packed: 0x03,
                range: 0,
                dispatcher_byte: 0x02,
            }, // 光箭走 0x02 attack
        );
        map.insert(
            "三重矢".to_string(),
            SpellEntry {
                packed: 0x83,
                range: 0,
                dispatcher_byte: 0x24,
            }, // 三重矢獨佔 0x24
        );
        map.insert(
            "烈炎術".to_string(),
            SpellEntry {
                packed: 0x2D,
                range: 3,
                dispatcher_byte: 0x16,
            }, // 0x16 attack
        );
        map.insert(
            "加速術".to_string(),
            SpellEntry {
                packed: 0x2A,
                range: 0,
                dispatcher_byte: 0x04,
            }, // 0x04 buff
        );
        map.insert(
            "封印禁地".to_string(),
            SpellEntry {
                packed: 0xA0,
                range: 0,
                dispatcher_byte: 0x01,
            }, // 0x01 自身 buff 大 group
        );
        map.insert(
            "初級治癒術".to_string(),
            SpellEntry {
                packed: 0x00,
                range: 0,
                dispatcher_byte: 0x00,
            }, // 0x00 heal
        );
        map.insert(
            "指定傳送".to_string(),
            SpellEntry {
                packed: 0x04,
                range: 0,
                dispatcher_byte: 0x03,
            }, // 0x03 傳送
        );
        let book = SpellBook {
            map,
            book_ptr: 0xBB,
        };

        let attacks: Vec<&str> = book.attack_names_iter().collect();
        assert!(attacks.contains(&"光箭"), "光箭 (0x02) 應該保留");
        assert!(attacks.contains(&"三重矢"), "三重矢 (0x24) 應該保留");
        assert!(attacks.contains(&"烈炎術"), "烈炎術 (0x16) 應該保留");
        assert!(!attacks.contains(&"加速術"), "加速術 (0x04 buff) 應該排除");
        assert!(
            !attacks.contains(&"封印禁地"),
            "封印禁地 (0x01 buff) 應該排除"
        );
        assert!(
            !attacks.contains(&"初級治癒術"),
            "治癒術 (0x00 heal) 應該排除"
        );
        assert!(
            !attacks.contains(&"指定傳送"),
            "傳送 (0x03 utility) 應該排除"
        );
        assert_eq!(attacks.len(), 3);
    }

    #[test]
    fn is_attack_uses_whitelist() {
        // 守住 whitelist 不被誤改 — RE 結論變動代表需要重新分組
        let cases = [
            (0x00u8, false, "heal"),
            (0x01, false, "self-buff (45-skill group)"),
            (0x02, true, "光箭/冰箭 attack"),
            (0x03, false, "傳送"),
            (0x04, false, "weapon/element buff"),
            (0x06, true, "毒咒/闇盲 debuff"),
            (0x18, true, "AoE 攻擊"),
            (0x24, true, "三重矢"),
            (0x2A, true, "精準射擊"),
            (0x30, false, "覺醒 transformation"),
            (0x39, false, "幻覺/passive"),
            (0x3C, true, "骷髏毀壞"),
            (0xFF, false, "未知 byte 預設 false"),
        ];
        for (byte, expected, label) in cases {
            let entry = SpellEntry {
                packed: 0,
                range: 0,
                dispatcher_byte: byte,
            };
            assert_eq!(
                entry.is_attack(),
                expected,
                "byte 0x{byte:02X} ({label}) 預期 is_attack={expected}"
            );
        }
    }

    #[test]
    fn fresh_cache_matching_ptr_is_not_stale() {
        let book = fake_book(0x12345678);
        assert!(!book.is_stale_for(0x12345678));
    }

    #[test]
    fn cache_with_different_ptr_is_stale() {
        // 模擬換角:遊戲為新角色重新分配 spell_book object,位址變了
        let book = fake_book(0x12345678);
        assert!(book.is_stale_for(0xAABBCCDD));
    }

    #[test]
    fn zero_current_ptr_treated_as_stale() {
        // 玩家退選角 / 連線斷,SPELL_BOOK_PTR 還沒重填
        // → 視為 stale(rebuild 也會 fail,但統一走 rebuild 路徑由 build() 報錯)
        let book = fake_book(0x12345678);
        assert!(book.is_stale_for(0));
    }

    #[test]
    fn default_book_with_zero_ptr_is_stale_against_real_ptr() {
        // Default::default() 拿到的 SpellBook book_ptr=0,任何非零當前 ptr 都該視為 stale
        let book = SpellBook::default();
        assert!(book.is_stale_for(0x12345678));
    }
}
