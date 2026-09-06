//! 直通快路径：同协议不改名的出站转发。
//!
//! 请求体仅做目标性补丁（计费用 `stream_options.include_usage`、会话缓存键），
//! 响应字节块直通下游，旁路逐 SSE 帧嗅探 usage 结算。

use std::{sync::Arc, time::Duration};

use axum::{
    Json,
    body::Body,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures_util::Stream;
use serde_json::{Value, json};
use tokio::sync::OwnedSemaphorePermit;

use crate::{
    config::{Protocol, SessionCacheKeyMode},
    core::billing::PriceSnapshot,
    core::ir::{StreamEvent, Usage},
    runtime::RuntimeSnapshot,
    store::resources::{Channel, ChannelRecord, StoredChannelKey, Token},
};

use super::attempt::{BillingAttemptStart, begin_billing_attempt, spawn_reservation_heartbeat};
use super::stream_task::{
    SSE_REASSEMBLY_OVERFLOW_MESSAGE, UpstreamByteStream, append_logged_body,
    inbound_stream_error_frame, is_content_event, send_to_downstream, spawn_piped_stream_task,
};
use super::{
    Deps, HopDispatch, OutboundAuth, billed_price, channel_idle, is_retryable_status,
    outbound_model_for_log, parse_retry_after, remaining_timeout, take_upstream_body,
    upstream_error_message,
};
use crate::gateway::failover::{Outbound, channel_request_budget};
use crate::gateway::logging::{Billing, RequestLogDraft, queue_request_log};
use crate::gateway::network::NetworkPolicy;
use crate::gateway::sse::{
    OpenAiDoneFilter, data_frame_to_wire, frame_to_wire, receiver_stream, take_frame,
};

use crate::gateway::protocol;

/// 直通快路径的流式出站：原始字节块直搬，旁路逐 SSE 帧嗅探 usage 计费。
///
/// 请求体仅做目标性补丁（OpenAI 流式注入 `stream_options.include_usage` 供计费，
/// 并按渠道开关回写会话缓存键；Anthropic 无需补丁），响应以字节流直通到下游，
/// 不做完整解码。流结束后按嗅探到的 usage 结算并落日志。渠道 timeout 约束响应头
/// 与流式读取的空闲间隔；流建立后不再受请求总时限约束。
pub(super) async fn passthrough_stream_completion(
    ctx: &HopDispatch<'_>,
    record: &ChannelRecord,
    key: &StoredChannelKey,
    deadline: tokio::time::Instant,
) -> Outbound {
    let channel = &record.channel;
    let outbound = passthrough_patch_request(
        ctx.raw_body,
        true,
        channel.protocol,
        channel.session_cache_key,
        ctx.session_identity,
    );
    // 直通的前提是出站模型名与入站一致，URL 路径上的模型名无需改写。
    let upstream_url = passthrough_upstream_url(channel, &ctx.request.model, true);
    let network_policy = NetworkPolicy::new(
        ctx.snapshot.allow_private_networks,
        &ctx.snapshot.private_network_allowlist,
    );
    if let Err(err) = network_policy.validate_target(&upstream_url) {
        return Outbound::Retryable {
            channel: channel.name.clone(),
            status: None,
            retry_after: None,
            message: err.to_string(),
        };
    }

    let price = billed_price(ctx.snapshot, record, ctx.routed_model);
    let billing_attempt_id = match begin_billing_attempt(BillingAttemptStart {
        deps: ctx.deps,
        snapshot: ctx.snapshot,
        request_id: ctx.request_id,
        request: ctx.request,
        token: ctx.token,
        routed_model: ctx.routed_model,
        channel,
        channel_key: key,
        started: ctx.started,
        price,
        inbound_protocol: ctx.inbound_protocol,
        request_body: ctx.request_body.clone(),
        ever_dispatched: ctx.ever_dispatched,
        deadline,
    })
    .await
    {
        Ok(attempt) => attempt,
        Err(outbound) => return outbound,
    };

    if let Err(outbound) = billing_attempt_id.mark_dispatched().await {
        return outbound;
    }
    let upstream = tokio::time::timeout(
        remaining_timeout(deadline, channel_idle(channel.timeout_ms)),
        ctx.deps
            .outbound_clients
            .for_policy(
                &network_policy,
                network_policy.target_allowlisted(&upstream_url),
            )
            .post(&upstream_url)
            .apply_outbound_auth_with_version(channel.protocol, key, ctx.inbound_anthropic_version)
            .apply_feature_headers(ctx.inbound_headers)
            .header("content-type", "application/json")
            .body(outbound)
            .send(),
    )
    .await;

    let resp = match upstream {
        Ok(Ok(resp)) => resp,
        Ok(Err(err)) => {
            return billing_attempt_id
                .record_failure(
                    502,
                    None,
                    None,
                    Some(&err),
                    Outbound::Retryable {
                        channel: channel.name.clone(),
                        status: None,
                        retry_after: None,
                        message: "直通流式上游不可达".to_string(),
                    },
                )
                .await;
        }
        Err(_) => {
            return billing_attempt_id
                .record_failure(
                    504,
                    None,
                    None,
                    None,
                    Outbound::Retryable {
                        channel: channel.name.clone(),
                        status: None,
                        retry_after: None,
                        message: "直通流式上游响应超时".to_string(),
                    },
                )
                .await;
        }
    };

    let status_code = resp.status().as_u16();
    if !resp.status().is_success() {
        let retry_after = parse_retry_after(resp.headers());
        let upstream_body = match tokio::time::timeout(
            remaining_timeout(deadline, channel_idle(channel.timeout_ms)),
            take_upstream_body(
                resp,
                &channel.name,
                ctx.snapshot.max_response_bytes,
                "直通流式上游读体失败",
            ),
        )
        .await
        {
            Ok(body) => body,
            Err(_) => {
                return billing_attempt_id
                    .record_failure(
                        504,
                        None,
                        None,
                        None,
                        Outbound::Retryable {
                            channel: channel.name.clone(),
                            status: None,
                            retry_after: None,
                            message: "直通流式上游读体超时".to_string(),
                        },
                    )
                    .await;
            }
        };
        let upstream_body = match upstream_body {
            Ok(body) => body,
            Err(outbound) => {
                return billing_attempt_id
                    .record_failure(502, None, None, None, outbound)
                    .await;
            }
        };
        let parsed = serde_json::from_slice::<Value>(&upstream_body).unwrap_or(Value::Null);
        let reported_usage = protocol::sniff_usage(&parsed, channel.protocol);
        if is_retryable_status(status_code) {
            return billing_attempt_id
                .record_failure(
                    status_code,
                    Some(upstream_body.to_vec()),
                    reported_usage.clone(),
                    None,
                    Outbound::Retryable {
                        channel: channel.name.clone(),
                        status: Some(status_code),
                        retry_after,
                        message: "上游返回可重试错误".to_string(),
                    },
                )
                .await;
        }
        return billing_attempt_id
            .record_failure(
                status_code,
                Some(upstream_body.to_vec()),
                reported_usage,
                None,
                Outbound::Fatal {
                    channel: channel.name.clone(),
                    status: status_code,
                    message: upstream_error_message(&parsed, status_code),
                },
            )
            .await;
    }

    // 与 IR 流式路径保持相同的首帧语义：在向下游返回响应前先检查流内错误、
    // 空流与首帧超时。这样 200 + overloaded_error 也能在首字节前切换渠道。
    let idle = channel_idle(channel.timeout_ms);
    let byte_stream: UpstreamByteStream = Box::pin(resp.bytes_stream());
    let (peek, byte_stream) = peek_passthrough_stream_head_until(
        byte_stream,
        channel.protocol,
        idle,
        ctx.snapshot.sse_reassembly_max(),
        deadline,
    )
    .await;
    let peeked = match peek {
        PassthroughPeek::Content(chunks) => chunks,
        PassthroughPeek::UpstreamError(message) => {
            return billing_attempt_id
                .record_failure(
                    502,
                    None,
                    None,
                    None,
                    Outbound::Retryable {
                        channel: channel.name.clone(),
                        status: None,
                        retry_after: None,
                        message: format!("上游流内错误：{message}"),
                    },
                )
                .await;
        }
        PassthroughPeek::Interrupted(reason) => {
            return billing_attempt_id
                .record_failure(
                    504,
                    None,
                    None,
                    None,
                    Outbound::Retryable {
                        channel: channel.name.clone(),
                        status: None,
                        retry_after: None,
                        message: reason,
                    },
                )
                .await;
        }
    };

    // 逐 SSE 帧嗅探 usage 计费，同时原样转发字节流到下游。
    let task = PassthroughStreamTask {
        deps: ctx.deps.clone(),
        snapshot: ctx.snapshot.clone(),
        token: ctx.token.clone(),
        request_model: ctx.request.model.clone(),
        routed_model: ctx.routed_model.to_string(),
        channel: channel.clone(),
        channel_key_name: key.name.clone(),
        status_code,
        started: ctx.started,
        price,
        protocol: channel.protocol,
        request_body: ctx.request_body.clone(),
        response_body: Vec::new(),
        request_id: ctx.request_id.to_string(),
        billing_attempt_id: billing_attempt_id.attempt_id,
        peeked,
        _active_permit: ctx
            .active_permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take(),
        // 流任务持有 permit 直到结算完成；日志持久化预算按本渠道的预首字节
        // 时限在流首重锚，长流结束时不受入站起算的旧时刻约束。
        log_deadline: tokio::time::Instant::now() + channel_request_budget(channel),
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);
    spawn_piped_stream_task(
        task.request_id.clone(),
        task.billing_attempt_id.clone(),
        task.channel.name.clone(),
        pipe_passthrough_stream(byte_stream, tx, task),
    );

    let stream = receiver_stream(rx);
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Outbound::Success(response)
}

/// 直通快路径的非流式出站：响应体整体透传，从响应 JSON 嗅探 usage 计费。
///
/// 请求体仅做目标性补丁（OpenAI 按渠道开关回写会话缓存键），响应体原样
/// 返回，不做完整解码；读体与请求总时限同受截止约束。
pub(super) async fn passthrough_non_stream_completion(
    ctx: &HopDispatch<'_>,
    record: &ChannelRecord,
    key: &StoredChannelKey,
    deadline: tokio::time::Instant,
) -> Outbound {
    let channel = &record.channel;
    let outbound = passthrough_patch_request(
        ctx.raw_body,
        false,
        channel.protocol,
        channel.session_cache_key,
        ctx.session_identity,
    );
    let upstream_url = passthrough_upstream_url(channel, &ctx.request.model, false);
    let network_policy = NetworkPolicy::new(
        ctx.snapshot.allow_private_networks,
        &ctx.snapshot.private_network_allowlist,
    );
    if let Err(err) = network_policy.validate_target(&upstream_url) {
        return Outbound::Retryable {
            channel: channel.name.clone(),
            status: None,
            retry_after: None,
            message: err.to_string(),
        };
    }

    let price = billed_price(ctx.snapshot, record, ctx.routed_model);
    let billing_attempt_id = match begin_billing_attempt(BillingAttemptStart {
        deps: ctx.deps,
        snapshot: ctx.snapshot,
        request_id: ctx.request_id,
        request: ctx.request,
        token: ctx.token,
        routed_model: ctx.routed_model,
        channel,
        channel_key: key,
        started: ctx.started,
        price,
        inbound_protocol: ctx.inbound_protocol,
        request_body: ctx.request_body.clone(),
        ever_dispatched: ctx.ever_dispatched,
        deadline,
    })
    .await
    {
        Ok(attempt) => attempt,
        Err(outbound) => return outbound,
    };

    if let Err(outbound) = billing_attempt_id.mark_dispatched().await {
        return outbound;
    }
    let upstream = tokio::time::timeout(
        remaining_timeout(deadline, channel_idle(channel.timeout_ms)),
        ctx.deps
            .outbound_clients
            .for_policy(
                &network_policy,
                network_policy.target_allowlisted(&upstream_url),
            )
            .post(&upstream_url)
            .apply_outbound_auth_with_version(channel.protocol, key, ctx.inbound_anthropic_version)
            .apply_feature_headers(ctx.inbound_headers)
            .header("content-type", "application/json")
            .body(outbound)
            .send(),
    )
    .await;

    let resp = match upstream {
        Ok(Ok(resp)) => resp,
        Ok(Err(err)) => {
            return billing_attempt_id
                .record_failure(
                    502,
                    None,
                    None,
                    Some(&err),
                    Outbound::Retryable {
                        channel: channel.name.clone(),
                        status: None,
                        retry_after: None,
                        message: "直通非流式上游不可达".to_string(),
                    },
                )
                .await;
        }
        Err(_) => {
            return billing_attempt_id
                .record_failure(
                    504,
                    None,
                    None,
                    None,
                    Outbound::Retryable {
                        channel: channel.name.clone(),
                        status: None,
                        retry_after: None,
                        message: "直通非流式上游响应超时".to_string(),
                    },
                )
                .await;
        }
    };

    let status_code = resp.status().as_u16();
    let is_success = resp.status().is_success();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("application/json"));
    let retry_after = parse_retry_after(resp.headers());
    let idle = channel_idle(channel.timeout_ms);
    let max_bytes = ctx.snapshot.max_response_bytes;
    let upstream_body = match tokio::time::timeout(
        remaining_timeout(deadline, idle),
        take_upstream_body(resp, &channel.name, max_bytes, "直通非流式上游读体失败"),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(outbound)) => {
            return billing_attempt_id
                .record_failure(502, None, None, None, outbound)
                .await;
        }
        Err(_) => {
            return billing_attempt_id
                .record_failure(
                    504,
                    None,
                    None,
                    None,
                    Outbound::Retryable {
                        channel: channel.name.clone(),
                        status: None,
                        retry_after: None,
                        message: "直通非流式上游读体超时".to_string(),
                    },
                )
                .await;
        }
    };
    let parsed = serde_json::from_slice::<Value>(&upstream_body).unwrap_or(Value::Null);
    let reported_usage = protocol::sniff_usage(&parsed, channel.protocol);

    let response_failed = is_success
        && channel.protocol == Protocol::OpenAiResponses
        && crate::core::openai_responses::response_is_failed(&parsed);
    if response_failed {
        // Responses 可用 HTTP 2xx 承载失败对象；直通时也要转换成明确的
        // 网关错误，否则调用方会把失败响应当成成功结果继续处理。
        let returned_status = StatusCode::BAD_GATEWAY;
        let inbound = protocol::encode_error(
            returned_status.as_u16(),
            &upstream_error_message(&parsed, returned_status.as_u16()),
            ctx.inbound_protocol,
        );
        let discount_bp = ctx.snapshot.discount_bp_for_token(ctx.token);
        let billing = Billing::calculated(
            reported_usage.clone().unwrap_or_default(),
            reported_usage.is_some(),
            price,
            discount_bp,
            ctx.request_body.clone(),
            ctx.snapshot
                .full_body
                .then(|| serde_json::to_vec(&inbound).unwrap_or_default()),
        );
        if let Err(err) = queue_request_log(
            ctx.deps,
            RequestLogDraft {
                token: ctx.token,
                model: &ctx.request.model,
                outbound_model: outbound_model_for_log(channel, ctx.routed_model),
                channel: &channel.name,
                channel_key: Some(&key.name),
                status: returned_status.as_u16(),
                started: ctx.started,
                billing,
                inbound_protocol: ctx.inbound_protocol,
                request_id: ctx.request_id,
                billing_attempt_id: Some(billing_attempt_id.id()),
                upstream_reached: true,
                dispatched: true,
                deadline: Some(deadline),
            },
        )
        .await
        {
            tracing::error!(
                error = %err,
                attempt_id = %billing_attempt_id.id(),
                "请求结果无法立即持久化，将由恢复任务继续处理"
            );
        }
        Outbound::Success((returned_status, Json(inbound)).into_response())
    } else if is_success {
        // 响应体原样透传（字节级一致），从 JSON 嗅探 usage 计费。
        let usage_reported = reported_usage.is_some();
        let usage = reported_usage.unwrap_or_default();
        let discount_bp = ctx.snapshot.discount_bp_for_token(ctx.token);
        // 计费持久化与下游响应是两条独立结果：费用不可表示时原始响应仍应交付，
        // outbox 以未结算状态保留完整计算错误，不能因是否流式而改变响应语义。
        let billing = Billing::calculated(
            usage,
            usage_reported,
            price,
            discount_bp,
            ctx.request_body.clone(),
            ctx.snapshot.full_body.then(|| upstream_body.to_vec()),
        );
        if let Err(err) = queue_request_log(
            ctx.deps,
            RequestLogDraft {
                token: ctx.token,
                model: &ctx.request.model,
                outbound_model: outbound_model_for_log(channel, ctx.routed_model),
                channel: &channel.name,
                channel_key: Some(&key.name),
                status: status_code,
                started: ctx.started,
                billing,
                inbound_protocol: ctx.inbound_protocol,
                request_id: ctx.request_id,
                billing_attempt_id: Some(billing_attempt_id.id()),
                upstream_reached: true,
                dispatched: true,
                deadline: Some(deadline),
            },
        )
        .await
        {
            tracing::error!(
                error = %err,
                attempt_id = %billing_attempt_id.id(),
                "直通请求结果无法立即持久化，将由恢复任务继续处理"
            );
        }
        let mut response = Response::new(Body::from(upstream_body));
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
        Outbound::Success(response)
    } else if is_retryable_status(status_code) {
        billing_attempt_id
            .record_failure(
                status_code,
                Some(upstream_body.to_vec()),
                reported_usage.clone(),
                None,
                Outbound::Retryable {
                    channel: channel.name.clone(),
                    status: Some(status_code),
                    retry_after,
                    message: "上游返回可重试错误".to_string(),
                },
            )
            .await
    } else {
        billing_attempt_id
            .record_failure(
                status_code,
                Some(upstream_body.to_vec()),
                reported_usage,
                None,
                Outbound::Fatal {
                    channel: channel.name.clone(),
                    status: status_code,
                    message: upstream_error_message(&parsed, status_code),
                },
            )
            .await
    }
}

