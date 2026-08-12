//! Kairos AI 模型网关库。
//!
//! 语义边界：`core`（协议内核）、`gateway`（HTTP 服务）、`store`（SQLite）、
//! `runtime`（运行时资源内存快照）。

pub mod config;
pub mod core;
pub mod gateway;
pub mod runtime;
pub mod store;
