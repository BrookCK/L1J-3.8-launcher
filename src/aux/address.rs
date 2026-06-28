//! 3.8 版輔助功能位址常數
//!
//! 為什麼集中在這檔案:避免位址 hardcode 到各 hook 模組裡造成搜尋困難。
//! 每個常數附信度等級,呼叫端可決定是否套用(★★ 以下禁止 Release 啟用)。
//!
//! 信度標示：
//!   ★★★★★ 多次運行時驗證
//!   ★★★★  靜態 + 動態驗證
//!   ★★★   單次驗證或反組譯確認
//!   ★★    候選位址(未經運行時驗證,禁止 Release 啟用)
//!   ★     純猜測(禁止使用)
//!
//! 已驗證位址來源:memory/game_database.md + hp_mp_patch.rs 啟動鏈
//! Option<u32> = None 表示位址尚未抓到,呼叫端需做 None 守衛 fail-soft

// ════════════════════════════════════════════════
// 已驗證 3.8 位址（直接套用，不需重抓）
// ════════════════════════════════════════════════

/// g_mapId：當前地圖 ID（dword）★★★★
pub const G_MAP_ID: u32 = 0x00965B60;

/// g_gameState：遊戲狀態（1=登入 2=選角 3=遊戲中 9=斷線）★★★★
pub const G_GAME_STATE: u32 = 0x009AB5E8;

/// g_lightLevel：光照等級 byte（全白天用，寫 0xFA）★★★★★

/// maxHP：最大 HP（dword，hp_mp_patch 已修為 32-bit）★★★★★
pub const MAX_HP: u32 = 0x00C31E90;

/// maxMP：最大 MP（dword）★★★★★
pub const MAX_MP: u32 = 0x00C31E8C;

// .data 角色屬性區塊(0xC31E80~0xC31E93) ★★★★★ (2026-04-28 驗證)
// 連續 6 byte 為六大屬性,實機 attach 比對使用者報告值已對齊
/// 正義值 Lawful — 2-byte signed short,負值代表 chaotic

/// g_playerPtr：玩家物件指標 ★★★★
pub const G_PLAYER_PTR: u32 = 0x00C2D2B8;

/// LOCAL_PLAYER_PTR — `click_attack` thiscall 的 `this` 來源(同 `G_PLAYER_PTR`)。 ★★★★★
///
/// 為什麼另立別名:`bootstrap_click_attack` shellcode 邏輯上「讀本地玩家指標當作 this」,
/// 跟 entity scan 的 `G_PLAYER_PTR` 是同一格記憶體但語意不同 — 別名讓 shellcode 程式碼
/// 自我說明。
pub const LOCAL_PLAYER_PTR: u32 = G_PLAYER_PTR;

// ════════════════════════════════════════════════
// click-attack bootstrap 用 globals(2026-05-16 RE 驗證 reverse-32-mcp)
// ════════════════════════════════════════════════

/// `[0xC2D2B4]` — CLICK_HANDLER 寫入的「當前攻擊目標 entity 指標」。 ★★★★★
///
/// 來源 instruction:`0x004F6019 mov [0xC2D2B4], ecx`(CLICK_HANDLER 內讀完 cursor →
/// hover entity 後寫入)。 `Function A`(auto-repeat)gate 之一:此值為 0 → 鏈斷。
pub const ATTACK_TARGET_PTR: u32 = 0x00C2D2B4;

/// `[0x9ABCA4]` — 攻擊目標的 raw X(對應 `target_entity+0x34`)。 ★★★★★
///
/// 來源:`0x004F600E call 0x40D510`(memcpy/setter)把 target+0x34 拷貝到此 global。
/// 純 packet trace 可讀性用,server 端忽略 — 寫入只是讓 client 內部狀態跟手點怪一致。
pub const ATTACK_TARGET_X: u32 = 0x009ABCA4;

/// `[0x9ABCA8]` — 攻擊目標的 Y(對應 `target_entity+0x38`)。 ★★★★★
pub const ATTACK_TARGET_Y: u32 = 0x009ABCA8;

/// `[0x9AB31C]` — CLICK_HANDLER mode byte(0=normal click,1/2=不同 click context)。 ★★★★★
///
/// bootstrap 一律寫 0(normal click)。 來源 instruction:`0x004F6037 / 0x004F6043 /
/// 0x004F604F mov [0x9AB31C], imm`(三個 click context 分支)。
pub const ATTACK_MODE_FLAG: u32 = 0x009AB31C;

/// `[0xAC450C]` — 客戶端 auto-attack 啟用旗標(byte)。 ★★★★★
///
/// `Function A`(auto-repeat scheduler 重排)gate:此 byte != 1 → 不再 enqueue 自己 → 鏈斷。
/// bootstrap 寫 1 開啟連打;shutdown / master toggle off 寫 0 切鏈。
/// 來源 instruction:`0x004F6CE1 / 0x004F6D1F mov byte ptr [0xAC450C], imm`(click handler
/// 內依 click 種類設 0 或 1)。
pub const AUTO_ATTACK_FLAG: u32 = 0x00AC450C;

