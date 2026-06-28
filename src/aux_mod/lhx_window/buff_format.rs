use super::*;

/// 物品堆疊數會變,儲存/比對時要用 base name。
///
/// 遊戲記憶體存的物品名格式:`<中文名> (<數量>)`,數量大於 999 時客戶端會插入
/// 逗號千分位(例:`金幣 (17,099)`、`變形卷軸 (1,000)`)。strip_qty 需把整個
/// 「空白 + 括號 + 數字/逗號 + 括號」尾段都剝乾淨,只留 base name 給 INI 比對用。
pub(crate) fn strip_qty(name: &str) -> &str {
    if let Some(p) = name.rfind(" (") {
        // 確認 (..) 內只有 ASCII 數字 + 逗號才剝(逗號 = 千分位分隔)
        if let Some(close) = name[p..].rfind(')') {
            let inner = &name[p + 2..p + close];
            if !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit() || b == b',') {
                return name[..p].trim_end();
            }
        }
    }
    name.trim()
}

/// 從物品名尾端剝掉「狀態裝飾」尾綴(`(揮舞)` / `(使用中)`)— 取得 base name。
///
/// 3.8 客戶端把裝備穿戴/揮舞狀態用括號附在物品名後面(例 `銀劍 (揮舞)`、
/// `胸甲 (使用中)`)。比對 user 的 `/IA=胸甲` 或 `/IW=銀劍` 過濾器時,
/// 必須先把狀態尾綴從 item 名稱拿掉,否則永遠比不到。
///
/// 不會剝「描述性括號」(例 `提煉魔石 (一級)`、`卷軸 (擬似魔法武器)`)— 因為
/// 那些括號內容不在這個白名單。如果未來有需要再擴。
pub(crate) fn strip_state_paren(name: &str) -> &str {
    let trimmed = name.trim_end();
    for suffix in ["(揮舞)", "(使用中)"] {
        if let Some(stripped) = trimmed.strip_suffix(suffix) {
            return stripped.trim_end();
        }
    }
    trimmed
}

/// 物品名比對前的標準化:剝數量括號 + 剝狀態尾綴。
///
/// 例:
/// - `銀劍 (揮舞)` → `銀劍`
/// - `胸甲 (使用中)` → `胸甲`
/// - `金幣 (17,099)` → `金幣`
/// - `卷軸 (擬似魔法武器)` → `卷軸 (擬似魔法武器)` (描述性括號不剝)
pub(crate) fn clean_item_name(name: &str) -> &str {
    strip_state_paren(strip_qty(name))
}

