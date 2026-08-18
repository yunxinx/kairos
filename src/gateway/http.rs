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

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, Sse},
    },
    routing::{get, post},
};
use bytes::{Bytes, BytesMut};
use futures_util::Stream;
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::{
    config::Protocol,
    core::billing,
    core::billing::PriceSnapshot,
    core::ir::{ChatRequest, ChatResponse, StreamEvent, Usage},
    core::stream::{SseFrame, StreamAccumulator},
    runtime::{RuntimeSnapshot, SnapshotHandle},
    store,
    store::resources::{Channel, ChannelRecord, Token},
};

use super::failover::{Outbound, RetryBackoff, run_failover};
use super::logging::{Billing, log_request, new_request_id, unix_millis};
use super::rate_limit::{RequestRateLimiter, effective_rate_limit_rpm};
use super::sse::{
    OpenAiDoneFilter, data_frame_to_wire, event_from_frame, frame_to_wire, receiver_stream,
    take_frame,
};
use super::throttle::AuthThrottle;

use super::{protocol, routing};

/// 网关依赖：存储连接池 + 出站 HTTP 客户端 + 运行时资源快照句柄。
#[derive(Clone)]
pub struct Deps {
    pub(super) pool: SqlitePool,
    pub(super) client: reqwest::Client,
    pub(super) snapshot: SnapshotHandle,
    pub(super) auth_throttle: AuthThrottle,
    pub(super) request_rate: RequestRateLimiter,
}

