//! Toggle 功能（待實現）

pub mod all_day;
pub mod underwater_pump;

use windows::Win32::Foundation::HANDLE;

#[allow(dead_code)] // is_safe/name 為 default method,暫未呼叫,保留
pub trait Toggle {
    fn enable(&self, h: HANDLE) -> anyhow::Result<()>;
    fn disable(&self, h: HANDLE) -> anyhow::Result<()>;
    fn is_safe(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        ""
    }
}