/// 解析 INI buff 條目 → BuffItem,支援 legacy(`/` 分隔)與 native(`_` 分隔)兩種格式。
///
/// 格式範例:
/// - `0_自我加速藥水`        → id=0, name="自我加速藥水", item_type='I'(物品)
/// - `0_加速術/ME`           → id=0, name="加速術", item_type='S'(技能/魔法)
/// - `0_強力加速術/M`        → id=0, name="強力加速術", item_type='S'
/// - `153_生命之樹果實`      → id=153, name="生命之樹果實", item_type='I'
///
/// id = state byte array index(`[buff_array + id]` = 0/1)。
/// 字尾規則(legacy):
/// - 技能系:`/M` `/ME` `/MIA` `/MIW` `/MI` `/MME` `/M=<name>`(`/MT` 為向下相容 alias,行為等同 `/M`)
/// - 物品系:`/I` `/IA`(對使用中防具)`/IW`(對揮舞武器)`/I=<name>` `/ID` `/INFO`
/// - 狀態頁:`/KEY=F<n>` `/DKEY=F<n>`
/// - 無字尾 = 物品 USE_ITEM
pub(crate) fn parse_buff_item(raw: &str) -> crate::aux::runtime::BuffItem {
    use crate::aux::runtime::BuffItem;
    let trimmed = raw.trim();

    // 偵測 legacy 格式(有 '/'):`<id>_<name>/<suffix>` → 自動 migration 成 native 等價
    if trimmed.contains('/') {
        return parse_buff_item_legacy(trimmed);
    }

    // Native 格式:`<name>_<id_or_target>_<suffix>` (3 段底線分隔)
    // 第 2 段對 MI/II/IP 是 target name,其他是 numeric state_id
    let parts: Vec<&str> = trimmed.splitn(3, '_').collect();
    let (name, id_or_target, suffix) = match parts.as_slice() {
        [a, b, c] => (a.trim(), b.trim(), c.trim()),
        // 2 段時 disambiguation:
        //   "0_自我加速藥水"   ← legacy 格式 id_name(無 suffix)
        //   "肉_-1"             ← native 缺 suffix(視為物品)
        // 用「第 1 段是純數字」判斷 legacy 格式
        [a, b] if a.trim().parse::<i32>().is_ok() => {
            return parse_buff_item_legacy(trimmed);
        }
        [a, b] => (a.trim(), b.trim(), ""),
        [a] => (a.trim(), "-1", ""), // 只有名字 → 物品
        _ => unreachable!("splitn(3) 至少回 1 個元素"),
    };

    let (item_type, cast_target) = parse_suffix_70(suffix, id_or_target);
    // numeric id:MI/II/IP 的 second segment 是 name 而非數字 → 解析失敗 fallback -1
    let id: i32 = id_or_target.parse().unwrap_or(-1);
    BuffItem {
        id,
        name: name.to_string(),
        item_type,
        cast_target,
    }
}

