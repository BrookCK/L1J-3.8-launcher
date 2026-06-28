/// 喝水分頁的單一 row：HP 閾值 + 物品
#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PotionRow {
    pub enabled: bool,
    pub threshold: u32,
    pub item: String,
}

/// 洗魔規則：HP >= lower && MP <= upper → 用 item
#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MpWhenSafe {
    pub enabled: bool,
    pub hp_lower: u32,
    pub mp_upper: u32,
    pub item: String,
}

/// 施法/物品目標的 suffix 系統 — 描述「對誰使用」的列舉。
///
/// INI 條目格式:`<name>_<id_or_target>_<suffix>`(3 段底線分隔)。
///
/// 為什麼用 enum 而非字串:user 設定階段就把字串解析掉,buff_tick 拿到 CastTarget
/// 直接 match,避免 polling thread 反覆 parse 同一條 INI(每 tick 100ms 級頻率)。
///
/// 第二段(id_or_target)雙重身分:
/// - 一般情況:state byte 索引(`-1` = 未指定,`buff_array[id]` 0/1)
/// - `MI` `II` `IP`:目標物品/玩家名稱(放在這個位置給 dispatcher 使用)
///
/// 範例:
/// - `保護罩_-1_MME` (對自己施法)
/// - `提煉魔石_紅魔石_MI` (對紅魔石施提煉魔石,target name 在 id 位置)
/// - `肉_-1_I` (吃肉)
///
/// suffix → behavior 對照表:
///
/// **魔法系**:
/// - `M`    → [`CastTarget::NoSpec`]      不指定 target(packet 不送 target 欄位)
/// - `MME`  → [`CastTarget::Self_`]       對自己(target = self char_id)
/// - `MT`   → [`CastTarget::HoverTarget`] 對鼠標當下目標
/// - `MIA`  → [`CastTarget::OnInUseItem`] 對「(使用中)」物品施法
/// - `MIW`  → [`CastTarget::OnWieldedItem`] 對「(揮舞)」物品施法
/// - `MI`   → [`CastTarget::OnNamedItem`](name) 對指定名稱物品施法
///
/// **物品系**:
/// - `I`(或無 suffix) → [`CastTarget::Item`] 普通物品 USE_ITEM(自喝藥水/卷軸/補品)
/// - `ID`   → [`CastTarget::DropItem`]       銷毀/丟棄物品
///
/// **debug**:
/// - `INFO` → [`CastTarget::Info`] 印 spr/buff state 到對話框
///
/// **狀態頁(輔助 buff_tick 不觸發)**:
/// - `KEY=F<n>`  → [`CastTarget::Key`](n)
/// - `DKEY=F<n>` → [`CastTarget::DelayKey`](n)
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum CastTarget {
    /// `I` 或無 suffix — 普通物品 USE_ITEM,從背包找同名物品
    #[default]
    Item,
    /// `M` 不指定 target — packet 不送 target 欄位,server 由 session 推斷
    NoSpec,
    /// `MME` 對自己 — 設 `[0x97C910]=[0xABF4B4]`(自己 char_id)後 spell_book_cast
    Self_,
    /// `MT` 對鼠標當下目標 — **3.8 未實作,退化成 NoSpec**。
    /// 為什麼:3.8 找不到「滑鼠 hover entity」全域可抓(`[0x97C910]` 全 process 沒人
    /// 寫,dispatcher 0x73C260 也不走)。替代方案:用 `/IT=<entity名>` 全自動,
    /// 或 `/ME` 對自身 buff。
    HoverTarget,
    /// `IT=<entity名>` 對「指定名字的玩家/召喚物/變身玩家」全自動施放
    ///
    /// 走 entity scan(vfptr `0x008DC08C`)+ name 比對(REMOTE entity `+0x6C`),
    /// 拿到 target_id(`+0x0C`)後送 `cdd 0xA4` packet。
    /// 對齊「右鍵卷軸 → 使用 → 點目標」但不需要玩家手動點。
    OnNamedEntity(String),
    /// `IME` 對自己施放卷軸 — `/IT=<self>` 的捷徑,target=自己 char_id
    ///
    /// **Why**:`/I` USE_ITEM 0x12 對需 target 的卷軸(治癒卷軸等)只進「目標選擇模式」,
    /// 不送施放 packet,server 看到沒完成的 cast 會回 `施咒失敗`。
    /// `IME` 直接走 `cdd 0xA4` II packet,target = `[0xABF4B4]`(自己 char_id)讀來,
    /// 對齊技能 `/ME` 的 self-cast 概念,適用所有需 target 但只想對自己用的卷軸。
    SelfItem,
    /// `MIA` / `IA` 對「(使用中)」物品施法/施放
    ///
    /// `Option<String>` = 名字過濾:
    /// - `None` → 找背包第一件含 `(使用中)` 的物品
    /// - `Some(name)` → 找名字 = `name` 且狀態 `(使用中)` 的物品
    OnInUseItem(Option<String>),
    /// `MIW` / `IW` 對「(揮舞)」物品施法/施放(同 [`Self::OnInUseItem`] 命名語意)
    OnWieldedItem(Option<String>),
    /// `MI` 對指定名稱物品施法 — name 來自 INI 第 2 段
    OnNamedItem(String),
    /// `ID` 銷毀/丟棄物品 — 送 C_DELETE_ITEM packet 把物品從背包移除
    DropItem,
    /// `INFO` debug — dump spr / mouseSpr / buff state
    Info,
    /// `KEY=F<n>` 模擬按 Fn 鍵(1~12)— 屬狀態頁
    Key(u8),
    /// `DKEY=F<n>` 同 Key 但加 delay — 屬狀態頁
    DelayKey(u8),
}

