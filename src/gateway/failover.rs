//! 渠道 failover 编排：统一处理重试预算、可重试错误与最终错误归因。

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use crate::config::{Channel, Protocol};

use super::{protocol, routing};

/// 单次出站调用的结果。
pub(super) enum Outbound {
    /// 成功：响应已就绪，可直接交给下游。
    Success(Response),
    /// 可重试错误（网络错误/429/5xx）。
    Retryable {
        channel: String,
        status: Option<u16>,
        message: String,
    },
    /// 不可重试错误（其他 4xx）。
    Fatal {
        channel: String,
        status: u16,
        message: String,
    },
}

/// 按渠道路由顺序发起出站调用，遇可重试错误自动 failover。
pub(super) async fn run_failover<'a, A, L>(
    route: &'a routing::Route,
    mut attempt: A,
    log_failure: L,
    inbound_protocol: Protocol,
) -> Response
where
    A: FnMut(&Channel) -> BoxFuture<'a, Outbound>,
    L: Fn(&str, u16, bool) -> BoxFuture<'a, ()>,
{
    let mut last_retryable: Option<(String, Option<u16>, String)> = None;

    for channel in &route.channels {
        let max_attempts = (channel.max_retries + 1) as usize;
        for attempt_no in 0..max_attempts {
            match attempt(channel).await {
                Outbound::Success(response) => return response,
                Outbound::Fatal {
                    channel,
                    status,
                    message,
                } => {
                    log_failure(&channel, status, false).await;
                    let body =
                        upstream_error_body(status, &message, &channel, false, inbound_protocol);
                    return (
                        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                        Json(body),
                    )
                        .into_response();
                }
                Outbound::Retryable {
                    channel,
                    status,
                    message,
                } => {
                    last_retryable = Some((channel, status, message));
                    if attempt_no + 1 < max_attempts {
                        continue;
                    }
                    break;
                }
            }
        }
    }

    let (channel, status, message) = last_retryable
        .unwrap_or_else(|| ("unknown".to_string(), None, "所有渠道均不可用".to_string()));
    let status_code = status.unwrap_or(502);
    log_failure(&channel, status_code, true).await;
    let body = upstream_error_body(status_code, &message, &channel, true, inbound_protocol);
    (
        StatusCode::from_u16(status_code).unwrap_or(StatusCode::BAD_GATEWAY),
        Json(body),
    )
        .into_response()
}

/// 构造带网关归因的错误响应体。
pub(super) fn upstream_error_body(
    status: u16,
    message: &str,
    channel: &str,
    failover: bool,
    inbound_protocol: Protocol,
) -> Value {
    let mut body = protocol::encode_error(status, message, inbound_protocol);
    let gateway = json!({ "channel": channel, "failover": failover });
    if let Value::Object(map) = &mut body
        && let Some(error) = map.get_mut("error").and_then(Value::as_object_mut)
    {
        error.insert("gateway".into(), gateway);
    }
    body
}