/// `0x005A3770` — `click_attack` 初擊 dispatcher(thiscall,ret 0x10)。 ★★★★★
///
/// 原型:
/// ```c
/// __thiscall void click_attack(
///     Player* this,    // ECX = [LOCAL_PLAYER_PTR]
///     Entity* target,  // [ebp+8]
///     int x,           // [ebp+0xC]  (target+0x34)
///     int y,           // [ebp+0x10] (target+0x38)
///     int mode         // [ebp+0x14] (0=normal)
/// );
/// ```
///
/// 內建:
/// - CD gate(`cmp ecx, [CLICK_ATTACK_NEXT_TICK]; jb skip`)— 還沒到時間 → 直接 ret。
/// - weapon dispatch(target+0x14 byte ∈ {0x2C, 0x2D} → ranged 0x7B,否則 melee 0xE5)。
/// - 播放動畫 + 透過 SendPacketData 送 0xE5 / 0x7B cdhh packet 給 server。
/// - 更新 `[CLICK_ATTACK_NEXT_TICK] = current_tick + 武器攻擊間隔`,並把 `Function A`
///   重排進 scheduler queue 接手連打。
///
/// 用法:`bootstrap_click_attack` 寫完 5 個 globals 之後,組一段 shellcode
/// `push 0; push y; push x; push target_ptr; mov ecx, this; call 0x5A3770` 由
/// `CreateRemoteThread` 跑一次,client 就會自己連打到 target 死或 `[AUTO_ATTACK_FLAG]=0`。
pub const CLICK_ATTACK_FUNC: u32 = 0x005A3770;

/// `[0xC2D27C]` — 客戶端 auto-attack 「下次允許攻擊」tick(GetTickCount 時間戳)。 ★★★★★
///
/// `Function A` / `Function B` 內部 CD gate 比的就是這個 — `cmp ecx, [0xC2D27C]; jb skip`。
/// 穩態下 client 根據武器 + haste + debuff 自己算攻擊間隔並更新此值,bot 不該碰。
///
/// **bootstrap 時要寫 0**:切到新 target 時舊 CD 還停在未來(剛打過 A 的攻擊間隔尚未到),
/// Function B 一進來 CD gate 直接 `jb skip` ret 掉,不送 packet 也不 enqueue Function A,
/// 整個 chain 啟動不了。 寫 0 後 `cmp ecx, 0` 必定 `>=` → CD pass → 第一發立刻 fire。
/// 之後 Function B 會把這格更新成 `now + weapon_interval`,steady-state CD 仍由 client 管控。
pub const CLICK_ATTACK_NEXT_TICK: u32 = 0x00C2D27C;

/// g_hasteBuffTable：加速 buff 表（4 bytes）★★★★
pub const G_HASTE_BUFF_TABLE: u32 = 0x00ABF4C8;

/// 玩家職業 byte ★★★★★ (0=君主, 1=皇族, 2=戰士, 3=法師, 4=妖精, 6=幻術師)
///
/// 為什麼需要:apply_buff(0x4EE400) 在 ~10 個 packet 路徑會依職業套不同 byte_table state_id。
/// 例 sub_type=4 buff(行走加速):戰士寫 byte[24],法師寫 byte[42],其他職業寫 byte[37]。
/// 來源 INI 寫的「行走加速=37」是「其他職業」視角,法師玩家上需 remap 成 42 才能正確
/// 偵測既有 buff 避免重複施放。完整對映表見 [`crate::aux::class_remap`]。
pub const G_CLASS: u32 = 0x00C31544;

/// SendPacketData：高層封包發送函數 ★★★★★
pub const SEND_PACKET_DATA: u32 = 0x00580E50;

/// ProcessPacket：封包分發入口 ★★★★★

/// cast_magic：施法 dispatcher(高層) ★★★★★ (2026-04-28 驗證,加速術 packed=42)
///
/// 原型:`void __cdecl cast_magic(u32 packed_skill_id, u8 byte_flag)`
/// - `packed_skill_id` = spell DB 陣列 index(見 [`SPELL_DB_PTR`])
/// - `byte_flag` = 0 一般單體/自身,非 0 為 AOE/範圍模式
///
/// 內部依遊戲狀態自動分派(無目標→自身,鎖目標→指定目標,範圍→AOE)。
/// **launcher 不直接 call 此函數** — self path 在 0x73C449 走 `is_ready` 檢查
/// `[magic_info+8]` 必須非 0(玩家點過技能書才會),launcher 條件不符會被擋下。
/// 改走 [`DO_CAST`] 直通方案。