/// 輔助分頁的條目(物品 / 技能 / 指令 / 按鍵)
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BuffItem {
    /// state_id (`[buff_array + id]` 0/1)— -1 = 未指定
    pub id: i32,
    /// 顯示用的乾淨名稱(suffix 已剝掉,`/M=xxx` 的 xxx 在 [`Self::cast_target`] 內)
    pub name: String,
    /// 概略類型:`'I'`=物品,`'S'`=技能,`'K'`=按鍵
    /// (跟 `cast_target` 一致;保留是為了不破壞舊 UI 邏輯)
    pub item_type: char,
    /// suffix 解析後的施法目標路徑
    pub cast_target: CastTarget,
}

impl Default for BuffItem {
    fn default() -> Self {
        Self {
            id: -1,
            name: String::new(),
            item_type: 'I',
            cast_target: CastTarget::Item,
        }
    }
}

/// 狀態分頁的 F1-F4 巨集
#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FKeyMacro {
    pub enabled: bool,
    pub command: String,
}

/// 「其他」分頁 24 項 toggle
#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MiscToggles {
    pub all_day: bool,
    pub underwater_pump: bool,
    pub low_cpu: bool,
    pub monster_level_color: bool,
    pub show_clock: bool,
    pub show_attack_dmg: bool,
    #[serde(default)]
    pub damage_at_feet: bool,
}

/// 定時分頁的單一 row
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TimerRow {
    pub enabled: bool,
    pub interval_sec: u32,
    pub command: String,
}

impl Default for TimerRow {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_sec: 5,
            command: String::new(),
        }
    }
}

/// 所有輔助功能的設定（對應 LinHelperZ 8 tabs）
#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AuxSettings {
    // ─── 通用 ───
    pub current_profile: String, // 保留給 profile 選擇 UI；空字串代表目前未指定。

    // ─── tab1: 喝水 ───
    pub potion_rows: [PotionRow; 7],
    pub mp_when_safe: MpWhenSafe,
    pub potion_use_percent: bool,
    pub potion_show_inventory: bool,

    // ─── tab2: 輔助 ───
    pub buff_enabled: bool,
    pub buff_items: Vec<BuffItem>,
    pub buff_inventory_items: Vec<BuffItem>,

    // ─── tab3: 狀態 ───
    pub status_show_exp: bool,
    pub status_whetstone: bool,
    pub status_eat_meat: bool,
    pub status_transform_enabled: bool,
    pub status_transform_item: String,
    pub status_transform_cond: String,
    pub status_antidote_enabled: bool,
    pub status_antidote_item: String,
    pub fkey_macros: [FKeyMacro; 4],

    // ─── tab4: 刪物 ───
    #[serde(default)]
    pub delete_enabled: bool,
    /// 直接刪除清單 — 走 C_DELETE_ITEM opcode
    #[serde(default)]
    pub delete_list: Vec<String>,
    /// 溶解清單 — 走 0xA4 II opcode,需要背包有「溶解劑」
    #[serde(default)]
    pub dissolve_list: Vec<String>,

    // ─── tab5: 喊話 ───
    pub shout_enabled: bool,
    pub shout_interval_sec: u32,
    pub shout_messages: Vec<String>,

    // ─── tab6: 其他（24 項 toggle） ───
    pub misc: MiscToggles,

    // ─── tab7: 定時 ───
    pub timer_master_enabled: bool,
    pub timer_rows: [TimerRow; 6],
}
