//! 決策層 — 把 perception 觀察 + action 動作組合成 state machine 規則。
//!
//! ## 模組分工
//!
//! - **target.rs**: 從 WorldView 選目標(Phase 1 用白名單;Phase 1 step 5 RE 怪物 vfptr 後升級)
//! - **hunt.rs**: hunt 子狀態的 tick body(選目標 → 攻擊 → cooldown 等)
//! - return_home.rs: 何時觸發回家(Phase 2)
//! - resupply.rs: 何時觸發補貨(Phase 4)
//!
//! ## 設計原則
//!
//! - 決策層**不直接** read memory / send packet —— 全部透過 perception(讀) + action(送)
//! - 純函數優先(testability):pure decision + impure execution 分開
//! - tick 之間的狀態(last_cast Instant 等)由 caller(engine.rs)管理,decide
//!   只暴露 `tick(snapshot, &mut state) -> Outcome` 樣式 API

pub mod hunt;
pub mod pathfind;