/// 直通快路径的出站请求体：以下游请求体为准，仅做目标性 JSON 补丁。
///
/// 直通补丁共两项，均只作用于 OpenAI Chat 出站：流式时注入
/// `stream_options.include_usage`（供逐帧嗅探 usage 计费；非流式响应体已自带
/// 顶层 usage）；按渠道开关回写会话缓存键（与 IR 路径的
/// [`protocol::write_session_cache_key`] 同一语义与同一补丁点）。Anthropic
/// 流式自带 usage（message_delta），无补丁，请求体字节级原样转发。`stream`
/// 字段由下游请求自带，不做改写。
fn passthrough_patch_request(
    raw_body: &[u8],
    stream: bool,
    protocol: Protocol,
    session_cache_key: SessionCacheKeyMode,
    session_identity: &str,
) -> Vec<u8> {
    if protocol != Protocol::OpenAiChat {
        return raw_body.to_vec();
    }
    if !stream && session_cache_key == SessionCacheKeyMode::Off {
        return raw_body.to_vec();
    }
    let mut value: Value = match serde_json::from_slice(raw_body) {
        Ok(value) => value,
        Err(_) => return raw_body.to_vec(),
    };
    if stream && let Value::Object(map) = &mut value {
        // 合并而非覆盖：下游自带的 stream_options 其他字段保留，仅补 include_usage。
        // 既有值不是对象（null/标量）时整体换为对象——IR 路径本就重建该字段，
        // 直通对齐后 include_usage 注入不因下游畸形值而缺席（缺席即计费缺据）。
        let stream_options = map
            .entry("stream_options".to_string())
            .or_insert_with(|| json!({}));
        if !stream_options.is_object() {
            *stream_options = json!({});
        }
        if let Value::Object(so) = stream_options {
            so.insert("include_usage".into(), Value::Bool(true));
        }
    }
    // 会话缓存键回写：与 IR 路径共用同一补丁语义，直通同样获得缓存亲和。
    protocol::write_session_cache_key(&mut value, protocol, session_cache_key, session_identity);
    serde_json::to_vec(&value).unwrap_or_else(|_| raw_body.to_vec())
}