/// Legacy 格式 migration:`<id>_<name>/<suffix>` → native BuffItem
///
/// Legacy → native suffix 對映:
///   /M  → M    (NoSpec)
///   /ME → MME  (Self_)
///   無 suffix → I (Item)
///   其他擴充式 /M=name 等改成最接近的 native suffix:
///     /M=name → MI (對 name 物品施法)
///     /MI → MI   (假設 user 想對某物品施法,name 從別處拿)
///     /KEY=Fn / /DKEY=Fn → 維持
pub(crate) fn parse_buff_item_legacy(trimmed: &str) -> crate::aux::runtime::BuffItem {
    use crate::aux::runtime::{BuffItem, CastTarget};

    // 1. 嘗試拆 <id>_<rest>;有些 INI section(例 [AllAntidote])沒帶 id 前綴,
    //    格式直接是 <name>/<suffix>,要 fallback 把整段當 name。
    let (id, rest) = match trimmed.split_once('_') {
        Some((id_str, r)) if id_str.trim().parse::<i32>().is_ok() => {
            (id_str.parse::<i32>().unwrap_or(-1), r)
        }
        // 無 `_<id>` 前綴 → id=-1,整串繼續往下 split '/'
        _ => (-1, trimmed),
    };

    // 2. 拆 <name>/<suffix>
    let (name, suffix) = match rest.split_once('/') {
        Some((n, s)) => (n.trim().to_string(), s),
        None => {
            return BuffItem {
                id,
                name: rest.trim().to_string(),
                item_type: 'I',
                cast_target: CastTarget::Item,
            }
        }
    };

    // 3. 舊 suffix 對映到新 CastTarget
    let s = suffix.trim();
    let (item_type, cast_target): (char, CastTarget) = if let Some(rest) = s.strip_prefix("KEY=") {
        parse_fkey(rest)
            .map(|n| ('K', CastTarget::Key(n)))
            .unwrap_or(('I', CastTarget::Item))
    } else if let Some(rest) = s.strip_prefix("DKEY=") {
        parse_fkey(rest)
            .map(|n| ('K', CastTarget::DelayKey(n)))
            .unwrap_or(('I', CastTarget::Item))
    } else if let Some(target_name) = s.strip_prefix("MI=") {
        // /MI=<物品名> — 對指定名稱物品施法(技能系)
        ('S', CastTarget::OnNamedItem(target_name.trim().to_string()))
    } else if let Some(target_name) = s.strip_prefix("M=") {
        // 擴充字尾 /M=name → 等價 native MI(對 name 物品施法)
        ('S', CastTarget::OnNamedItem(target_name.trim().to_string()))
    } else if let Some(target_name) = s.strip_prefix("I=") {
        // /I=<物品名> — 對指定名稱物品使用卷軸(不限狀態)
        ('I', CastTarget::OnNamedItem(target_name.trim().to_string()))
    } else if let Some(target_name) = s.strip_prefix("IA=") {
        // /IA=<物品名> — 對名字 + (使用中) 物品施放(精確指定)
        (
            'I',
            CastTarget::OnInUseItem(Some(target_name.trim().to_string())),
        )
    } else if let Some(target_name) = s.strip_prefix("IW=") {
        // /IW=<物品名> — 對名字 + (揮舞) 物品施放(精確指定)
        (
            'I',
            CastTarget::OnWieldedItem(Some(target_name.trim().to_string())),
        )
    } else if let Some(target_name) = s.strip_prefix("IT=") {
        // /IT=<entity名> — 全自動對指定名玩家/召喚物施放(走 entity scan + cdd 0xA4)
        (
            'I',
            CastTarget::OnNamedEntity(target_name.trim().to_string()),
        )
    } else if s == "IME" {
        // /IME — 對自己施放卷軸(II packet, target=self_char_id),對齊技能 /ME
        ('I', CastTarget::SelfItem)
    } else if s == "M" {
        ('S', CastTarget::NoSpec)
    } else if s == "ME" {
        ('S', CastTarget::Self_)
    } else if s == "MIA" {
        // /MIA — 對「(使用中)」物品施法(找第一件)
        ('S', CastTarget::OnInUseItem(None))
    } else if s == "MIW" {
        // /MIW — 對「(揮舞)」物品施法(找第一件)
        ('S', CastTarget::OnWieldedItem(None))
    } else if s == "MI" {
        // 舊 /MI 沒帶 name(我們的「道具上的魔法」誤譯)→ 沒辦法救,當 NoSpec
        ('S', CastTarget::NoSpec)
    } else if s == "IA" {
        // 物品系 — 對任何「(使用中)」物品(找第一件)
        ('I', CastTarget::OnInUseItem(None))
    } else if s == "IW" {
        // 物品系 — 對任何「(揮舞)」武器(找第一件)
        ('I', CastTarget::OnWieldedItem(None))
    } else if s == "IT" {
        // 物品系 — 對鼠標 hover 的 entity(玩家/召喚物/怪物)
        ('I', CastTarget::HoverTarget)
    } else if s == "ID" {
        ('I', CastTarget::DropItem)
    } else if s == "INFO" {
        ('I', CastTarget::Info)
    } else if s == "I" {
        ('I', CastTarget::Item)
    } else {
        // /M? /M?? /s 等舊擴充 — 沒對等,當物品保險
        ('I', CastTarget::Item)
    };

    BuffItem {
        id,
        name,
        item_type,
        cast_target,
    }
}

