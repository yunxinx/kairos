//! HTTP 网关组合层：暴露路由构造 API，并集中管理网关内部模块。
//!
//! 具体的 HTTP 入站、认证、准入、出站与日志实现位于 [`http`]。保留本模块作为
//! facade，使二进制入口和端到端测试继续使用稳定的 `gateway::router` API。

mod admin;
mod admin_auth;
mod admin_billing;
mod failover;
mod http;
pub mod logging;
mod protocol;
mod rate_limit;
mod routing;
mod sse;
mod throttle;
mod webui;

pub use admin::router as admin_router;
pub use http::{Deps, router};
pub use webui::is_available as webui_available;
