//! 場上實體觀察 — 包裝 `aux::entity_scan::list_all_entities`。
//!
//! ## 2026-05-14 重構:範圍狩獵模式
//!
//! 從「按使用者填入的怪物名白名單比對」改成「**範圍內任何 entity**」。 vfptr
//! `0x008DC08C` 涵蓋 LOCAL + REMOTE + avatar + 召喚物 + 怪物(實機驗證,memory
//! note `entity_target_id_layout` 標 NPC/怪物「待驗」是過時的保守註解)。
//!
//! 過濾規則:
//! - target_id > 0x10000 → 跳掉 local player(自己 char_id 小於 0x10000)
//! - distance ≤ hunt_range_tiles → 限制範圍,避免遠處怪一路追過去
//! - name 前綴比對 blacklist → 排除特定 entity(NPC、隊友召喚物等)
//!
//! NPC 在 server 端通常會拒收攻擊 packet(浪費一個 packet 但無害),所以不特別過濾。
//! 遠端玩家也會被掃到 — 在 farming 區罕見,真要排除可加進 blacklist。

use anyhow::Result;
use windows::Win32::Foundation::HANDLE;

use crate::aux::entity_scan::{list_all_entities, ScannedEntity};
#[cfg(test)]
use crate::bot::perception::classifier::{is_attackable_monster, EntityFacts};
#[cfg(test)]
use crate::bot::perception::position::{decode_x, PlayerPosition};

/// 場上實體觀察快照
#[derive(Debug, Clone)]
pub struct WorldView {
    entities: Vec<ScannedEntity>,
}

impl WorldView {
    /// 列舉所有 vfptr-matched 實體
    pub fn read(h: HANDLE) -> Result<Self> {
        Ok(Self {
            entities: list_all_entities(h),
        })
    }

    /// 名字完全相符的第一個實體 — UI / diagnostic 用。
    #[cfg(test)]
    pub fn first_by_name(&self, query: &str) -> Option<&ScannedEntity> {
        self.entities.iter().find(|e| e.name == query)
    }

    /// 範圍狩獵候選列表 — 距離玩家 ≤ `range_tiles` 的 entity,跳過 local player 跟 blacklist。
    ///
    /// 距離用 Chebyshev(8-direction step distance,跟遊戲移動規則一致)。
    /// blacklist 用 `starts_with` 前綴比對:同時擋短名 `"史萊姆"` 跟詳名 `"史萊姆#45060:998"`。
    ///
    /// **lifetime 設計**:回傳的 `&ScannedEntity` 只跟 `self` 綁定,不跟 `blacklist`
    /// 綁。 內部把 blacklist trim + 過濾空白後 clone 進 closure,caller 傳臨時值
    /// (e.g. `&vec![]`)也安全。
    #[cfg(test)]
    pub fn list_in_range<'a>(
        &'a self,
        player: PlayerPosition,
        range_tiles: u32,
        blacklist: &[String],
    ) -> impl Iterator<Item = &'a ScannedEntity> + 'a {
        let blacklist: Vec<String> = blacklist
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        self.entities.iter().filter(move |e| {
            if e.target_id < 0x10000 {
                return false;
            }
            if !is_attackable_monster(EntityFacts {
                target_id: e.target_id,
                sprite_id: e.sprite_id,
                action_state: e.action_state,
                name: &e.name,
            }) {
                return false;
            }
            if chebyshev_to_entity(player, e) > range_tiles {
                return false;
            }
            for b in &blacklist {
                if e.name.starts_with(b.as_str()) {
                    return false;
                }
            }
            true
        })
    }

    /// 提供原始實體列表 — 偵錯 / UI 顯示用
    pub fn entities(&self) -> &[ScannedEntity] {
        &self.entities
    }

    /// **僅 test 使用** — 直接從 entity 列表組 WorldView,跳過記憶體讀取。
    #[cfg(test)]
    pub fn __for_test(entities: Vec<ScannedEntity>) -> Self {
        Self { entities }
    }
}