/// 直通快路径的出站 URL：base + 协议路径。
fn passthrough_upstream_url(channel: &Channel, model: &str, stream: bool) -> String {
    format!(
        "{}{}",
        channel.base_url.trim_end_matches('/'),
        protocol::upstream_path(channel.protocol, model, stream)
    )
}

/// 直通快路径流式请求的共享任务数据：出站目标、计费与日志所需的请求侧信息。
struct PassthroughStreamTask {
    deps: Deps,
    snapshot: Arc<RuntimeSnapshot>,
    token: Token,
    request_model: String,
    routed_model: String,
    channel: Channel,
    channel_key_name: String,
    status_code: u16,
    started: i64,
    price: PriceSnapshot,
    /// 直通协议（与入站同协议），用于 usage 嗅探与终止哨兵。
    protocol: Protocol,
    request_body: Option<Bytes>,
    response_body: Vec<u8>,
    request_id: String,
    /// 本流对应的实际出站尝试；流结束后的 usage 只结算这一条预留。
    billing_attempt_id: String,
    /// 首帧检查阶段已消费的原始字节块；流水任务先重放，再继续读取上游。
    peeked: Vec<Bytes>,
    /// 由请求入口转移而来的活动请求 permit，流水和结算完成后自动释放。
    _active_permit: Option<OwnedSemaphorePermit>,
    /// 流内读取与下发不再受请求总时限约束（仅受渠道空闲超时约束）；本字段
    /// 只作为结算日志持久化的上界预算，长流结束时从当下重新起算。
    log_deadline: tokio::time::Instant,
}

