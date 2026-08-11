//! HTTP 网关：入站路由 + 令牌认证 + 渠道选择 + 出站调用 + 请求日志。
//!
//! 本模块承载 Chat Completions 非流式垂直切片的完整链路：下游以 OpenAI Chat
//! Completions 协议带令牌发请求，网关认证后经 IR 转换出站到目标渠道，返回入站
//! 协议格式的响应，并在 SQLite 落一条请求日志。协议转换由 `core::openai_chat`
//! 适配器承担，wire 类型不出适配器边界。

use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::{
    config::{Channel, Config, Token},
    core::openai_chat,
    store,
};

/// 网关依赖：存储连接池 + 出站 HTTP 客户端 + 认证令牌表 + 渠道表。
#[derive(Clone)]
pub struct Deps {
    pool: SqlitePool,
    client: reqwest::Client,
    tokens: HashMap<String, Token>,
    channels: Vec<Channel>,
}

/// 组装网关路由。`cfg` 持有认证令牌与渠道配置。
pub fn router(cfg: &Config, pool: SqlitePool) -> Router {
    let tokens: HashMap<String, Token> = cfg
        .tokens
        .iter()
        .map(|token| (token.key.clone(), token.clone()))
        .collect();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client 构建不应失败");

    let deps = Deps {
        pool,
        client,
        tokens,
        channels: cfg.channels.clone(),
    };

    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .fallback(not_found)
        .with_state(deps)
}

/// 未实现路径的确定响应：404 + 可读提示。
async fn not_found() -> (StatusCode, &'static str) {
    (StatusCode::NOT_FOUND, "路径未实现")
}

/// Chat Completions 非流式端点：认证 → 解码 → 准入 → 出站 → 响应 → 落日志。
async fn chat_completions(
    State(deps): State<Deps>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let started = unix_millis();

    // 1. 认证：Bearer 或 x-api-key 两种头都接受。
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
            )
            .await;
        }
    };

    // 2. 解码入站请求为 IR。
    let request = match openai_chat::decode_request(&body) {
        Ok(request) => request,
        Err(err) => {
            let message = format!("请求体无法解析为 Chat Completions: {err}");
            return error_response(
                StatusCode::BAD_REQUEST,
                &message,
                &deps,
                Some(token),
                None,
                started,
            )
            .await;
        }
    };

    // 非流式范围：本票拒绝 stream=true。
    if request.stream {
        let message = "流式请求（stream=true）尚未支持，本票仅覆盖非流式";
        return error_response(
            StatusCode::BAD_REQUEST,
            message,
            &deps,
            Some(token),
            Some(&request.model),
            started,
        )
        .await;
    }

    // 3. 准入：模型必须有候选渠道。
    let channel = match select_channel(&deps.channels, &request.model) {
        Some(channel) => channel,
        None => {
            let message = format!("模型 {} 未配置任何可用渠道", request.model);
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &message,
                &deps,
                Some(token),
                Some(&request.model),
                started,
            )
            .await;
        }
    };

    // 4. 出站：编码 IR 为出站协议，调用上游。
    let outbound = openai_chat::encode_request(&request);
    let upstream_url = format!(
        "{}/chat/completions",
        channel.base_url.trim_end_matches('/')
    );

    let upstream = deps
        .client
        .post(&upstream_url)
        .bearer_auth(&channel.api_key)
        .json(&outbound)
        .send()
        .await;

    match upstream {
        Ok(resp) => {
            let status = resp.status();
            let status_code = status.as_u16();
            let upstream_body = resp.text().await.unwrap_or_default();
            let parsed = serde_json::from_str::<Value>(&upstream_body).unwrap_or(Value::Null);

            if status.is_success() {
                // 5. 响应：解码上游响应为 IR，再重编码为入站协议返回。
                match openai_chat::decode_response(&parsed) {
                    Ok(ir) => {
                        let inbound = openai_chat::encode_response(&ir);
                        log_request(
                            &deps,
                            token,
                            &request.model,
                            &channel.name,
                            status_code,
                            started,
                        )
                        .await;
                        Json(inbound).into_response()
                    }
                    Err(err) => {
                        let message = format!("上游响应无法解析: {err}");
                        error_response(
                            StatusCode::BAD_GATEWAY,
                            &message,
                            &deps,
                            Some(token),
                            Some(&request.model),
                            started,
                        )
                        .await
                    }
                }
            } else {
                // 上游错误：状态码原样 + OpenAI 错误格式（可解析则透传，否则合成）。
                let body = if parsed.is_object() {
                    parsed
                } else {
                    openai_chat::encode_error(status_code, "上游返回了非标准错误响应")
                };
                log_request(
                    &deps,
                    token,
                    &request.model,
                    &channel.name,
                    status_code,
                    started,
                )
                .await;
                (status, Json(body)).into_response()
            }
        }
        Err(_) => {
            let message = "上游不可达";
            error_response(
                StatusCode::BAD_GATEWAY,
                message,
                &deps,
                Some(token),
                Some(&request.model),
                started,
            )
            .await
        }
    }
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

/// 选择 `model` 的候选渠道：取第一个（路由/failover 属 #06）。
///
/// 命中条件：渠道 `models` 列表含该模型，或别名短名（`model_aliases` 的 key）
/// 匹配。别名指向的上游真实模型名（value）不参与匹配，出站模型名重写在 #06 落地。
fn select_channel<'a>(channels: &'a [Channel], model: &str) -> Option<&'a Channel> {
    channels
        .iter()
        .find(|c| c.models.iter().any(|m| m == model) || c.model_aliases.contains_key(model))
}

/// 构造 OpenAI 错误格式的响应，并落一条请求日志。
async fn error_response(
    status: StatusCode,
    message: &str,
    deps: &Deps,
    token: Option<&Token>,
    model: Option<&str>,
    started: i64,
) -> Response {
    let body = openai_chat::encode_error(status.as_u16(), message);
    if let (Some(token), Some(model)) = (token, model) {
        log_request(deps, token, model, "", status.as_u16(), started).await;
    }
    (status, Json(body)).into_response()
}

/// 落一条请求日志。await 以保证响应返回时日志已落库（测试与后续对账依赖）。
async fn log_request(
    deps: &Deps,
    token: &Token,
    model: &str,
    channel: &str,
    status: u16,
    started: i64,
) {
    let now = unix_millis();
    let log = store::RequestLog {
        created_at: now,
        token_name: token.name.clone(),
        inbound_protocol: "openai_chat".to_string(),
        model: model.to_string(),
        channel: channel.to_string(),
        status_code: status as i64,
        latency_ms: now - started,
    };
    if let Err(err) = store::insert_request_log(&deps.pool, &log).await {
        eprintln!("请求日志落库失败: {err}");
    }
}

/// 当前 unix 毫秒时间戳。
fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
