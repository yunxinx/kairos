//! HTTP 网关实现：入站路由 + 令牌认证 + 渠道选择 + 出站调用 + 请求日志。
//!
//! 本模块承载完整链路：下游以 OpenAI Chat Completions 或 Anthropic Messages 协议
//! 带令牌发请求，网关认证与计费准入后出站到目标渠道。同协议且所有候选都不
//! 改写出站名时走直通快路径（请求体仅目标性补丁、响应原始字节块直搬、旁路嗅探
//! usage 计费）；跨协议或任一候选命中别名时经 IR 完整路径转换。协议转换由 `core`
//! 各适配器承担，wire 类型不出适配器边界；本模块经 `protocol` 分派到对应适配器。
//!
//! v2 起运行时资源（渠道/令牌/价格/模型组/统一模型/开关）来自 [`crate::runtime::RuntimeSnapshot`]：
//! 请求在准入时刻抓取一个快照引用，整个请求生命周期只读该引用，不受后续原子
//! 替换影响。入站请求体上限、full_body、认证限流、SSE 重装上限与同渠道退避同样来自快照设置。统一模型按成员
//! 顺序一次只出站一条，该条再走渠道路由；计价按实际打到的成员。三种入站协议
//! 的标准模型列表（`GET /v1/models`）按令牌分组与统一模型隐藏过滤。
//!
//! 文件内布局：本文件承载路由、认证、准入、渠道路由、重试与错误响应面；
//! 出站计费尝试在 [`attempt`]，同协议直通快路径在 [`passthrough`]，IR 完整
//! 路径补全在 [`completion`]，流式任务基础设施在 [`stream_task`]。

use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::{Bytes, BytesMut};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::Sha256;
use sqlx::SqlitePool;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{
    config::Protocol,
    core::billing,
    core::billing::PriceSnapshot,
    core::ir::{ChatRequest, Usage, prefix_hash},
    runtime::{PlanBinding, RuntimeSnapshot, SnapshotHandle},
    store,
    store::resources::{Channel, ChannelRecord, StoredChannelKey, Token, protocol_to_wire},
};

use super::failover::{
    ChannelCooldowns, FailoverOutcome, FailoverPolicy, KeyCooldowns, Outbound, RetryBackoff,
    channel_request_budget, run_failover,
};
use super::logging::{
    Billing, RequestLogDraft, RequestLogWriter, new_request_id, queue_request_log, unix_millis,
};
use super::network::OutboundClients;
use super::rate_limit::{RequestRateLimiter, SessionStickyCache};
use super::rectifier;
use super::throttle::AuthThrottle;

use super::{protocol, routing};

use completion::{non_stream_completion, stream_completion};
use passthrough::{passthrough_non_stream_completion, passthrough_stream_completion};

mod attempt;
mod completion;
mod passthrough;
mod stream_task;

/// 网关依赖：存储连接池 + 出站 HTTP 客户端 + 运行时资源快照句柄。
#[derive(Clone)]
pub struct Deps {
    pub(super) pool: SqlitePool,
    pub(super) outbound_clients: OutboundClients,
    pub(super) snapshot: SnapshotHandle,
    pub(super) auth_throttle: AuthThrottle,
    pub(super) request_rate: RequestRateLimiter,
    pub(super) sticky: SessionStickyCache,
    pub(super) key_cooldowns: KeyCooldowns,
    /// 渠道域跨请求冷却表；与管理面健康端点共享同一实例。
    pub(super) channel_cooldowns: ChannelCooldowns,
    pub(super) request_log_writer: RequestLogWriter,
    /// 进程级活动请求上限；permit 覆盖 provider 调用、下游断连处理和结算入队。
    pub(super) active_requests: Arc<Semaphore>,
}

/// 组装网关路由。`snapshot` 为已加载的运行时资源快照句柄，请求路径从其中读取
/// 当前资源；管理 API 写库后可原子替换该快照使新资源即时生效。`channel_cooldowns`
/// 由组装点创建，与管理面共享同一实例。
pub async fn router(
    pool: SqlitePool,
    snapshot: SnapshotHandle,
    channel_cooldowns: ChannelCooldowns,
) -> Router {
    router_with_writer(
        pool.clone(),
        snapshot,
        RequestLogWriter::start(pool),
        channel_cooldowns,
    )
    .await
}

/// 组装网关路由并注入请求日志写入器。
pub async fn router_with_writer(
    pool: SqlitePool,
    snapshot: SnapshotHandle,
    request_log_writer: RequestLogWriter,
    channel_cooldowns: ChannelCooldowns,
) -> Router {
    // 不设客户端级 timeout：reqwest 的 timeout 覆盖到响应体读完，会截断长流式
    // 响应。超时按阶段施加：请求总时限只约束首字节之前（含流首 peek 与
    // failover），流建立后上游读取与下游下发仅受渠道空闲超时约束。
    let deps = Deps {
        pool,
        outbound_clients: OutboundClients::new(),
        snapshot,
        auth_throttle: AuthThrottle::new(),
        request_rate: RequestRateLimiter::new(),
        sticky: SessionStickyCache::new(),
        key_cooldowns: KeyCooldowns::new(),
        channel_cooldowns,
        request_log_writer,
        active_requests: Arc::new(Semaphore::new(128)),
    };

    // 禁用 axum 默认的 2MB 请求体上限：入站上限来自运行时开关 `max_request_bytes`，
    // 由 handler 按 Content-Length 提前 413，再对流式读取施加同一字节上限。若不禁用，
    // 使用 `Bytes`/`Json` 等提取器的路径会被 2MB 截住，使大于 2MB 的合法运行时上限失效。
    // `layer` 只作用于其之前已添加的路由，故先注册路由再挂层。
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .route("/v1/responses", post(responses))
        .route("/v1/models", get(list_models))
        // Gemini 入站端点：模型名承载在路径上（model-in-path），动作后缀区分
        // 非流式与流式（流式官方要求 `?alt=sse`，此处不强制——端点本身即 SSE）。
        .route("/v1beta/models", get(list_models_gemini))
        .route("/v1beta/models/{*model_method}", post(generate_content))
        .fallback(not_found)
        .layer(DefaultBodyLimit::disable())
        .with_state(deps)
}

/// 未实现路径的确定响应：404 + 可读提示。
async fn not_found() -> (StatusCode, &'static str) {
    (StatusCode::NOT_FOUND, "路径未实现")
}

/// Chat Completions 入站端点。
async fn chat_completions(
    State(deps): State<Deps>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    handle_request(deps, Protocol::OpenAiChat, addr.ip(), request, None).await
}

/// Anthropic Messages 入站端点。
async fn messages(
    State(deps): State<Deps>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    handle_request(deps, Protocol::AnthropicMessages, addr.ip(), request, None).await
}

/// OpenAI Responses 入站端点。
async fn responses(
    State(deps): State<Deps>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    handle_request(deps, Protocol::OpenAiResponses, addr.ip(), request, None).await
}

