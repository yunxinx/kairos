//! 渠道连通性探测与上游模型发现。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Protocol;
use crate::core::ir::{ChatRequest, ContentPart, Message, Role};
use crate::gateway::http::{OutboundAuth, upstream_error_message};
use crate::gateway::protocol;
use crate::store::resources::{Channel, StoredChannelKey, select_channel_key};

use super::channels::{parse_channel_id, read_channel_record, reject_non_http_url};
use super::{AdminDeps, AdminError};

pub(super) fn routes() -> Router<AdminDeps> {
    Router::new()
        .route("/channels/models", post(list_upstream_models))
        .route("/channels/{id}/test", post(test_channel))
}

// --- 渠道连通性探测 ---

/// 渠道探测请求：指定要测的模型（清单条目或别名映射的主模型名）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelProbeRequest {
    model: String,
}

/// 渠道探测结果：可达性、超时、状态码、延迟、错误摘要与上游 body 截断。
///
/// 探测不经令牌认证/计费、不落 `request_log`。超时沿用渠道 `timeout_ms`。
#[derive(Debug, Serialize)]
struct ChannelProbeResult {
    reachable: bool,
    timed_out: bool,
    status_code: Option<u16>,
    latency_ms: u64,
    error: Option<String>,
    upstream_body: Option<String>,
}

/// 解析探测出站模型名：清单里的主模型名、清单里的别名、或仅别名生效时的主模型名。
fn resolve_probe_model(channel: &Channel, requested: &str) -> Option<String> {
    if requested.is_empty() {
        return None;
    }
    if let Some(canonical) = channel.model_aliases.get(requested)
        && channel
            .models
            .iter()
            .any(|item| item == requested || item == canonical)
    {
        return Some(canonical.clone());
    }
    if channel.models.iter().any(|item| item == requested) {
        return Some(requested.to_string());
    }
    if channel.models.iter().any(|item| {
        channel
            .model_aliases
            .get(item)
            .is_some_and(|canonical| canonical == requested)
    }) {
        return Some(requested.to_string());
    }
    None
}

/// 向渠道 `base_url` 发一条最小非流式请求，按渠道协议编码，回报可达性。
async fn test_channel(
    State(deps): State<AdminDeps>,
    Path(raw_id): Path<String>,
    body: Result<Json<ChannelProbeRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ChannelProbeResult>, AdminError> {
    let Json(req) = body.map_err(AdminError::bad_body)?;
    let requested = req.model.trim();
    if requested.is_empty() {
        return Err(AdminError::InvalidBody("model 不能为空".to_string()));
    }
    let id = parse_channel_id(raw_id)?;
    let record = read_channel_record(&deps, id).await?;
    let channel = &record.channel;
    reject_non_http_url(&channel.base_url)?;
    let model = resolve_probe_model(channel, requested).ok_or_else(|| {
        AdminError::InvalidBody(format!("模型 {requested} 不在渠道 {id} 的清单中"))
    })?;
    let key = select_channel_key(&record.keys, requested).ok_or_else(|| {
        AdminError::InvalidBody(format!("渠道 {id} 没有可用于模型 {requested} 的启用密钥"))
    })?;
    let request = minimal_probe_request(&model);
    let mut warnings = Vec::new();
    let outbound = protocol::encode_request(&request, channel.protocol, &mut warnings);
    let upstream_url = format!(
        "{}{}",
        channel.base_url.trim_end_matches('/'),
        protocol::upstream_path(channel.protocol)
    );

    let started = Instant::now();
    let send = deps
        .client
        .post(&upstream_url)
        .timeout(Duration::from_millis(channel.timeout_ms))
        .apply_outbound_auth(channel.protocol, key)
        .json(&outbound)
        .send()
        .await;

    let result = match send {
        Ok(resp) => {
            let status_code = resp.status().as_u16();
            let body_text = resp.text().await.unwrap_or_default();
            let error = if (200..300).contains(&status_code) {
                None
            } else {
                Some(probe_error_summary(&body_text, status_code))
            };
            let upstream_body = if body_text.is_empty() {
                None
            } else {
                Some(truncate_error(body_text))
            };
            ChannelProbeResult {
                reachable: true,
                timed_out: false,
                status_code: Some(status_code),
                latency_ms: elapsed_ms(started),
                error,
                upstream_body,
            }
        }
        Err(err) => ChannelProbeResult {
            reachable: false,
            timed_out: err.is_timeout(),
            status_code: None,
            latency_ms: elapsed_ms(started),
            error: Some(truncate_error(upstream_unreachable_message(&err))),
            upstream_body: None,
        },
    };
    Ok(Json(result))
}

/// 探测用最小非流式请求：单条 user 文本 + `max_tokens = 1`。
fn minimal_probe_request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentPart::Text {
                text: "ping".to_string(),
                provider_options: HashMap::new(),
            }],
            provider_options: HashMap::new(),
        }],
        stream: false,
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: Some(1),
        n: None,
        stop: Vec::new(),
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        response_format: None,
        tools: Vec::new(),
        tool_choice: None,
        provider_options: HashMap::new(),
    }
}

