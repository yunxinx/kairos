//! HTTP 网关组合层：暴露路由构造 API，并集中管理网关内部模块。
//!
//! 具体的 HTTP 入站、认证、准入、出站与日志实现位于 [`http`]。保留本模块作为
//! facade，使二进制入口和端到端测试继续使用稳定的 `gateway::router` API。

mod admin;
mod failover;
mod http;
mod logging;
mod network;
mod protocol;
mod rate_limit;
mod rectifier;
mod routing;
mod sse;
mod throttle;
mod webui;

pub use admin::{router as admin_router, router_with_writer as admin_router_with_writer};
pub use http::{Deps, router, router_with_writer};
pub use logging::{RequestLogWriter, unix_millis};
pub use webui::is_available as webui_available;