/// Gemini generateContent 入站端点：
/// `POST /v1beta/models/{model}:generateContent`（非流式）与
/// `POST /v1beta/models/{model}:streamGenerateContent`（流式）。
async fn generate_content(
    State(deps): State<Deps>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(model_method): Path<String>,
    request: Request,
) -> Response {
    let Some((model, stream)) = split_model_method(&model_method) else {
        return (StatusCode::NOT_FOUND, "路径未实现").into_response();
    };
    handle_request(
        deps,
        Protocol::Gemini,
        addr.ip(),
        request,
        Some((model, stream)),
    )
    .await
}

/// 拆分 Gemini 路径尾段 `{model}:{action}`；动作不是官方端点时返回 `None`。
fn split_model_method(model_method: &str) -> Option<(String, bool)> {
    let (model, action) = model_method.rsplit_once(':')?;
    let model = model.strip_prefix("models/").unwrap_or(model);
    if model.is_empty() {
        return None;
    }
    match action {
        "generateContent" => Some((model.to_string(), false)),
        "streamGenerateContent" => Some((model.to_string(), true)),
        _ => None,
    }
}

/// 下游标准模型列表：`GET /v1/models`。
///
/// OpenAI Chat Completions 与 Responses 共用官方 Models API（无 `anthropic-version`
/// 时按 OpenAI list 编码）。带 `anthropic-version` 时按 Anthropic list 编码。
/// 认证与现有入站协议一致；成功体至少含各可见模型的 `id`。
async fn list_models(
    State(deps): State<Deps>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let inbound_protocol = list_models_protocol(&headers);
    list_models_for(deps, addr.ip(), headers, inbound_protocol).await
}

/// Gemini 客户端的模型列表端点：`GET /v1beta/models`。
async fn list_models_gemini(
    State(deps): State<Deps>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    list_models_for(deps, addr.ip(), headers, Protocol::Gemini).await
}

/// 模型列表的共享实现：认证、限流与编码按入站协议分派。
async fn list_models_for(
    deps: Deps,
    peer: IpAddr,
    headers: HeaderMap,
    inbound_protocol: Protocol,
) -> Response {
    let snapshot = deps.snapshot.read().await.clone();
    let started = unix_millis();
    let request_id = new_request_id();
    if deps.auth_throttle.is_blocked(
        peer,
        extract_key(&headers).as_deref(),
        snapshot.auth_throttle_max_failures,
        snapshot.auth_throttle_window(),
    ) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "认证尝试过于频繁，请稍后再试",
            &deps,
            snapshot.full_body,
            None,
            None,
            started,
            inbound_protocol,
            None,
            &request_id,
        )
        .await;
    }
    let token = match authenticate(&snapshot, &headers) {
        Ok(token) => token,
        Err(err) => {
            deps.auth_throttle.record_failure(
                peer,
                extract_key(&headers).as_deref(),
                snapshot.auth_throttle_max_failures,
                snapshot.auth_throttle_window(),
            );
            return error_response(
                StatusCode::UNAUTHORIZED,
                &err.to_string(),
                &deps,
                snapshot.full_body,
                None,
                None,
                started,
                inbound_protocol,
                None,
                &request_id,
            )
            .await;
        }
    };
    if let Err(retry_after) = token_rate_limited(&deps, token, &snapshot) {
        return too_many_token_requests(
            &deps,
            snapshot.full_body,
            Some(token),
            None,
            started,
            inbound_protocol,
            None,
            retry_after,
            &request_id,
        )
        .await;
    }

    let ids = if snapshot.token_group_assigned(token) {
        store::resources::visible_model_ids(
            &snapshot.model_groups,
            &snapshot.unified_models,
            snapshot.channels.iter().map(|record| &record.channel),
            &token.model_group,
        )
    } else {
        Vec::new()
    };
    let body = protocol::encode_model_list(&ids, inbound_protocol);
    Json(body).into_response()
}

/// 列表接口的入站协议：Anthropic 客户端必带 `anthropic-version`；其余走 OpenAI
/// Models API（Chat Completions 与 Responses 形状相同）。
fn list_models_protocol(headers: &HeaderMap) -> Protocol {
    if headers.contains_key("anthropic-version") {
        Protocol::AnthropicMessages
    } else {
        Protocol::OpenAiChat
    }
}

/// 入站 body 读取超出 `max_request_bytes` 或底层读失败。
enum LimitedBodyError {
    TooLarge,
    ReadFailed,
}

/// 从 `Content-Length` 头解析声明长度；缺失或无法解析则视为未知（走流式封顶）。
fn declared_content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// 按字节上限流式读取请求体；超过上限立即停止，不把超限块并入缓冲。
async fn read_body_with_limit(
    body: Body,
    max_bytes: u64,
) -> Result<bytes::Bytes, LimitedBodyError> {
    use futures_util::StreamExt as _;

    let max = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut collected = BytesMut::new();
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| LimitedBodyError::ReadFailed)?;
        if collected.len().saturating_add(chunk.len()) > max {
            return Err(LimitedBodyError::TooLarge);
        }
        collected.extend_from_slice(&chunk);
    }
    Ok(collected.freeze())
}

/// 按字节上限读取上游响应体；声明长度或实际读取超过上限立即停止。
async fn read_upstream_body(
    resp: reqwest::Response,
    max_bytes: u64,
) -> Result<bytes::Bytes, LimitedBodyError> {
    if let Some(declared) = resp.content_length()
        && declared > max_bytes
    {
        return Err(LimitedBodyError::TooLarge);
    }
    use futures_util::StreamExt as _;
    let max = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut collected = BytesMut::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| LimitedBodyError::ReadFailed)?;
        if collected.len().saturating_add(chunk.len()) > max {
            return Err(LimitedBodyError::TooLarge);
        }
        collected.extend_from_slice(&chunk);
    }
    Ok(collected.freeze())
}

/// 读取上游非流式或错误响应体；超限或读失败转为可换渠道的 Retryable。
async fn take_upstream_body(
    resp: reqwest::Response,
    channel: &str,
    max_bytes: u64,
    read_failed: &str,
) -> Result<bytes::Bytes, Outbound> {
    match read_upstream_body(resp, max_bytes).await {
        Ok(body) => Ok(body),
        Err(LimitedBodyError::TooLarge) => Err(oversized_upstream_response(channel, max_bytes)),
        Err(LimitedBodyError::ReadFailed) => Err(Outbound::Retryable {
            channel: channel.to_string(),
            status: None,
            retry_after: None,
            message: read_failed.to_string(),
        }),
    }
}

/// 上游响应超过 `max_response_bytes`：这是本请求与当前策略共同决定的终局结果，
/// 重试只会重新下载同一份超限内容，因此直接终止并保留明确归因。
fn oversized_upstream_response(channel: &str, max_bytes: u64) -> Outbound {
    Outbound::Fatal {
        channel: channel.to_string(),
        status: 502,
        message: format!("上游响应超过上限 {max_bytes} 字节"),
    }
}

/// 入站协议格式的 413。认证之后调用时可归因到令牌；认证前调用则不落日志。
async fn payload_too_large(
    deps: &Deps,
    full_body: bool,
    started: i64,
    inbound_protocol: Protocol,
    max_request_bytes: u64,
    token: Option<&Token>,
    request_id: &str,
) -> Response {
    let message = format!("请求体超过上限 {max_request_bytes} 字节");
    error_response(
        StatusCode::PAYLOAD_TOO_LARGE,
        &message,
        deps,
        full_body,
        token,
        None,
        started,
        inbound_protocol,
        None,
        request_id,
    )
    .await
}

