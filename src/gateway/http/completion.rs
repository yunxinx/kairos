//! IR 完整路径的出站补全：非流式与流式。
//!
//! 跨协议或命中别名时经规范表示转换：按渠道协议编码出站、解码响应为 IR，
//! 再重编码为入站协议；流式经流水任务逐帧转换并结算。

use axum::{
    Json,
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event as SseEvent, Sse},
    },
};
use serde_json::Value;

use crate::{
    config::Protocol,
    core::ir::ChatRequest,
    store::resources::{Channel, StoredChannelKey},
};

use super::attempt::begin_attempt_for_call;
use super::stream_task::{
    PeekHead, StreamTask, UpstreamByteStream, peek_stream_head_until, pipe_stream,
    spawn_piped_stream_task,
};
use super::{
    OutboundAuth, OutboundCall, channel_idle, is_retryable_status, parse_retry_after,
    rectify_for_retry, remaining_timeout, take_upstream_body, upstream_error_message,
};
use crate::gateway::failover::{Outbound, channel_request_budget};
use crate::gateway::logging::{Billing, RequestLogDraft, queue_request_log};
use crate::gateway::network::NetworkPolicy;
use crate::gateway::sse::receiver_stream;

use crate::gateway::{protocol, routing};

/// 非流式出站调用单个渠道，返回可重试判定。
///
/// 按渠道协议编码出站请求、调用上游、解码响应为 IR，再重编码为入站协议返回。
/// 已发出的尝试无论成败都随结果入队：回报 usage 的按用量结算，缺失 usage
/// 的释放预留并以零费用落日志。
pub(super) async fn non_stream_completion(
    ctx: &mut OutboundCall<'_>,
    request: &ChatRequest,
    channel: &Channel,
    key: &StoredChannelKey,
    allow_rectify: bool,
) -> Outbound {
    let deps = ctx.deps;
    let snapshot = ctx.snapshot;
    let token = ctx.token;
    let price = ctx.price;
    let started = ctx.started;
    let inbound_protocol = ctx.inbound_protocol;
    // 别名重写：请求模型用该渠道自己的出站名。reasoning 兼容输出的自动挡
    // 按出站模型名判定，先于编码解析。
    let outbound_model = routing::outbound_model(channel, ctx.routed_model);
    let reasoning_content = channel
        .reasoning_output
        .enables_reasoning_content(outbound_model, &channel.base_url);
    let mut request_warnings = Vec::new();
    // 入站解码侧的兼容动作（如非法 arguments 兜底）随响应面回传，流式路径同。
    request_warnings.extend(request.warnings.iter().cloned());
    let mut outbound_value = protocol::encode_request_with_model(
        request,
        channel.protocol,
        outbound_model,
        reasoning_content,
        &mut request_warnings,
    );
    // Gemini 的模型名在 URL 路径上，请求体不带（写回会与路径冲突）。
    if let Value::Object(map) = &mut outbound_value
        && channel.protocol != Protocol::Gemini
    {
        map.insert("model".into(), Value::String(outbound_model.to_string()));
    }
    // 会话缓存键回写：按渠道开关把解析出的会话标识写为上游缓存亲和键。
    protocol::write_session_cache_key(
        &mut outbound_value,
        channel.protocol,
        channel.session_cache_key,
        ctx.session_identity,
    );
    // 渠道级自动缓存断点注入：开启时按序为 Anthropic 出站请求补断点。
    protocol::inject_cache_breakpoints(
        &mut outbound_value,
        channel.protocol,
        channel.injects_cache_breakpoints,
    );

    // Gemini 的模型名承载在 URL 路径上；其余协议在请求体（下方补丁）。
    let upstream_url = format!(
        "{}{}",
        channel.base_url.trim_end_matches('/'),
        protocol::upstream_path(channel.protocol, outbound_model, false)
    );
    let network_policy = NetworkPolicy::new(
        snapshot.allow_private_networks,
        &snapshot.private_network_allowlist,
    );
    if let Err(err) = network_policy.validate_target(&upstream_url) {
        return Outbound::Retryable {
            channel: channel.name.clone(),
            status: None,
            retry_after: None,
            message: err.to_string(),
        };
    }

    let billing_attempt_id = match begin_attempt_for_call(ctx, request, channel, key, price).await {
        Ok(attempt) => attempt,
        Err(outbound) => return outbound,
    };
    if let Err(outbound) = billing_attempt_id.mark_dispatched().await {
        return outbound;
    }
    let upstream = deps
        .outbound_clients
        .for_policy(
            &network_policy,
            network_policy.target_allowlisted(&upstream_url),
        )
        .post(&upstream_url)
        .timeout(remaining_timeout(
            ctx.deadline,
            channel_idle(channel.timeout_ms),
        ))
        .apply_outbound_auth(channel.protocol, key)
        .apply_feature_headers(ctx.inbound_headers)
        .json(&outbound_value)
        .send()
        .await;

    let resp = match upstream {
        Ok(resp) => resp,
        Err(err) => {
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
                        message: "上游不可达".to_string(),
                    },
                )
                .await;
        }
    };

    let status = resp.status();
    let status_code = status.as_u16();
    let retry_after = parse_retry_after(resp.headers());
    let upstream_body = match tokio::time::timeout(
        remaining_timeout(ctx.deadline, channel_idle(channel.timeout_ms)),
        take_upstream_body(
            resp,
            &channel.name,
            snapshot.max_response_bytes,
            "上游读体失败",
        ),
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
                        message: "上游读体超时".to_string(),
                    },
                )
                .await;
        }
    };
    let parsed = serde_json::from_slice::<Value>(&upstream_body).unwrap_or(Value::Null);
    let reported_usage = protocol::sniff_usage(&parsed, channel.protocol);

    if status.is_success() {
        // 解码上游响应为 IR，结算费用，再重编码为入站协议返回。
        // 命中别名时重写响应模型名为入站短名。
        match protocol::decode_response(&parsed, channel.protocol) {
            Ok(mut ir) => {
                if request.model != outbound_model {
                    ir.model = request.model.clone();
                }
                // 请求侧转换的信息损失随响应回传，下游可感知而非莫名降级。
                ir.warnings.extend(request_warnings);
                let usage = &ir.usage;
                let usage_reported = reported_usage.is_some();
                let discount_bp = snapshot.discount_bp_for_token(token);
                // Responses 允许以 HTTP 2xx 承载 `status=failed`。该状态不是正常
                // stop：保留已回报 usage 参与结算，但向下游返回明确失败，避免调用
                // 方把空输出当成成功结果继续业务流程。
                let response_failed = matches!(
                    ir.finish_reason.unified,
                    crate::core::ir::FinishReasonUnified::Error
                );
                let returned_status = if response_failed { 502 } else { status_code };
                let inbound = if response_failed {
                    protocol::encode_error(
                        returned_status,
                        &upstream_error_message(&parsed, returned_status),
                        inbound_protocol,
                    )
                } else {
                    protocol::encode_response(&ir, inbound_protocol)
                };
                // full_body 记录实际返回下游的入站响应字节（重编码结果）；
                // 跨协议时它与上游响应体不同，不能拿上游字节顶替。
                let inbound_wire = snapshot
                    .full_body
                    .then(|| serde_json::to_vec(&inbound).unwrap_or_default());
                // 与直通及流式路径共用同一错误语义：交付 provider 结果，同时
                // 把不可结算原因持久化隔离，避免协议模式影响调用结果。
                let billing = Billing::calculated(
                    usage.clone(),
                    usage_reported,
                    price,
                    discount_bp,
                    ctx.request_body.clone(),
                    inbound_wire,
                );
                if let Err(err) = queue_request_log(
                    deps,
                    RequestLogDraft {
                        token,
                        model: &request.model,
                        outbound_model: Some(outbound_model),
                        channel: &channel.name,
                        channel_key: Some(&key.name),
                        status: returned_status,
                        started,
                        billing,
                        inbound_protocol,
                        request_id: ctx.request_id,
                        billing_attempt_id: Some(billing_attempt_id.id()),
                        upstream_reached: true,
                        dispatched: true,
                        deadline: Some(ctx.deadline),
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
                if response_failed {
                    Outbound::Success((StatusCode::BAD_GATEWAY, Json(inbound)).into_response())
                } else {
                    Outbound::Success(Json(inbound).into_response())
                }
            }
            Err(err) => {
                let message = format!("上游响应无法解析: {err}");
                billing_attempt_id
                    .record_failure(
                        502,
                        Some(upstream_body.to_vec()),
                        reported_usage.clone(),
                        None,
                        Outbound::Fatal {
                            channel: channel.name.clone(),
                            status: 502,
                            message,
                        },
                    )
                    .await
            }
        }
    } else if is_retryable_status(status_code) {
        // 可重试错误（429/5xx）：failover 到下一渠道。
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
        let message = upstream_error_message(&parsed, status_code);
        // 上游 400 中可自动修正的一类：整流后重试一次，重试结果按常规语义
        // 上抛（成功/可重试/仍失败各自处理）。每渠道至多一次由 `allow_rectify`
        // 关断与 400 的 Fatal 短路共同保证。
        if allow_rectify
            && status_code == 400
            && let Some(corrected) =
                rectify_for_retry(deps, snapshot, request, channel, &message).await
        {
            let recorded = billing_attempt_id
                .record_failure(
                    status_code,
                    Some(upstream_body.to_vec()),
                    reported_usage.clone(),
                    None,
                    Outbound::Retryable {
                        channel: channel.name.clone(),
                        status: Some(status_code),
                        retry_after: None,
                        message: "请求已整流，准备重试".to_string(),
                    },
                )
                .await;
            if matches!(&recorded, Outbound::Fatal { status: 500, .. }) {
                return recorded;
            }
            return Box::pin(non_stream_completion(ctx, &corrected, channel, key, false)).await;
        }
        // 其余不可重试 4xx：直接返回，状态码原样 + 入站协议错误格式。
        billing_attempt_id
            .record_failure(
                status_code,
                Some(upstream_body.to_vec()),
                reported_usage,
                None,
                Outbound::Fatal {
                    channel: channel.name.clone(),
                    status: status_code,
                    message,
                },
            )
            .await
    }
}

/// 流式出站调用单个渠道：SSE 全链路，返回可重试判定。
///
/// 按渠道协议编码出站请求（强制流式，OpenAI 另注入 `stream_options.include_usage`
/// 供计费），逐 SSE 帧解码为 IR 流事件，仅保留 finish 携带的 usage，
/// 同时重编码为入站协议 SSE 帧流回下游。流结束后按该 usage 结算并落日志。
pub(super) async fn stream_completion(
    ctx: &mut OutboundCall<'_>,
    request: &ChatRequest,
    channel: &Channel,
    key: &StoredChannelKey,
    allow_rectify: bool,
) -> Outbound {
    let deps = ctx.deps;
    let snapshot = ctx.snapshot;
    let token = ctx.token;
    let price = ctx.price;
    let started = ctx.started;
    let inbound_protocol = ctx.inbound_protocol;
    let mut request_warnings = Vec::new();
    // 入站解码侧的兼容动作随流首下发，非流式路径同。
    request_warnings.extend(request.warnings.iter().cloned());
    let outbound_model = routing::outbound_model(channel, ctx.routed_model);
    let reasoning_content = channel
        .reasoning_output
        .enables_reasoning_content(outbound_model, &channel.base_url);
    let mut outbound = protocol::encode_request_with_model(
        request,
        channel.protocol,
        outbound_model,
        reasoning_content,
        &mut request_warnings,
    );
    // 目标性 JSON 补丁：强制流式；OpenAI 另注入 stream_options.include_usage
    // （Anthropic 流式自带 usage）。别名重写用该渠道自己的出站模型名。
    // Gemini 的流式与否由路径端点决定，请求体无 stream/model 字段；
    // 其余协议在体上强制流式（OpenAI 另注入 include_usage 供计费）。
    if let Value::Object(map) = &mut outbound
        && channel.protocol != Protocol::Gemini
    {
        map.insert("stream".into(), Value::Bool(true));
        if channel.protocol == Protocol::OpenAiChat {
            map.insert(
                "stream_options".into(),
                serde_json::json!({ "include_usage": true }),
            );
        }
        map.insert("model".into(), Value::String(outbound_model.to_string()));
    }
    // 会话缓存键回写：与流式路径同规，多轮流式请求同样获得缓存亲和。
    protocol::write_session_cache_key(
        &mut outbound,
        channel.protocol,
        channel.session_cache_key,
        ctx.session_identity,
    );
    // 渠道级自动缓存断点注入：与流式路径同规。
    protocol::inject_cache_breakpoints(
        &mut outbound,
        channel.protocol,
        channel.injects_cache_breakpoints,
    );
    let upstream_url = format!(
        "{}{}",
        channel.base_url.trim_end_matches('/'),
        protocol::upstream_path(channel.protocol, outbound_model, true)
    );
    let network_policy = NetworkPolicy::new(
        snapshot.allow_private_networks,
        &snapshot.private_network_allowlist,
    );
    if let Err(err) = network_policy.validate_target(&upstream_url) {
        return Outbound::Retryable {
            channel: channel.name.clone(),
            status: None,
            retry_after: None,
            message: err.to_string(),
        };
    }

    // 渠道 timeout 只约束到响应头（send 返回）：reqwest 的 `.timeout` 覆盖到
    // 响应体读完，会把长流式响应截断；流建立后上游读取与下发只受渠道空闲
    // 超时约束，请求总时限到此为止。
    let billing_attempt_id = match begin_attempt_for_call(ctx, request, channel, key, price).await {
        Ok(attempt) => attempt,
        Err(outbound) => return outbound,
    };
    if let Err(outbound) = billing_attempt_id.mark_dispatched().await {
        return outbound;
    }
    let upstream = tokio::time::timeout(
        remaining_timeout(ctx.deadline, channel_idle(channel.timeout_ms)),
        deps.outbound_clients
            .for_policy(
                &network_policy,
                network_policy.target_allowlisted(&upstream_url),
            )
            .post(&upstream_url)
            .apply_outbound_auth(channel.protocol, key)
            .apply_feature_headers(ctx.inbound_headers)
            .json(&outbound)
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
                        message: "流式上游不可达".to_string(),
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
                        message: "流式上游响应超时".to_string(),
                    },
                )
                .await;
        }
    };

    let status = resp.status();
    let status_code = status.as_u16();
    // 上游非 2xx：SSE 流此时尚未开始，直接按错误处理。
    if !status.is_success() {
        let retry_after = parse_retry_after(resp.headers());
        let upstream_body = match tokio::time::timeout(
            remaining_timeout(ctx.deadline, channel_idle(channel.timeout_ms)),
            take_upstream_body(
                resp,
                &channel.name,
                ctx.snapshot.max_response_bytes,
                "流式上游读体失败",
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
                            message: "流式上游读体超时".to_string(),
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
        let message = upstream_error_message(&parsed, status_code);
        // 与非流式路径同规：上游 400 的可修正类整流后重试一次（首帧尚未
        // 下发，整流对下游零痕迹）。
        if allow_rectify
            && status_code == 400
            && let Some(corrected) =
                rectify_for_retry(deps, ctx.snapshot, request, channel, &message).await
        {
            let recorded = billing_attempt_id
                .record_failure(
                    status_code,
                    Some(upstream_body.to_vec()),
                    reported_usage.clone(),
                    None,
                    Outbound::Retryable {
                        channel: channel.name.clone(),
                        status: Some(status_code),
                        retry_after: None,
                        message: "请求已整流，准备重试".to_string(),
                    },
                )
                .await;
            if matches!(&recorded, Outbound::Fatal { status: 500, .. }) {
                return recorded;
            }
            return Box::pin(stream_completion(ctx, &corrected, channel, key, false)).await;
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
                    message,
                },
            )
            .await;
    }

    // spawn 流任务前先 peek 首块：首块前的流内错误（200 下的 overloaded_error
    // 等）、空流或首帧超时按 Retryable 上抛换渠道——此时尚未向下游转发任何帧，
    // failover 对下游零痕迹。正常流只多读首个内容帧，peek 受空闲超时约束。
    let idle = channel_idle(channel.timeout_ms);
    let byte_stream: UpstreamByteStream = Box::pin(resp.bytes_stream());
    let (peek, byte_stream) = peek_stream_head_until(
        byte_stream,
        channel.protocol,
        idle,
        ctx.snapshot.sse_reassembly_max(),
        ctx.deadline,
    )
    .await;
    let peeked = match peek {
        PeekHead::Content(frames) => frames,
        PeekHead::UpstreamError(message) => {
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
        PeekHead::Interrupted(reason) => {
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

    // 逐上游 SSE 帧处理：解码 → 提取 usage（计费）→ 重编码为入站 SSE 帧。
    // 在派生任务中消费上游字节流并推送到 mpsc 通道，主函数把通道接成 SSE 响应。
    let (tx, rx) = tokio::sync::mpsc::channel::<SseEvent>(64);
    let ctx = StreamTask {
        deps: deps.clone(),
        snapshot: ctx.snapshot.clone(),
        token: token.clone(),
        request_model: request.model.clone(),
        routed_model: ctx.routed_model.to_string(),
        channel: channel.clone(),
        channel_key_name: key.name.clone(),
        inbound_model: (request.model != outbound_model).then(|| request.model.clone()),
        request_warnings,
        status_code,
        started,
        price,
        inbound_protocol,
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
    spawn_piped_stream_task(
        ctx.request_id.clone(),
        ctx.billing_attempt_id.clone(),
        ctx.channel.name.clone(),
        pipe_stream(byte_stream, tx, ctx),
    );

    let stream = receiver_stream(rx);
    Outbound::Success(Sse::new(stream).into_response())
}