/// spell_book::cast (thiscall) ★★★★★ (2026-04-28 實機追蹤鎖定)
///
/// 原型:`void __thiscall spell_book_cast(spell_book* this, u32 packed, u32 byte_flag)`
///   ret 8 (callee 自清)
///
/// 這是**手動施法的真正入口** — 跟玩家點技能書再點目標走的同一條路徑:
/// 1. 走訪 `this->m_spells[i]`(`this+0x58` 陣列,`this+0x2C` 數量)找 `entry+4 == packed`
/// 2. 驗證 MP/range/狀態
/// 3. 設定 `[magic_info+5/+C/+10]` 等 begin_cast 欄位
/// 4. 透過 `SendPacketData` 送出 cast request 封包到 server
/// 5. server 套 buff + 廣播動畫封包回來 — 客戶端動畫由那條 path 觸發
///
/// 直接 call do_cast (0x734A90) 不會觸發以上整條鏈;它只是 setter,
/// 主迴圈 magic_tick (0x734AC0) 處理時若 `[+8]` (target_id) 為 0 就 reset 走人。
///
/// `byte_flag = 1` 是手動 manual cast 用的標誌;`= 0` 是 magic_tick 自呼叫(begin_cast 後)。
/// launcher 應該用 1。
pub const SPELL_BOOK_CAST: u32 = 0x0073ECE0;

/// 玩家的 spell_book 物件指標 (heap)— `this` 參數來源 ★★★★★
///
/// `[0xC31324]` 在進場後填入,跨重啟 heap addr 會變但**陣列內容不變**。
pub const SPELL_BOOK_PTR: u32 = 0x00C31324;

/// spell DB 指標 ★★★★★ (2026-04-28 驗證)
///
/// `[SPELL_DB_PTR]` = heap 上的指標陣列基址,index = packed_skill_id,
/// 每 entry 4 bytes 指向 spell record(以 Big5 技能名開頭,後接 metadata)。
///
/// 用法:scan 0..256,讀名稱配對 INI [AllState] 條目,得 `state_id → packed` 對映。
pub const SPELL_DB_PTR: u32 = 0x009A8ED4;

// ════════════════════════════════════════════════
// 角色狀態位址(補水系統用)
// ════════════════════════════════════════════════

/// 當前 HP — XOR 加密物件(12 bytes)位址 ★★★★★(2026-04-28 驗證)
///
/// 物件結構:
/// - this+0 (4B) = encrypted_index(XOR with [`STAT_XOR_MAGIC`] 取得 0..15 範圍 plain_index)
/// - this+4 (4B) = key_array_ptr(指向 heap 上的 16 個 dword 金鑰陣列)
/// - this+8 (4B) = salt(每次 setter 呼叫重新隨機)
///
/// 解密公式:
/// ```text
/// plain_idx = read_u32(this+0) ^ STAT_XOR_MAGIC
/// key_ptr   = read_u32(this+4)
/// salt      = read_u32(this+8)
/// enc_value = read_u32(key_ptr + plain_idx*4)
/// value     = enc_value ^ salt
/// ```
///
/// XOR 加密 setter 在 0x579E10,reflection 自 hp_mp_patch.rs (S_STATUS curHP→setter)。
pub const CURRENT_HP_OBJ: u32 = 0x00BDC828;
/// 當前 MP — 同上 XOR 加密物件 ★★★★★(2026-04-28 驗證)
pub const CURRENT_MP_OBJ: u32 = 0x00BDC834;
/// XOR 解密用的常數 magic value
pub const STAT_XOR_MAGIC: u32 = 0xC001_7921;

/// 飽食度 raw byte ★★★★★(2026-04-28 驗證:127→52%, 130→57% 對應公式)
///
/// 顯示百分比 = `raw * 100 / 225`
/// 在 .data 區的角色屬性區塊內,跟 STR/INT/WIS/DEX/CON/CHA 同連續 6 byte 之後
pub const FOOD_LEVEL: Option<u32> = Some(0x00C31E8B);
/// 飽食度顯示用分母 — `raw / 225 * 100` 得百分比顯示
pub const FOOD_LEVEL_DIVISOR: u32 = 225;

/// 負重度 raw byte ★★★★★(2026-04-28 驗證)
///
/// 顯示百分比 = `raw * 100 / 240`
pub const CURRENT_WEIGHT: Option<u32> = Some(0x00C31E8A);
/// 負重度顯示用分母
pub const WEIGHT_DIVISOR: u32 = 240;

/// 當前經驗值 — 3.8 位址待重抓
///
/// 為什麼空著:exp_tracker 目前讀 `0x00C31EA4` cumulative,即時 delta exp 可以
/// 從 cumulative 差分算出,不一定需要此常數。若未來要做 exp/小時即時顯示再補。