/// 入站端点公共处理：认证 → 解码 → 准入 →（直通快路径 | IR 完整路径）。
///
/// `inbound_protocol` 决定入站解码/响应编码与错误格式；出站侧按渠道 `protocol`
/// 分派。同协议且未命中别名时走直通快路径（响应字节流直通、逐帧嗅探 usage
/// 计费），否则经 IR 完整路径。
async fn handle_request(
    deps: Deps,
    inbound_protocol: Protocol,
    peer: IpAddr,
    request: Request,
    // Gemini 的 model-in-path：`(模型名, 是否流式)`，由路径端点提取；其余
    // 协议模型名与流式标志都在请求体上，传 `None`。
    path_model: Option<(String, bool)>,
) -> Response {
    let started = unix_millis();
    let request_id = new_request_id();
    // 准入时刻抓取快照引用：在途请求持有该引用直到结束，不受后续原子替换影响。
    let snapshot = deps.snapshot.read().await.clone();
    let full_body = snapshot.full_body;
    let max_request_bytes = snapshot.max_request_bytes;

    let (parts, body) = request.into_parts();
    let headers = parts.headers;

    // 1. 认证：只看请求头。限流与认证必须在缓冲 body 之前，避免未认证请求占满
    // 最多 `max_request_bytes` 的内存，也让失败计数不必等 body 读完。
    if deps.auth_throttle.is_blocked(
        peer,
        extract_key(&headers).as_deref(),
        snapshot.auth_throttle_max_failures,
        snapshot.auth_throttle_window(),
    ) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "认证尝试过于频繁，请稍后再试",
            &deps,
            full_body,
            None,
            None,
            started,
            inbound_protocol,
            None,
            &request_id,
        )
        .await;
    }
    let token = match authenticate(&snapshot, &headers) {
        Ok(token) => token,
        Err(err) => {
            deps.auth_throttle.record_failure(
                peer,
                extract_key(&headers).as_deref(),
                snapshot.auth_throttle_max_failures,
                snapshot.auth_throttle_window(),
            );
            let message = err.to_string();
            return error_response(
                StatusCode::UNAUTHORIZED,
                &message,
                &deps,
                full_body,
                None,
                None,
                started,
                inbound_protocol,
                None,
                &request_id,
            )
            .await;
        }
    };

    if let Err(retry_after) = token_rate_limited(&deps, token, &snapshot) {
        return too_many_token_requests(
            &deps,
            full_body,
            Some(token),
            None,
            started,
            inbound_protocol,
            None,
            retry_after,
            &request_id,
        )
        .await;
    }

    // 2. 入站请求体上限：先看 Content-Length，声明即超限则不读 body；无
    // Content-Length（chunked）或声明未超限时，再按同一上限流式读取。
    if let Some(declared) = declared_content_length(&headers)
        && declared > max_request_bytes
    {
        return payload_too_large(
            &deps,
            full_body,
            started,
            inbound_protocol,
            max_request_bytes,
            Some(token),
            &request_id,
        )
        .await;
    }
    let body = match read_body_with_limit(body, max_request_bytes).await {
        Ok(bytes) => bytes,
        Err(LimitedBodyError::TooLarge) => {
            return payload_too_large(
                &deps,
                full_body,
                started,
                inbound_protocol,
                max_request_bytes,
                Some(token),
                &request_id,
            )
            .await;
        }
        Err(LimitedBodyError::ReadFailed) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "读取请求体失败",
                &deps,
                full_body,
                Some(token),
                None,
                started,
                inbound_protocol,
                None,
                &request_id,
            )
            .await;
        }
    };

    // 仅放行后的请求才为 full_body 预取请求字节；`Bytes` 克隆只增加引用计数。
    let request_body_for_log = full_body.then(|| body.clone());

    // 3. 解码入站请求为 IR（同时用于准入与出站路径选择）。
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            let message = format!("请求体不是合法 JSON: {err}");
            return error_response(
                StatusCode::BAD_REQUEST,
                &message,
                &deps,
                full_body,
                Some(token),
                None,
                started,
                inbound_protocol,
                request_body_for_log,
                &request_id,
            )
            .await;
        }
    };
    let mut request = match protocol::decode_request(&parsed, inbound_protocol) {
        Ok(request) => request,
        Err(err) => {
            let message = format!("请求体无法解析为入站协议: {err}");
            return error_response(
                StatusCode::BAD_REQUEST,
                &message,
                &deps,
                full_body,
                Some(token),
                None,
                started,
                inbound_protocol,
                request_body_for_log,
                &request_id,
            )
            .await;
        }
    };
    // model-in-path：Gemini 的模型名与流式标志承载在 URL 路径端点上，
    // 路径为权威来源，覆盖请求体可能自带的同名键。
    if let Some((model, stream)) = path_model {
        request.model = model;
        request.stream = stream;
    }

    // 3. 模型组允许名单：组外、空组、已撤组一律按「不存在」拒绝，不提分组、不用 503。
    if !snapshot.token_group_assigned(token)
        || !store::resources::group_allows(
            &snapshot.model_groups,
            &token.model_group,
            &request.model,
        )
    {
        let message = format!("模型 {} 不存在", request.model);
        return error_response(
            StatusCode::NOT_FOUND,
            &message,
            &deps,
            full_body,
            Some(token),
            Some(&request.model),
            started,
            inbound_protocol,
            request_body_for_log,
            &request_id,
        )
        .await;
    }

    // 4. 准入：解析出站跳（普通模型一条；统一模型按成员顺序，只收已定价可路由的）。
    // IR 已解码，此处才能稳定计算无头请求的前缀亲和标识。
    let session = request_session(&headers, &request, token, &snapshot);
    let session_identity = session_cache_identity(&headers, &request, token, &snapshot);
    let hops = match resolve_route_hops(
        &snapshot,
        &request.model,
        &token.model_group,
        &deps.sticky,
        session,
    ) {
        Ok(hops) => hops,
        Err((status, message)) => {
            return error_response(
                status,
                &message,
                &deps,
                full_body,
                Some(token),
                Some(&request.model),
                started,
                inbound_protocol,
                request_body_for_log,
                &request_id,
            )
            .await;
        }
    };

    // 5. 活动请求容量与 RPM 是两类独立约束：RPM 控制时间窗口内的请求数，
    // Semaphore 控制同时占用 provider、下游响应和结算资源的请求数。排队等待
    // 以首个候选渠道的预首字节预算为界——渠道级总时限自此起算，避免排队后
    // 又重新获得完整的 provider 超时预算。
    let first_channel = &snapshot.channels[hops[0].route.channel_indices[0]].channel;
    let admission_deadline = tokio::time::Instant::now()
        .checked_add(channel_request_budget(first_channel))
        .unwrap_or_else(tokio::time::Instant::now);
    let active_permit = match tokio::time::timeout_at(
        admission_deadline,
        deps.active_requests.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => Arc::new(Mutex::new(Some(permit))),
        Ok(Err(_)) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "网关活动请求容量已关闭",
                &deps,
                full_body,
                Some(token),
                None,
                started,
                inbound_protocol,
                request_body_for_log,
                &request_id,
            )
            .await;
        }
        Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "网关活动请求容量暂时已满",
                &deps,
                full_body,
                Some(token),
                None,
                started,
                inbound_protocol,
                request_body_for_log,
                &request_id,
            )
            .await;
        }
    };

    let inbound_anthropic_version = headers.get("anthropic-version");

    // 6. 出站：按跳顺序一次一条；该跳内再走渠道路由。费用在每次实际调用
    // provider 前按当次渠道价格原子预留，重试和渠道切换各自拥有独立结算身份。
    //
    // 同渠道 failover 由 `run_failover` 处理（429/5xx 可重试，其它 4xx 为 Fatal
    // 立即返回）。统一模型 hop 之间：400 视为请求本身有问题，不再打后续成员；
    // 429/5xx 及其余非 2xx 继续下一成员。hop 间不等待——成员钉在不同渠道，
    // 换成员不是同渠道退避。
    //
    // ever_dispatched 是请求级「曾出站」标记：任一物理尝试真正发送前置位。
    // 终局失败时据此区分「未出站即终局」（落一条 dispatched=0 行，此前这类
    // 请求在日志与统计中完全隐形）与已有逐尝试日志的已派发失败。
    let ever_dispatched = AtomicBool::new(false);
    let mut last_failure: Option<FailoverOutcome> = None;
    for hop in &hops {
        let outcome = dispatch_hop(
            &deps,
            &snapshot,
            &request,
            hop,
            token,
            started,
            inbound_protocol,
            &body,
            request_body_for_log.clone(),
            inbound_anthropic_version,
            &headers,
            &request_id,
            &session_identity,
            active_permit.clone(),
            &ever_dispatched,
        )
        .await;
        if outcome.response.status().is_success() {
            return outcome.response;
        }
        if !should_try_next_hop(outcome.response.status()) {
            last_failure = Some(outcome);
            break;
        }
        last_failure = Some(outcome);
    }
    let terminal = last_failure.expect("准入已保证至少一条可路由跳");
    // 单一终局收口：只有「从未出站的失败」补一条 dispatched=0 的零费用行，
    // 状态码与响应体是实际返回下游的内容；已派发失败的逐尝试行早已入队。
    if !ever_dispatched.load(Ordering::Acquire)
        && let Some(failure) = terminal.failure
    {
        queue_undispatched_failure_log(
            &deps,
            token,
            &request.model,
            failure.status,
            started,
            inbound_protocol,
            snapshot.discount_bp_for_token(token),
            request_body_for_log,
            snapshot.full_body.then_some(failure.wire),
            &request_id,
        )
        .await;
    }
    terminal.response
}

