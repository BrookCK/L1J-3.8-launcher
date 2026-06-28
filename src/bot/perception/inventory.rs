//! 物品欄觀察 — 包裝 `aux::inventory::list_items`,加 bot 常用查詢。
//!
//! 不重複實作走訪邏輯(aux 已驗證 stable),只在 raw `Vec<Item>` 上面跑 filter。

use anyhow::Result;
use windows::Win32::Foundation::HANDLE;

use crate::aux::inventory::{list_items, Item};

/// 物品欄觀察快照。 一個 tick 內可重複用 `count_*` 函數查不同條件。
#[derive(Debug, Clone)]
pub struct InventoryView {
    items: Vec<Item>,
}

impl InventoryView {
    pub fn read(h: HANDLE) -> Result<Self> {
        let items = list_items(h)?;
        Ok(Self { items })
    }

    /// 目前物品數(stack 物算 1 格)
    pub fn slot_count(&self) -> usize {
        self.items.len()
    }

    /// 指定 `item_param`(server-side item ID)的物品總數。
    /// stack 物會把 `count` 加起來;非 stack 物每格算 1。
    pub fn total_by_param(&self, item_param: u32) -> u64 {
        self.items
            .iter()
            .filter(|it| it.item_param == item_param)
            .map(|it| if it.count == 0 { 1 } else { it.count as u64 })
            .sum()
    }

    /// 名稱完全相等的物品總數(用於使用者按名稱輸入消耗品)。
    pub fn total_by_name(&self, query: &str) -> u64 {
        self.items
            .iter()
            .filter(|it| it.name_lossy() == query)
            .map(|it| if it.count == 0 { 1 } else { it.count as u64 })
            .sum()
    }

    /// 提供原始物品列表 — 偵錯 / UI 顯示用
    pub fn items(&self) -> &[Item] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(item_param: u32, name: &str, count: u32) -> Item {
        Item {
            entry_addr: 0,
            item_param,
            item_type: 0,
            icon: 0,
            equipped: false,
            count,
            name_raw: name.as_bytes().to_vec(),
        }
    }

    fn view(items: Vec<Item>) -> InventoryView {
        InventoryView { items }
    }

    #[test]
    fn slot_count_returns_item_vec_len() {
        let v = view(vec![
            item(40308, "紅色藥水", 50),
            item(40309, "藍色藥水", 30),
        ]);
        assert_eq!(v.slot_count(), 2);
    }

    #[test]
    fn total_by_param_sums_stack_counts() {
        let v = view(vec![
            item(40308, "紅色藥水", 50),
            item(40308, "紅色藥水", 30),
            item(40309, "藍色藥水", 99),
        ]);
        assert_eq!(v.total_by_param(40308), 80);
        assert_eq!(v.total_by_param(40309), 99);
        assert_eq!(v.total_by_param(40310), 0);
    }

    #[test]
    fn total_by_param_treats_zero_count_as_one() {
        // 非 stack 物(裝備、卷軸)count 可能是 0,代表「1 個」
        let v = view(vec![item(40308, "回家卷軸", 0), item(40308, "回家卷軸", 0)]);
        assert_eq!(v.total_by_param(40308), 2);
    }

    #[test]
    fn total_by_name_exact_match() {
        let v = view(vec![
            item(40308, "紅色藥水", 50),
            item(40310, "藍色藥水", 30),
        ]);
        assert_eq!(v.total_by_name("紅色藥水"), 50);
        // 部分匹配不算
        assert_eq!(v.total_by_name("紅色"), 0);
    }
}
