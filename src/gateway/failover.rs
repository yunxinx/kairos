//! 渠道 failover 编排：统一处理重试预算、可重试错误与最终错误归因。

use std::time::Duration;

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use crate::config::Protocol;
use crate::store::resources::ChannelRecord;

use super::{protocol, routing};

/// 同渠道重试的退避参数（来自运行时设置）。
#[derive(Debug, Clone, Copy)]
pub(super) struct RetryBackoff {
    /// 无 `Retry-After` 时的基础间隔。
    pub base: Duration,
    /// 指数退避封顶。
    pub cap: Duration,
    /// 上游 `Retry-After` 的最大等待。
    pub after_cap: Duration,
}

impl RetryBackoff {
    /// 由设置毫秒/秒构造；各字段至少为 1，避免零间隔忙等。
    pub(super) fn from_ms(base_ms: u64, cap_ms: u64, after_cap_secs: u64) -> Self {
        Self {
            base: Duration::from_millis(base_ms.max(1)),
            cap: Duration::from_millis(cap_ms.max(1)),
            after_cap: Duration::from_secs(after_cap_secs.max(1)),
        }
    }
}

/// 单次出站调用的结果。
pub(super) enum Outbound {
    /// 成功：响应已就绪，可直接交给下游。
    Success(Response),
    /// 可重试错误（网络错误/429/5xx）。
    Retryable {
        channel: String,
        status: Option<u16>,
        message: String,
        /// 上游 `Retry-After` 的 delta-seconds；无则按指数退避。
        retry_after: Option<Duration>,
    },
    /// 不可重试错误（其他 4xx）。
    Fatal {
        channel: String,
        status: u16,
        message: String,
    },
}

/// 同渠道下一次重试前等待的时长。
pub(super) fn retry_delay(
    attempt_no: usize,
    retry_after: Option<Duration>,
    backoff: RetryBackoff,
) -> Duration {
    if let Some(after) = retry_after {
        return after.min(backoff.after_cap);
    }
    let shift = u32::try_from(attempt_no).unwrap_or(u32::MAX).min(16);
    backoff.base.saturating_mul(1u32 << shift).min(backoff.cap)
}

/// 按渠道路由顺序发起出站调用，遇可重试错误自动 failover。
///
/// `log_failure` 接收（渠道名、状态码、是否已 failover、返回下游的错误响应体
/// wire 字节）：wire 字节先于日志构造，保证 full_body 开启时失败日志也能记录
/// 实际返回下游的入站响应。
pub(super) async fn run_failover<'a, A, L>(
    route: &'a routing::Route,
    mut attempt: A,
    log_failure: L,
    inbound_protocol: Protocol,
    backoff: RetryBackoff,
) -> Response
where
    A: FnMut(&ChannelRecord) -> BoxFuture<'a, Outbound>,
    L: Fn(&str, u16, bool, &[u8]) -> BoxFuture<'a, ()>,
{
    let mut last_retryable: Option<(String, Option<u16>, String)> = None;

    for record in &route.channels {
        let max_attempts = (record.channel.max_retries + 1) as usize;
        for attempt_no in 0..max_attempts {
            match attempt(record).await {
                Outbound::Success(response) => return response,
                Outbound::Fatal {
                    channel,
                    status,
                    message,
                } => {
                    let body =
                        upstream_error_body(status, &message, &channel, false, inbound_protocol);
                    let wire = serde_json::to_vec(&body).unwrap_or_default();
                    log_failure(&channel, status, false, &wire).await;
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
                    retry_after,
                } => {
                    last_retryable = Some((channel, status, message));
                    if attempt_no + 1 < max_attempts {
                        tokio::time::sleep(retry_delay(attempt_no, retry_after, backoff)).await;
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
    let body = upstream_error_body(status_code, &message, &channel, true, inbound_protocol);
    let wire = serde_json::to_vec(&body).unwrap_or_default();
    log_failure(&channel, status_code, true, &wire).await;
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

#[cfg(test)]
mod tests {
    use super::{RetryBackoff, retry_delay};
    use std::time::Duration;

    fn default_backoff() -> RetryBackoff {
        RetryBackoff::from_ms(200, 5_000, 60)
    }

    #[test]
    fn retry_delay_is_exponential_and_capped() {
        let backoff = default_backoff();
        assert_eq!(retry_delay(0, None, backoff), Duration::from_millis(200));
        assert_eq!(retry_delay(1, None, backoff), Duration::from_millis(400));
        assert_eq!(retry_delay(2, None, backoff), Duration::from_millis(800));
        assert_eq!(retry_delay(16, None, backoff), Duration::from_secs(5));
        assert_eq!(
            retry_delay(0, Some(Duration::from_secs(3)), backoff),
            Duration::from_secs(3)
        );
        assert_eq!(
            retry_delay(0, Some(Duration::from_secs(120)), backoff),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn retry_delay_uses_configured_base_and_caps() {
        let backoff = RetryBackoff::from_ms(100, 1_000, 10);
        assert_eq!(retry_delay(0, None, backoff), Duration::from_millis(100));
        assert_eq!(retry_delay(4, None, backoff), Duration::from_millis(1_000));
        assert_eq!(
            retry_delay(0, Some(Duration::from_secs(30)), backoff),
            Duration::from_secs(10)
        );
    }
}