/// 一次出站跳：已登记模型名 + 已定价渠道的 failover 顺序。
///
/// 普通请求只有一条；统一模型按成员顺序，跳过未定价或不可路由的成员。
/// 各渠道单价不同，结算时按实际打到的渠道取价。
struct RouteHop {
    routed_model: String,
    route: routing::Route,
}

/// 单条已登记模型无法作为出站跳的原因。
enum HopDeny {
    NoRoute,
    NoPrice,
}

/// 为钉死的统一成员构造一条出站跳：只走该渠道，不按同名扩到其他渠道。
fn hop_for_member(
    snapshot: &RuntimeSnapshot,
    member: &crate::store::resources::UnifiedMember,
    sticky: &SessionStickyCache,
    session: u64,
) -> Result<RouteHop, HopDeny> {
    let Some((channel_index, record)) = snapshot
        .channels
        .iter()
        .enumerate()
        .find(|(_, record)| record.id == member.channel_id)
    else {
        return Err(HopDeny::NoRoute);
    };
    if !record.channel.enabled
        || !crate::store::resources::channel_lists_callable(&record.channel, &member.model)
    {
        return Err(HopDeny::NoRoute);
    }
    if snapshot
        .price_for_channel(record.id, &member.model)
        .is_none()
    {
        return Err(HopDeny::NoPrice);
    }
    let key_id = sticky
        .select(record.id, &member.model, session, &record.keys)
        .ok_or(HopDeny::NoRoute)?;
    Ok(RouteHop {
        routed_model: member.model.clone(),
        route: routing::Route {
            channel_indices: vec![channel_index],
            selected_key_ids: std::collections::HashMap::from([(record.id, key_id)]),
        },
    })
}

/// 为可调用名构造一条出站跳：须有启用且已定价的渠道。
///
/// 自定义组若把该名钉在若干渠道上，候选只留这些渠道。
fn hop_for_callable(
    snapshot: &RuntimeSnapshot,
    model: &str,
    group_name: &str,
    sticky: &SessionStickyCache,
    session: u64,
) -> Result<RouteHop, HopDeny> {
    let mut route =
        routing::indexed_route(&snapshot.routing_candidates, model).ok_or(HopDeny::NoRoute)?;
    if let Some(group) = snapshot.model_groups.get(group_name)
        && let Some(pinned) = store::resources::pinned_channel_ids(group, model)
    {
        route
            .channel_indices
            .retain(|index| pinned.contains(&snapshot.channels[*index].id));
        if route.channel_indices.is_empty() {
            return Err(HopDeny::NoRoute);
        }
    }
    let had_price = route.channel_indices.iter().any(|index| {
        snapshot
            .price_for_channel(snapshot.channels[*index].id, model)
            .is_some()
    });
    route.channel_indices.retain(|index| {
        let record = &snapshot.channels[*index];
        if snapshot.price_for_channel(record.id, model).is_none() {
            return false;
        }
        let Some(key_id) = sticky.select(record.id, model, session, &record.keys) else {
            return false;
        };
        route.selected_key_ids.insert(record.id, key_id);
        true
    });
    if route.channel_indices.is_empty() {
        return Err(if had_price {
            HopDeny::NoRoute
        } else {
            HopDeny::NoPrice
        });
    }
    Ok(RouteHop {
        routed_model: model.to_string(),
        route,
    })
}

/// 准入已保证该渠道对该可调用名有价格。
fn billed_price(snapshot: &RuntimeSnapshot, record: &ChannelRecord, model: &str) -> PriceSnapshot {
    snapshot
        .price_for_channel(record.id, model)
        .map(PriceSnapshot::from_store_price)
        .expect("准入已过滤无价格渠道")
}

