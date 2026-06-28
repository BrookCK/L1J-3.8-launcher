//! 多開數量限制(防外掛)— 用 N 把具名 mutex `Global\L38MultiSlot_<i>` 當 slot。
//!
//! 只算本登入器啟動的遊戲(只有本程式碰這些 mutex 名),不分 server(名不含 IP/port)。
//! 搶到 slot 的 launcher 持有 SlotGuard 直到遊戲結束(stage1 等 game process)。
//! mutex ownership 綁 thread:process 死 → abandoned → 下個 waiter 拿 WAIT_ABANDONED
//! 仍成功 → slot 自癒;acquire 為原子 → race-safe。

use std::iter::once;

use crate::logger::log_line;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK, MB_SYSTEMMODAL};

const SLOT_PREFIX: &str = "Global\\L38MultiSlot_";

/// 搶 slot 的結果。
pub enum SlotOutcome {
    /// limit==0,無限多開,未持有任何 slot。
    Unlimited,
    /// 搶到一個 slot,guard 在 drop 時釋放。
    Acquired(SlotGuard),
    /// 全部 `limit` 個 slot 已被佔住。
    Full(u32),
}

/// 持有一把 slot mutex,drop 時 ReleaseMutex + CloseHandle。
pub struct SlotGuard {
    handle: HANDLE,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// 由設定算有效上限:未允許多開 → 1(單開);允許多開 → 直接用設定值(0=無限)。
pub fn effective_limit(multi_instance: bool, multi_instance_limit: u32) -> u32 {
    if !multi_instance {
        1
    } else {
        multi_instance_limit
    }
}

/// 依有效上限搶 slot(production 用 Global 命名)。
pub fn acquire_launch_slot(limit: u32) -> SlotOutcome {
    acquire_slot_with_prefix(SLOT_PREFIX, limit)
}

fn acquire_slot_with_prefix(prefix: &str, limit: u32) -> SlotOutcome {
    if limit == 0 {
        return SlotOutcome::Unlimited;
    }
    for i in 0..limit {
        if let Some(guard) = try_acquire_one(&format!("{prefix}{i}")) {
            return SlotOutcome::Acquired(guard);
        }
    }
    SlotOutcome::Full(limit)
}

fn try_acquire_one(name: &str) -> Option<SlotGuard> {
    let wide: Vec<u16> = name.encode_utf16().chain(once(0)).collect();
    unsafe {
        // CreateMutexW 失敗(權限/資源)不可與「被佔住」混為一談,否則會誤判 Full;
        // 記 warn 後一樣回 None,但留下可診斷的線索。
        let handle = match CreateMutexW(None, false, PCWSTR(wide.as_ptr())) {
            Ok(h) => h,
            Err(e) => {
                log_line!("[multi-limit] WARN CreateMutexW({name}) 失敗: {e:#}");
                return None;
            }
        };
        let wait = WaitForSingleObject(handle, 0);
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            Some(SlotGuard { handle })
        } else {
            let _ = CloseHandle(handle);
            None
        }
    }
}

/// 達上限時的提示視窗。
pub fn show_limit_reached_message(limit: u32) {
    let text: Vec<u16> = format!("已達多開上限 {limit},無法再啟動遊戲。")
        .encode_utf16()
        .chain(once(0))
        .collect();
    let caption: Vec<u16> = "防外掛".encode_utf16().chain(once(0)).collect();
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OK | MB_ICONWARNING | MB_SYSTEMMODAL,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_limit_rules() {
        assert_eq!(effective_limit(false, 0), 1); // 未允許多開 → 單開
        assert_eq!(effective_limit(false, 5), 1); // 未勾時忽略數字
        assert_eq!(effective_limit(true, 0), 0); // 允許 + 0 → 無限
        assert_eq!(effective_limit(true, 3), 3); // 允許 + N → N
    }

    #[test]
    fn unlimited_returns_without_handle() {
        assert!(matches!(
            acquire_slot_with_prefix("Local\\L38TestUnlimited_", 0),
            SlotOutcome::Unlimited
        ));
    }

    #[test]
    fn slots_fill_across_threads_then_free() {
        use std::sync::mpsc::channel;
        use std::sync::{Arc, Barrier};
        use std::thread;

        let prefix = "Local\\L38TestSlotsFill_";
        let limit = 2u32;

        let (acq_tx, acq_rx) = channel::<bool>();
        // limit 個 worker + main 一起在 barrier 會合,確保 main 檢查 Full 時
        // 所有 worker 仍持有 slot(無 sleep,確定性)。
        let barrier = Arc::new(Barrier::new((limit + 1) as usize));

        let mut handles = Vec::new();
        for _ in 0..limit {
            let acq_tx = acq_tx.clone();
            let barrier = Arc::clone(&barrier);
            let prefix = prefix.to_string();
            handles.push(thread::spawn(move || {
                match acquire_slot_with_prefix(&prefix, limit) {
                    SlotOutcome::Acquired(guard) => {
                        acq_tx.send(true).unwrap();
                        barrier.wait(); // 持有 slot 等 main 完成 Full 檢查
                        drop(guard);
                    }
                    _ => {
                        acq_tx.send(false).unwrap();
                        barrier.wait();
                    }
                }
            }));
        }

        // 兩條 thread 都搶到
        assert!(acq_rx.recv().unwrap());
        assert!(acq_rx.recv().unwrap());

        // 此刻所有 slot 都被 worker 持有 → main 必為 Full
        assert!(matches!(
            acquire_slot_with_prefix(prefix, limit),
            SlotOutcome::Full(2)
        ));

        // 放行 worker 釋放 slot
        barrier.wait();
        for h in handles {
            h.join().unwrap();
        }

        // 釋放後 main 應能再搶到
        assert!(matches!(
            acquire_slot_with_prefix(prefix, limit),
            SlotOutcome::Acquired(_)
        ));
    }
}
