//! 順跑預編碼 IR 型別 — 所有 pipeline 階段共用。

use std::collections::HashMap;

/// 走路類動作碼白名單 — 「哪些 base_action 算走路」的單一真相源。
///
/// 以前 classify.rs(判斷 walk 訊號)與 parse.rs(inline 掃描)各自抄一份同樣的
/// 14 個數字,改一邊忘改另一邊就會 silently 漂移。收斂到此處後兩邊共用。
pub const WALK_ACTIONS: &[u32] = &[0, 4, 11, 20, 24, 40, 46, 50, 54, 58, 62, 83, 88, 119];

/// parse 階段 inline 掃描在 [`WALK_ACTIONS`] 之外額外納入的動作碼(天R 風格 0/4 之外的 32/33)。
/// 掃描順序 = `WALK_ACTIONS` 後接這兩個,與 legacy 一致(順序影響 sprite.actions 排列,須保留)。
pub const INLINE_EXTRA_ACTIONS: &[u32] = &[32, 33];

/// 順跑左右腳交替的兩個自訂 slot — 「98/99 到底代表什麼」的單一真相源。
///
/// 預編碼階段(emit)把 RunL/RunR 動作寫進變身檔 slot 98/99 行;runtime hook 依
/// toggle 在這兩 slot 間切換顯示。slot 號與 frame_data 偏移(`slot*8+4`)的關係
/// 以前只活在 hook 的 `0x0314 // 98*8+4` 註解裡。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunSlot {
    /// 左腳 — slot 98(RunL)
    Left,
    /// 右腳 — slot 99(RunR)
    Right,
}

impl RunSlot {
    /// 變身檔 slot 編號(98 / 99)。
    pub const fn slot(self) -> u32 {
        match self {
            RunSlot::Left => 98,
            RunSlot::Right => 99,
        }
    }

    /// action table 中該 slot 的 frame_data_ptr 偏移 = `slot*8 + 4`。
    pub const fn frame_data_off(self) -> u32 {
        self.slot() * 8 + 4
    }
}

/// 整個變身檔的結構化表示
#[derive(Debug, Clone)]
pub struct SpriteFile {
    /// 第一行(精靈總數 header,例如 "300 0 41210")。忠實 IR 欄位,目前 emit 未讀但保留完整性。
    #[allow(dead_code)]
    pub file_header: String,
    /// 所有 sprite,按出現順序
    pub sprites: Vec<Sprite>,
    /// 原始 line 對應(供 emit 階段保留註解 / 110.framerate / 其他指令行)
    pub raw_lines: Vec<String>,
    /// 原始文本是否以 newline 結尾(供 emit 階段補充尾部 newline)
    pub ends_with_newline: bool,
}

#[derive(Debug, Clone)]
pub struct Sprite {
    pub sid: u16,
    pub header_line_idx: usize,
    /// 忠實 IR 欄位,目前下游未讀但保留解析完整性
    #[allow(dead_code)]
    pub header_text: String,
    pub img_count: u32,
    pub gfx_id: Option<u32>,
    /// 忠實 IR 欄位,目前下游未讀但保留解析完整性
    #[allow(dead_code)]
    pub name: String,
    /// 110.framerate 行內容(若有)。由 sprite 內最近一次出現的 110 line 決定。
    pub framerate: Option<String>,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone)]
pub struct Action {
    pub line_idx: usize,
    /// 行首縮排("\t" or "  " 等)。忠實 IR 欄位,目前下游未讀但保留完整性
    #[allow(dead_code)]
    pub indent: String,
    /// 主動作號(如 0/4/11/32/33)
    pub base_action: u32,
    /// dash 副動作編號(`X-1`/`X-2` 語法 → Some(1) / Some(2),其他 None)
    pub dash_variant: Option<u32>,
    /// 動作名稱(已小寫且 trim,如 "walk"、"runl"、"runr onehandsword")
    pub name: String,
    /// 括號內完整內容("1 8,8.0:2 8.1:2 ...")
    pub content: String,
    /// 解析自 content 的方向(0/1)
    pub direction: u32,
    /// 解析自 content 的幀數
    pub frame_count: u32,
    /// 第一張 spr 編號(content 第一個逗號後 . 前的數字)
    pub first_spr: u32,
    /// 解析時最近一次 110.X 的內容(在此 action 之前出現的最近一次 framerate);
    /// 對應 legacy `cur_framerate`。若該 action 之前同 sprite 內無 110 line → None。
    pub framerate_at_parse: Option<String>,
}

/// Sprite 角色分類
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpriteRole {
    /// 純 walk sprite(有走路動作,無 RunL/RunR 訊號)— 映射目標
    Walk,
    /// 純 run sprite(有 RunL/RunR 訊號,無走路動作)— 映射來源
    Run,
    /// 既有走路也有 RunL/RunR — 自帶完整動作,不參與 cross-sprite 映射
    Both,
    /// 既無走路也無 run 訊號
    None,
}

/// RunL/RunR 萃取結果(asymmetric — legacy `insert_tianm_run_pair` 允許單側乾淨單側髒,
/// 只儲存乾淨那側;dash 變體 v1/v2 也獨立可選)
#[derive(Debug, Clone)]
pub struct RunPair {
    pub runl: Option<String>,
    pub runr: Option<String>,
    pub framerate: Option<String>,
    /// 來源 run sprite 的 img_count(供 emit 階段更新 walk sprite header)
    pub source_img_count: u32,
}

/// Roles map 別名(供 phase 之間傳遞)
pub type RoleMap = HashMap<u16, SpriteRole>;

/// 萃取結果 map
pub type RunPairMap = HashMap<u16, RunPair>;