/// 用完整 IR 请求的序列化大小和保守输出上限估算一次实际出站尝试的费用。
///
/// 估算结果只用于准入时的余额预检（在结算发生前冻结额度），不进入任何
/// 结算路径：结算只按上游明确回报的 usage，缺失 usage 的尝试全额释放。
/// IR 序列化包含消息、工具定义、JSON Schema、provider options 及所有类型化
/// 请求字段，因此不会因转换路径遗漏工具或逃生舱而低估输入。UTF-8 字节数是
/// 无 tokenizer 时可验证的保守上界；输入相关的多个价格档取最高单价，避免
/// 把同一份输入字节重复计入缓存读取、缓存写入和 1h 写入。正常结算后释放
/// 未使用的预留差额。
fn estimate_attempt_cost_micros(
    price: PriceSnapshot,
    discount_bp: i64,
    request: &ChatRequest,
) -> Result<i64, String> {
    const DEFAULT_OUTPUT_RESERVATION_TOKENS: u32 = 32_768;
    let max_output_tokens = request
        .max_tokens
        .filter(|tokens| *tokens > 0)
        .unwrap_or(DEFAULT_OUTPUT_RESERVATION_TOKENS);
    let input_tokens = serde_json::to_vec(request)
        .map_err(|err| format!("请求无法序列化以估算费用: {err}"))?
        .len() as u64;
    let mut input_price = price.input_micros;
    if price.cache_read_micros > input_price {
        input_price = price.cache_read_micros;
    }
    if price.cache_write_micros > input_price {
        input_price = price.cache_write_micros;
    }
    if price.cache_write_1h_micros > input_price {
        input_price = price.cache_write_1h_micros;
    }
    let base = billing::cost_micros(
        &Usage {
            input_tokens,
            output_tokens: u64::from(max_output_tokens),
            ..Usage::default()
        },
        &PriceSnapshot {
            input_micros: input_price,
            output_micros: price.output_micros,
            ..PriceSnapshot::default()
        },
    )
    .map_err(|err| format!("保守费用无法计算: {err}"))?;
    billing::discounted_cost_micros(base, discount_bp)
        .map_err(|err| format!("折后保守费用无法计算: {err}"))
}

/// 解析本次请求的出站跳序列。
///
/// 命中统一模型时按成员顺序收集已定价可路由的跳；一条都没有则 503，文案说明
/// 各成员失效原因（不是「模型不存在」）。普通模型保持原 503 文案。
fn resolve_route_hops(
    snapshot: &RuntimeSnapshot,
    model: &str,
    group_name: &str,
    sticky: &SessionStickyCache,
    session: u64,
) -> Result<Vec<RouteHop>, (StatusCode, String)> {
    if let Some(unified) = snapshot.unified_models.get(model) {
        let mut hops = Vec::new();
        let mut reasons = Vec::new();
        for member in &unified.models {
            match hop_for_member(snapshot, member, sticky, session) {
                Ok(hop) => hops.push(hop),
                Err(HopDeny::NoRoute) => {
                    reasons.push(format!("成员 {member} 没有可用渠道"));
                }
                Err(HopDeny::NoPrice) => {
                    reasons.push(format!("成员 {member} 未配置价格"));
                }
            }
        }
        if hops.is_empty() {
            let detail = if reasons.is_empty() {
                "成员列表为空".to_string()
            } else {
                reasons.join("；")
            };
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                format!("统一模型 {model} 没有已定价且可路由的成员：{detail}"),
            ));
        }
        return Ok(hops);
    }
    match hop_for_callable(snapshot, model, group_name, sticky, session) {
        Ok(hop) => Ok(vec![hop]),
        Err(HopDeny::NoRoute) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("模型 {model} 未配置任何可用渠道"),
        )),
        Err(HopDeny::NoPrice) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("模型 {model} 未配置价格，无法计费"),
        )),
    }
}

/// 取请求级会话标识：显式头优先，缺失时使用解码后 IR 的稳定前缀哈希。
fn request_session(
    headers: &HeaderMap,
    request: &ChatRequest,
    token: &Token,
    snapshot: &RuntimeSnapshot,
) -> u64 {
    let signal = headers
        .get("x-kairos-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("prefix:{:016x}", prefix_hash(request)));
    let digest = session_digest(snapshot, token, "sticky", &signal);
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

/// 解析会话标识原文，供会话缓存键回写：显式 `x-kairos-session-id` 头优先，
/// 缺失时用 IR 稳定前缀哈希的十六进制兜底（与粘性路由同源，仅形态不同）。
fn session_cache_identity(
    headers: &HeaderMap,
    request: &ChatRequest,
    token: &Token,
    snapshot: &RuntimeSnapshot,
) -> String {
    let signal = headers
        .get("x-kairos-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("prefix:{:016x}", prefix_hash(request)));
    session_digest_hex(snapshot, token, "prompt-cache", &signal)
}

/// 使用实例级秘密派生用途隔离、按用户作用域的出站缓存标识。
fn session_digest(
    snapshot: &RuntimeSnapshot,
    token: &Token,
    purpose: &str,
    signal: &str,
) -> [u8; 32] {
    // HMAC 接受任意长度密钥；固定 32 字节实例密钥不会触发长度错误。
    let mut mac = Hmac::<Sha256>::new_from_slice(&snapshot.session_cache_secret)
        .expect("HMAC 应接受固定长度实例密钥");
    mac.update(b"kairos/session/v1/");
    mac.update(purpose.as_bytes());
    mac.update(b"/");
    mac.update(token.user_id.to_string().as_bytes());
    mac.update(b"/");
    mac.update(signal.as_bytes());
    let output = mac.finalize().into_bytes();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&output);
    digest
}

fn session_digest_hex(
    snapshot: &RuntimeSnapshot,
    token: &Token,
    purpose: &str,
    signal: &str,
) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = session_digest(snapshot, token, purpose, signal);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 单次出站调用的请求侧上下文：入站请求、认证令牌与计费/日志所需的
/// 请求级信息。作为 `*_completion` 的参数打包，避免过长参数列表。
struct OutboundCall<'a> {
    deps: &'a Deps,
    /// 准入时刻的快照引用（Arc 共享，流式派生任务可克隆）。
    snapshot: &'a Arc<RuntimeSnapshot>,
    /// 本跳出站用的已登记模型名（统一模型时为成员，否则同入站名）。
    routed_model: &'a str,
    token: &'a Token,
    /// 本尝试渠道对该可调用名的单价（准入已过滤无价格渠道）。
    price: PriceSnapshot,
    started: i64,
    /// 入站 wire 协议：响应重编码与错误格式按此分派。
    inbound_protocol: Protocol,
    request_body: Option<Bytes>,
    inbound_headers: &'a HeaderMap,
    request_id: &'a str,
    /// 按调用者隔离的会话缓存标识，供出站缓存亲和使用。
    session_identity: &'a str,
    /// 由请求入口持有的活动容量许可；流式路径会把所有权转交给后台流水任务。
    active_permit: Arc<Mutex<Option<OwnedSemaphorePermit>>>,
    /// 请求级「曾出站」标记；物理尝试真正发送前置位。
    ever_dispatched: &'a AtomicBool,
    /// 本渠道入口重锚的预首字节截止时刻；同渠道重试退避与响应读取共享，
    /// 切换渠道时由新渠道的 request_timeout_ms 重新起算。
    deadline: tokio::time::Instant,
}

/// 按一条跳的渠道路由发起出站：直通或 IR，遇可重试错误在该跳内 failover。
#[allow(clippy::too_many_arguments)]
async fn dispatch_hop(
    deps: &Deps,
    snapshot: &Arc<RuntimeSnapshot>,
    request: &ChatRequest,
    hop: &RouteHop,
    token: &Token,
    started: i64,
    inbound_protocol: Protocol,
    raw_body: &[u8],
    request_body_for_log: Option<Bytes>,
    inbound_anthropic_version: Option<&HeaderValue>,
    inbound_headers: &HeaderMap,
    request_id: &str,
    session_identity: &str,
    active_permit: Arc<Mutex<Option<OwnedSemaphorePermit>>>,
    ever_dispatched: &AtomicBool,
) -> FailoverOutcome {
    let hop_dispatch = HopDispatch {
        deps,
        snapshot,
        request,
        routed_model: &hop.routed_model,
        token,
        started,
        raw_body,
        inbound_protocol,
        request_body: request_body_for_log,
        inbound_anthropic_version,
        inbound_headers,
        request_id,
        session_identity,
        active_permit,
        ever_dispatched,
    };
    hop_with_failover(&hop_dispatch, &hop.route).await
}

/// 一跳出站的共享上下文：出站目标、计费与日志所需的请求级信息。
///
/// 直通与 IR 两条出站路径共用；`raw_body` 是入站原始字节，仅直通路径用作
/// 出站请求体（施加目标性补丁），IR 路径从请求 IR 重编码。
struct HopDispatch<'a> {
    deps: &'a Deps,
    /// 准入时刻的快照引用（Arc 共享，流式派生任务可克隆）。
    snapshot: &'a Arc<RuntimeSnapshot>,
    request: &'a ChatRequest,
    /// 本跳出站用的已登记模型名（统一模型时为成员，否则同入站名）。
    routed_model: &'a str,
    token: &'a Token,
    started: i64,
    raw_body: &'a [u8],
    /// 入站 wire 协议：响应重编码与错误格式按此分派。
    inbound_protocol: Protocol,
    request_body: Option<Bytes>,
    /// 直通时转发下游的 `anthropic-version`；缺省则出站用官方默认。
    inbound_anthropic_version: Option<&'a HeaderValue>,
    inbound_headers: &'a HeaderMap,
    request_id: &'a str,
    /// 按调用者隔离的会话缓存标识，供出站缓存亲和使用。
    session_identity: &'a str,
    /// 活动请求 permit 的所有权容器；流式任务创建后从此处取走并持有到结算结束。
    active_permit: Arc<Mutex<Option<OwnedSemaphorePermit>>>,
    /// 请求级「曾出站」标记：物理尝试真正发送前由计费尝试的派发标记置位，
    /// 供请求终局时区分「未出站即终局」与已派发失败。
    ever_dispatched: &'a AtomicBool,
}