/// 从上游错误 body 提取可读摘要；非 JSON 时回退状态码描述。
fn probe_error_summary(body: &str, status: u16) -> String {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    truncate_error(upstream_error_message(&parsed, status))
}

/// 错误摘要截到 512 字节（按 UTF-8 字符边界），避免把整段上游 body 回给管理面。
fn truncate_error(mut message: String) -> String {
    const MAX: usize = 512;
    if message.len() > MAX {
        let end = message.floor_char_boundary(MAX);
        message.truncate(end);
    }
    message
}

/// `Instant` 经过的毫秒，夹到 `u64`。
fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

// --- 上游模型列表 ---

/// 上游模型列表的路径段（相对 `base_url`）：OpenAI 与 Anthropic 均为 `{base}/models`。
const UPSTREAM_MODELS_PATH: &str = "/models";

/// 拉取上游模型列表的草稿请求：仅含出站相关字段，渠道无需已保存。
///
/// 管理面新建渠道向导可在保存前同步模型；`timeout_ms` 沿用为本次请求超时。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpstreamModelsDraft {
    protocol: Protocol,
    base_url: String,
    api_key: String,
    timeout_ms: u64,
}

/// 上游模型列表响应：模型 id 数组，保持上游返回顺序，排序由调用方负责。
#[derive(Debug, Serialize)]
struct UpstreamModelsView {
    models: Vec<String>,
}

/// 按渠道草稿拉取上游模型列表：GET `{base_url}/models`。
///
/// OpenAI（chat/responses）与 Anthropic（messages）的模型列表同为
/// `{"data": [{"id": ...}]}` 形态，故统一解析；认证头按协议复用 `OutboundAuth`。
/// 上游不可达/非 2xx/响应形态非法均映射为 502 `upstream_error`。
async fn list_upstream_models(
    State(deps): State<AdminDeps>,
    body: Result<Json<UpstreamModelsDraft>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<UpstreamModelsView>, AdminError> {
    let Json(draft) = body.map_err(AdminError::bad_body)?;
    if draft.base_url.trim().is_empty() {
        return Err(AdminError::InvalidBody("base_url 不能为空".to_string()));
    }
    reject_non_http_url(&draft.base_url)?;
    if draft.api_key.trim().is_empty() {
        return Err(AdminError::InvalidBody("api_key 不能为空".to_string()));
    }
    if draft.timeout_ms < 1 {
        return Err(AdminError::InvalidBody("timeout_ms 不能小于 1".to_string()));
    }
    let key = StoredChannelKey {
        id: 0,
        channel_id: 0,
        name: "draft".to_string(),
        api_key: draft.api_key,
        weight: 1,
        enabled: true,
        models: None,
        blocked_models: None,
        created_at: 0,
    };
    let url = format!(
        "{}{}",
        draft.base_url.trim_end_matches('/'),
        UPSTREAM_MODELS_PATH
    );
    let send = deps
        .client
        .get(&url)
        .timeout(Duration::from_millis(draft.timeout_ms))
        .apply_outbound_auth(draft.protocol, &key)
        .send()
        .await;
    let response = match send {
        Ok(response) => response,
        Err(err) => {
            return Err(AdminError::Upstream(truncate_error(
                upstream_unreachable_message(&err),
            )));
        }
    };
    let status_code = response.status().as_u16();
    let body_text = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status_code) {
        return Err(AdminError::Upstream(probe_error_summary(
            &body_text,
            status_code,
        )));
    }
    let models = parse_upstream_models(&body_text)?;
    Ok(Json(UpstreamModelsView { models }))
}

/// 从 `{"data": [{"id": ...}]}` 解析模型 id 数组；无 `id` 的条目跳过。
fn parse_upstream_models(body: &str) -> Result<Vec<String>, AdminError> {
    let parsed: Value = serde_json::from_str(body)
        .map_err(|_| AdminError::Upstream("上游响应不是合法 JSON".to_string()))?;
    let data = parsed
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| AdminError::Upstream("上游响应缺少 data 数组".to_string()))?;
    Ok(data
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect())
}

/// 出站请求发送失败的错误摘要：超时与连接失败措辞与探测保持一致。
fn upstream_unreachable_message(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "请求超时".to_string()
    } else {
        format!("上游不可达: {err}")
    }
}