/// 解析 native suffix(bare 字串,無前導 `/`)— 回傳 `(item_type, cast_target)`。
///
/// `id_or_target_str` = INI 第 2 段;對 `MI` `II` 是 target name,其他忽略。
///
/// 3.8 支援的 suffix:
/// 魔法系:M / MME / MT / MIA / MIW / MI
/// 物品系:I / IA / IW(短後綴)/ IIA / IIW / II / ID
/// debug:INFO
/// 狀態頁:KEY=F<n> / DKEY=F<n>
///
/// **物品系 IA/IW 用途**:魔法卷軸 / 修理工具等需要選「揮舞中武器」或「使用中防具」當目標。
/// 走 `SendPacketData("cdd", 0xA4, scroll_param, target_param)` 路徑(II packet)。
pub(crate) fn parse_suffix_70(
    suffix: &str,
    id_or_target_str: &str,
) -> (char, crate::aux::runtime::CastTarget) {
    use crate::aux::runtime::CastTarget;
    let s = suffix.trim();

    // 狀態頁按鍵巨集
    if let Some(rest) = s.strip_prefix("KEY=") {
        if let Some(n) = parse_fkey(rest) {
            return ('K', CastTarget::Key(n));
        }
    }
    if let Some(rest) = s.strip_prefix("DKEY=") {
        if let Some(n) = parse_fkey(rest) {
            return ('K', CastTarget::DelayKey(n));
        }
    }

    // 第二段是「target name」還是 numeric id?
    // 對 IA/IIA/IW/IIW 也允許用第二段攜帶名字過濾(`卷軸_胸甲_IIA` = 對名字胸甲且使用中的物品)。
    let target_name_opt = if id_or_target_str.parse::<i32>().is_ok() {
        None
    } else {
        Some(id_or_target_str.to_string())
    };

    match s {
        // 物品系 — 普通 USE_ITEM
        "" | "I" => ('I', CastTarget::Item),
        "ID" => ('I', CastTarget::DropItem),
        // 物品系 — 對既有物品施放(II packet, 0xA4)
        // 短後綴 IA/IW 與長後綴 IIA/IIW/II 同義(item_type='I' 已表達 I 前綴)
        "IA" | "IIA" => ('I', CastTarget::OnInUseItem(target_name_opt)),
        "IW" | "IIW" => ('I', CastTarget::OnWieldedItem(target_name_opt)),
        "II" => ('I', CastTarget::OnNamedItem(id_or_target_str.to_string())),
        // 物品系對 entity(玩家/召喚物)— 兩種:
        //   IT 無 target_name → HoverTarget(半自動 USE_ITEM 快捷鍵,user 點目標)
        //   IT 有 target_name → OnNamedEntity(全自動 entity scan + cdd 0xA4 packet)
        "IT" => match target_name_opt {
            Some(n) => ('I', CastTarget::OnNamedEntity(n)),
            None => ('I', CastTarget::HoverTarget),
        },
        // 物品系對自己 — `/IME`,送 cdd 0xA4 II packet target=self_char_id
        // (對齊技能 `/ME`;`/I` USE_ITEM 0x12 對需 target 卷軸只進選擇模式不送 cast)
        "IME" => ('I', CastTarget::SelfItem),
        // 魔法系(MIA/MIW 目前不接 name 過濾,`MI=name` 走 OnNamedItem)
        "M" => ('S', CastTarget::NoSpec),
        "MME" => ('S', CastTarget::Self_),
        "MT" => ('S', CastTarget::HoverTarget),
        "MIA" => ('S', CastTarget::OnInUseItem(None)),
        "MIW" => ('S', CastTarget::OnWieldedItem(None)),
        "MI" => ('S', CastTarget::OnNamedItem(id_or_target_str.to_string())),
        // debug
        "INFO" => ('I', CastTarget::Info),
        // 未知 suffix → 保險當物品(IT / IBM / IP 等仍未實作的後綴會走到這裡)
        _ => ('I', CastTarget::Item),
    }
}

/// `F1`..`F12` → 1..12;其他回 None
pub(crate) fn parse_fkey(s: &str) -> Option<u8> {
    let rest = s.trim().strip_prefix(|c: char| c == 'F' || c == 'f')?;
    let n: u8 = rest.parse().ok()?;
    if (1..=12).contains(&n) {
        Some(n)
    } else {
        None
    }
}