/// 直通流首帧检查结果。
enum PassthroughPeek {
    /// 已看到首个内容或正常收尾事件；字节块保持原样交给流水任务重放。
    Content(Vec<Bytes>),
    /// 首帧前上游主动发送流内错误，可在下游收到响应头前切换渠道。
    UpstreamError(String),
    /// 首字节之前流被截断（空流、超时、读取失败或重装超限）：已缓冲的字节
    /// 一并丢弃，按可重试语义上抛换渠道，对下游零痕迹。
    Interrupted(String),
}

/// 直通流在返回响应前检查首帧，保留原始字节块以便后续无损重放。
///
/// 直通路径不重编码响应，因此不能复用 IR 路径只保存 JSON 帧的 peek 结果；
/// 这里保留已消费的网络块，并在流水任务开头先发送它们。检查同时受 SSE 重装
/// 上限约束，避免上游长时间只发无法组成完整帧的字节时无限增长。
///
/// 语义与 IR 路径的 `peek_stream_head_until` 对齐：只有看到内容/收尾事件或
/// 「已解出帧但无 IR 映射」的活性信号才算 Content；重装超限、读取失败、EOF
/// 与首帧超时一律 Interrupted——此刻尚未向下游转发任何字节，failover 零痕迹，
/// 不把 200 + 残缺流伪装成可交付响应。
async fn peek_passthrough_stream_head_until(
    byte_stream: UpstreamByteStream,
    protocol: Protocol,
    idle: Duration,
    max_bytes: usize,
    deadline: tokio::time::Instant,
) -> (PassthroughPeek, UpstreamByteStream) {
    use futures_util::StreamExt as _;

    let mut byte_stream = byte_stream;
    let mut decoder = protocol::make_decoder(protocol);
    let mut buffer: Vec<u8> = Vec::new();
    let mut peeked: Vec<Bytes> = Vec::new();
    let mut buffered_bytes = 0usize;
    let mut saw_message_start = false;

    loop {
        if let Some((event_name, frame)) = take_frame(&mut buffer) {
            if frame.is_empty() {
                continue;
            }
            // 帧以 UTF-8 文本交给解码器直解 wire 类型（丢失字节的坏帧按
            // 解码失败留痕跳过，与原 Null 行为一致：不因个别坏帧中断 peek）。
            let frame_text = String::from_utf8_lossy(&frame);
            let decoded = decoder.process(&frame_text);
            let mut content = false;
            if matches!(protocol, Protocol::AnthropicMessages)
                && event_name.as_deref() == Some("message_start")
            {
                // 部分合法 Anthropic 流省略 `message.model`；SSE 事件名仍足以确认流首序言。
                saw_message_start = true;
            }
            for event in &decoded.events {
                match event {
                    StreamEvent::Error { message } => {
                        return (PassthroughPeek::UpstreamError(message.clone()), byte_stream);
                    }
                    StreamEvent::Finish { finish_reason, .. }
                        if finish_reason.unified == crate::core::ir::FinishReasonUnified::Error =>
                    {
                        return (
                            PassthroughPeek::UpstreamError("上游响应以失败状态结束".to_string()),
                            byte_stream,
                        );
                    }
                    StreamEvent::Finish { .. } => content = true,
                    StreamEvent::ResponseMetadata { .. } => saw_message_start = true,
                    _ if is_content_event(event) => {
                        if matches!(protocol, Protocol::AnthropicMessages) && !saw_message_start {
                            return (
                                PassthroughPeek::Interrupted(
                                    "上游流缺少 message_start".to_string(),
                                ),
                                byte_stream,
                            );
                        }
                        content = true;
                    }
                    _ => {}
                }
            }
            // 与 IR 路径同规的活性信号：已解出完整帧但无 IR 映射（如心跳帧）
            // 说明上游存活，此后断流不能再安全地切换渠道。
            if decoded.events.is_empty() {
                content = true;
            }
            if content {
                return (PassthroughPeek::Content(peeked), byte_stream);
            }
            continue;
        }

        match tokio::time::timeout(
            remaining_timeout(deadline, idle),
            byte_stream.as_mut().next(),
        )
        .await
        {
            Ok(Some(Ok(bytes))) => {
                buffered_bytes = buffered_bytes.saturating_add(bytes.len());
                buffer.extend_from_slice(&bytes);
                peeked.push(bytes);
                if buffered_bytes > max_bytes {
                    return (
                        PassthroughPeek::Interrupted(SSE_REASSEMBLY_OVERFLOW_MESSAGE.to_string()),
                        byte_stream,
                    );
                }
            }
            Ok(Some(Err(_))) => {
                return (
                    PassthroughPeek::Interrupted("上游流读取失败".to_string()),
                    byte_stream,
                );
            }
            Ok(None) => {
                return (
                    PassthroughPeek::Interrupted("上游流未产出内容即结束（空流）".to_string()),
                    byte_stream,
                );
            }
            Err(_) => {
                return (
                    PassthroughPeek::Interrupted("上游流首帧超时".to_string()),
                    byte_stream,
                );
            }
        }
    }
}