impl<'a> HopDispatch<'a> {
    /// 单渠道的路径判定：协议与入站一致且出站名与入站名相同才可字节直通
    /// （直通无法改写请求体模型名）；其余渠道走 IR 编码路径。
    fn channel_is_passthrough(&self, record: &ChannelRecord) -> bool {
        record.channel.protocol == self.inbound_protocol
            && routing::outbound_model(&record.channel, self.routed_model) == self.request.model
            // Responses 的 failed/incomplete 终态需要转换为网关错误语义；流式
            // 直通无法在已发送响应头后改变状态，因此统一走 IR 路径处理。
            && !(self.request.stream && record.channel.protocol == Protocol::OpenAiResponses)
    }
}

/// 一跳出站：路径选择下沉到单渠道——同协议且出站名一致的渠道字节直通，
/// 其余渠道按 IR 编码，混合协议候选互不拖累。
///
/// 两路径共享同一套 failover/日志/结算设施：可重试错误（网络错误/429/5xx）
/// 在首字节之前切换下一渠道（按接手渠道各自判定路径）；不可重试 4xx 直接
/// 返回。快路径同样不免认证与计费（已在准入阶段完成）。终局失败时由
/// [`run_failover`] 携带返回下游的响应面，请求级路径据此在「从未出站」时
/// 落一条 dispatched=0 的失败日志。
async fn hop_with_failover(ctx: &HopDispatch<'_>, route: &routing::Route) -> FailoverOutcome {
    run_failover(
        route,
        &ctx.snapshot.channels,
        ctx.routed_model,
        |record, key, channel_deadline| {
            let passthrough = ctx.channel_is_passthrough(record);
            Box::pin(async move {
                if passthrough {
                    if ctx.request.stream {
                        passthrough_stream_completion(ctx, record, key, channel_deadline).await
                    } else {
                        passthrough_non_stream_completion(ctx, record, key, channel_deadline).await
                    }
                } else {
                    let mut call_ctx = OutboundCall {
                        deps: ctx.deps,
                        snapshot: ctx.snapshot,
                        routed_model: ctx.routed_model,
                        token: ctx.token,
                        price: billed_price(ctx.snapshot, record, ctx.routed_model),
                        started: ctx.started,
                        inbound_protocol: ctx.inbound_protocol,
                        request_body: ctx.request_body.clone(),
                        inbound_headers: ctx.inbound_headers,
                        request_id: ctx.request_id,
                        session_identity: ctx.session_identity,
                        active_permit: ctx.active_permit.clone(),
                        ever_dispatched: ctx.ever_dispatched,
                        deadline: channel_deadline,
                    };
                    if ctx.request.stream {
                        stream_completion(&mut call_ctx, ctx.request, &record.channel, key, true)
                            .await
                    } else {
                        non_stream_completion(
                            &mut call_ctx,
                            ctx.request,
                            &record.channel,
                            key,
                            true,
                        )
                        .await
                    }
                }
            })
        },
        FailoverPolicy {
            inbound_protocol: ctx.inbound_protocol,
            retry_backoff: retry_backoff(ctx.snapshot),
            key_cooldowns: &ctx.deps.key_cooldowns,
            channel_cooldowns: &ctx.deps.channel_cooldowns,
        },
    )
    .await
}

/// 从请求头提取并校验令牌 key，返回匹配的令牌定义；禁用的令牌在此被拒绝。
fn authenticate<'a>(
    snapshot: &'a RuntimeSnapshot,
    headers: &HeaderMap,
) -> Result<&'a Token, AuthenticationError> {
    let key = extract_key(headers).ok_or(AuthenticationError::MissingToken)?;
    // 库内与快照只存 key 的 SHA-256 指纹；呈现的明文先换算再查找。
    let fingerprint = store::token_key_fingerprint(&key);
    let token = snapshot
        .tokens
        .get(&fingerprint)
        .ok_or(AuthenticationError::InvalidToken)?;
    if !token.enabled {
        return Err(AuthenticationError::DisabledToken);
    }
    if snapshot
        .users
        .get(&token.user_id)
        .is_none_or(|user| !user.enabled)
    {
        return Err(AuthenticationError::DisabledOwner);
    }
    Ok(token)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
enum AuthenticationError {
    #[error("缺少认证令牌：请提供 Authorization: Bearer <key> 或 x-api-key")]
    MissingToken,
    #[error("无效的认证令牌")]
    InvalidToken,
    #[error("认证令牌已被禁用")]
    DisabledToken,
    #[error("所属用户已禁用")]
    DisabledOwner,
}

/// 从入站认证头提取令牌 key：Bearer、`x-api-key`（Anthropic）与
/// `x-goog-api-key`（Gemini），按顺序取首个非空者。
fn extract_key(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get("authorization") {
        let value = value.to_str().ok()?;
        if let Some(key) = extract_bearer(value) {
            return Some(key.to_string());
        }
    }
    for name in ["x-api-key", "x-goog-api-key"] {
        if let Some(value) = headers.get(name) {
            return value.to_str().ok().map(|value| value.trim().to_string());
        }
    }
    None
}