/// BuffItem → INI 字串(`parse_buff_item` 的反向)
///
/// 優先輸出 user INI 慣用的 legacy 格式 `<id>_<name>[/<suffix>]`;
/// 物品系 OnInUseItem/OnWieldedItem/OnNamedItem 用短後綴 `/IA` `/IW` `/I=<name>`,
/// 技能系語意保留 native `_MIA` `_MIW` `_MI` 格式以避免歧義。
pub fn format_buff_item(b: &crate::aux::runtime::BuffItem) -> String {
    use crate::aux::runtime::CastTarget;
    match &b.cast_target {
        // Legacy 格式 — 對應 user INI(linhelperZ.ini)實際寫法
        CastTarget::Item => format!("{}_{}", b.id, b.name),
        CastTarget::NoSpec => format!("{}_{}/M", b.id, b.name),
        CastTarget::Self_ => format!("{}_{}/ME", b.id, b.name),
        CastTarget::Key(n) => format!("{}_{}/KEY=F{}", b.id, b.name, n),
        CastTarget::DelayKey(n) => format!("{}_{}/DKEY=F{}", b.id, b.name, n),
        // 物品系對 hover entity(II packet, target=char_id)— 短後綴
        CastTarget::HoverTarget if b.item_type == 'I' => format!("{}_{}/IT", b.id, b.name),
        // 物品系對指定名 entity(全自動)— `/IT=name`
        CastTarget::OnNamedEntity(n) if b.item_type == 'I' => {
            format!("{}_{}/IT={}", b.id, b.name, n)
        }
        // 物品系對自己(II packet, target=self_char_id)— `/IME`
        CastTarget::SelfItem => format!("{}_{}/IME", b.id, b.name),
        // 物品系對既有物品施放(II packet, 0xA4)— 短後綴
        CastTarget::OnInUseItem(None) if b.item_type == 'I' => format!("{}_{}/IA", b.id, b.name),
        CastTarget::OnInUseItem(Some(n)) if b.item_type == 'I' => {
            format!("{}_{}/IA={}", b.id, b.name, n)
        }
        CastTarget::OnWieldedItem(None) if b.item_type == 'I' => format!("{}_{}/IW", b.id, b.name),
        CastTarget::OnWieldedItem(Some(n)) if b.item_type == 'I' => {
            format!("{}_{}/IW={}", b.id, b.name, n)
        }
        CastTarget::OnNamedItem(n) if b.item_type == 'I' => format!("{}_{}/I={}", b.id, b.name, n),
        // Native 格式 — 技能系(MIA/MIW 目前不帶 name)
        CastTarget::HoverTarget => format!("{}_{}_MT", b.name, b.id),
        CastTarget::OnInUseItem(_) => format!("{}_{}_MIA", b.name, b.id),
        CastTarget::OnWieldedItem(_) => format!("{}_{}_MIW", b.name, b.id),
        CastTarget::OnNamedItem(n) => format!("{}_{}_MI", b.name, n),
        // OnNamedEntity 目前只支援物品系(`/IT=name`),技能系若意外進到這分支
        // 退化成 hover target(skill MT 行為)— 比丟例外友善
        CastTarget::OnNamedEntity(_) => format!("{}_{}_MT", b.name, b.id),
        CastTarget::DropItem => format!("{}_{}_ID", b.name, b.id),
        CastTarget::Info => format!("{}_{}_INFO", b.name, b.id),
    }
}

