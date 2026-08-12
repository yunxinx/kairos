//! HTTP 网关实现：入站路由 + 令牌认证 + 渠道选择 + 出站调用 + 请求日志。
//!
//! 本模块承载完整链路：下游以 OpenAI Chat Completions 或 Anthropic Messages 协议
//! 带令牌发请求，网关认证与计费准入后出站到目标渠道。同协议且未命中别名时走
//! 直通快路径（请求体仅目标性补丁、响应字节流直通、逐帧嗅探 usage 计费）；
//! 跨协议或命中别名时经 IR 完整路径转换。协议转换由 `core` 各适配器承担，
//! wire 类型不出适配器边界；本模块经 `protocol` 分派到对应适配器。

use std::{collections::HashMap, time::Duration};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, Sse},
    },
    routing::post,
};
use futures_util::Stream;
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::{
    config::{Channel, Config, Price, Protocol, Token},
    core::billing,
    core::billing::PriceSnapshot,
    core::ir::{ChatRequest, ChatResponse, StreamEvent, Usage},
    core::stream::StreamAccumulator,
    store,
};

use super::failover::{Outbound, run_failover};
use super::logging::{Billing, log_request, unix_millis};
use super::sse::{event_from_frame, receiver_stream, take_frame};

use super::{protocol, routing};

/// 网关依赖：存储连接池 + 出站 HTTP 客户端 + 认证令牌表 + 渠道表 + 价格表。
#[derive(Clone)]
pub struct Deps {
    pub(super) pool: SqlitePool,
    pub(super) client: reqwest::Client,
    pub(super) tokens: HashMap<String, Token>,
    pub(super) channels: Vec<Channel>,
    pub(super) prices: HashMap<String, Price>,
    pub(super) full_body: bool,
}

/// 组装网关路由。`cfg` 持有认证令牌、渠道与价格配置。
pub fn router(cfg: &Config, pool: SqlitePool) -> Router {
    let tokens: HashMap<String, Token> = cfg
        .tokens
        .iter()
        .map(|token| (token.key.clone(), token.clone()))
        .collect();

    // 不设客户端级 timeout：reqwest 的 timeout 覆盖到响应体读完，会截断长流式
    // 响应；超时统一按渠道在请求级施加（非流式 `.timeout`，流式仅约束到响应头）。
    let client = reqwest::Client::builder()
        .build()
        .expect("reqwest client 构建不应失败");

    let deps = Deps {
        pool,
        client,
        tokens,
        channels: cfg.channels.clone(),
        prices: cfg.prices.0.clone(),
        full_body: cfg.logging.full_body,
    };

    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .route("/v1/responses", post(responses))
        .fallback(not_found)
        .with_state(deps)
}

/// 未实现路径的确定响应：404 + 可读提示。
async fn not_found() -> (StatusCode, &'static str) {
    (StatusCode::NOT_FOUND, "路径未实现")
}

/// Chat Completions 入站端点。
async fn chat_completions(
    State(deps): State<Deps>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    handle_request(deps, Protocol::OpenAiChat, headers, body).await
}

/// Anthropic Messages 入站端点。
async fn messages(
    State(deps): State<Deps>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    handle_request(deps, Protocol::AnthropicMessages, headers, body).await
}

/// OpenAI Responses 入站端点。
async fn responses(
    State(deps): State<Deps>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    handle_request(deps, Protocol::OpenAiResponses, headers, body).await
}

