//! 流式 IR 路径（#05）端到端黑盒测试：mock 上游以 SSE 流响应，断言下游逐帧
//! 收到入站协议 SSE 事件、流式 usage 计费正确。
//!
//! 主接缝：端到端 HTTP 黑盒，断言外部可观察行为（下游收到的 SSE 帧、SQLite
//! 中的计费与日志）。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use futures_util::StreamExt;
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

/// 发起流式 Chat Completions 请求。
async fn send_stream(base: &str) -> reqwest::Response {
    let client = reqwest::Client::new();
    client
        .post(format!("{}/v1/chat/completions", base))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关")
}

/// 流式文本：下游逐帧收到 `chat.completion.chunk`，文本累积完整。
#[tokio::test]
async fn streaming_text_delivers_chunks() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "Hel" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "content": "lo" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }]
        }))
        .unwrap(),
        // 真实 OpenAI `include_usage`：usage 在独立末帧，choices 为空。
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        }))
        .unwrap(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let frames = collect_sse_frames(resp).await;

    // 首帧携带 role，后续帧只带 content 增量。
    assert_eq!(frames[0]["object"], "chat.completion.chunk");
    assert_eq!(frames[0]["choices"][0]["delta"]["role"], "assistant");
    assert_eq!(frames[0]["choices"][0]["delta"]["content"], "Hel");
    assert_eq!(frames[1]["choices"][0]["delta"]["content"], "lo");
    // 有 finish 帧（带 finish_reason）与独立 usage 帧。
    let finish = frames
        .iter()
        .find(|f| f["choices"][0]["finish_reason"] == "stop")
        .expect("应有 finish 帧");
    assert_eq!(finish["choices"][0]["finish_reason"], "stop");
    let usage_frame = frames.last().expect("应有 usage 帧");
    assert_eq!(usage_frame["usage"]["completion_tokens"], 2);
}

/// 流式 tool-call：tool_input 跨帧累积，末帧带完整参数。
#[tokio::test]
async fn streaming_tool_call_accumulates() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "tool_calls": [{
                "index": 0, "id": "call_1", "type": "function",
                "function": { "name": "get_weather", "arguments": "" }
            }] } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "tool_calls": [{
                "index": 0, "function": { "arguments": r#"{"city":"SF"}"# }
            }] } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6 }
        }))
        .unwrap(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let frames = collect_sse_frames(resp).await;

    // 工具调用首帧带 id 与 name。
    assert_eq!(
        frames[0]["choices"][0]["delta"]["tool_calls"][0]["id"],
        "call_1"
    );
    assert_eq!(
        frames[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    // 增量帧只带 arguments。
    assert_eq!(
        frames[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
        r#"{"city":"SF"}"#
    );
    let last = frames.last().expect("应有 finish 帧");
    assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
}

/// 流式 usage 计费：按实际 usage 四分量精确扣减并落日志。
#[tokio::test]
async fn streaming_usage_is_billed() {
    let mut gw = TestGateway::start().await;
    // usage：input 1000 / output 100 / cache_read 200 / cache_write 50。
    // wire 折算：input = prompt - cached - cache_write = 1250-200-50 = 1000。
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "ok" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }]
        }))
        .unwrap(),
        // usage 独立末帧（真实 `include_usage` 帧型）。
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [],
            "usage": {
                "prompt_tokens": 1250, "completion_tokens": 100, "total_tokens": 1350,
                "prompt_tokens_details": { "cached_tokens": 200, "cache_write_tokens": 50 }
            }
        }))
        .unwrap(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    // 消费完流，确保结算已落库。
    collect_sse_frames(resp).await;

    // 期望费用 = 1000*2.5 + 100*10 + 200*1.25 + 50*10 = 2500+1000+250+500 = 4250。
    let row: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT ub.balance_usd_micros, tb.settled_usd_micros, input_tokens, cost_usd_micros \
         FROM tokens t \
         JOIN user_balance ub ON ub.user_id = t.user_id \
         JOIN token_balance tb ON tb.token_key = t.token_key \
         JOIN request_log ON request_log.token_key = t.token_key \
         WHERE t.token_key = ?",
    )
    .bind(TEST_TOKEN_KEY)
    .fetch_one(&gw.pool)
    .await
    .expect("应能查询余额与日志");
    assert_eq!(row.0, 5_000_000 - 4250, "余额应扣减 4250");
    assert_eq!(row.1, 4250, "累计结算应增加 4250");
    assert_eq!(row.2, 1000, "日志应记录 input=1000");
    assert_eq!(row.3, 4250, "日志应记录费用 4250");
}

/// 流式上游返回非 2xx：状态码原样透传，不扣费。
#[tokio::test]
async fn streaming_upstream_error_is_passthrough_and_not_billed() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Status429);

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    let row: (i64, i64) = sqlx::query_as(
        "SELECT ub.balance_usd_micros, tb.settled_usd_micros \
         FROM tokens t \
         JOIN user_balance ub ON ub.user_id = t.user_id \
         JOIN token_balance tb ON tb.token_key = t.token_key \
         WHERE t.token_key = ?",
    )
    .bind(TEST_TOKEN_KEY)
    .fetch_one(&gw.pool)
    .await
    .expect("令牌余额应存在");
    assert_eq!(row.0, 5_000_000, "流式失败不应扣费");
    assert_eq!(row.1, 0);
}