/// BuffItem -> command syntax used by timer/F-key text boxes.
///
/// This keeps the parser's legacy compatibility but shows users the shorter
/// `<name>[/suffix]` form documented in the command help.
pub fn format_command_item(b: &crate::aux::runtime::BuffItem) -> String {
    use crate::aux::runtime::CastTarget;

    fn with_suffix(name: &str, suffix: &str) -> String {
        if suffix.is_empty() {
            name.to_string()
        } else {
            format!("{name}/{suffix}")
        }
    }

    match &b.cast_target {
        CastTarget::Item => b.name.clone(),
        CastTarget::NoSpec => with_suffix(&b.name, "M"),
        CastTarget::Self_ => with_suffix(&b.name, "ME"),
        CastTarget::Key(n) => with_suffix(&b.name, &format!("KEY=F{n}")),
        CastTarget::DelayKey(n) => with_suffix(&b.name, &format!("DKEY=F{n}")),
        CastTarget::HoverTarget if b.item_type == 'I' => with_suffix(&b.name, "IT"),
        CastTarget::HoverTarget => with_suffix(&b.name, "MT"),
        CastTarget::OnNamedEntity(n) if b.item_type == 'I' => {
            with_suffix(&b.name, &format!("IT={n}"))
        }
        CastTarget::OnNamedEntity(_) => with_suffix(&b.name, "MT"),
        CastTarget::SelfItem => with_suffix(&b.name, "IME"),
        CastTarget::OnInUseItem(None) if b.item_type == 'I' => with_suffix(&b.name, "IA"),
        CastTarget::OnInUseItem(Some(n)) if b.item_type == 'I' => {
            with_suffix(&b.name, &format!("IA={n}"))
        }
        CastTarget::OnInUseItem(_) => with_suffix(&b.name, "MIA"),
        CastTarget::OnWieldedItem(None) if b.item_type == 'I' => with_suffix(&b.name, "IW"),
        CastTarget::OnWieldedItem(Some(n)) if b.item_type == 'I' => {
            with_suffix(&b.name, &format!("IW={n}"))
        }
        CastTarget::OnWieldedItem(_) => with_suffix(&b.name, "MIW"),
        CastTarget::OnNamedItem(n) if b.item_type == 'I' => with_suffix(&b.name, &format!("I={n}")),
        CastTarget::OnNamedItem(n) => with_suffix(&b.name, &format!("MI={n}")),
        CastTarget::DropItem => with_suffix(&b.name, "ID"),
        CastTarget::Info => with_suffix(&b.name, "INFO"),
    }
}

/// 一次性遷移:把 saved `buff_items` 的 `cast_target` 對齊 INI canonical 條目。
///
/// **動機**:早期版本 suffix 語意未統一,曾把 `/M` `/ME` 通通寫成 `Self_`,
/// 導致 user 重開後右側列表全變 `_MME` 後綴,而且 runtime 拿到 Self_ 也會觸發
/// 錯的 shellcode 路徑(/M 不指定 target 卻送 target=self)。
///
/// 動作:對每個 saved BuffItem,用 `(name, id)` 配對 `[AllState]` INI 行,如果有對應
/// 就把 INI 規範的 cast_target 蓋過去。沒對應的(user 自訂條目)保持原樣。
///
/// 規則一致 → 之後 INI 改變或新增 suffix,只要重啟 launcher 就會自動同步,user 完全
/// 不需要動手清快取或刪 JSON。
pub(crate) fn migrate_buff_items_against_ini(buff_items: &mut [crate::aux::runtime::BuffItem]) {
    let canonical_lines = load_state_list_ini();
    let canonical: Vec<crate::aux::runtime::BuffItem> = canonical_lines
        .iter()
        .map(|raw| parse_buff_item(raw))
        .collect();

    let mut fixed = 0usize;
    for item in buff_items.iter_mut() {
        if let Some(c) = canonical
            .iter()
            .find(|c| c.name == item.name && c.id == item.id)
        {
            // 用粗略 discriminant 比對(不展開 String 欄位)
            let same =
                std::mem::discriminant(&item.cast_target) == std::mem::discriminant(&c.cast_target);
            if !same {
                crate::log_line!(
                    "[lhx] migrate buff: name={:?} id={} cast_target {:?} → {:?}",
                    item.name,
                    item.id,
                    item.cast_target,
                    c.cast_target
                );
                item.cast_target = c.cast_target.clone();
                item.item_type = c.item_type;
                fixed += 1;
            }
        }
    }
    if fixed > 0 {
        crate::log_line!("[lhx] migrate buff: 共修 {} 條過期 cast_target", fixed);
    }
}
