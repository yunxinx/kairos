//! 请求整流器端到端黑盒测试。
//!
//! 主接缝：测试内启动网关 + 可编程 mock 上游，上游先以可修正的 400 拒绝、
//! 再返回成功，断言整流重试对下游与出站请求两侧的可观测效果：第二次出站
//! 请求已按最小修正改写、整流动作落系统日志、开关关闭时 400 原样返回。

mod common;

use common::{TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use kairos::config;
use serde_json::{Value, json};

/// mock 上游的 400 错误体：`error.message` 携带给定文本。
fn error400(message: &str) -> UpstreamBehavior {
    UpstreamBehavior::Error {
        status: 400,
        body: json!({ "error": { "message": message, "type": "invalid_request_error" } }),
    }
}

/// Anthropic 上游的非流式成功响应。
fn anthropic_ok(model: &str) -> Value {
    json!({
        "id": "msg_up", "type": "message", "role": "assistant", "model": model,
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 2 }
    })
}

/// OpenAI Chat 上游的非流式成功响应。
fn chat_ok() -> Value {
    json!({
        "id": "chatcmpl-up", "object": "chat.completion", "model": "gpt-4o-mini",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "ok" },
                     "logprobs": null, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    })
}

/// Anthropic 协议渠道：别名命中强制 IR 路径，thinking 块经逃生舱原样往返。
fn anthropic_channel_seed(base: &str) -> common::Seed {
    let mut seed = common::test_seed(base);
    seed.channels[0].protocol = config::Protocol::AnthropicMessages;
    seed
}

/// 发起 Anthropic Messages 入站请求，历史 assistant 轮携带 thinking 块。
async fn post_messages_with_thinking(base: &str, stream: bool) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .header("x-api-key", TEST_TOKEN_KEY)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": "fast",
            "max_tokens": 1024,
            "stream": stream,
            "messages": [
                { "role": "user", "content": [{ "type": "text", "text": "hi" }] },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "previous thought", "signature": "sig-1" },
                    { "type": "text", "text": "will do" },
                ] },
                { "role": "user", "content": [{ "type": "text", "text": "continue" }] },
            ],
        }))
        .send()
        .await
        .expect("应能请求网关")
}

/// 统计 wire 消息里 thinking/redacted_thinking 块的数量。
fn count_thinking_blocks(body: &Value) -> usize {
    body["messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .flat_map(|message| message["content"].as_array().into_iter().flatten())
                .filter(|block| {
                    matches!(
                        block["type"].as_str(),
                        Some("thinking") | Some("redacted_thinking")
                    )
                })
                .count()
        })
        .unwrap_or(0)
}

/// signature 失效的 400 整流后重试成功：第二次出站请求已剥离 thinking 块，
/// 动作落系统日志，下游收到成功响应且整流 warning 随响应面回传。
#[tokio::test]
async fn signature_400_is_rectified_and_retried_once() {
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    gw.upstream.set_behavior(error400(
        "messages.1.content.0: Invalid `signature` in `thinking` block",
    ));
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(anthropic_ok("gpt-4o-mini")));

    let resp = post_messages_with_thinking(&gw.base_url(), false).await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);

    let requests = gw.upstream.received();
    assert_eq!(requests.len(), 2, "整流重试应恰好再发一次出站请求");
    assert!(
        count_thinking_blocks(&requests[0]) > 0,
        "首次出站请求应携带 thinking 块"
    );
    assert_eq!(
        count_thinking_blocks(&requests[1]),
        0,
        "整流后的出站请求应剥离全部 thinking 块"
    );

    // 下游响应面：整流动作随 gateway.warnings 回传。
    let body: Value = resp.json().await.expect("响应应可解析");
    let warnings = body["gateway"]["warnings"]
        .as_array()
        .expect("应有 warnings");
    assert!(
        warnings
            .iter()
            .any(|warning| warning["feature"] == "reasoning"
                && warning["details"]
                    .as_str()
                    .is_some_and(|details| details.contains("整流"))),
        "warnings 应含整流剥离的 reasoning 告警: {warnings:?}"
    );

    // 审计：整流动作落系统日志。
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM system_log WHERE target = 'rectifier' AND event_code = 'rectifier.request_rectified'",
    )
    .fetch_one(&gw.pool)
    .await
    .expect("应能查询系统日志");
    assert_eq!(count, 1, "整流重试应落一条系统日志");
}

