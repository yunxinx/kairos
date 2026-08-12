//! HTTP 网关组合层：暴露路由构造 API，并集中管理网关内部模块。
//!
//! 具体的 HTTP 入站、认证、准入、出站与日志实现位于 [`http`]。保留本模块作为
//! facade，使二进制入口和端到端测试继续使用稳定的 `gateway::router` API。

mod failover;
mod http;
mod logging;
mod protocol;
mod routing;
mod sse;

pub use http::{Deps, router};