/// 入站端点公共处理：认证 → 解码 → 准入 →（直通快路径 | IR 完整路径）。
///
/// `inbound_protocol` 决定入站解码/响应编码与错误格式；出站侧按渠道 `protocol`
/// 分派。同协议且未命中别名时走直通快路径（响应字节流直通、逐帧嗅探 usage
/// 计费），否则经 IR 完整路径。
async fn handle_request(
    deps: Deps,
    inbound_protocol: Protocol,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let started = unix_millis();
    let request_body_for_log = deps.full_body.then(|| body.to_vec());

    // 1. 认证：Bearer 或 x-api-key 两种头都接受。认证先行，未认证不出站。
    let token = match authenticate(&deps, &headers) {
        Ok(token) => token,
        Err(err) => {
            let message = err.to_string();
            return error_response(
                StatusCode::UNAUTHORIZED,
                &message,
                &deps,
                None,
                None,
                started,
                inbound_protocol,
            )
            .await;
        }
    };

    // 2. 解码入站请求为 IR（同时用于准入与出站路径选择）。
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            let message = format!("请求体不是合法 JSON: {err}");
            return error_response(
                StatusCode::BAD_REQUEST,
                &message,
                &deps,
                Some(token),
                None,
                started,
                inbound_protocol,
            )
            .await;
        }
    };
    let request = match protocol::decode_request(&parsed, inbound_protocol) {
        Ok(request) => request,
        Err(err) => {
            let message = format!("请求体无法解析为入站协议: {err}");
            return error_response(
                StatusCode::BAD_REQUEST,
                &message,
                &deps,
                Some(token),
                None,
                started,
                inbound_protocol,
            )
            .await;
        }
    };

    // 3. 准入：模型必须有候选渠道（按 failover 顺序排列）。
    let route = match routing::route(&deps.channels, &request.model) {
        Some(route) => route,
        None => {
            let message = format!("模型 {} 未配置任何可用渠道", request.model);
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &message,
                &deps,
                Some(token),
                Some(&request.model),
                started,
                inbound_protocol,
            )
            .await;
        }
    };

    // 4. 计费准入：模型必须配置价格；令牌余额与累计上限须通过。
    let price = match deps.prices.get(&request.model) {
        Some(price) => billing::PriceSnapshot::from_price(price),
        None => {
            let message = format!("模型 {} 未配置价格，无法计费", request.model);
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &message,
                &deps,
                Some(token),
                Some(&request.model),
                started,
                inbound_protocol,
            )
            .await;
        }
    };
    let mut conn = match deps.pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => {
            return db_error_response(&deps, token, &request.model, started, err, inbound_protocol)
                .await;
        }
    };
    let balance = match store::ensure_token_balance(
        &mut conn,
        &token.key,
        token.balance_usd,
        started,
    )
    .await
    {
        Ok(balance) => balance,
        Err(err) => {
            return db_error_response(&deps, token, &request.model, started, err, inbound_protocol)
                .await;
        }
    };
    if balance.balance_usd_micros <= 0 {
        let message = format!(
            "令牌 {} 余额不足（当前 {:.2} USD）",
            token.name,
            balance.balance_usd_micros as f64 / 1_000_000.0
        );
        return error_response(
            StatusCode::PAYMENT_REQUIRED,
            &message,
            &deps,
            Some(token),
            Some(&request.model),
            started,
            inbound_protocol,
        )
        .await;
    }
    if let Some(limit) = token.limit_usd {
        let limit_micros = (limit * 1_000_000.0).round() as i64;
        if balance.settled_usd_micros >= limit_micros {
            let message = format!(
                "令牌 {} 累计结算已超上限（limit_usd = {:.2}）",
                token.name, limit
            );
            return error_response(
                StatusCode::PAYMENT_REQUIRED,
                &message,
                &deps,
                Some(token),
                Some(&request.model),
                started,
                inbound_protocol,
            )
            .await;
        }
    }

    // 5. 出站：同协议且未命中别名时走直通快路径，否则经 IR 完整路径。
    // 快路径不免认证与计费（上面已准入），且 failover 同样只发生在首字节之前。
    // 直通需全部候选渠道同协议：跨协议 failover 会向异协议渠道发原生字节，故此时回落 IR。
    let passthrough = request.model == route.outbound_model
        && route
            .channels
            .iter()
            .all(|c| c.protocol == inbound_protocol);
    if passthrough {
        let passthrough_ctx = PassthroughCtx {
            deps: &deps,
            request: &request,
            token,
            price,
            started,
            raw_body: &body,
            inbound_protocol,
            request_body: request_body_for_log,
        };
        return passthrough_with_failover(&passthrough_ctx, &route).await;
    }

    outbound_with_failover(
        &deps,
        &request,
        &route,
        token,
        price,
        started,
        inbound_protocol,
        request_body_for_log,
    )
    .await
}

/// 单次出站调用的请求侧上下文：入站请求、路由、认证令牌与计费/日志所需的
/// 请求级信息。作为 `*_completion` 的参数打包，避免过长参数列表。
struct CallCtx<'a> {
    deps: &'a Deps,
    request: &'a ChatRequest,
    route: &'a routing::Route,
    token: &'a Token,
    price: PriceSnapshot,
    started: i64,
    /// 入站 wire 协议：响应重编码与错误格式按此分派。
    inbound_protocol: Protocol,
    request_body: Option<Vec<u8>>,
}

/// 按渠道路由顺序发起出站调用，遇可重试错误自动 failover。
///
/// 每个候选渠道按其自身 `max_retries` 尝试（首试 + max_retries 次重试）；
/// 渠道耗尽或请求须整体失败时切换到下一候选。剩余候选全失败或遇到不可
/// 重试 4xx 时返回最终错误响应。成功时返回下游响应。
#[allow(clippy::too_many_arguments)]
async fn outbound_with_failover(
    deps: &Deps,
    request: &ChatRequest,
    route: &routing::Route,
    token: &Token,
    price: PriceSnapshot,
    started: i64,
    inbound_protocol: Protocol,
    request_body_for_log: Option<Vec<u8>>,
) -> Response {
    run_failover(
        route,
        |channel| {
            let channel = channel.clone();
            let request_body_for_log = request_body_for_log.clone();
            Box::pin(async move {
                let mut ctx = CallCtx {
                    deps,
                    request,
                    route,
                    token,
                    price,
                    started,
                    inbound_protocol,
                    request_body: request_body_for_log.clone(),
                };
                if request.stream {
                    stream_completion(&mut ctx, &channel).await
                } else {
                    non_stream_completion(&mut ctx, &channel).await
                }
            })
        },
        |channel, status, _failover| {
            let channel = channel.to_string();
            Box::pin(async move {
                log_request(
                    deps,
                    token,
                    &request.model,
                    &channel,
                    status,
                    started,
                    Billing::default(),
                    inbound_protocol,
                )
                .await;
            })
        },
        inbound_protocol,
    )
    .await
}