/// 物品欄基址 ★★★★ (2026-04-28 Ghidra 靜態驗證)
///
/// 結構:`[INVENTORY_BASE]` = 物品欄物件指標(heap)
/// - inv+0x2C (i32) = 物品數量
/// - inv+0x58 (ptr) = 物品指標陣列(每元素 4 bytes,指向 item_entry)
/// - 每個 item_entry 結構參考 `inventory.rs::ITEM_*` 偏移
///
/// 出處:0x40F915 `MOV ECX, [0x9A7230]` → 0x40F91B `CALL 0x4B1E50`(item lookup),
/// 0x4B1E50 內 `[this+0x2C]` 走訪 `[this+0x58][i*4]` 比對 `[item+4]==item_param`
pub const INVENTORY_BASE: Option<u32> = Some(0x009A9250);

/// item_entry 結構偏移
pub mod item_offset {
    /// 物品 ID(server-assigned param,4 bytes)— FUN_004B1E50 用此找物品
    /// 是否存在(1=有效)
    /// 是否裝備中(1=已裝備)— FUN_004B41C0 過濾條件 `[+8]!=0 && [+9]==0`
    pub const EQUIPPED: u32 = 0x09;
    /// 物品名稱字串指標
    /// 物品類型 byte(switch dispatcher 的 case key,例如 0x01=藥水)
    pub const ITEM_TYPE: u32 = 0x98;
    /// 動畫 / icon 編號(short)
    pub const ICON_NUM: u32 = 0x9A;
    /// 堆疊數量 dword ★★★★★(2026-05-02 Frida 驗證)
    ///
    /// 對 stack 物品:= 當前堆疊數量(例 綠色藥水 365 → +0xA0 = 0x16D)。
    /// 對非堆疊物品:= 0 或 1,送 delete 時直接代進 quantity 欄位。
    ///
    /// **3.8 踩過的坑**:`entry+0xA4` 在 3.8 是 enchant level / charges 不是 count,
    /// 若誤用 +0xA4 當數量送 delete 會刪錯數量。必須用 +0xA0 才會被 server
    /// 正確識別為「整疊刪」:`SendPacketData("cdd", 0x8A, obj_id, [+0xA0])`。
    pub const ITEM_COUNT: u32 = 0xA0;
}

// ════════════════════════════════════════════════
// 封包 Opcode（C→S）
// ════════════════════════════════════════════════
// 來源：memory/opcode_tables.md

/// C_USE_ITEM / II packet opcode observed from this client.
///
/// L1J source tables are not authoritative for this packed client build; local
/// SendPacketData spy captures item-use packets as opcode 0xA4.
pub const C_USE_ITEM: Option<u8> = Some(0xA4);
/// C_Teleport ACK opcode.
///
/// L1J 3.80 `C_Teleport` has no payload and completes a pending server-side
/// teleport created by `S_Teleport`.
pub const C_TELEPORT: Option<u8> = Some(0x34);
/// C_SKILL 技能施放 opcode
/// C_DELETE_ITEM 刪除物品 opcode ★★★★★ (2026-05-02 Frida capture 確認)
///
/// 觸發路徑:玩家把背包道具拖到背包視窗右下角的「垃圾桶」icon → 直接從 server 移除。
/// (跟「丟到地上」是不同封包 — 丟地上是 drop,垃圾桶是 delete。)
///
/// SendPacketData 格式:
/// ```text
/// SendPacketData("cdd", 0x8A, item.item_param, 0)
/// ```
/// - c (1B) = opcode 0x8A
/// - d (4B) = item.item_param (obj_id)
/// - d (4B) = 0 (推測為 quantity,垃圾桶整疊刪所以填 0)
///
/// Frida capture 範例(2026-05-02):
///   `#13 fmt='cdd' opcode=0x8a args=[0x1dcd6aa8 0x00000000 ...]`
pub const C_DELETE_ITEM: Option<u8> = Some(0x8A);
/// C_REFINE 精煉/合成 opcode（已知）★★★★★

/// C_CHAT 喊話 / 一般聊天 opcode ★★★★★(2026-05-02 Frida capture 確認)
///
/// fmt `"ccs"` — opcode + channel byte + null-terminated 訊息字串(Big5 編碼)。
/// channel 值:
/// - `0x00` = 一般訊息(綠字,區域內) ← `shout_tick` 實際使用此 channel
/// - `0x02` = 喊話(全頻廣播)
///
/// 同 opcode 也用在 dialog reply(`0x88 ccs [button, typed_text]`),
/// 是泛用「client text input」封包,server 依 channel/context byte 分流。
pub const C_CHAT_OPCODE: u8 = 0x88;

/// 喊話 channel byte

/// 一般對話 channel byte ← shout_tick 實際使用此 channel
pub const CHAT_CHANNEL_NORMAL: u8 = 0x00;
