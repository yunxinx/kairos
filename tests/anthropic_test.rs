//! Anthropic Messages 适配器（#08）端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒，断言外部可观察行为——mock 上游收到的出站请求体、
//! 下游收到的响应、SQLite 中的计费与日志。覆盖：OpenAI chat 入站调 Anthropic 渠道
//! （非流式/流式/tool calling）、Anthropic 入站调 OpenAI 渠道、thinking signature
//! 同协议族无损回传、跨协议族 reasoning 丢弃记 warning。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use futures_util::StreamExt;
use kairos::config;
use serde_json::{Value, json};

/// 解析下游 SSE 响应体，返回所有 `data:` 帧的 JSON 值列表。
async fn collect_sse_frames(resp: reqwest::Response) -> Vec<Value> {
    let mut frames = Vec::new();
    let mut buffer = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("响应流应可读");
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buffer.find("\n\n") {
            let frame: String = buffer.drain(..end + 2).collect();
            for line in frame.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(data) {
                        frames.push(value);
                    }
                }
            }
        }
    }
    frames
}

/// 构造指向 mock 上游的 Anthropic 渠道 seed（其余沿用测试默认）。
fn anthropic_channel_seed(base: &str) -> common::Seed {
    let mut seed = common::test_seed(base);
    seed.channels[0].protocol = config::Protocol::AnthropicMessages;
    seed
}