/// 直通快路径的请求侧共享上下文：出站目标、计费与日志所需的请求级信息。
///
/// 打包 `passthrough_*_completion` 的公共参数，避免过长参数列表。`raw_body`
/// 用于出站请求的目标性补丁。
struct PassthroughCtx<'a> {
    deps: &'a Deps,
    request: &'a ChatRequest,
    token: &'a Token,
    price: PriceSnapshot,
    started: i64,
    raw_body: &'a [u8],
    /// 入站 wire 协议（直通时与出站同协议）。
    inbound_protocol: Protocol,
    request_body: Option<Vec<u8>>,
}

/// 直通快路径：按渠道路由顺序发起同协议出站调用，遇可重试错误自动 failover。
///
/// 与 IR 路径的 failover 语义一致（共用 [`run_failover`]）：可重试错误（网络
/// 错误/429/5xx）在首字节之前切换下一渠道重试；不可重试 4xx 直接返回。快路径
/// 同样不免认证与计费（已在准入阶段完成）。
async fn passthrough_with_failover(ctx: &PassthroughCtx<'_>, route: &routing::Route) -> Response {
    run_failover(
        route,
        |channel| {
            let channel = channel.clone();
            Box::pin(async move {
                if ctx.request.stream {
                    passthrough_stream_completion(ctx, &channel).await
                } else {
                    passthrough_non_stream_completion(ctx, &channel).await
                }
            })
        },
        |channel, status, _failover| {
            let channel = channel.to_string();
            Box::pin(async move {
                log_request(
                    ctx.deps,
                    ctx.token,
                    &ctx.request.model,
                    &channel,
                    status,
                    ctx.started,
                    Billing::default(),
                    ctx.inbound_protocol,
                )
                .await;
            })
        },
        ctx.inbound_protocol,
    )
    .await
}

/// 直通快路径的流式出站：字节流直通转发，逐 SSE 帧嗅探 usage 计费。
///
/// 请求体仅做目标性补丁（OpenAI 流式注入 `stream_options.include_usage` 供计费，
/// Anthropic 无需补丁），响应以字节流直通到下游，不做完整解码。流结束后按嗅探
/// 累积的 usage 结算并落日志。渠道 timeout 只约束到响应头。
async fn passthrough_stream_completion(ctx: &PassthroughCtx<'_>, channel: &Channel) -> Outbound {
    let outbound = passthrough_patch_request(ctx.raw_body, true, channel.protocol);
    let upstream_url = passthrough_upstream_url(channel);

    let upstream = tokio::time::timeout(
        Duration::from_millis(channel.timeout_ms),
        ctx.deps
            .client
            .post(&upstream_url)
            .apply_outbound_auth(channel)
            .header("content-type", "application/json")
            .body(outbound)
            .send(),
    )
    .await;

    let resp = match upstream {
        Ok(Ok(resp)) => resp,
        Ok(Err(_)) => {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: None,
                message: "直通流式上游不可达".to_string(),
            };
        }
        Err(_) => {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: None,
                message: "直通流式上游响应超时".to_string(),
            };
        }
    };

    let status_code = resp.status().as_u16();
    if !resp.status().is_success() {
        let upstream_body = resp.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<Value>(&upstream_body).unwrap_or(Value::Null);
        if is_retryable_status(status_code) {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: Some(status_code),
                message: "上游返回可重试错误".to_string(),
            };
        }
        return Outbound::Fatal {
            channel: channel.name.clone(),
            status: status_code,
            message: upstream_error_message(&parsed, status_code),
        };
    }

    // 逐 SSE 帧嗅探 usage 计费，同时原样转发字节流到下游。
    let task = PassthroughStreamTask {
        deps: ctx.deps.clone(),
        token: ctx.token.clone(),
        request: ctx.request.clone(),
        channel: channel.clone(),
        status_code,
        started: ctx.started,
        price: ctx.price,
        protocol: channel.protocol,
        request_body: ctx.request_body.clone(),
        response_body: Vec::new(),
    };
    let byte_stream = resp.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel::<SseEvent>(64);
    tokio::spawn(async move {
        pipe_passthrough_stream(byte_stream, tx, task).await;
    });

    let stream = receiver_stream(rx);
    Outbound::Success(Sse::new(stream).into_response())
}