/// 从 `Authorization` 头值取出 Bearer token；scheme 大小写不敏感（RFC 9110）。
pub(super) fn extract_bearer(value: &str) -> Option<&str> {
    let value = value.trim();
    let (scheme, rest) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        let key = rest.trim();
        if key.is_empty() { None } else { Some(key) }
    } else {
        None
    }
}

/// 判断上游 HTTP 状态码是否可重试：网络错误与 429/5xx 允许 failover。
fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

/// 统一模型 hop 是否继续下一成员：400 不换下一成员，
/// 429/5xx 换；其余非 2xx（如 401/403）也换，因为可能是该成员渠道的密钥/权限问题。
fn should_try_next_hop(status: StatusCode) -> bool {
    let code = status.as_u16();
    if is_retryable_status(code) {
        return true;
    }
    if code == 400 || (200..300).contains(&code) {
        return false;
    }
    true
}

/// 只解析 `Retry-After` 的 delta-seconds；HTTP-date 忽略。上限由设置
/// `retry_after_cap_secs` 在退避时施加。
fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    let secs: u64 = value.parse().ok()?;
    Some(Duration::from_secs(secs))
}

/// 从快照构造同渠道重试退避。
fn retry_backoff(snapshot: &RuntimeSnapshot) -> RetryBackoff {
    RetryBackoff::from_ms(
        snapshot.retry_backoff_ms,
        snapshot.retry_backoff_cap_ms,
        snapshot.retry_after_cap_secs,
    )
}

/// 流式空闲超时与非流式读体超时：渠道 `timeout_ms`，至少 1ms。
///
/// 下限保持 1 而非管理面校验的 1000ms：校验只拦新增写入，存量库中可能仍有
/// 更小的旧值，运行时行为不因校验收紧而回溯改变。
fn channel_idle(timeout_ms: u64) -> Duration {
    Duration::from_millis(timeout_ms.clamp(1, crate::store::resources::MAX_CHANNEL_TIMEOUT_MS))
}

/// 把局部空闲/渠道超时夹进请求剩余总预算。截止时刻已过则返回零，让外层
/// `timeout` 在下一次 poll 立即结束；切换渠道、密钥或进入流任务都不会重置预算。
fn remaining_timeout(deadline: tokio::time::Instant, local: Duration) -> Duration {
    deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .min(local)
}

/// 日志用的出站模型名：按该渠道自己的别名表改写。
fn outbound_model_for_log<'a>(channel: &'a Channel, inbound: &'a str) -> Option<&'a str> {
    Some(routing::outbound_model(channel, inbound))
}

/// Anthropic 出站在下游未带版本头时使用的官方默认。
const DEFAULT_ANTHROPIC_VERSION: HeaderValue = HeaderValue::from_static("2023-06-01");

/// 入站功能头白名单：直通与 IR 都原样转发出站，不含认证与 hop-by-hop。
const FORWARDED_FEATURE_HEADERS: &[&str] = &["anthropic-beta"];

/// 按渠道协议设置出站认证头。
///
/// OpenAI 用 `Authorization: Bearer`；Anthropic 用 `x-api-key` 并带
/// `anthropic-version`。直通路径转发下游版本头，避免强行降级；IR / 探测仍钉默认。
pub(super) trait OutboundAuth {
    fn apply_outbound_auth(self, protocol: Protocol, key: &StoredChannelKey) -> Self;
    fn apply_outbound_auth_with_version(
        self,
        protocol: Protocol,
        key: &StoredChannelKey,
        inbound_version: Option<&HeaderValue>,
    ) -> Self;
    fn apply_feature_headers(self, inbound: &HeaderMap) -> Self;
}

impl OutboundAuth for reqwest::RequestBuilder {
    fn apply_outbound_auth(self, protocol: Protocol, key: &StoredChannelKey) -> Self {
        self.apply_outbound_auth_with_version(protocol, key, None)
    }

    fn apply_outbound_auth_with_version(
        self,
        protocol: Protocol,
        key: &StoredChannelKey,
        inbound_version: Option<&HeaderValue>,
    ) -> Self {
        match protocol {
            Protocol::OpenAiChat | Protocol::OpenAiResponses => {
                self.bearer_auth(key.expose_api_key())
            }
            Protocol::AnthropicMessages => self.header("x-api-key", key.expose_api_key()).header(
                "anthropic-version",
                inbound_version
                    .cloned()
                    .unwrap_or(DEFAULT_ANTHROPIC_VERSION),
            ),
            Protocol::Gemini => self.header("x-goog-api-key", key.expose_api_key()),
        }
    }

    fn apply_feature_headers(self, inbound: &HeaderMap) -> Self {
        let mut builder = self;
        for name in FORWARDED_FEATURE_HEADERS {
            if let Some(value) = inbound.get(*name) {
                builder = builder.header(*name, value.clone());
            }
        }
        builder
    }
}

/// 整流闸门：开关开启且上游 400 命中可修正模式时，做最小修正并落审计日志，
/// 返回修正后的请求 IR；否则返回 `None`（原样失败）。
async fn rectify_for_retry(
    deps: &Deps,
    snapshot: &RuntimeSnapshot,
    request: &ChatRequest,
    channel: &Channel,
    error_message: &str,
) -> Option<ChatRequest> {
    if !snapshot.request_rectify {
        return None;
    }
    let rectification = rectifier::rectify(error_message, request)?;
    // 动作明细落 system_log，自动修正行为可回溯；落库失败只记 tracing。
    store::record_system_warn(
        &deps.pool,
        "rectifier",
        &store::SystemLogEvent::new(
            "rectifier.request_rectified",
            serde_json::json!({
                "channel": channel.name,
                "protocol": protocol_to_wire(channel.protocol),
                "rule": rectification.rule.as_str(),
                "error": error_message,
                "actions": rectification.actions,
            }),
            format!(
                "渠道 {} 的上游 400 已整流重试：{}",
                channel.name,
                rectification.actions.join("；")
            ),
        ),
    )
    .await;
    Some(rectification.request)
}

/// 从上游错误 body 提取可读消息（OpenAI/Anthropic 均为 `error.message`），
/// 避免把整个 JSON 串塞进下游 message。
pub(super) fn upstream_error_message(parsed: &Value, status: u16) -> String {
    parsed
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("上游返回状态码 {status}"))
}

/// 落一条「未出站即终局」的失败日志：零费用、无渠道与计费身份、dispatched=0。
///
/// 准入前置失败与出站阶段的终局失败（全部渠道冷却 / 无可用密钥、本地计费
/// 拒绝、出站安全策略拒绝等从未建立上游连接的结局）共用本写法，让这类请求
/// 在 request_log 与统计中不再隐形。状态码与响应体按实际（或即将）返回下游
/// 的内容记录，归属用户与令牌照常定格，统计侧由 `not_dispatched` 单列呈现。
#[allow(clippy::too_many_arguments)]
async fn queue_undispatched_failure_log(
    deps: &Deps,
    token: &Token,
    model: &str,
    status: u16,
    started: i64,
    inbound_protocol: Protocol,
    discount_bp: i64,
    request_body: Option<Bytes>,
    response_wire: Option<Vec<u8>>,
    request_id: &str,
) {
    let billing = Billing {
        discount_bp,
        request_body,
        response_body: response_wire,
        ..Billing::default()
    };
    if let Err(err) = queue_request_log(
        deps,
        RequestLogDraft {
            token,
            model,
            outbound_model: None,
            channel: "",
            channel_key: None,
            status,
            started,
            billing,
            inbound_protocol,
            request_id,
            billing_attempt_id: None,
            upstream_reached: false,
            dispatched: false,
            deadline: None,
        },
    )
    .await
    {
        tracing::error!(error = %err, request_id, "未出站即终局的失败日志无法持久化");
    }
}

