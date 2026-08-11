//! Kairos AI 模型网关库。
//!
//! 语义边界：`core`（协议内核）、`gateway`（HTTP 服务）、`store`（SQLite）。
//! 当前票只落地 `gateway` 与 `store` 的技术栈冒烟验证。

pub mod config;
pub mod gateway;
pub mod store;