/// 直通快路径的非流式出站：响应体整体透传，从响应 JSON 嗅探 usage 计费。
///
/// 请求体仅做目标性补丁（OpenAI 流式注入 stream_options；Anthropic 无需），
/// 响应体原样返回，不做完整解码。
async fn passthrough_non_stream_completion(
    ctx: &PassthroughCtx<'_>,
    channel: &Channel,
) -> Outbound {
    let outbound = passthrough_patch_request(ctx.raw_body, false, channel.protocol);
    let upstream_url = passthrough_upstream_url(channel);

    let upstream = tokio::time::timeout(
        Duration::from_millis(channel.timeout_ms),
        ctx.deps
            .client
            .post(&upstream_url)
            .apply_outbound_auth(channel)
            .header("content-type", "application/json")
            .body(outbound)
            .send(),
    )
    .await;

    let resp = match upstream {
        Ok(Ok(resp)) => resp,
        Ok(Err(_)) => {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: None,
                message: "直通非流式上游不可达".to_string(),
            };
        }
        Err(_) => {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: None,
                message: "直通非流式上游响应超时".to_string(),
            };
        }
    };

    let status_code = resp.status().as_u16();
    let is_success = resp.status().is_success();
    // 字节级透传：直接取上游响应字节，不经 String 转码。
    let upstream_body = resp.bytes().await.unwrap_or_default();
    let parsed = serde_json::from_slice::<Value>(&upstream_body).unwrap_or(Value::Null);

    if is_success {
        // 响应体原样透传（字节级一致），从 JSON 嗅探 usage 计费。
        let usage = protocol::sniff_usage(&parsed, channel.protocol).unwrap_or_default();
        let cost = billing::cost_micros(&usage, &ctx.price);
        // 成功且 usage 非零才结算；失败或零输出不扣费。
        if cost > 0 {
            match ctx.deps.pool.acquire().await {
                Ok(mut settle_conn) => {
                    if let Err(err) =
                        store::settle_charge(&mut settle_conn, &ctx.token.key, cost).await
                    {
                        eprintln!("直通非流式结算失败: {err}");
                    }
                }
                Err(err) => eprintln!("直通非流式结算连接失败: {err}"),
            }
        }
        log_request(
            ctx.deps,
            ctx.token,
            &ctx.request.model,
            &channel.name,
            status_code,
            ctx.started,
            Billing {
                usage,
                price: ctx.price,
                cost_usd_micros: cost,
                request_body: ctx.request_body.clone(),
                response_body: ctx.deps.full_body.then(|| upstream_body.to_vec()),
            },
            ctx.inbound_protocol,
        )
        .await;
        Outbound::Success(Response::new(Body::from(upstream_body)))
    } else if is_retryable_status(status_code) {
        Outbound::Retryable {
            channel: channel.name.clone(),
            status: Some(status_code),
            message: "上游返回可重试错误".to_string(),
        }
    } else {
        Outbound::Fatal {
            channel: channel.name.clone(),
            status: status_code,
            message: upstream_error_message(&parsed, status_code),
        }
    }
}

/// 直通快路径的出站请求体：以下游请求体为准，仅做目标性 JSON 补丁。
///
/// spec 授权的补丁仅一项：OpenAI 流式时注入 `stream_options.include_usage`（供
/// 逐帧嗅探 usage 计费；非流式响应体已自带顶层 usage）。Anthropic 流式自带
/// usage（message_delta），无需补丁，请求体字节级原样转发。`stream` 字段由下游
/// 请求自带，不做改写。
fn passthrough_patch_request(raw_body: &[u8], stream: bool, protocol: Protocol) -> Vec<u8> {
    if !stream || protocol != Protocol::OpenAiChat {
        return raw_body.to_vec();
    }
    let mut value: Value = match serde_json::from_slice(raw_body) {
        Ok(value) => value,
        Err(_) => return raw_body.to_vec(),
    };
    if let Value::Object(map) = &mut value {
        // 合并而非覆盖：下游自带的 stream_options 其他字段保留，仅补 include_usage。
        let stream_options = map
            .entry("stream_options".to_string())
            .or_insert_with(|| json!({}));
        if let Value::Object(so) = stream_options {
            so.insert("include_usage".into(), Value::Bool(true));
        }
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| raw_body.to_vec())
}

/// 直通快路径的出站 URL：base + 协议路径。
fn passthrough_upstream_url(channel: &Channel) -> String {
    format!(
        "{}{}",
        channel.base_url.trim_end_matches('/'),
        protocol::upstream_path(channel.protocol)
    )
}

/// 直通快路径流式请求的共享任务数据：出站目标、计费与日志所需的请求侧信息。
#[derive(Clone)]
struct PassthroughStreamTask {
    deps: Deps,
    token: Token,
    request: ChatRequest,
    channel: Channel,
    status_code: u16,
    started: i64,
    price: PriceSnapshot,
    /// 直通协议（与入站同协议），用于 usage 嗅探与终止哨兵。
    protocol: Protocol,
    request_body: Option<Vec<u8>>,
    response_body: Vec<u8>,
}