/// tool schema 不合规的 400 整流后重试成功：第二次出站请求的 input schema
/// 已摊平为显式 object 形态。
#[tokio::test]
async fn tool_schema_400_triggers_normalization_retry() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(error400(
        "Invalid schema for tools[0]: root must be an object",
    ));
    gw.upstream.set_behavior(UpstreamBehavior::Json(chat_ok()));

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "parameters": { "anyOf": [
                        { "type": "object", "properties": { "city": { "type": "string" } } },
                    ] },
                },
            }],
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), axum::http::StatusCode::OK);

    let requests = gw.upstream.received();
    assert_eq!(requests.len(), 2, "整流重试应恰好再发一次出站请求");
    let first_parameters = &requests[0]["tools"][0]["function"]["parameters"];
    assert!(
        first_parameters.get("anyOf").is_some(),
        "首次出站请求应保留根级 union schema"
    );
    assert_eq!(
        requests[1]["tools"][0]["function"]["parameters"],
        json!({ "type": "object", "properties": { "city": { "type": "string" } } }),
        "整流后的出站 schema 应摊平为显式 object"
    );
}

/// 整流开关关闭：可修正的 400 原样返回下游，不重试。
#[tokio::test]
async fn rectify_disabled_returns_400_without_retry() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = anthropic_channel_seed(base);
        seed.settings.insert(
            "request_rectify".to_string(),
            serde_json::Value::Bool(false),
        );
        seed
    })
    .await;
    gw.upstream.set_behavior(error400(
        "messages.1.content.0: Invalid `signature` in `thinking` block",
    ));

    let resp = post_messages_with_thinking(&gw.base_url(), false).await;
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    let requests = gw.upstream.received();
    assert_eq!(requests.len(), 1, "开关关闭时不应重试");

    let body: Value = resp.json().await.expect("错误体应可解析");
    assert_eq!(body["error"]["gateway"]["channel"], "test-channel");
    assert_eq!(body["error"]["gateway"]["failover"], false);
}

/// 无修正余地的 400（模式不命中或命中后 IR 无改写空间）不触发重试。
#[tokio::test]
async fn unrectifiable_400_returns_without_retry() {
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;

    // 模式不命中。
    gw.upstream
        .set_behavior(error400("totally unknown failure"));
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": "fast",
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }],
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(gw.upstream.received().len(), 1, "模式不命中时不应重试");

    // 模式命中但请求不含 reasoning 内容，无改写空间。
    gw.upstream.set_behavior(error400(
        "a final `assistant` message must start with a thinking block",
    ));
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": "fast",
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }],
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(gw.upstream.received().len(), 2, "无修正余地时不应重试");
}

/// 流式路径同规：上游 400 整流后重试成功，下游收到完整 SSE 流。
#[tokio::test]
async fn stream_signature_400_is_rectified_and_retried() {
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    gw.upstream.set_behavior(error400(
        "Unable to submit request because Thought signature is not valid.",
    ));
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        json!({
            "type": "message_start",
            "message": { "id": "msg_01s", "model": "gpt-4o-mini", "usage": { "input_tokens": 10, "output_tokens": 0 } }
        })
        .to_string(),
        json!({
            "type": "content_block_delta", "index": 0,
            "content_block": { "type": "text", "text": "" },
            "delta": { "type": "text_delta", "text": "ok" }
        })
        .to_string(),
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "input_tokens": 10, "output_tokens": 2 }
        })
        .to_string(),
    ]));

    let mut body = post_messages_with_thinking(&gw.base_url(), true)
        .await
        .bytes_stream();
    let mut raw = Vec::new();
    use futures_util::StreamExt;
    while let Some(chunk) = body.next().await {
        raw.extend_from_slice(&chunk.expect("流分块应可读"));
    }
    let text = String::from_utf8(raw).expect("SSE 流应为 UTF-8");
    assert!(
        text.contains("message_start") && text.contains("message_stop"),
        "整流重试后下游应收到完整流: {text}"
    );
    assert!(
        text.contains("整流"),
        "流首应携带整流 action 的 warnings: {text}"
    );
    assert_eq!(gw.upstream.received().len(), 2, "流式路径应恰好重试一次");
}