/// 组装网关路由。`snapshot` 为已加载的运行时资源快照句柄，请求路径从其中读取
/// 当前资源；管理 API 写库后可原子替换该快照使新资源即时生效。
pub async fn router(pool: SqlitePool, snapshot: SnapshotHandle) -> Router {
    // 不设客户端级 timeout：reqwest 的 timeout 覆盖到响应体读完，会截断长流式
    // 响应；超时统一按渠道在请求级施加（非流式 `.timeout`，流式仅约束到响应头）。
    let client = reqwest::Client::builder()
        .build()
        .expect("reqwest client 构建不应失败");

    let deps = Deps {
        pool,
        client,
        snapshot,
        auth_throttle: AuthThrottle::new(),
        request_rate: RequestRateLimiter::new(),
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
    handle_request(deps, Protocol::OpenAiChat, addr.ip(), request).await
}

/// Anthropic Messages 入站端点。
async fn messages(
    State(deps): State<Deps>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    handle_request(deps, Protocol::AnthropicMessages, addr.ip(), request).await
}

/// OpenAI Responses 入站端点。
async fn responses(
    State(deps): State<Deps>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    handle_request(deps, Protocol::OpenAiResponses, addr.ip(), request).await
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
    let snapshot = deps.snapshot.read().await.clone();
    let inbound_protocol = list_models_protocol(&headers);
    let started = unix_millis();
    let request_id = new_request_id();
    if deps.auth_throttle.is_blocked(
        addr.ip(),
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
                addr.ip(),
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
    let ids = store::resources::visible_model_ids(
        &snapshot.model_groups,
        &snapshot.unified_models,
        snapshot.channels.iter().map(|record| &record.channel),
        &token.model_group,
    );
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

/// 上游响应超过 `max_response_bytes`：可换渠道重试，不把超大体读进内存。
fn oversized_upstream_response(channel: &str, max_bytes: u64) -> Outbound {
    Outbound::Retryable {
        channel: channel.to_string(),
        status: None,
        retry_after: None,
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
    let request = match protocol::decode_request(&parsed, inbound_protocol) {
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

    // 3. 模型组允许名单：组外名字按「不存在」拒绝，不提分组、不用 503。
    if !store::resources::group_allows(&snapshot.model_groups, &token.model_group, &request.model) {
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
    let hops = match resolve_route_hops(&snapshot, &request.model) {
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

    // 5. 计费准入：令牌余额与累计上限须通过（单价按实际跳在出站时选用）。
    let mut conn = match deps.pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => {
            return db_error_response(
                &deps,
                full_body,
                token,
                &request.model,
                started,
                err,
                inbound_protocol,
                request_body_for_log,
                &request_id,
            )
            .await;
        }
    };
    // 余额独立存 token_balance 表：建行发生在创建令牌时，准入只读、不写。
    // 行缺失视为 0（与「首次出现按 0」一致），由后续 402 挡住，不在热路径 INSERT。
    let balance = match store::get_token_balance(&mut conn, &token.token_key).await {
        Ok(Some(balance)) => balance,
        Ok(None) => store::TokenBalance {
            balance_usd_micros: 0,
            settled_usd_micros: 0,
        },
        Err(err) => {
            return db_error_response(
                &deps,
                full_body,
                token,
                &request.model,
                started,
                err,
                inbound_protocol,
                request_body_for_log,
                &request_id,
            )
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
    if let Some(limit) = token.limit_usd_micros
        && balance.settled_usd_micros >= limit
    {
        let message = format!(
            "令牌 {} 累计结算已超上限（limit_usd_micros = {limit}）",
            token.name
        );
        return error_response(
            StatusCode::PAYMENT_REQUIRED,
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
    if let Some(max_tokens) = request.max_tokens.filter(|&n| n > 0) {
        let estimate = estimate_admission_cost_micros(&hops, &snapshot, max_tokens);
        if estimate > balance.balance_usd_micros {
            let message = format!(
                "令牌 {} 预估费用超过余额（预估 {:.2} USD，当前 {:.2} USD）",
                token.name,
                estimate as f64 / 1_000_000.0,
                balance.balance_usd_micros as f64 / 1_000_000.0
            );
            return error_response(
                StatusCode::PAYMENT_REQUIRED,
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
        if let Some(limit) = token.limit_usd_micros
            && balance.settled_usd_micros.saturating_add(estimate) > limit
        {
            let message = format!(
                "令牌 {} 预估费用将超过累计结算上限（limit_usd_micros = {limit}）",
                token.name
            );
            return error_response(
                StatusCode::PAYMENT_REQUIRED,
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
    }
    // 准入连接只服务余额读取；last_used 在落日志提交后尽力刷新。
    drop(conn);

    let inbound_anthropic_version = headers.get("anthropic-version");

    // 6. 出站：按跳顺序一次一条；该跳内再走渠道路由。
    //
    // 同渠道 failover 由 `run_failover` 处理（429/5xx 可重试，其它 4xx 为 Fatal
    // 立即返回）。统一模型 hop 之间对齐 one-api `shouldRetry`：400 视为请求本身
    // 有问题，不再打后续成员（bifrost 对 400/404/422 也不做 key 轮换）；429/5xx
    // 及其余非 2xx 继续下一成员。hop 间不等待——成员钉在不同渠道，换成员不是
    // 同渠道退避。
    let mut last_failure: Option<Response> = None;
    for hop in &hops {
        let response = dispatch_hop(
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
        )
        .await;
        if response.status().is_success() {
            return response;
        }
        if !should_try_next_hop(response.status()) {
            return response;
        }
        last_failure = Some(response);
    }
    last_failure.expect("准入已保证至少一条可路由跳")
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
) -> Result<RouteHop, HopDeny> {
    let Some(record) = snapshot
        .channels
        .iter()
        .find(|record| record.id == member.channel_id)
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
    Ok(RouteHop {
        routed_model: member.model.clone(),
        route: routing::Route {
            channels: vec![record.clone()],
        },
    })
}

/// 为可调用名构造一条出站跳：须有启用且已定价的渠道。
fn hop_for_callable(snapshot: &RuntimeSnapshot, model: &str) -> Result<RouteHop, HopDeny> {
    let mut route = routing::route(&snapshot.channels, model).ok_or(HopDeny::NoRoute)?;
    route
        .channels
        .retain(|record| snapshot.price_for_channel(record.id, model).is_some());
    if route.channels.is_empty() {
        return Err(HopDeny::NoPrice);
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

/// 候选跳里最高的 output 单价 × `max_tokens`，挡住极端输出上限。
fn estimate_admission_cost_micros(
    hops: &[RouteHop],
    snapshot: &RuntimeSnapshot,
    max_tokens: u32,
) -> i64 {
    let mut max_output = 0i64;
    for hop in hops {
        for record in &hop.route.channels {
            let price = billed_price(snapshot, record, &hop.routed_model);
            max_output = max_output.max(price.output_micros);
        }
    }
    billing::estimate_max_output_cost_micros(max_tokens, max_output)
}

/// 解析本次请求的出站跳序列。
///
/// 命中统一模型时按成员顺序收集已定价可路由的跳；一条都没有则 503，文案说明
/// 各成员失效原因（不是「模型不存在」）。普通模型保持原 503 文案。
fn resolve_route_hops(
    snapshot: &RuntimeSnapshot,
    model: &str,
) -> Result<Vec<RouteHop>, (StatusCode, String)> {
    if let Some(unified) = snapshot.unified_models.get(model) {
        let mut hops = Vec::new();
        let mut reasons = Vec::new();
        for member in &unified.models {
            match hop_for_member(snapshot, member) {
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
    match hop_for_callable(snapshot, model) {
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

/// 单次出站调用的请求侧上下文：入站请求、认证令牌与计费/日志所需的
/// 请求级信息。作为 `*_completion` 的参数打包，避免过长参数列表。
struct CallCtx<'a> {
    deps: &'a Deps,
    /// 准入时刻的快照引用（Arc 共享，流式派生任务可克隆）。
    snapshot: &'a Arc<RuntimeSnapshot>,
    request: &'a ChatRequest,
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
) -> Response {
    // 直通需全部候选渠道同协议：跨协议 failover 会向异协议渠道发原生字节，故此时回落 IR。
    // 任一渠道命中别名、或统一模型成员名与入站名不同时也回落 IR：直通无法改写请求体模型名。
    let passthrough = hop.route.channels.iter().all(|record| {
        record.channel.protocol == inbound_protocol
            && routing::outbound_model(&record.channel, &hop.routed_model) == request.model
    });
    if passthrough {
        let passthrough_ctx = PassthroughCtx {
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
        };
        return passthrough_with_failover(&passthrough_ctx, &hop.route).await;
    }
    outbound_with_failover(
        deps,
        snapshot,
        request,
        &hop.routed_model,
        &hop.route,
        token,
        started,
        inbound_protocol,
        request_body_for_log,
        inbound_headers,
        request_id,
    )
    .await
}

/// 按渠道路由顺序发起出站调用，遇可重试错误自动 failover。
///
/// 每个候选渠道按其自身 `max_retries` 尝试（首试 + max_retries 次重试）；
/// 渠道耗尽或请求须整体失败时切换到下一候选。剩余候选全失败或遇到不可
/// 重试 4xx 时返回最终错误响应。成功时返回下游响应。
#[allow(clippy::too_many_arguments)]
async fn outbound_with_failover(
    deps: &Deps,
    snapshot: &Arc<RuntimeSnapshot>,
    request: &ChatRequest,
    routed_model: &str,
    route: &routing::Route,
    token: &Token,
    started: i64,
    inbound_protocol: Protocol,
    request_body_for_log: Option<Bytes>,
    inbound_headers: &HeaderMap,
    request_id: &str,
) -> Response {
    run_failover(
        route,
        |record| {
            let record = record.clone();
            let request_body_for_log = request_body_for_log.clone();
            Box::pin(async move {
                let mut ctx = CallCtx {
                    deps,
                    snapshot,
                    request,
                    routed_model,
                    token,
                    price: billed_price(snapshot, &record, routed_model),
                    started,
                    inbound_protocol,
                    request_body: request_body_for_log.clone(),
                    inbound_headers,
                    request_id,
                };
                if request.stream {
                    stream_completion(&mut ctx, &record.channel).await
                } else {
                    non_stream_completion(&mut ctx, &record.channel).await
                }
            })
        },
        |channel, status, _failover, body_wire| {
            let outbound_model =
                outbound_model_for_channel_name(route, channel, routed_model).map(str::to_string);
            let channel = channel.to_string();
            let request_body = request_body_for_log.clone();
            let response_body = snapshot.full_body.then(|| body_wire.to_vec());
            let request_id = request_id.to_string();
            Box::pin(async move {
                log_request(
                    deps,
                    token,
                    &request.model,
                    outbound_model.as_deref(),
                    &channel,
                    status,
                    started,
                    Billing {
                        request_body,
                        response_body,
                        ..Default::default()
                    },
                    inbound_protocol,
                    &request_id,
                )
                .await;
            })
        },
        inbound_protocol,
        retry_backoff(snapshot),
    )
    .await
}

/// 直通快路径的请求侧共享上下文：出站目标、计费与日志所需的请求级信息。
///
/// 打包 `passthrough_*_completion` 的公共参数，避免过长参数列表。`raw_body`
/// 用于出站请求的目标性补丁。
struct PassthroughCtx<'a> {
    deps: &'a Deps,
    /// 准入时刻的快照引用（Arc 共享，流式派生任务可克隆）。
    snapshot: &'a Arc<RuntimeSnapshot>,
    request: &'a ChatRequest,
    /// 本跳出站用的已登记模型名（统一模型时为成员）。
    routed_model: &'a str,
    token: &'a Token,
    started: i64,
    raw_body: &'a [u8],
    /// 入站 wire 协议（直通时与出站同协议）。
    inbound_protocol: Protocol,
    request_body: Option<Bytes>,
    /// 直通时转发下游的 `anthropic-version`；缺省则出站用官方默认。
    inbound_anthropic_version: Option<&'a HeaderValue>,
    inbound_headers: &'a HeaderMap,
    request_id: &'a str,
}

/// 直通快路径：按渠道路由顺序发起同协议出站调用，遇可重试错误自动 failover。
///
/// 与 IR 路径的 failover 语义一致（共用 [`run_failover`]）：可重试错误（网络
/// 错误/429/5xx）在首字节之前切换下一渠道重试；不可重试 4xx 直接返回。快路径
/// 同样不免认证与计费（已在准入阶段完成）。
async fn passthrough_with_failover(ctx: &PassthroughCtx<'_>, route: &routing::Route) -> Response {
    run_failover(
        route,
        |record| {
            let record = record.clone();
            Box::pin(async move {
                if ctx.request.stream {
                    passthrough_stream_completion(ctx, &record).await
                } else {
                    passthrough_non_stream_completion(ctx, &record).await
                }
            })
        },
        |channel, status, _failover, body_wire| {
            let outbound_model = outbound_model_for_channel_name(route, channel, ctx.routed_model)
                .map(str::to_string);
            let channel = channel.to_string();
            let request_body = ctx.request_body.clone();
            let response_body = ctx.snapshot.full_body.then(|| body_wire.to_vec());
            let request_id = ctx.request_id.to_string();
            Box::pin(async move {
                log_request(
                    ctx.deps,
                    ctx.token,
                    &ctx.request.model,
                    outbound_model.as_deref(),
                    &channel,
                    status,
                    ctx.started,
                    Billing {
                        request_body,
                        response_body,
                        ..Default::default()
                    },
                    ctx.inbound_protocol,
                    &request_id,
                )
                .await;
            })
        },
        ctx.inbound_protocol,
        retry_backoff(ctx.snapshot),
    )
    .await
}

/// 直通快路径的流式出站：原始字节块直搬，旁路逐 SSE 帧嗅探 usage 计费。
///
/// 请求体仅做目标性补丁（OpenAI 流式注入 `stream_options.include_usage` 供计费，
/// Anthropic 无需补丁），响应以字节流直通到下游，不做完整解码。流结束后按嗅探
/// 累积的 usage 结算并落日志。渠道 timeout 只约束到响应头。
async fn passthrough_stream_completion(
    ctx: &PassthroughCtx<'_>,
    record: &ChannelRecord,
) -> Outbound {
    let channel = &record.channel;
    let outbound = passthrough_patch_request(ctx.raw_body, true, channel.protocol);
    let upstream_url = passthrough_upstream_url(channel);

    let upstream = tokio::time::timeout(
        Duration::from_millis(channel.timeout_ms),
        ctx.deps
            .client
            .post(&upstream_url)
            .apply_outbound_auth_with_version(channel, ctx.inbound_anthropic_version)
            .apply_feature_headers(ctx.inbound_headers)
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
                retry_after: None,
                message: "直通流式上游不可达".to_string(),
            };
        }
        Err(_) => {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: None,
                retry_after: None,
                message: "直通流式上游响应超时".to_string(),
            };
        }
    };

    let status_code = resp.status().as_u16();
    if !resp.status().is_success() {
        let retry_after = parse_retry_after(resp.headers());
        let upstream_body = match take_upstream_body(
            resp,
            &channel.name,
            ctx.snapshot.max_response_bytes,
            "直通流式上游读体失败",
        )
        .await
        {
            Ok(body) => body,
            Err(outbound) => return outbound,
        };
        let parsed = serde_json::from_slice::<Value>(&upstream_body).unwrap_or(Value::Null);
        if is_retryable_status(status_code) {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: Some(status_code),
                retry_after,
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
        snapshot: ctx.snapshot.clone(),
        token: ctx.token.clone(),
        request: ctx.request.clone(),
        routed_model: ctx.routed_model.to_string(),
        channel: channel.clone(),
        status_code,
        started: ctx.started,
        price: billed_price(ctx.snapshot, record, ctx.routed_model),
        protocol: channel.protocol,
        request_body: ctx.request_body.clone(),
        response_body: Vec::new(),
        request_id: ctx.request_id.to_string(),
    };
    let byte_stream = resp.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);
    tokio::spawn(async move {
        pipe_passthrough_stream(byte_stream, tx, task).await;
    });

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
/// 请求体仅做目标性补丁（OpenAI 流式注入 stream_options；Anthropic 无需），
/// 响应体原样返回，不做完整解码。
async fn passthrough_non_stream_completion(
    ctx: &PassthroughCtx<'_>,
    record: &ChannelRecord,
) -> Outbound {
    let channel = &record.channel;
    let outbound = passthrough_patch_request(ctx.raw_body, false, channel.protocol);
    let upstream_url = passthrough_upstream_url(channel);

    let upstream = tokio::time::timeout(
        Duration::from_millis(channel.timeout_ms),
        ctx.deps
            .client
            .post(&upstream_url)
            .apply_outbound_auth_with_version(channel, ctx.inbound_anthropic_version)
            .apply_feature_headers(ctx.inbound_headers)
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
                retry_after: None,
                message: "直通非流式上游不可达".to_string(),
            };
        }
        Err(_) => {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: None,
                retry_after: None,
                message: "直通非流式上游响应超时".to_string(),
            };
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
        idle,
        take_upstream_body(resp, &channel.name, max_bytes, "直通非流式上游读体失败"),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(outbound)) => return outbound,
        Err(_) => {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: None,
                retry_after: None,
                message: "直通非流式上游读体超时".to_string(),
            };
        }
    };
    let parsed = serde_json::from_slice::<Value>(&upstream_body).unwrap_or(Value::Null);

    if is_success {
        // 响应体原样透传（字节级一致），从 JSON 嗅探 usage 计费。
        let usage = protocol::sniff_usage(&parsed, channel.protocol).unwrap_or_default();
        let price = billed_price(ctx.snapshot, record, ctx.routed_model);
        let cost = billing::cost_micros(&usage, &price);
        log_request(
            ctx.deps,
            ctx.token,
            &ctx.request.model,
            outbound_model_for_log(channel, ctx.routed_model),
            &channel.name,
            status_code,
            ctx.started,
            Billing {
                usage,
                price,
                cost_usd_micros: cost,
                request_body: ctx.request_body.clone(),
                response_body: ctx.snapshot.full_body.then(|| upstream_body.to_vec()),
            },
            ctx.inbound_protocol,
            ctx.request_id,
        )
        .await;
        let mut response = Response::new(Body::from(upstream_body));
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
        Outbound::Success(response)
    } else if is_retryable_status(status_code) {
        Outbound::Retryable {
            channel: channel.name.clone(),
            status: Some(status_code),
            retry_after,
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
    snapshot: Arc<RuntimeSnapshot>,
    token: Token,
    request: ChatRequest,
    routed_model: String,
    channel: Channel,
    status_code: u16,
    started: i64,
    price: PriceSnapshot,
    /// 直通协议（与入站同协议），用于 usage 嗅探与终止哨兵。
    protocol: Protocol,
    request_body: Option<Bytes>,
    response_body: Vec<u8>,
    request_id: String,
}

/// 把上游 SSE 原始字节块直搬到下游，并在旁路缓冲中逐帧嗅探 usage。
///
/// 转发不等待分帧：每个成功读取的块直接送入响应体。旁路缓冲仅负责提取完整 SSE
/// 数据事件，并按协议把 usage 逐分量取 max（Anthropic 的 usage 分散在
/// message_start/message_delta）；它不决定普通块的转发。上游流结束时按累积 usage
/// 结算并落日志；OpenAI 追加 `[DONE]`，Anthropic 以上游 message_stop 收尾。
async fn pipe_passthrough_stream<S>(
    byte_stream: S,
    tx: tokio::sync::mpsc::Sender<bytes::Bytes>,
    mut ctx: PassthroughStreamTask,
) where
    S: Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    use futures_util::StreamExt as _;

    let mut usage = Usage::default();
    let mut sse_buffer: Vec<u8> = Vec::new();
    let mut downstream_open = true;
    let mut truncated = false;
    let mut done_filter = OpenAiDoneFilter::default();
    let idle = channel_idle(ctx.channel.timeout_ms);
    let mut byte_stream = Box::pin(byte_stream);
    let log_body_max = ctx.snapshot.log_body_max();

    loop {
        let chunk = match tokio::time::timeout(idle, byte_stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
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
                )
                .await
                {
                    // 下游断开：停止发送，但继续消费上游直至结算。
                    downstream_open = false;
                    break;
                }
            }
        }

        while let Some((_event_name, frame)) = take_frame(&mut sse_buffer) {
            if frame.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_slice::<Value>(&frame)
                && let Some(sniffed) = protocol::sniff_usage(&value, ctx.protocol)
            {
                usage.union_max(sniffed);
            }
        }
        if sse_buffer.len() > ctx.snapshot.sse_reassembly_max() {
            truncated = true;
            break;
        }
    }

    if downstream_open && ctx.protocol == Protocol::OpenAiChat {
        for trailing in done_filter.finish() {
            if !send_passthrough_chunk(
                &tx,
                trailing,
                ctx.snapshot.full_body,
                log_body_max,
                &mut ctx.response_body,
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
    let cost = billing::cost_micros(&usage, &ctx.price);
    log_request(
        &ctx.deps,
        &ctx.token,
        &ctx.request.model,
        outbound_model_for_log(&ctx.channel, &ctx.routed_model),
        &ctx.channel.name,
        ctx.status_code,
        ctx.started,
        Billing {
            usage,
            price: ctx.price,
            cost_usd_micros: cost,
            request_body: ctx.request_body.clone(),
            response_body: ctx.snapshot.full_body.then(|| ctx.response_body.clone()),
        },
        ctx.protocol,
        &ctx.request_id,
    )
    .await;
    // OpenAI 协议约定以 `data: [DONE]` 终止；Anthropic 以上游
    // message_stop 收尾，无需哨兵。
    if downstream_open && ctx.protocol == Protocol::OpenAiChat {
        let _ = tx
            .send(bytes::Bytes::from_static(b"data: [DONE]\n\n"))
            .await;
    }
}

/// 下发一个直通块；full_body 仅保留已被响应通道接受的字节，并按日志上限封顶。
async fn send_passthrough_chunk(
    tx: &tokio::sync::mpsc::Sender<bytes::Bytes>,
    chunk: bytes::Bytes,
    full_body: bool,
    max_bytes: usize,
    response_body: &mut Vec<u8>,
) -> bool {
    let response_body_len = response_body.len();
    if full_body {
        append_logged_body(response_body, &chunk, max_bytes);
    }
    if tx.send(chunk).await.is_ok() {
        true
    } else {
        response_body.truncate(response_body_len);
        false
    }
}

/// 非流式出站调用单个渠道，返回可重试判定。
///
/// 按渠道协议编码出站请求、调用上游、解码响应为 IR，再重编码为入站协议返回。
/// 成功且 usage 非零才结算；失败或零输出不扣费。
async fn non_stream_completion(ctx: &mut CallCtx<'_>, channel: &Channel) -> Outbound {
    let deps = ctx.deps;
    let snapshot = ctx.snapshot;
    let request = ctx.request;
    let token = ctx.token;
    let price = ctx.price;
    let started = ctx.started;
    let inbound_protocol = ctx.inbound_protocol;
    // 别名重写：请求模型用该渠道自己的出站名。
    let mut request_warnings = Vec::new();
    let mut outbound_value =
        protocol::encode_request(request, channel.protocol, &mut request_warnings);
    let outbound_model = routing::outbound_model(channel, ctx.routed_model);
    if let Value::Object(map) = &mut outbound_value {
        map.insert("model".into(), Value::String(outbound_model.to_string()));
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
        .apply_feature_headers(ctx.inbound_headers)
        .json(&outbound_value)
        .send()
        .await;

    let resp = match upstream {
        Ok(resp) => resp,
        Err(_) => {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: None,
                retry_after: None,
                message: "上游不可达".to_string(),
            };
        }
    };

    let status = resp.status();
    let status_code = status.as_u16();
    let retry_after = parse_retry_after(resp.headers());
    let upstream_body = match take_upstream_body(
        resp,
        &channel.name,
        snapshot.max_response_bytes,
        "上游读体失败",
    )
    .await
    {
        Ok(body) => body,
        Err(outbound) => return outbound,
    };
    let parsed = serde_json::from_slice::<Value>(&upstream_body).unwrap_or(Value::Null);

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
                let cost = billing::cost_micros(usage, &price);
                let inbound = protocol::encode_response(&ir, inbound_protocol);
                // full_body 记录实际返回下游的入站响应字节（重编码结果）；
                // 跨协议时它与上游响应体不同，不能拿上游字节顶替。
                let inbound_wire = snapshot
                    .full_body
                    .then(|| serde_json::to_vec(&inbound).unwrap_or_default());
                log_request(
                    deps,
                    token,
                    &request.model,
                    Some(outbound_model),
                    &channel.name,
                    status_code,
                    started,
                    Billing {
                        usage: usage.clone(),
                        price,
                        cost_usd_micros: cost,
                        request_body: ctx.request_body.clone(),
                        response_body: inbound_wire,
                    },
                    inbound_protocol,
                    ctx.request_id,
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
            retry_after,
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
    let token = ctx.token;
    let price = ctx.price;
    let started = ctx.started;
    let inbound_protocol = ctx.inbound_protocol;
    let mut request_warnings = Vec::new();
    let mut outbound = protocol::encode_request(request, channel.protocol, &mut request_warnings);
    let outbound_model = routing::outbound_model(channel, ctx.routed_model);
    // 目标性 JSON 补丁：强制流式；OpenAI 另注入 stream_options.include_usage
    // （Anthropic 流式自带 usage）。别名重写用该渠道自己的出站模型名。
    if let Value::Object(map) = &mut outbound {
        map.insert("stream".into(), Value::Bool(true));
        if channel.protocol == Protocol::OpenAiChat {
            map.insert(
                "stream_options".into(),
                serde_json::json!({ "include_usage": true }),
            );
        }
        map.insert("model".into(), Value::String(outbound_model.to_string()));
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
            .apply_feature_headers(ctx.inbound_headers)
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
                retry_after: None,
                message: "流式上游不可达".to_string(),
            };
        }
        Err(_) => {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: None,
                retry_after: None,
                message: "流式上游响应超时".to_string(),
            };
        }
    };

    let status = resp.status();
    let status_code = status.as_u16();
    // 上游非 2xx：SSE 流此时尚未开始，直接按错误处理。
    if !status.is_success() {
        let retry_after = parse_retry_after(resp.headers());
        let upstream_body = match take_upstream_body(
            resp,
            &channel.name,
            ctx.snapshot.max_response_bytes,
            "流式上游读体失败",
        )
        .await
        {
            Ok(body) => body,
            Err(outbound) => return outbound,
        };
        let parsed = serde_json::from_slice::<Value>(&upstream_body).unwrap_or(Value::Null);
        if is_retryable_status(status_code) {
            return Outbound::Retryable {
                channel: channel.name.clone(),
                status: Some(status_code),
                retry_after,
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
        snapshot: ctx.snapshot.clone(),
        token: token.clone(),
        request: request.clone(),
        routed_model: ctx.routed_model.to_string(),
        channel: channel.clone(),
        inbound_model: (request.model != outbound_model).then(|| request.model.clone()),
        request_warnings,
        status_code,
        started,
        price,
        inbound_protocol,
        request_body: ctx.request_body.clone(),
        response_body: Vec::new(),
        request_id: ctx.request_id.to_string(),
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
    snapshot: Arc<RuntimeSnapshot>,
    token: Token,
    request: ChatRequest,
    routed_model: String,
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
    request_body: Option<Bytes>,
    response_body: Vec<u8>,
    request_id: String,
}

/// 把上游 SSE 字节流逐帧解码 → 累积 → 重编码，推送到下游通道。
///
/// 每收到一个完整 SSE 数据帧，解码为 IR 流事件并累积（供计费），同时重编码为
/// 入站协议 chunk 帧推给下游。上游流结束时按累积 usage 结算并落日志。
async fn pipe_stream<S>(
    byte_stream: S,
    tx: tokio::sync::mpsc::Sender<SseEvent>,
    mut ctx: StreamTask,
) where
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
    let mut truncated = false;
    let idle = channel_idle(ctx.channel.timeout_ms);
    let mut byte_stream = Box::pin(byte_stream);

    // 流首先下发 message_start（Anthropic 需要；OpenAI 无）与 stream-start 的
    // warnings，让下游在任何内容之前就感知信息损失（跨协议族丢弃的 reasoning 等）。
    if let Some(frame) = encoder.message_start() {
        record_frame_wire(&mut ctx, &frame);
        if tx.send(event_from_frame(&frame)).await.is_err() {
            downstream_open = false;
        }
    }
    let start_event = StreamEvent::StreamStart {
        warnings: ctx.request_warnings.clone(),
    };
    accumulator.push(start_event.clone());
    for frame in encoder.encode(&start_event) {
        if downstream_open {
            record_frame_wire(&mut ctx, &frame);
        }
        if tx.send(event_from_frame(&frame)).await.is_err() {
            downstream_open = false;
            break;
        }
    }

    loop {
        // 尝试从已缓冲字节提取完整 SSE 数据帧。
        if let Some((_event_name, frame)) = take_frame(&mut sse_buffer) {
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
                        record_frame_wire(&mut ctx, &frame);
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
        match tokio::time::timeout(idle, byte_stream.next()).await {
            Ok(Some(Ok(bytes))) => {
                sse_buffer.extend_from_slice(&bytes);
                if sse_buffer.len() > ctx.snapshot.sse_reassembly_max() {
                    truncated = true;
                    break;
                }
            }
            Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
        }
    }

    if truncated && downstream_open {
        let frame =
            inbound_stream_error_frame(ctx.inbound_protocol, SSE_REASSEMBLY_OVERFLOW_MESSAGE);
        record_frame_wire(&mut ctx, &frame);
        if tx.send(event_from_frame(&frame)).await.is_err() {
            downstream_open = false;
        }
    }

    // 流结束：若上游未发 finish 帧（异常中断），补发一个。
    // 缓冲超限已向下游发过错误事件，不再合成 Finish，以免被当成完整成功。
    let response = accumulator.finish();
    if !saw_finish && downstream_open && !truncated {
        let finish_event = StreamEvent::Finish {
            finish_reason: response.finish_reason.clone(),
            usage: response.usage.clone(),
            provider_metadata: response.provider_metadata.clone(),
        };
        for frame in encoder.encode(&finish_event) {
            record_frame_wire(&mut ctx, &frame);
            if tx.send(event_from_frame(&frame)).await.is_err() {
                break;
            }
        }
    }
    // 终止哨兵也是入站响应的一部分，full_body 开启时先记入再结算，
    // 保证日志带全实际下发的字节（结算仍先于哨兵下发）。
    if downstream_open
        && ctx.snapshot.full_body
        && let Some(terminator) = terminator.as_ref()
    {
        append_logged_body(
            &mut ctx.response_body,
            &data_frame_to_wire(terminator),
            ctx.snapshot.log_body_max(),
        );
    }
    // 先结算再发终止哨兵：下游读到终止时计费必定已落库。
    settle_and_log(&ctx, response).await;
    // OpenAI 协议约定以 `data: [DONE]` 结束；Anthropic 以 message_stop 收尾。
    if downstream_open && let Some(terminator) = terminator {
        let _ = tx.send(SseEvent::default().data(terminator)).await;
    }
}

/// full_body 开启时把一个即将下发的入站协议帧记入响应字节；关闭时无操作。
fn record_frame_wire(ctx: &mut StreamTask, frame: &SseFrame) {
    if ctx.snapshot.full_body {
        append_logged_body(
            &mut ctx.response_body,
            &frame_to_wire(frame),
            ctx.snapshot.log_body_max(),
        );
    }
}

/// SSE 重装缓冲超限时写入日志与下游错误事件的固定文案。
const SSE_REASSEMBLY_OVERFLOW_MESSAGE: &str = "SSE 重装缓冲超过上限，流已截断";

/// 把即将落库的响应字节封顶追加，达到 `log_body_max_bytes` 后停止。
fn append_logged_body(buf: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) {
    if buf.len() >= max_bytes {
        return;
    }
    let take = (max_bytes - buf.len()).min(chunk.len());
    buf.extend_from_slice(&chunk[..take]);
}

/// 流中途失败时的入站协议错误 SSE 帧，让下游能感知截断。
fn inbound_stream_error_frame(protocol: Protocol, message: &str) -> SseFrame {
    let body = protocol::encode_error(500, message, protocol);
    let data = serde_json::to_string(&body).unwrap_or_else(|_| message.to_string());
    match protocol {
        Protocol::OpenAiChat => SseFrame::data(data),
        Protocol::OpenAiResponses | Protocol::AnthropicMessages => SseFrame::named("error", data),
    }
}

/// 结算流式请求费用并落日志。
async fn settle_and_log(ctx: &StreamTask, response: ChatResponse) {
    let usage = &response.usage;
    let cost = billing::cost_micros(usage, &ctx.price);
    log_request(
        &ctx.deps,
        &ctx.token,
        &ctx.request.model,
        outbound_model_for_log(&ctx.channel, &ctx.routed_model),
        &ctx.channel.name,
        ctx.status_code,
        ctx.started,
        Billing {
            usage: response.usage.clone(),
            price: ctx.price,
            cost_usd_micros: cost,
            request_body: ctx.request_body.clone(),
            response_body: ctx.snapshot.full_body.then(|| ctx.response_body.clone()),
        },
        ctx.inbound_protocol,
        &ctx.request_id,
    )
    .await;
}

/// 从请求头提取并校验令牌 key，返回匹配的令牌定义；禁用的令牌在此被拒绝。
fn authenticate<'a>(
    snapshot: &'a RuntimeSnapshot,
    headers: &HeaderMap,
) -> anyhow::Result<&'a Token> {
    let key = extract_key(headers).ok_or_else(|| {
        anyhow::anyhow!("缺少认证令牌：请提供 Authorization: Bearer <key> 或 x-api-key")
    })?;
    let token = snapshot
        .tokens
        .get(&key)
        .ok_or_else(|| anyhow::anyhow!("无效的认证令牌"))?;
    if !token.enabled {
        return Err(anyhow::anyhow!("认证令牌已被禁用"));
    }
    Ok(token)
}

/// 从两种头任一种提取令牌 key。
fn extract_key(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get("authorization") {
        let value = value.to_str().ok()?;
        if let Some(key) = extract_bearer(value) {
            return Some(key.to_string());
        }
    }
    headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
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

/// 统一模型 hop 是否继续下一成员。对齐 one-api `shouldRetry`：400 不换下一成员，
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
fn channel_idle(timeout_ms: u64) -> Duration {
    Duration::from_millis(timeout_ms.max(1))
}

/// 日志用的出站模型名：按该渠道自己的别名表改写。
fn outbound_model_for_log<'a>(channel: &'a Channel, inbound: &'a str) -> Option<&'a str> {
    Some(routing::outbound_model(channel, inbound))
}

/// 按渠道名从本次路由结果取出发给上游的模型名；未知渠道则尚未出站。
fn outbound_model_for_channel_name<'a>(
    route: &'a routing::Route,
    channel_name: &str,
    inbound: &'a str,
) -> Option<&'a str> {
    route
        .channels
        .iter()
        .find(|record| record.channel.name == channel_name)
        .map(|record| routing::outbound_model(&record.channel, inbound))
}

/// Anthropic 出站在下游未带版本头时使用的官方默认。
const DEFAULT_ANTHROPIC_VERSION: HeaderValue = HeaderValue::from_static("2023-06-01");

/// 入站功能头白名单：直通与 IR 都原样转发出站，不含认证与 hop-by-hop。
const FORWARDED_FEATURE_HEADERS: &[&str] =
    &["anthropic-beta", "openai-organization", "openai-project"];

/// 按渠道协议设置出站认证头。
///
/// OpenAI 用 `Authorization: Bearer`；Anthropic 用 `x-api-key` 并带
/// `anthropic-version`。直通路径转发下游版本头，避免强行降级；IR / 探测仍钉默认。
pub(super) trait OutboundAuth {
    fn apply_outbound_auth(self, channel: &Channel) -> Self;
    fn apply_outbound_auth_with_version(
        self,
        channel: &Channel,
        inbound_version: Option<&HeaderValue>,
    ) -> Self;
    fn apply_feature_headers(self, inbound: &HeaderMap) -> Self;
}

impl OutboundAuth for reqwest::RequestBuilder {
    fn apply_outbound_auth(self, channel: &Channel) -> Self {
        self.apply_outbound_auth_with_version(channel, None)
    }

    fn apply_outbound_auth_with_version(
        self,
        channel: &Channel,
        inbound_version: Option<&HeaderValue>,
    ) -> Self {
        match channel.protocol {
            Protocol::OpenAiChat | Protocol::OpenAiResponses => self.bearer_auth(&channel.api_key),
            Protocol::AnthropicMessages => self.header("x-api-key", &channel.api_key).header(
                "anthropic-version",
                inbound_version
                    .cloned()
                    .unwrap_or(DEFAULT_ANTHROPIC_VERSION),
            ),
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

/// 构造入站协议错误格式的响应，并落一条请求日志（无计费数据）。
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
        log_request(
            deps,
            token,
            model,
            None,
            "",
            status.as_u16(),
            started,
            Billing {
                usage: Usage::default(),
                price: PriceSnapshot::default(),
                cost_usd_micros: 0,
                request_body,
                response_body: response_wire,
            },
            inbound_protocol,
            request_id,
        )
        .await;
    }
    (status, Json(body)).into_response()
}

/// 令牌生效 RPM：缺省跟随全局兜底，令牌写出的值（含 `0`）覆盖全局。
fn token_rate_limited(
    deps: &Deps,
    token: &Token,
    snapshot: &RuntimeSnapshot,
) -> Result<(), Duration> {
    deps.request_rate.try_acquire(
        &token.token_key,
        effective_rate_limit_rpm(token.rate_limit_rpm, snapshot.rate_limit_rpm),
    )
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

/// 准入阶段数据库错误：500 + OpenAI 错误格式 + 落日志。
#[allow(clippy::too_many_arguments)]
async fn db_error_response(
    deps: &Deps,
    full_body: bool,
    token: &Token,
    model: &str,
    started: i64,
    err: impl std::fmt::Display,
    inbound_protocol: Protocol,
    request_body: Option<Bytes>,
    request_id: &str,
) -> Response {
    let message = format!("计费状态读取失败: {err}");
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        &message,
        deps,
        full_body,
        Some(token),
        Some(model),
        started,
        inbound_protocol,
        request_body,
        request_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{append_logged_body, extract_bearer, inbound_stream_error_frame};
    use crate::config::Protocol;

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

    #[test]
    fn append_logged_body_stops_at_cap() {
        let mut buf = Vec::new();
        append_logged_body(&mut buf, b"hello ", 8);
        append_logged_body(&mut buf, b"world!!!", 8);
        assert_eq!(buf, b"hello wo");
        append_logged_body(&mut buf, b"more", 8);
        assert_eq!(buf, b"hello wo");
    }

    #[test]
    fn inbound_stream_error_frame_names_responses_event() {
        let chat = inbound_stream_error_frame(Protocol::OpenAiChat, "截断");
        assert!(chat.event.is_none(), "Chat Completions 只用 data 行");
        let responses = inbound_stream_error_frame(Protocol::OpenAiResponses, "截断");
        assert_eq!(responses.event.as_deref(), Some("error"));
        let anthropic = inbound_stream_error_frame(Protocol::AnthropicMessages, "截断");
        assert_eq!(anthropic.event.as_deref(), Some("error"));
    }
}
