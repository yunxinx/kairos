//! 请求日志与计费结果的持久化适配。

use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    config::Protocol,
    core::{billing::PriceSnapshot, ir::Usage},
    store,
    store::resources::Token,
};

use super::http::Deps;

/// 一次请求的计费结果，供日志落库。
#[derive(Debug, Clone, Default)]
pub(super) struct Billing {
    pub(super) usage: Usage,
    pub(super) price: PriceSnapshot,
    pub(super) cost_usd_micros: i64,
    pub(super) request_body: Option<Vec<u8>>,
    pub(super) response_body: Option<Vec<u8>>,
}

/// 落一条请求日志。await 以保证响应返回时日志已落库。
///
/// `model` 为入站名（下游请求的模型 ID）；`outbound_model` 为实际发给上游的
/// 模型名。尚未出站（准入失败）时出站名为 `None`。
#[allow(clippy::too_many_arguments)]
pub(super) async fn log_request(
    deps: &Deps,
    token: &Token,
    model: &str,
    outbound_model: Option<&str>,
    channel: &str,
    status: u16,
    started: i64,
    billing: Billing,
    inbound_protocol: Protocol,
) {
    let now = unix_millis();
    let log = store::RequestLog {
        id: 0,
        created_at: now,
        token_name: token.name.clone(),
        token_key: token.token_key.clone(),
        inbound_protocol: protocol_name(inbound_protocol).to_string(),
        model: model.to_string(),
        outbound_model: outbound_model.map(str::to_string),
        channel: channel.to_string(),
        status_code: status as i64,
        latency_ms: now - started,
        input_tokens: billing.usage.input_tokens,
        output_tokens: billing.usage.output_tokens,
        cache_read_tokens: billing.usage.cache_read_tokens,
        cache_write_tokens: billing.usage.cache_write_tokens,
        price: billing.price,
        cost_usd_micros: billing.cost_usd_micros,
        request_body: billing.request_body,
        response_body: billing.response_body,
    };
    if let Err(err) = store::insert_request_log(&deps.pool, &log).await {
        eprintln!("请求日志落库失败: {err}");
    }
}

/// 入站协议名（日志落库用）。
pub(super) fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenAiChat => "openai_chat",
        Protocol::OpenAiResponses => "openai_responses",
        Protocol::AnthropicMessages => "anthropic_messages",
    }
}

/// 当前 unix 毫秒时间戳。
pub(super) fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