/// 把上游 SSE 字节流逐帧转发到下游，同时逐帧嗅探 usage 计费。
///
/// 每收到一个完整 SSE 数据帧，原样转发给下游（含 `event:` 名，不改装帧），并按
/// 协议嗅探帧内 usage 逐分量取 max 累积计费（Anthropic 的 usage 分散在
/// message_start/message_delta）。上游流结束时按累积 usage 结算并落日志；
/// OpenAI 追加 `[DONE]` 终止哨兵，Anthropic 以上游 message_stop 收尾。
async fn pipe_passthrough_stream<S>(
    byte_stream: S,
    tx: tokio::sync::mpsc::Sender<SseEvent>,
    mut ctx: PassthroughStreamTask,
) where
    S: Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    use futures_util::StreamExt as _;

    let mut usage = Usage::default();
    let mut sse_buffer: Vec<u8> = Vec::new();
    let mut downstream_open = true;
    let mut byte_stream = Box::pin(byte_stream);

    loop {
        // 尝试从已缓冲字节提取完整 SSE 数据帧。
        if let Some((event_name, frame, rest)) = take_frame(&sse_buffer) {
            sse_buffer = rest;
            if frame.is_empty() {
                continue;
            }
            // 逐帧嗅探 usage 计费（不完整解码），多帧逐分量取 max。
            if let Ok(chunk) = serde_json::from_slice::<Value>(&frame)
                && let Some(sniffed) = protocol::sniff_usage(&chunk, ctx.protocol)
            {
                usage.union_max(sniffed);
            }
            if downstream_open {
                let data = String::from_utf8_lossy(&frame).into_owned();
                let mut event = SseEvent::default().data(data);
                if let Some(name) = event_name {
                    event = event.event(name);
                }
                if tx.send(event).await.is_err() {
                    // 下游断开：停止发送，但继续消费上游直至结算。
                    downstream_open = false;
                }
            }
            continue;
        }

        // 缓冲不足一帧：从上游读取更多字节。
        match byte_stream.next().await {
            Some(Ok(bytes)) => {
                if ctx.deps.full_body {
                    ctx.response_body.extend_from_slice(&bytes);
                }
                sse_buffer.extend_from_slice(&bytes);
            }
            Some(Err(_)) | None => {
                // 流结束：按嗅探累积的 usage 结算并落日志。
                let cost = billing::cost_micros(&usage, &ctx.price);
                if cost > 0 {
                    match ctx.deps.pool.acquire().await {
                        Ok(mut settle_conn) => {
                            if let Err(err) =
                                store::settle_charge(&mut settle_conn, &ctx.token.key, cost).await
                            {
                                eprintln!("直通流式结算失败: {err}");
                            }
                        }
                        Err(err) => eprintln!("直通流式结算连接失败: {err}"),
                    }
                }
                log_request(
                    &ctx.deps,
                    &ctx.token,
                    &ctx.request.model,
                    &ctx.channel.name,
                    ctx.status_code,
                    ctx.started,
                    Billing {
                        usage,
                        price: ctx.price,
                        cost_usd_micros: cost,
                        request_body: ctx.request_body.clone(),
                        response_body: ctx.deps.full_body.then(|| ctx.response_body.clone()),
                    },
                    ctx.protocol,
                )
                .await;
                // OpenAI 协议约定以 `data: [DONE]` 终止；Anthropic 以上游
                // message_stop 收尾，无需哨兵。
                if downstream_open && ctx.protocol == Protocol::OpenAiChat {
                    let _ = tx.send(SseEvent::default().data("[DONE]")).await;
                }
                return;
            }
        }
    }
}