/// OpenAI chat 入站 → Anthropic 渠道：非流式跨协议转换。
#[tokio::test]
async fn openai_inbound_to_anthropic_channel_non_stream() {
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    // 上游以 Anthropic Messages 响应（含 tool_use），网关解码为 IR 再重编码为 openai chat。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_01x", "type": "message", "role": "assistant", "model": "claude-sonnet",
        "content": [
            { "type": "text", "text": "I'll check the weather." },
            { "type": "tool_use", "id": "toolu_01", "name": "get_weather", "input": { "city": "San Francisco" } }
        ],
        "stop_reason": "tool_use", "stop_sequence": null,
        "usage": { "input_tokens": 25, "output_tokens": 12 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "What is the weather in San Francisco?" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 下游收到 openai chat.completion，tool_use 映射为 tool_calls。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "I'll check the weather."
    );
    assert_eq!(
        body["choices"][0]["message"]["tool_calls"][0]["id"],
        "toolu_01"
    );
    assert_eq!(
        body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");

    // 出站请求经 IR 重编码为 Anthropic 格式（含 tool 往返）。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1, "mock 上游应收一条请求");
    assert_eq!(received[0]["model"], TEST_MODEL);
    assert_eq!(received[0]["messages"][0]["role"], "user");
    assert_eq!(
        received[0]["messages"][0]["content"],
        "What is the weather in San Francisco?"
    );

    // 计费：input 25 + output 12。
    // 费用 = 25*2.5/1M + 12*10/1M = 62 + 120 = 182 微元。
    let row: (i64, i64) = sqlx::query_as(
        "SELECT settled_usd_micros, input_tokens FROM token_balance JOIN request_log \
         ON token_balance.token_key = request_log.token_key WHERE token_balance.token_key = ?",
    )
    .bind(TEST_TOKEN_KEY)
    .fetch_one(&gw.pool)
    .await
    .expect("应能查询计费");
    assert_eq!(row.0, 182);
    assert_eq!(row.1, 25);
}

/// OpenAI chat 入站 → Anthropic 渠道：thinking 响应跨协议族丢弃并记 warning。
#[tokio::test]
async fn openai_inbound_drops_anthropic_reasoning_with_warning() {
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    // 上游 Anthropic 返回 thinking + text；openai chat 无 reasoning 通道，丢弃记 warning。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_01r", "type": "message", "role": "assistant", "model": "claude-sonnet",
        "content": [
            { "type": "thinking", "thinking": "先算 925 ÷ 5", "signature": "ErUBCkY" },
            { "type": "text", "text": "结果是 185" }
        ],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 5 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "What is 925/5?" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: Value = resp.json().await.expect("响应应可解析");
    // reasoning 被丢弃，仅 text 保留。
    assert_eq!(body["choices"][0]["message"]["content"], "结果是 185");
    // 显式 warning：reasoning 丢弃。
    assert_eq!(body["gateway"]["warnings"][0]["type"], "unsupported");
    assert_eq!(body["gateway"]["warnings"][0]["feature"], "reasoning");
}

/// Anthropic 入站 → OpenAI 渠道：非流式。
#[tokio::test]
async fn anthropic_inbound_to_openai_channel() {
    let mut gw = TestGateway::start().await; // 默认 openai_chat 渠道。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-123", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hello!"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 下游收到 Anthropic Messages 格式。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    assert_eq!(body["content"][0]["type"], "text");
    assert_eq!(body["content"][0]["text"], "Hello!");
    assert_eq!(body["stop_reason"], "end_turn");

    // 出站请求经 IR 重编码为 openai chat 格式。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["model"], TEST_MODEL);
    assert_eq!(received[0]["messages"][0]["content"], "hi");

    // 日志 inbound_protocol 落 anthropic_messages。
    let protocol: String = sqlx::query_scalar("SELECT inbound_protocol FROM request_log")
        .fetch_one(&gw.pool)
        .await
        .expect("应有日志");
    assert_eq!(protocol, "anthropic_messages");
}

/// Anthropic 入站 → Anthropic 渠道（同协议直通）：字节流直通，usage 计费。
#[tokio::test]
async fn anthropic_passthrough_forwards_and_bills() {
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    // 同协议（anthropic ↔ anthropic）且未命中别名 → 直通快路径。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_01p", "type": "message", "role": "assistant", "model": TEST_MODEL,
        "content": [{ "type": "text", "text": "直通" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 100, "output_tokens": 20 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 直通：响应原样透传（含 type/role/stop_reason）。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["id"], "msg_01p");
    assert_eq!(body["content"][0]["text"], "直通");

    // 出站请求体与下游一致（Anthropic 无补丁）。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["model"], TEST_MODEL);
    assert_eq!(received[0]["messages"][0]["content"], "hi");

    // 计费：input 100 + output 20 = 100*2.5/1M + 20*10/1M = 250 + 200 = 450 微元。
    let row: (i64, i64) = sqlx::query_as(
        "SELECT settled_usd_micros, input_tokens FROM token_balance JOIN request_log \
         ON token_balance.token_key = request_log.token_key WHERE token_balance.token_key = ?",
    )
    .bind(TEST_TOKEN_KEY)
    .fetch_one(&gw.pool)
    .await
    .expect("应能查询计费");
    assert_eq!(row.0, 450);
    assert_eq!(row.1, 100);
}

/// OpenAI chat 入站 → Anthropic 渠道：流式跨协议，下游收到 openai chunk 帧。
#[tokio::test]
async fn openai_inbound_to_anthropic_channel_streaming() {
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    // 上游以 Anthropic SSE 流响应。
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "type": "message_start",
            "message": { "id": "msg_01s", "model": "claude-sonnet", "usage": { "input_tokens": 10, "output_tokens": 0 } }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "text", "text": "" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "Hel" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "lo" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "input_tokens": 10, "output_tokens": 2 }
        }))
        .unwrap(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let frames = collect_sse_frames(resp).await;
    // 下游逐帧收到 openai chat.completion.chunk，文本累积完整。
    assert_eq!(frames[0]["object"], "chat.completion.chunk");
    let text: String = frames
        .iter()
        .map(|f| f["choices"][0]["delta"]["content"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(text, "Hello");
    // finish 帧带 finish_reason 与 usage。
    let finish = frames
        .iter()
        .find(|f| f["choices"][0]["finish_reason"] == "stop")
        .expect("应有 finish 帧");
    assert_eq!(finish["usage"]["completion_tokens"], 2);

    // 计费：input 10 + output 2 = 10*2.5/1M + 2*10/1M = 25 + 20 = 45 微元。
    let row: (i64,) =
        sqlx::query_as("SELECT settled_usd_micros FROM token_balance WHERE token_key = ?")
            .bind(TEST_TOKEN_KEY)
            .fetch_one(&gw.pool)
            .await
            .expect("应能查询计费");
    assert_eq!(row.0, 45);
}

/// Anthropic 入站 → Anthropic 渠道：流式直通，usage 跨事件逐分量 max 合并计费。
///
/// Anthropic 的 usage 分散在 `message_start`（输入侧 input）与 `message_delta`
///（最终 output），直通快路径逐帧嗅探后按分量取 max，账单完整。
#[tokio::test]
async fn anthropic_passthrough_streaming_bills_split_usage() {
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    // 同协议（anthropic ↔ anthropic）流式直通。
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "type": "message_start",
            "message": { "id": "msg_01", "model": "claude-sonnet", "usage": { "input_tokens": 50, "output_tokens": 0 } }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "text", "text": "" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "直通" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "input_tokens": 50, "output_tokens": 5 }
        }))
        .unwrap(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "max_tokens": 1024,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 消费完流，确保结算已落库；直通转发文本增量。
    let frames = collect_sse_frames(resp).await;
    assert!(
        frames.iter().any(|f| f["type"] == "content_block_delta"),
        "直通应转发 Anthropic 事件帧"
    );

    // 计费：input 50 + output 5 = 50*2.5/1M + 5*10/1M = 125 + 50 = 175 微元
    // （message_start 的 input 与 message_delta 的 output 逐分量 max 合并）。
    let row: (i64,) =
        sqlx::query_as("SELECT settled_usd_micros FROM token_balance WHERE token_key = ?")
            .bind(TEST_TOKEN_KEY)
            .fetch_one(&gw.pool)
            .await
            .expect("应能查询计费");
    assert_eq!(row.0, 175);
}

/// Anthropic 入站错误：下游收到 Anthropic 错误格式。
#[tokio::test]
async fn anthropic_inbound_error_uses_anthropic_shape() {
    let gw = TestGateway::start().await;
    let client = reqwest::Client::new();
    // 无认证 → 401，Anthropic 错误格式。
    let resp = client
        .post(format!("{}/v1/messages", gw.base_url()))
        .json(&json!({ "model": TEST_MODEL, "max_tokens": 1, "messages": [] }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = resp.json().await.expect("错误体应可解析");
    assert_eq!(body["type"], "error");
    assert!(body["error"]["message"].is_string());
    assert!(gw.upstream.received().is_empty(), "未认证不应出站");
}
