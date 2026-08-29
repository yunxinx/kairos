//! 流式 IR 路径（#05）端到端黑盒测试：mock 上游以 SSE 流响应，断言下游逐帧
//! 收到入站协议 SSE 事件、流式 usage 计费正确。
//!
//! 主接缝：端到端 HTTP 黑盒，断言外部可观察行为（下游收到的 SSE 帧、SQLite
//! 中的计费与日志）。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior, collect_sse_frames};
use serde_json::json;

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
    assert_eq!(frames[0].data["object"], "chat.completion.chunk");
    assert_eq!(frames[0].data["choices"][0]["delta"]["role"], "assistant");
    assert_eq!(frames[0].data["choices"][0]["delta"]["content"], "Hel");
    assert_eq!(frames[1].data["choices"][0]["delta"]["content"], "lo");
    // 有 finish 帧（带 finish_reason）与独立 usage 帧。
    let finish = frames
        .iter()
        .find(|f| f.data["choices"][0]["finish_reason"] == "stop")
        .expect("应有 finish 帧");
    assert_eq!(finish.data["choices"][0]["finish_reason"], "stop");
    let usage_frame = frames.last().expect("应有 usage 帧");
    assert_eq!(usage_frame.data["usage"]["completion_tokens"], 2);
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
        frames[0].data["choices"][0]["delta"]["tool_calls"][0]["id"],
        "call_1"
    );
    assert_eq!(
        frames[0].data["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    // 增量帧只带 arguments。
    assert_eq!(
        frames[1].data["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
        r#"{"city":"SF"}"#
    );
    let last = frames.last().expect("应有 finish 帧");
    assert_eq!(last.data["choices"][0]["finish_reason"], "tool_calls");
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

/// 流中途上游报错（Anthropic 200 后 `event: error`）：错误以入站协议错误帧
/// 下发、不再合成 Finish，结算按已累积 usage 落账（此处为零）。
///
/// 走跨协议路由（chat 入站 → Anthropic 渠道）以经过 IR 流式面——同协议
/// 直通路径原样转发字节。
#[tokio::test]
async fn midstream_error_delivers_error_frame_and_settles() {
    fn anthropic_channel_seed(base: &str) -> common::Seed {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = kairos::config::Protocol::AnthropicMessages;
        seed
    }
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "type": "message_start",
            "message": { "type": "message", "role": "assistant", "id": "msg_1", "model": "claude-sonnet", "content": [] }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_start", "index": 0, "content_block": { "type": "text" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "你好" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" }
        }))
        .unwrap(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let frames = collect_sse_frames(resp).await;

    // 错误前的内容增量正常下发。
    assert!(
        frames
            .iter()
            .any(|f| f.data["choices"][0]["delta"]["content"] == json!("你好")),
        "错误前的内容帧应照常透传"
    );
    // 末帧为入站协议错误帧（chat data 帧），全程无 finish 帧（不合成完整成功）。
    let last = frames.last().expect("应有错误帧");
    assert_eq!(last.data["error"]["message"], json!("Overloaded"));
    assert!(
        frames
            .iter()
            .all(|f| f.data["choices"][0]["finish_reason"].is_null())
    );

    // 结算按已累积 usage 落账（错误前无 usage 上报 → 零费用），日志落一行。
    let row: (i64, i64) = sqlx::query_as(
        "SELECT ub.balance_usd_micros, rl.cost_usd_micros \
         FROM tokens t \
         JOIN user_balance ub ON ub.user_id = t.user_id \
         JOIN request_log rl ON rl.token_key = t.token_key \
         WHERE t.token_key = ?",
    )
    .bind(TEST_TOKEN_KEY)
    .fetch_one(&gw.pool)
    .await
    .expect("应有结算日志");
    assert_eq!(row.0, 5_000_000, "零累积 usage 不扣费");
    assert_eq!(row.1, 0, "日志按已累积 usage 落账（零费用）");
}