/// 把适配器产出的 SSE 帧转为 axum SSE 事件：有事件名时写 `event:` 字段。
/// 非流式出站调用单个渠道，返回可重试判定。
///
/// 按渠道协议编码出站请求、调用上游、解码响应为 IR，再重编码为入站协议返回。
/// 成功且 usage 非零才结算；失败或零输出不扣费。
async fn non_stream_completion(ctx: &mut CallCtx<'_>, channel: &Channel) -> Outbound {
    let deps = ctx.deps;
    let request = ctx.request;
    let route = ctx.route;
    let token = ctx.token;
    let price = ctx.price;
    let started = ctx.started;
    let inbound_protocol = ctx.inbound_protocol;
    // 别名重写：请求模型用出站真实名。
    let mut request_warnings = Vec::new();
    let mut outbound_value =
        protocol::encode_request(request, channel.protocol, &mut request_warnings);
    if let Value::Object(map) = &mut outbound_value {
        map.insert("model".into(), Value::String(route.outbound_model.clone()));
    }

    let upstream_url = format!(
        "{}{}",
        channel.base_url.trim_end_matches('/'),
        protocol::upstream_path(channel.protocol)
    );

    let upstream = deps
        .client
        .post(&upstream_url)
        .timeout(Duration::from_millis(channel.timeout_ms))
        .apply_outbound_auth(channel)
        .json(&outbound_value)
        .send()
        .await;

    let resp = match upstream {
        Ok(resp) => resp,
        Err(_) => {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: None,
                message: "上游不可达".to_string(),
            };
        }
    };

    let status = resp.status();
    let status_code = status.as_u16();
    let upstream_body = resp.text().await.unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&upstream_body).unwrap_or(Value::Null);

    if status.is_success() {
        // 解码上游响应为 IR，结算费用，再重编码为入站协议返回。
        // 命中别名时重写响应模型名为入站短名。
        match protocol::decode_response(&parsed, channel.protocol) {
            Ok(mut ir) => {
                if request.model != route.outbound_model {
                    ir.model = request.model.clone();
                }
                // 请求侧转换的信息损失随响应回传，下游可感知而非莫名降级。
                ir.warnings.extend(request_warnings);
                let usage = &ir.usage;
                let cost = billing::cost_micros(usage, &price);
                // 成功且 usage 非零才结算；失败或零输出不扣费。
                if cost > 0 {
                    match deps.pool.acquire().await {
                        Ok(mut settle_conn) => {
                            if let Err(err) =
                                store::settle_charge(&mut settle_conn, &token.key, cost).await
                            {
                                eprintln!("结算失败: {err}");
                            }
                        }
                        Err(err) => eprintln!("结算连接失败: {err}"),
                    }
                }
                let inbound = protocol::encode_response(&ir, inbound_protocol);
                log_request(
                    deps,
                    token,
                    &request.model,
                    &channel.name,
                    status_code,
                    started,
                    Billing {
                        usage: usage.clone(),
                        price,
                        cost_usd_micros: cost,
                        request_body: ctx.request_body.clone(),
                        response_body: deps.full_body.then(|| upstream_body.as_bytes().to_vec()),
                    },
                    inbound_protocol,
                )
                .await;
                Outbound::Success(Json(inbound).into_response())
            }
            Err(err) => {
                let message = format!("上游响应无法解析: {err}");
                Outbound::Fatal {
                    channel: channel.name.clone(),
                    status: 502,
                    message,
                }
            }
        }
    } else if is_retryable_status(status_code) {
        // 可重试错误（429/5xx）：failover 到下一渠道。
        Outbound::Retryable {
            channel: channel.name.clone(),
            status: Some(status_code),
            message: "上游返回可重试错误".to_string(),
        }
    } else {
        // 不可重试 4xx：直接返回，状态码原样 + 入站协议错误格式。
        Outbound::Fatal {
            channel: channel.name.clone(),
            status: status_code,
            message: upstream_error_message(&parsed, status_code),
        }
    }
}

/// 流式出站调用单个渠道：SSE 全链路，返回可重试判定。
///
/// 按渠道协议编码出站请求（强制流式，OpenAI 另注入 `stream_options.include_usage`
/// 供计费），逐 SSE 帧解码为 IR 流事件，累积为 `ChatResponse` 以取 usage 计费，
/// 同时重编码为入站协议 SSE 帧流回下游。流结束后按累积 usage 结算并落日志。
async fn stream_completion(ctx: &mut CallCtx<'_>, channel: &Channel) -> Outbound {
    let deps = ctx.deps;
    let request = ctx.request;
    let route = ctx.route;
    let token = ctx.token;
    let price = ctx.price;
    let started = ctx.started;
    let inbound_protocol = ctx.inbound_protocol;
    let mut request_warnings = Vec::new();
    let mut outbound = protocol::encode_request(request, channel.protocol, &mut request_warnings);
    // 目标性 JSON 补丁：强制流式；OpenAI 另注入 stream_options.include_usage
    // （Anthropic 流式自带 usage）。别名重写出站模型名。
    if let Value::Object(map) = &mut outbound {
        map.insert("stream".into(), Value::Bool(true));
        if channel.protocol == Protocol::OpenAiChat {
            map.insert(
                "stream_options".into(),
                serde_json::json!({ "include_usage": true }),
            );
        }
        map.insert("model".into(), Value::String(route.outbound_model.clone()));
    }
    let upstream_url = format!(
        "{}{}",
        channel.base_url.trim_end_matches('/'),
        protocol::upstream_path(channel.protocol)
    );

    // 渠道 timeout 只约束到响应头（send 返回）：reqwest 的 `.timeout` 覆盖到
    // 响应体读完，会把长流式响应截断；流一旦开始，时长不受 timeout 限制。
    let upstream = tokio::time::timeout(
        Duration::from_millis(channel.timeout_ms),
        deps.client
            .post(&upstream_url)
            .apply_outbound_auth(channel)
            .json(&outbound)
            .send(),
    )
    .await;

    let resp = match upstream {
        Ok(Ok(resp)) => resp,
        Ok(Err(_)) => {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: None,
                message: "流式上游不可达".to_string(),
            };
        }
        Err(_) => {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: None,
                message: "流式上游响应超时".to_string(),
            };
        }
    };

    let status = resp.status();
    let status_code = status.as_u16();
    // 上游非 2xx：SSE 流此时尚未开始，直接按错误处理。
    if !status.is_success() {
        let upstream_body = resp.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<Value>(&upstream_body).unwrap_or(Value::Null);
        if is_retryable_status(status_code) {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: Some(status_code),
                message: "上游返回可重试错误".to_string(),
            };
        }
        return Outbound::Fatal {
            channel: channel.name.clone(),
            status: status_code,
            message: upstream_error_message(&parsed, status_code),
        };
    }

    // 逐上游 SSE 帧处理：解码 → 累积（计费）→ 重编码为入站 SSE 帧。
    // 在派生任务中消费上游字节流并推送到 mpsc 通道，主函数把通道接成 SSE 响应。
    let (tx, rx) = tokio::sync::mpsc::channel::<SseEvent>(64);
    let byte_stream = resp.bytes_stream();
    let ctx = StreamTask {
        deps: deps.clone(),
        token: token.clone(),
        request: request.clone(),
        channel: channel.clone(),
        inbound_model: (request.model != route.outbound_model).then(|| request.model.clone()),
        request_warnings,
        status_code,
        started,
        price,
        inbound_protocol,
        request_body: ctx.request_body.clone(),
        response_body: Vec::new(),
    };
    tokio::spawn(async move {
        pipe_stream(byte_stream, tx, ctx).await;
    });

    let stream = receiver_stream(rx);
    Outbound::Success(Sse::new(stream).into_response())
}