/// 把上游 SSE 原始字节块直搬到下游，并在旁路缓冲中逐帧嗅探 usage。
///
/// 转发不等待分帧：每个成功读取的块直接送入响应体。旁路缓冲仅负责提取完整 SSE
/// 数据事件，并按协议把 usage 逐分量取 max（Anthropic 的 usage 分散在
/// message_start/message_delta）；它不决定普通块的转发。上游读取与下游下发均只受
/// 渠道空闲超时约束，不受请求总时限约束。上游流结束时按累积 usage 结算并落日志；
/// 下游断连时按渠道 `abort_on_disconnect` 开关决定是否立即取消上游消费。
async fn pipe_passthrough_stream<S>(
    byte_stream: S,
    tx: tokio::sync::mpsc::Sender<bytes::Bytes>,
    mut ctx: PassthroughStreamTask,
) where
    S: Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    use futures_util::StreamExt as _;

    let mut usage = Usage::default();
    let mut usage_reported = false;
    let mut sse_buffer: Vec<u8> = Vec::new();
    let mut decoder = protocol::make_decoder(ctx.protocol);
    let mut downstream_open = true;
    let mut truncated = false;
    let mut done_filter = OpenAiDoneFilter::default();
    let idle = channel_idle(ctx.channel.timeout_ms);
    let mut byte_stream = Box::pin(byte_stream);
    let mut pending = ctx.peeked.into_iter();
    let log_body_max = ctx.snapshot.log_body_max();
    // 长流心跳：与 IR 流式路径同规，防止恢复任务把仍在消费中的长流误判孤儿。
    let reservation_heartbeat = spawn_reservation_heartbeat(&ctx.deps, &ctx.billing_attempt_id);

    loop {
        let chunk = if let Some(chunk) = pending.next() {
            chunk
        } else {
            match tokio::time::timeout(idle, byte_stream.next()).await {
                Ok(Some(Ok(chunk))) => chunk,
                Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
            }
        };
        sse_buffer.extend_from_slice(&chunk);
        // 传输快路径只处理原始块；旁路解析的结果不影响这个块是否转发。
        if downstream_open {
            let chunks = if ctx.protocol == Protocol::OpenAiChat {
                done_filter.push(chunk)
            } else {
                vec![chunk]
            };
            for forwarded in chunks {
                if !send_passthrough_chunk(
                    &tx,
                    forwarded,
                    ctx.snapshot.full_body,
                    log_body_max,
                    &mut ctx.response_body,
                    idle,
                )
                .await
                {
                    // 下游断开：停止发送；开关开启时立即取消上游消费，
                    // 触发断连的块不再参与嗅探，结算按此前已嗅探的 usage。
                    downstream_open = false;
                    break;
                }
            }
            if !downstream_open && ctx.channel.abort_on_disconnect {
                break;
            }
        }

        while let Some((_event_name, frame)) = take_frame(&mut sse_buffer) {
            if frame.is_empty() {
                continue;
            }
            // 帧以 UTF-8 文本交给解码器直解 wire 类型；usage 嗅探先经子串
            // 门控再由适配器按需物化（与 pipe_stream 同路径）。两者互不依赖：
            // 坏帧嗅探不出 usage，但同样进解码器留痕（warn + 按语义跳过）。
            let frame_text = String::from_utf8_lossy(&frame);
            if frame_text.contains("usage")
                && let Some(sniffed) = protocol::sniff_usage_str(&frame_text, ctx.protocol)
            {
                usage_reported = true;
                usage.union_max(sniffed);
            }
            let decoded = decoder.process(&frame_text);
            if decoded.events.iter().any(|event| match event {
                StreamEvent::Error { .. } => true,
                StreamEvent::Finish { finish_reason, .. } => {
                    finish_reason.unified == crate::core::ir::FinishReasonUnified::Error
                }
                _ => false,
            }) {
                // 响应头已经发出后无法再改变 HTTP 状态；日志仍记录失败终态，
                // 避免 200 掩盖 provider 的明确失败。
                ctx.status_code = 502;
            }
        }
        if sse_buffer.len() > ctx.snapshot.sse_reassembly_max() {
            truncated = true;
            break;
        }
    }
    // 上游消费已结束（自然收尾或断连取消）：先释放字节流再结算。
    drop(byte_stream);

    if downstream_open && ctx.protocol == Protocol::OpenAiChat {
        for trailing in done_filter.finish() {
            if !send_passthrough_chunk(
                &tx,
                trailing,
                ctx.snapshot.full_body,
                log_body_max,
                &mut ctx.response_body,
                idle,
            )
            .await
            {
                downstream_open = false;
                break;
            }
        }
    }
    if truncated && downstream_open {
        let frame = inbound_stream_error_frame(ctx.protocol, SSE_REASSEMBLY_OVERFLOW_MESSAGE);
        let wire = Bytes::from(frame_to_wire(&frame));
        if !send_passthrough_chunk(
            &tx,
            wire,
            ctx.snapshot.full_body,
            log_body_max,
            &mut ctx.response_body,
            idle,
        )
        .await
        {
            downstream_open = false;
        }
    }
    // OpenAI 协议约定以 `data: [DONE]` 终止；哨兵也是入站响应的一部分，
    // full_body 开启时在实际下发前记入（结算先于哨兵，日志此时能带全）。
    if ctx.snapshot.full_body && downstream_open && ctx.protocol == Protocol::OpenAiChat {
        append_logged_body(
            &mut ctx.response_body,
            &data_frame_to_wire("[DONE]"),
            log_body_max,
        );
    }
    // 流结束：按嗅探累积的 usage 结算并落日志。
    let discount_bp = ctx.snapshot.discount_bp_for_token(&ctx.token);
    if let Err(err) = queue_request_log(
        &ctx.deps,
        RequestLogDraft {
            token: &ctx.token,
            model: &ctx.request_model,
            outbound_model: outbound_model_for_log(&ctx.channel, &ctx.routed_model),
            channel: &ctx.channel.name,
            channel_key: Some(&ctx.channel_key_name),
            status: ctx.status_code,
            started: ctx.started,
            billing: Billing::calculated(
                usage,
                usage_reported,
                ctx.price,
                discount_bp,
                ctx.request_body.clone(),
                ctx.snapshot.full_body.then(|| ctx.response_body.clone()),
            ),
            inbound_protocol: ctx.protocol,
            request_id: &ctx.request_id,
            billing_attempt_id: Some(&ctx.billing_attempt_id),
            upstream_reached: true,
            dispatched: true,
            deadline: Some(ctx.log_deadline),
        },
    )
    .await
    {
        tracing::error!(error = %err, request_id = %ctx.request_id, "流式请求结果无法持久化");
    }
    reservation_heartbeat.abort();
    // OpenAI 协议约定以 `data: [DONE]` 终止；Anthropic 以上游
    // message_stop 收尾，无需哨兵。
    if downstream_open && ctx.protocol == Protocol::OpenAiChat {
        let _ = send_to_downstream(&tx, bytes::Bytes::from_static(b"data: [DONE]\n\n"), idle).await;
    }
}