/// 构造入站协议错误格式的响应，并落一条「未出站即终局」的请求日志
///（无计费数据）。
///
/// full_body 开启时错误日志同样带全 body：入站请求字节与实际返回下游的错误
/// JSON 字节，便于排障时重放失败请求。
#[allow(clippy::too_many_arguments)]
async fn error_response(
    status: StatusCode,
    message: &str,
    deps: &Deps,
    full_body: bool,
    token: Option<&Token>,
    model: Option<&str>,
    started: i64,
    inbound_protocol: Protocol,
    request_body: Option<Bytes>,
    request_id: &str,
) -> Response {
    let body = protocol::encode_error(status.as_u16(), message, inbound_protocol);
    if let (Some(token), Some(model)) = (token, model) {
        let response_wire = full_body.then(|| serde_json::to_vec(&body).unwrap_or_default());
        let discount_bp = deps.snapshot.read().await.discount_bp_for_token(token);
        queue_undispatched_failure_log(
            deps,
            token,
            model,
            status.as_u16(),
            started,
            inbound_protocol,
            discount_bp,
            request_body,
            response_wire,
            request_id,
        )
        .await;
    }
    (status, Json(body)).into_response()
}

/// 令牌生效 RPM：令牌桶上限 = 令牌 `rate_limit_rpm`（缺省跟随全局兜底，`0` 不限）；
/// 用户桶按「用户显式值 → 套餐默认值 → 系统兜底」取值，跨该用户全部令牌共享；
/// 套餐桶上限 = 套餐 `shared_rpm` 的正数值，跨该档全部用户共享。用户/套餐字段的
/// `0` 是显式不限该维度，不会继续回退。root 不挂套餐，只受令牌桶的系统兜底约束。
/// 快照原子替换后，本函数每次调用重读三维上限，自动生效。
fn token_rate_limited(
    deps: &Deps,
    token: &Token,
    snapshot: &RuntimeSnapshot,
) -> Result<(), Duration> {
    let token_limit = token.rate_limit_rpm.unwrap_or(snapshot.rate_limit_rpm);
    let (user_limit, plan_limit) = match snapshot.users.get(&token.user_id) {
        Some(user) => match user.plan {
            PlanBinding::Unrestricted => (None, None),
            PlanBinding::Plan(plan_id) => {
                let plan = snapshot.plans.get(&plan_id);
                let user_limit = match user.rate_limit_rpm {
                    // 显式 0 与令牌 RPM 一样表示该维度不限速；它是已填写的值，
                    // 因而不会因换档而改成套餐默认值。
                    Some(limit) => limit,
                    None => match plan.and_then(|plan| plan.default_rpm) {
                        Some(limit) => limit,
                        None => snapshot.rate_limit_rpm,
                    },
                };
                let user_limit = if user_limit > 0 {
                    Some((token.user_id, user_limit))
                } else {
                    None
                };
                let plan_limit = match plan.and_then(|plan| plan.shared_rpm) {
                    Some(limit) if limit > 0 => Some((plan_id, limit)),
                    _ => None,
                };
                (user_limit, plan_limit)
            }
        },
        None => (None, None),
    };
    deps.request_rate
        .try_acquire(&token.token_key, token_limit, user_limit, plan_limit)
}

/// 令牌 RPM 超限：429 + `Retry-After`。
#[allow(clippy::too_many_arguments)]
async fn too_many_token_requests(
    deps: &Deps,
    full_body: bool,
    token: Option<&Token>,
    model: Option<&str>,
    started: i64,
    inbound_protocol: Protocol,
    request_body: Option<Bytes>,
    retry_after: Duration,
    request_id: &str,
) -> Response {
    let mut response = error_response(
        StatusCode::TOO_MANY_REQUESTS,
        "令牌请求过于频繁",
        deps,
        full_body,
        token,
        model,
        started,
        inbound_protocol,
        request_body,
        request_id,
    )
    .await;
    let secs = retry_after.as_secs().max(1);
    if let Ok(value) = HeaderValue::from_str(&secs.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::extract_bearer;
    use crate::core::billing::PriceSnapshot;
    use serde_json::json;

    #[test]
    fn reservation_estimate_accounts_for_tools_and_provider_options() {
        let price = PriceSnapshot {
            input_micros: 1_000_000,
            output_micros: 1_000_000,
            cache_read_micros: 4_000_000,
            cache_write_micros: 2_000_000,
            cache_write_1h_micros: 3_000_000,
        };
        let plain: crate::core::ir::ChatRequest = serde_json::from_value(json!({
            "model": "m",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hello"}]}]
        }))
        .expect("基础请求应可解码");
        let with_tool: crate::core::ir::ChatRequest = serde_json::from_value(json!({
            "model": "m",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hello"}]}],
            "tools": [{
                "name": "lookup",
                "description": "look up a record",
                "parameters": {
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"]
                }
            }],
            "provider_options": {"provider": {"large_option": "retained"}}
        }))
        .expect("含工具的请求应可解码");
        let plain_cost =
            super::estimate_attempt_cost_micros(price, 10_000, &plain).expect("基础预留应可计算");
        let tool_cost = super::estimate_attempt_cost_micros(price, 10_000, &with_tool)
            .expect("工具预留应可计算");
        assert!(
            tool_cost > plain_cost,
            "工具 schema 与 provider options 应提高预留"
        );
    }

    #[test]
    fn split_model_method_parses_official_actions() {
        assert_eq!(
            super::split_model_method("gemini-2.5-pro:generateContent"),
            Some(("gemini-2.5-pro".to_string(), false))
        );
        assert_eq!(
            super::split_model_method("models/gemini-2.5-flash:streamGenerateContent"),
            Some(("gemini-2.5-flash".to_string(), true))
        );
        assert_eq!(super::split_model_method(":generateContent"), None);
        assert_eq!(super::split_model_method("gemini:bogus"), None);
        assert_eq!(super::split_model_method("gemini"), None);
    }

    #[test]
    fn extract_bearer_is_case_insensitive() {
        assert_eq!(extract_bearer("Bearer sk-abc"), Some("sk-abc"));
        assert_eq!(extract_bearer("bearer sk-abc"), Some("sk-abc"));
        assert_eq!(extract_bearer("BEARER sk-abc"), Some("sk-abc"));
        assert_eq!(extract_bearer("Bearer  sk-abc  "), Some("sk-abc"));
        assert_eq!(extract_bearer("Basic sk-abc"), None);
        assert_eq!(extract_bearer("Bearer"), None);
        assert_eq!(extract_bearer("Bearer "), None);
    }
}
