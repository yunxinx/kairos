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

/// 指数退避的随机抖动幅度：最终等待为计算值的 80%–120%。
const RETRY_JITTER_MIN: f64 = 0.8;
const RETRY_JITTER_MAX: f64 = 1.2;

/// 同渠道下一次重试前等待的时长。
///
/// 有 `Retry-After` 时用其值（仍受 `after_cap` 封顶），不加抖动——那是上游指定的
/// 等待。无则走指数退避，并施加 ±20% 抖动，避免 429 恢复瞬间所有在途重试同拍。
pub(super) fn retry_delay(
    attempt_no: usize,
    retry_after: Option<Duration>,
    backoff: RetryBackoff,
) -> Duration {
    if let Some(after) = retry_after {
        return after.min(backoff.after_cap);
    }
    jitter_delay(exponential_delay(attempt_no, backoff)).min(backoff.cap)
}

/// 无抖动的指数退避：`base * 2^attempt`，封顶 `cap`。
fn exponential_delay(attempt_no: usize, backoff: RetryBackoff) -> Duration {
    let shift = u32::try_from(attempt_no).unwrap_or(u32::MAX).min(16);
    backoff.base.saturating_mul(1u32 << shift).min(backoff.cap)
}

/// 把等待乘以 `[0.8, 1.2]` 均匀随机因子。
fn jitter_delay(base: Duration) -> Duration {
    let factor: f64 = rand::random_range(RETRY_JITTER_MIN..=RETRY_JITTER_MAX);
    base.mul_f64(factor)
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
    use super::{RetryBackoff, exponential_delay, jitter_delay, retry_delay};
    use std::time::Duration;

    fn default_backoff() -> RetryBackoff {
        RetryBackoff::from_ms(200, 5_000, 60)
    }

    #[test]
    fn exponential_delay_is_exponential_and_capped() {
        let backoff = default_backoff();
        assert_eq!(exponential_delay(0, backoff), Duration::from_millis(200));
        assert_eq!(exponential_delay(1, backoff), Duration::from_millis(400));
        assert_eq!(exponential_delay(2, backoff), Duration::from_millis(800));
        assert_eq!(exponential_delay(16, backoff), Duration::from_secs(5));
    }

    #[test]
    fn retry_delay_honors_retry_after_without_jitter() {
        let backoff = default_backoff();
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
    fn jitter_delay_does_not_exceed_cap() {
        let backoff = default_backoff();
        for _ in 0..32 {
            let delay = retry_delay(16, None, backoff);
            assert!(
                delay <= backoff.cap,
                "抖动后等待 {delay:?} 不应超过封顶 {:?}",
                backoff.cap
            );
        }
    }

    #[test]
    fn retry_delay_uses_configured_base_and_caps() {
        let backoff = RetryBackoff::from_ms(100, 1_000, 10);
        assert_eq!(exponential_delay(0, backoff), Duration::from_millis(100));
        assert_eq!(exponential_delay(4, backoff), Duration::from_millis(1_000));
        assert_eq!(
            retry_delay(0, Some(Duration::from_secs(30)), backoff),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn jitter_delay_stays_within_plus_minus_20_percent() {
        let base = Duration::from_millis(200);
        let lo = Duration::from_millis(160);
        let hi = Duration::from_millis(240);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..40 {
            let got = jitter_delay(base);
            assert!(
                got >= lo && got <= hi,
                "jitter {got:?} 应在 [{lo:?}, {hi:?}]"
            );
            seen.insert(got);
        }
        assert!(seen.len() > 1, "±20% 抖动应产生多于一个等待值");
    }
}