/// 流式请求的共享任务数据：出站目标、计费与日志所需的请求侧信息。
#[derive(Clone)]
struct StreamTask {
    deps: Deps,
    token: Token,
    request: ChatRequest,
    channel: Channel,
    /// 别名命中时入站模型名（用于重写响应模型名）；`None` 表示不覆盖。
    inbound_model: Option<String>,
    /// 请求侧转换的信息损失，以 `stream-start` 事件在流首下发。
    request_warnings: Vec<crate::core::ir::Warning>,
    status_code: u16,
    started: i64,
    price: PriceSnapshot,
    /// 入站 wire 协议：响应重编码按此分派。
    inbound_protocol: Protocol,
    request_body: Option<Vec<u8>>,
    response_body: Vec<u8>,
}

/// 把上游 SSE 字节流逐帧解码 → 累积 → 重编码，推送到下游通道。
///
/// 每收到一个完整 SSE 数据帧，解码为 IR 流事件并累积（供计费），同时重编码为
/// 入站协议 chunk 帧推给下游。上游流结束时按累积 usage 结算并落日志。
async fn pipe_stream<S>(byte_stream: S, tx: tokio::sync::mpsc::Sender<SseEvent>, ctx: StreamTask)
where
    S: Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    use futures_util::StreamExt as _;

    let mut decoder = protocol::make_decoder(ctx.channel.protocol);
    let mut encoder = protocol::make_encoder(ctx.inbound_protocol, ctx.inbound_model.clone());
    let terminator = encoder.terminator();
    let mut accumulator = StreamAccumulator::new();
    // 以字节缓冲、帧边界后再转文本：多字节 UTF-8 可能被拆在两个字节块里，
    // 提前转换会截坏字符。
    let mut sse_buffer: Vec<u8> = Vec::new();
    let mut saw_finish = false;
    let mut downstream_open = true;
    let mut byte_stream = Box::pin(byte_stream);

    // 流首先下发 message_start（Anthropic 需要；OpenAI 无）与 stream-start 的
    // warnings，让下游在任何内容之前就感知信息损失（跨协议族丢弃的 reasoning 等）。
    if let Some(frame) = encoder.message_start()
        && tx.send(event_from_frame(&frame)).await.is_err()
    {
        downstream_open = false;
    }
    let start_event = StreamEvent::StreamStart {
        warnings: ctx.request_warnings.clone(),
    };
    accumulator.push(start_event.clone());
    for frame in encoder.encode(&start_event) {
        if tx.send(event_from_frame(&frame)).await.is_err() {
            downstream_open = false;
            break;
        }
    }

    loop {
        // 尝试从已缓冲字节提取完整 SSE 数据帧。
        if let Some((_event_name, frame, rest)) = take_frame(&sse_buffer) {
            sse_buffer = rest;
            // 空载荷帧（keep-alive 注释、[DONE] 哨兵）直接消费。
            if frame.is_empty() {
                continue;
            }
            let chunk: Value = serde_json::from_slice(&frame).unwrap_or(Value::Null);
            let decoded = decoder.process(&chunk);
            for event in &decoded.events {
                if matches!(event, StreamEvent::Finish { .. }) {
                    saw_finish = true;
                }
                accumulator.push(event.clone());
                if downstream_open {
                    for frame in encoder.encode(event) {
                        if tx.send(event_from_frame(&frame)).await.is_err() {
                            // 下游断开：停止发送，但继续消费上游直至结算。
                            downstream_open = false;
                            break;
                        }
                    }
                }
            }
            continue;
        }

        // 缓冲不足一帧：从上游读取更多字节。
        match byte_stream.next().await {
            Some(Ok(bytes)) => sse_buffer.extend_from_slice(&bytes),
            Some(Err(_)) | None => {
                // 流结束：若上游未发 finish 帧（异常中断），补发一个。
                let response = accumulator.finish();
                if !saw_finish && downstream_open {
                    let finish_event = StreamEvent::Finish {
                        finish_reason: response.finish_reason.clone(),
                        usage: response.usage.clone(),
                        provider_metadata: response.provider_metadata.clone(),
                    };
                    for frame in encoder.encode(&finish_event) {
                        if tx.send(event_from_frame(&frame)).await.is_err() {
                            break;
                        }
                    }
                }
                // 先结算再发终止哨兵：下游读到终止时计费必定已落库。
                settle_and_log(&ctx, response).await;
                // OpenAI 协议约定以 `data: [DONE]` 结束；Anthropic 以 message_stop 收尾。
                if downstream_open && let Some(terminator) = terminator {
                    let _ = tx.send(SseEvent::default().data(terminator)).await;
                }
                return;
            }
        }
    }
}