/// 下发一个直通块；full_body 仅保留已被响应通道接受的字节，并按日志上限封顶。
///
/// 下发受渠道空闲超时约束；被拒即视为下游断开，返回 `false`。
async fn send_passthrough_chunk(
    tx: &tokio::sync::mpsc::Sender<bytes::Bytes>,
    chunk: bytes::Bytes,
    full_body: bool,
    max_bytes: usize,
    response_body: &mut Vec<u8>,
    idle: Duration,
) -> bool {
    let response_body_len = response_body.len();
    if full_body {
        append_logged_body(response_body, &chunk, max_bytes);
    }
    if send_to_downstream(tx, chunk, idle).await {
        true
    } else {
        response_body.truncate(response_body_len);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Protocol, SessionCacheKeyMode};
    use serde_json::json;

    #[test]
    /// 直通补丁矩阵：三开关 × 下游带键/不带键（流式与非流式同规），
    /// 非 chat 协议一律字节直搬。
    fn passthrough_patch_applies_session_cache_key_matrix() {
        const IDENTITY: &str = "sess-derived";
        let parse =
            |out: &[u8]| -> Value { serde_json::from_slice(out).expect("补丁输出应合法") };
        let body = |prompt_cache_key: Option<&str>| {
            let mut value = json!({ "model": "gpt-4o", "messages": [] });
            if let Some(key) = prompt_cache_key {
                value["prompt_cache_key"] = json!(key);
            }
            serde_json::to_vec(&value).unwrap()
        };

        for stream in [false, true] {
            // off：不回写；下游显式键原样保留。
            let out = passthrough_patch_request(
                &body(Some("downstream-key")),
                stream,
                Protocol::OpenAiChat,
                SessionCacheKeyMode::Off,
                IDENTITY,
            );
            assert_eq!(parse(&out)["prompt_cache_key"], json!("downstream-key"));
            let out = passthrough_patch_request(
                &body(None),
                stream,
                Protocol::OpenAiChat,
                SessionCacheKeyMode::Off,
                IDENTITY,
            );
            assert!(
                parse(&out).get("prompt_cache_key").is_none(),
                "off 不应回写"
            );

            // auto：不覆盖下游显式键，缺席时回写派生标识。
            let out = passthrough_patch_request(
                &body(Some("downstream-key")),
                stream,
                Protocol::OpenAiChat,
                SessionCacheKeyMode::Auto,
                IDENTITY,
            );
            assert_eq!(parse(&out)["prompt_cache_key"], json!("downstream-key"));
            let out = passthrough_patch_request(
                &body(None),
                stream,
                Protocol::OpenAiChat,
                SessionCacheKeyMode::Auto,
                IDENTITY,
            );
            assert_eq!(parse(&out)["prompt_cache_key"], json!(IDENTITY));

            // always：无条件覆盖。
            let out = passthrough_patch_request(
                &body(Some("downstream-key")),
                stream,
                Protocol::OpenAiChat,
                SessionCacheKeyMode::Always,
                IDENTITY,
            );
            assert_eq!(parse(&out)["prompt_cache_key"], json!(IDENTITY));
        }

        // 非 chat 协议：即使开关开启也不改写（字节级原样）。
        let raw = body(None);
        let out = passthrough_patch_request(
            &raw,
            false,
            Protocol::AnthropicMessages,
            SessionCacheKeyMode::Always,
            IDENTITY,
        );
        assert_eq!(out, raw, "非 chat 协议应字节直搬");
    }

    /// 流式直通的 include_usage 补丁照常注入，且与会话缓存键回写共存。
    #[test]
    fn passthrough_stream_patch_injects_include_usage_alongside_cache_key() {
        let raw = br#"{"model":"gpt-4o","messages":[],"stream_options":{"other":1}}"#;
        let out = passthrough_patch_request(
            raw,
            true,
            Protocol::OpenAiChat,
            SessionCacheKeyMode::Auto,
            "sess",
        );
        let value: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["stream_options"]["other"], json!(1), "既有字段保留");
        assert_eq!(value["stream_options"]["include_usage"], json!(true));
        assert_eq!(value["prompt_cache_key"], json!("sess"));
    }
}