/// 8-direction step distance(對角線算 1 格,跟遊戲移動規則一致)
#[cfg(test)]
fn chebyshev_to_entity(player: PlayerPosition, e: &ScannedEntity) -> u32 {
    let ex = decode_x(e.raw_x);
    let ey = e.y as i32;
    (ex - player.x)
        .unsigned_abs()
        .max((ey - player.y).unsigned_abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// test helper — sprite_id 預設 2489(SPR.txt 內 `102.(10)` MOB)
    /// 讓 `list_in_range` 的 `is_mob` 過濾不會誤殺;要驗非 MOB 場景的測試自己用
    /// `entity_with_sprite` 指定 type=0 等。
    fn entity(target_id: u32, name: &str, raw_x: u32, y: u32) -> ScannedEntity {
        entity_with_sprite(target_id, name, 2489, raw_x, y)
    }

    fn entity_with_sprite(
        target_id: u32,
        name: &str,
        sprite_id: u16,
        raw_x: u32,
        y: u32,
    ) -> ScannedEntity {
        ScannedEntity {
            addr: 0,
            target_id,
            name: name.to_string(),
            sprite_id,
            action_state: 0,
            raw_x,
            y,
        }
    }
    fn view(entities: Vec<ScannedEntity>) -> WorldView {
        WorldView { entities }
    }
    fn enc(display_x: i32) -> u32 {
        crate::bot::perception::position::encode_x(display_x)
    }

    #[test]
    fn first_by_name_exact_match() {
        let v = view(vec![entity(1, "邪靈", 0, 0), entity(2, "邪靈", 0, 0)]);
        assert_eq!(v.first_by_name("邪靈").unwrap().target_id, 1);
    }

    #[test]
    fn list_in_range_skips_local_player() {
        let player = PlayerPosition { x: 33000, y: 33000 };
        let v = view(vec![
            entity(0xC8, "Self", enc(33000), 33000), // local player tid < 0x10000
            entity(0x0BEBC2A1, "史萊姆", enc(33002), 33000), // monster, 2 tile away
        ]);
        let blk: Vec<String> = vec![];
        let ids: Vec<u32> = v
            .list_in_range(player, 10, &blk)
            .map(|e| e.target_id)
            .collect();
        assert_eq!(ids, vec![0x0BEBC2A1]);
    }

    #[test]
    fn list_in_range_filters_by_distance() {
        let player = PlayerPosition { x: 33000, y: 33000 };
        let v = view(vec![
            entity(0x0BEBC2A1, "近", enc(33005), 33000), // 5 tile away
            entity(0x0BEBC2A2, "邊界", enc(33010), 33000), // 10 tile away — 邊界,還算進
            entity(0x0BEBC2A3, "遠", enc(33011), 33000), // 11 tile away — 出範圍
        ]);
        let blk: Vec<String> = vec![];
        let ids: Vec<u32> = v
            .list_in_range(player, 10, &blk)
            .map(|e| e.target_id)
            .collect();
        assert_eq!(ids, vec![0x0BEBC2A1, 0x0BEBC2A2], "10 tile 含邊界,11 排除");
    }

    #[test]
    fn list_in_range_chebyshev_diagonal() {
        // 對角線距離以 Chebyshev = max(|dx|, |dy|) 計算
        let player = PlayerPosition { x: 33000, y: 33000 };
        let v = view(vec![
            entity(0x0BEBC2A1, "對角 5", enc(33005), 33005), // Chebyshev=5
            entity(0x0BEBC2A2, "對角 11", enc(33011), 33011), // Chebyshev=11
        ]);
        let blk: Vec<String> = vec![];
        let ids: Vec<u32> = v
            .list_in_range(player, 10, &blk)
            .map(|e| e.target_id)
            .collect();
        assert_eq!(ids, vec![0x0BEBC2A1]);
    }

    #[test]
    fn list_in_range_blacklist_filters_by_prefix() {
        let player = PlayerPosition { x: 33000, y: 33000 };
        let v = view(vec![
            entity(0x0BEBC2A1, "史萊姆", enc(33001), 33000),
            entity(0x0BEBC2A2, "史萊姆#1234:5", enc(33002), 33000), // 詳名 starts_with 史萊姆
            entity(0x0BEBC2A3, "兔子", enc(33003), 33000),
            entity(0x0BEBC2A4, "城衛兵", enc(33004), 33000),
        ]);
        let blk = vec!["史萊姆".to_string(), "城衛兵".to_string()];
        let ids: Vec<u32> = v
            .list_in_range(player, 30, &blk)
            .map(|e| e.target_id)
            .collect();
        assert_eq!(
            ids,
            vec![0x0BEBC2A3],
            "只剩兔子,史萊姆/城衛兵 都被 blacklist 擋"
        );
    }

    #[test]
    fn list_in_range_blank_blacklist_entries_ignored() {
        let player = PlayerPosition { x: 33000, y: 33000 };
        let v = view(vec![entity(0x0BEBC2A1, "史萊姆", enc(33001), 33000)]);
        let blk = vec!["".to_string(), "   ".to_string()];
        let ids: Vec<u32> = v
            .list_in_range(player, 10, &blk)
            .map(|e| e.target_id)
            .collect();
        assert_eq!(ids, vec![0x0BEBC2A1], "空 / 全空白 blacklist 不應命中");
    }

    #[test]
    fn list_in_range_zero_range_returns_empty() {
        // hunt_range = 0 是 NotConfigured 的訊號,理論上 hunt::tick 會早退;
        // 但 list 自己也應該回空(distance > 0 都不可能 <= 0)
        let player = PlayerPosition { x: 33000, y: 33000 };
        let v = view(vec![entity(0x0BEBC2A1, "史萊姆", enc(33001), 33000)]);
        let blk: Vec<String> = vec![];
        let ids: Vec<u32> = v
            .list_in_range(player, 0, &blk)
            .map(|e| e.target_id)
            .collect();
        assert!(ids.is_empty());
    }

    #[test]
    fn list_in_range_skips_torch_sprite_85() {
        // 路燈 sprite_id=85 → SPR.txt 寫 102.(0) 影子類 → 不該打。
        // dump_classify.log 實證:`#81177:85` 的兩根燈 BOT 之前會一直追過去打。
        let player = PlayerPosition { x: 33000, y: 33000 };
        let torch = entity_with_sprite(0x0BEBC2A1, "#81177:85", 85, enc(33002), 33000);
        let mob = entity_with_sprite(0x0BEBC2A2, "邪靈", 2489, enc(33004), 33000);
        let v = view(vec![torch, mob]);
        let blk: Vec<String> = vec![];
        let ids: Vec<u32> = v
            .list_in_range(player, 30, &blk)
            .map(|e| e.target_id)
            .collect();
        assert_eq!(ids, vec![0x0BEBC2A2], "路燈跳掉,只剩怪");
    }

    #[test]
    fn list_in_range_skips_magic_doll_sprite_range() {
        let player = PlayerPosition { x: 33000, y: 33000 };
        let doll = entity_with_sprite(0x0BEBC2A1, "magic doll", 12277, enc(33002), 33000);
        let mob = entity_with_sprite(0x0BEBC2A2, "?芷?", 2489, enc(33004), 33000);
        let v = view(vec![doll, mob]);
        let blk: Vec<String> = vec![];
        let ids: Vec<u32> = v
            .list_in_range(player, 30, &blk)
            .map(|e| e.target_id)
            .collect();
        assert_eq!(ids, vec![0x0BEBC2A2]);
    }

    #[test]
    fn list_in_range_skips_unknown_sprite_id() {
        // sprite_id=0(SPR.txt 沒寫,影子重複 slot 常見)→ 不打
        let player = PlayerPosition { x: 33000, y: 33000 };
        let phantom = entity_with_sprite(0x0BEBC2A1, "", 0, enc(33002), 33000);
        let v = view(vec![phantom]);
        let blk: Vec<String> = vec![];
        let ids: Vec<u32> = v
            .list_in_range(player, 30, &blk)
            .map(|e| e.target_id)
            .collect();
        assert!(ids.is_empty(), "未知 sprite_id 不該打");
    }

    #[test]
    fn list_in_range_skips_blank_named_mob_slot() {
        let player = PlayerPosition { x: 33000, y: 33000 };
        let phantom = entity_with_sprite(0x0BEBC2A1, "", 2489, enc(33002), 33000);
        let mob = entity_with_sprite(0x0BEBC2A2, "奇岩盜賊", 2489, enc(33004), 33000);
        let v = view(vec![phantom, mob]);
        let blk: Vec<String> = vec![];
        let ids: Vec<u32> = v
            .list_in_range(player, 30, &blk)
            .map(|e| e.target_id)
            .collect();
        assert_eq!(ids, vec![0x0BEBC2A2], "空名字 MOB slot 不可成為 BOT 目標");
    }

    #[test]
    fn list_in_range_skips_magic_doll_mob_sprite() {
        let player = PlayerPosition { x: 33000, y: 33000 };
        let doll = entity_with_sprite(0x0BEBC2A1, "magic doll", 12268, enc(33002), 33000);
        let mob = entity_with_sprite(0x0BEBC2A2, "mob", 2489, enc(33004), 33000);
        let v = view(vec![doll, mob]);
        let blk: Vec<String> = vec![];
        let ids: Vec<u32> = v
            .list_in_range(player, 30, &blk)
            .map(|e| e.target_id)
            .collect();
        assert_eq!(ids, vec![0x0BEBC2A2]);
    }

    #[test]
    fn list_in_range_skips_decoration_sprite_710() {
        // sprite_id=710 → SPR.txt 102.(0) 影子類(Group C 裝飾 dump 實證之一)
        let player = PlayerPosition { x: 33000, y: 33000 };
        let deco = entity_with_sprite(0x40000001, "", 710, enc(33002), 33000);
        let v = view(vec![deco]);
        let blk: Vec<String> = vec![];
        let ids: Vec<u32> = v
            .list_in_range(player, 30, &blk)
            .map(|e| e.target_id)
            .collect();
        assert!(ids.is_empty(), "靜態裝飾(type=0)不該打");
    }
}