/// 把 tokio mpsc 接收端适配为 axum SSE 可消费的流。
///
/// axum 的 `Sse` 需要 `Stream<Item = Result<Event, E>>`；这里把 `Receiver` 的一
/// 个个事件包成 `Ok`。通道关闭（发送端 drop）即流结束。
/// 结算流式请求费用并落日志。
async fn settle_and_log(ctx: &StreamTask, response: ChatResponse) {
    let usage = &response.usage;
    let cost = billing::cost_micros(usage, &ctx.price);
    if cost > 0 {
        match ctx.deps.pool.acquire().await {
            Ok(mut settle_conn) => {
                if let Err(err) = store::settle_charge(&mut settle_conn, &ctx.token.key, cost).await
                {
                    eprintln!("流式结算失败: {err}");
                }
            }
            Err(err) => eprintln!("流式结算连接失败: {err}"),
        }
    }
    log_request(
        &ctx.deps,
        &ctx.token,
        &ctx.request.model,
        &ctx.channel.name,
        ctx.status_code,
        ctx.started,
        Billing {
            usage: response.usage.clone(),
            price: ctx.price,
            cost_usd_micros: cost,
            request_body: ctx.request_body.clone(),
            response_body: ctx.deps.full_body.then(|| ctx.response_body.clone()),
        },
        ctx.inbound_protocol,
    )
    .await;
}

/// 从请求头提取并校验令牌 key，返回匹配的令牌。
fn authenticate<'a>(deps: &'a Deps, headers: &HeaderMap) -> anyhow::Result<&'a Token> {
    let key = extract_key(headers).ok_or_else(|| {
        anyhow::anyhow!("缺少认证令牌：请提供 Authorization: Bearer <key> 或 x-api-key")
    })?;
    deps.tokens
        .get(&key)
        .ok_or_else(|| anyhow::anyhow!("无效的认证令牌"))
}

/// 从两种头任一种提取令牌 key。
fn extract_key(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get("authorization") {
        let value = value.to_str().ok()?;
        if let Some(key) = value.strip_prefix("Bearer ") {
            return Some(key.trim().to_string());
        }
    }
    headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
}

/// 判断上游 HTTP 状态码是否可重试：网络错误与 429/5xx 允许 failover。
fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

/// 按渠道协议设置出站认证头。
///
/// OpenAI 用 `Authorization: Bearer`；Anthropic 用 `x-api-key` 并带
/// `anthropic-version`（官方 SDK 默认头）。
trait OutboundAuth {
    fn apply_outbound_auth(self, channel: &Channel) -> Self;
}

impl OutboundAuth for reqwest::RequestBuilder {
    fn apply_outbound_auth(self, channel: &Channel) -> Self {
        match channel.protocol {
            Protocol::OpenAiChat | Protocol::OpenAiResponses => self.bearer_auth(&channel.api_key),
            Protocol::AnthropicMessages => self
                .header("x-api-key", &channel.api_key)
                .header("anthropic-version", "2023-06-01"),
        }
    }
}

/// 从上游错误 body 提取可读消息（OpenAI/Anthropic 均为 `error.message`），
/// 避免把整个 JSON 串塞进下游 message。
fn upstream_error_message(parsed: &Value, status: u16) -> String {
    parsed
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("上游返回状态码 {status}"))
}

/// 构造入站协议错误格式的响应，并落一条请求日志（无计费数据）。
async fn error_response(
    status: StatusCode,
    message: &str,
    deps: &Deps,
    token: Option<&Token>,
    model: Option<&str>,
    started: i64,
    inbound_protocol: Protocol,
) -> Response {
    let body = protocol::encode_error(status.as_u16(), message, inbound_protocol);
    if let (Some(token), Some(model)) = (token, model) {
        log_request(
            deps,
            token,
            model,
            "",
            status.as_u16(),
            started,
            Billing {
                usage: Usage::default(),
                price: PriceSnapshot::default(),
                cost_usd_micros: 0,
                request_body: None,
                response_body: None,
            },
            inbound_protocol,
        )
        .await;
    }
    (status, Json(body)).into_response()
}

/// 准入阶段数据库错误：500 + OpenAI 错误格式 + 落日志。
async fn db_error_response(
    deps: &Deps,
    token: &Token,
    model: &str,
    started: i64,
    err: impl std::fmt::Display,
    inbound_protocol: Protocol,
) -> Response {
    let message = format!("计费状态读取失败: {err}");
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        &message,
        deps,
        Some(token),
        Some(model),
        started,
        inbound_protocol,
    )
    .await
}
