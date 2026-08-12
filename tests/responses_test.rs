//! OpenAI Responses 适配器（#09）端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒，断言外部可观察行为——mock 上游收到的出站请求体、
//! 下游收到的响应、SQLite 中的计费与日志。覆盖：Responses 入站调 openai_chat /
//! Anthropic 渠道（非流式/流式）、同协议直通、openai_chat 入站调 Responses 渠道
//! 反向、跨协议族 reasoning 丢弃记 warning、有状态特性 Out of Scope 显式 warning。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use futures_util::StreamExt;
use kairos::config;
use serde_json::{Value, json};

/// 解析下游 SSE 响应体，返回所有 `data:` 帧的 JSON 值列表（含 `event:` 名）。
async fn collect_sse_frames(resp: reqwest::Response) -> Vec<(Option<String>, Value)> {
    let mut frames = Vec::new();
    let mut buffer = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("响应流应可读");
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buffer.find("\n\n") {
            let frame: String = buffer.drain(..end + 2).collect();
            let mut event = None;
            let mut data = None;
            for line in frame.lines() {
                if let Some(name) = line.strip_prefix("event:") {
                    event = Some(name.trim().to_string());
                } else if let Some(d) = line.strip_prefix("data:") {
                    let d = d.trim();
                    if d.is_empty() || d == "[DONE]" {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(d) {
                        data = Some(value);
                    }
                }
            }
            if let Some(data) = data {
                frames.push((event, data));
            }
        }
    }
    frames
}

/// 构造指向 mock 上游的 Responses 渠道 seed（其余沿用测试默认）。
fn responses_channel_seed(base: &str) -> common::Seed {
    let mut seed = common::test_seed(base);
    seed.channels[0].protocol = config::Protocol::OpenAiResponses;
    seed
}

/// Responses 入站 → Responses 渠道（同协议直通）：非流式，字节直通 + usage 计费。
#[tokio::test]
async fn responses_passthrough_forwards_and_bills() {
    let mut gw = TestGateway::start_with(responses_channel_seed).await;
    // 同协议（responses ↔ responses）且未命中别名 → 直通快路径。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "resp_01p", "object": "response", "status": "completed", "model": TEST_MODEL,
        "output": [
            { "id": "msg_1", "type": "message", "role": "assistant",
              "content": [ { "type": "output_text", "text": "直通", "annotations": [] } ] }
        ],
        "usage": { "input_tokens": 100, "output_tokens": 20, "total_tokens": 120 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/responses", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_text", "text": "hi" }] }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 直通：响应原样透传（含 object/status/model）。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["id"], "resp_01p");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["output"][0]["content"][0]["text"], "直通");

    // 出站请求体与下游一致（Responses 直通无 JSON 补丁）。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["model"], TEST_MODEL);
    assert_eq!(received[0]["input"][0]["role"], "user");

    // 计费：input 100 + output 20 = 100*2.5/1M + 20*10/1M = 250 + 200 = 450 微元。
    let row: (i64,) =
        sqlx::query_as("SELECT settled_usd_micros FROM token_balance WHERE token_key = ?")
            .bind(TEST_TOKEN_KEY)
            .fetch_one(&gw.pool)
            .await
            .expect("应能查询计费");
    assert_eq!(row.0, 450);
}

/// Responses 入站 → openai_chat 渠道：非流式跨协议转换。
#[tokio::test]
async fn responses_inbound_to_openai_chat_channel() {
    let mut gw = TestGateway::start().await; // 默认 openai_chat 渠道。
    // 上游以 openai chat.completion 响应，网关解码为 IR 再重编码为 Responses。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-123", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "Hello!" },
                      "logprobs": null, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/responses", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_text", "text": "hi" }] }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 下游收到 Responses response 对象。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["output"][0]["type"], "message");
    assert_eq!(body["output"][0]["role"], "assistant");
    assert_eq!(body["output"][0]["content"][0]["type"], "output_text");
    assert_eq!(body["output"][0]["content"][0]["text"], "Hello!");

    // 出站请求经 IR 重编码为 openai chat 格式。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["model"], TEST_MODEL);
    assert_eq!(received[0]["messages"][0]["content"], "hi");

    // 日志 inbound_protocol 落 openai_responses。
    let protocol: String = sqlx::query_scalar("SELECT inbound_protocol FROM request_log")
        .fetch_one(&gw.pool)
        .await
        .expect("应有日志");
    assert_eq!(protocol, "openai_responses");
}

/// Responses 入站 → Anthropic 渠道：非流式跨协议转换。
#[tokio::test]
async fn responses_inbound_to_anthropic_channel() {
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_01", "type": "message", "role": "assistant", "model": "claude-sonnet",
        "content": [{ "type": "text", "text": "Hello!" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 2 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/responses", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_text", "text": "hi" }] }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["object"], "response");
    assert_eq!(body["output"][0]["content"][0]["text"], "Hello!");

    // 出站请求经 IR 重编码为 Anthropic 格式。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["model"], TEST_MODEL);
    assert_eq!(received[0]["messages"][0]["content"], "hi");
}

/// openai_chat 入站 → Responses 渠道：非流式反向转换。
#[tokio::test]
async fn openai_inbound_to_responses_channel() {
    let mut gw = TestGateway::start_with(responses_channel_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "resp_01", "object": "response", "status": "completed", "model": TEST_MODEL,
        "output": [
            { "id": "msg_1", "type": "message", "role": "assistant",
              "content": [ { "type": "output_text", "text": "Hello!", "annotations": [] } ] }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 2, "total_tokens": 12 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 下游收到 openai chat.completion。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "Hello!");

    // 出站请求经 IR 重编码为 Responses 格式。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["model"], TEST_MODEL);
    assert_eq!(received[0]["input"][0]["role"], "user");
    assert_eq!(received[0]["input"][0]["content"][0]["text"], "hi");
}

/// openai_chat 入站 → Responses 渠道：上游 Responses 返回 reasoning，跨协议族丢弃并记 warning。
#[tokio::test]
async fn openai_inbound_drops_responses_reasoning_with_warning() {
    let mut gw = TestGateway::start_with(responses_channel_seed).await;
    // 上游 Responses 返回 reasoning + text；openai chat 无 reasoning 通道，丢弃记 warning。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "resp_01r", "object": "response", "status": "completed", "model": TEST_MODEL,
        "output": [
            { "id": "reason_1", "type": "reasoning", "encrypted_content": "enc_x",
              "summary": [ { "type": "summary_text", "text": "先算 925 ÷ 5" } ] },
            { "id": "msg_1", "type": "message", "role": "assistant",
              "content": [ { "type": "output_text", "text": "结果是 185", "annotations": [] } ] }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 }
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

/// Responses 入站 → Responses 渠道（命中别名走 IR 路径）：有状态特性 Out of Scope 显式 warning。
#[tokio::test]
async fn responses_stateful_features_warn_and_drop() {
    let mut gw = TestGateway::start_with(responses_channel_seed).await;
    // 请求别名短名 `fast`（命中别名 → IR 完整路径），携带有状态特性 store。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "resp_01s", "object": "response", "status": "completed", "model": "gpt-4o-mini",
        "output": [
            { "id": "msg_1", "type": "message", "role": "assistant",
              "content": [ { "type": "output_text", "text": "ok", "annotations": [] } ] }
        ],
        "usage": { "input_tokens": 3, "output_tokens": 1, "total_tokens": 4 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/responses", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "store": true,
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_text", "text": "hi" }] }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: Value = resp.json().await.expect("响应应可解析");
    // 出站请求不携带 store（丢弃），响应显式 warning。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert!(
        received[0].get("store").is_none(),
        "store 有状态特性应被丢弃，不出站"
    );
    assert_eq!(body["gateway"]["warnings"][0]["type"], "unsupported");
    assert_eq!(body["gateway"]["warnings"][0]["feature"], "store");
}

/// Responses 入站 → Responses 渠道：流式直通，usage 计费。
#[tokio::test]
async fn responses_passthrough_streaming_bills() {
    let mut gw = TestGateway::start_with(responses_channel_seed).await;
    // 同协议流式直通：上游 Responses SSE 流。
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "type": "response.created",
            "response": { "id": "resp_s", "model": TEST_MODEL }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "response.output_item.added", "output_index": 0,
            "item": { "type": "message", "id": "msg_1", "phase": "final_answer" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "response.output_text.delta", "item_id": "msg_1",
            "output_index": 0, "content_index": 0, "delta": "直通"
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "response.completed",
            "response": {
                "id": "resp_s", "object": "response", "status": "completed", "model": TEST_MODEL,
                "output": [], "usage": { "input_tokens": 50, "output_tokens": 5, "total_tokens": 55 }
            }
        }))
        .unwrap(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/responses", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "stream": true,
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_text", "text": "hi" }] }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 消费完流，确保结算已落库；直通转发 Responses 事件帧（mock 上游只发 data 行，
    // 事件名在 data 的 `type` 字段）。
    let frames = collect_sse_frames(resp).await;
    assert!(
        frames
            .iter()
            .any(|(_, data)| data["type"] == "response.output_text.delta"),
        "直通应转发 Responses 事件帧"
    );

    // 计费：input 50 + output 5 = 50*2.5/1M + 5*10/1M = 125 + 50 = 175 微元。
    let row: (i64,) =
        sqlx::query_as("SELECT settled_usd_micros FROM token_balance WHERE token_key = ?")
            .bind(TEST_TOKEN_KEY)
            .fetch_one(&gw.pool)
            .await
            .expect("应能查询计费");
    assert_eq!(row.0, 175);
}

/// Responses 入站 → openai_chat 渠道：流式跨协议，下游收到 Responses SSE 帧。
#[tokio::test]
async fn responses_inbound_to_openai_chat_channel_streaming() {
    let mut gw = TestGateway::start().await; // 默认 openai_chat 渠道。
    // 上游以 openai chat SSE 流响应。
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-9", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "Hel" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-9", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "content": "lo" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-9", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        }))
        .unwrap(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/responses", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "stream": true,
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_text", "text": "hi" }] }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let frames = collect_sse_frames(resp).await;
    // 下游逐帧收到 Responses 事件：首帧 response.created 携带真实 id（非占位），
    // 随后 output_text.delta 累积文本。
    assert_eq!(frames[0].1["type"], "response.created");
    assert_eq!(frames[0].0.as_deref(), Some("response.created"));
    assert_eq!(
        frames[0].1["response"]["id"], "chatcmpl-9",
        "response.created 应携带真实响应 id，而非占位 id"
    );
    let text: String = frames
        .iter()
        .filter(|(_, d)| d["type"] == "response.output_text.delta")
        .filter_map(|(_, d)| d["delta"].as_str())
        .collect();
    assert_eq!(text, "Hello");
    // 终止帧 response.completed 带 usage 与 finish_reason 语义。
    let completed = frames
        .iter()
        .find(|(_, d)| d["type"] == "response.completed")
        .expect("应有 response.completed 帧");
    assert_eq!(completed.1["response"]["status"], "completed");
    assert_eq!(completed.1["response"]["usage"]["output_tokens"], 2);
    // 无 [DONE] 哨兵：Responses 以 response.completed 收尾。
    assert!(
        frames.iter().all(|(_, d)| d["type"] != "[DONE]"),
        "Responses 流不应有 [DONE] 哨兵"
    );
}

/// Responses 入站 → openai_chat 渠道：流式 tool calling，下游 function_call 项被 `output_item.done` 收尾。
///
/// openai_chat 解码器不发 ToolInputEnd（累积器 flush 收尾），Responses 编码器须在
/// Finish 前把仍打开的 function_call 项关闭，否则 Responses 客户端永远收不到工具调用。
#[tokio::test]
async fn responses_inbound_tool_call_stream_closes_function_call() {
    let mut gw = TestGateway::start().await; // 默认 openai_chat 渠道。
    // 上游以 openai chat SSE 流响应（文本 + tool calling，两个 output item）。
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-9", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "Let me check." } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-9", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "tool_calls": [{
                "index": 0, "id": "call_1", "type": "function",
                "function": { "name": "get_weather", "arguments": "" }
            }] } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-9", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "tool_calls": [{
                "index": 0, "function": { "arguments": "{\"city\":\"SF\"}" }
            }] } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-9", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        }))
        .unwrap(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/responses", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "stream": true,
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_text", "text": "weather?" }] }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let frames = collect_sse_frames(resp).await;
    // 下游应收到 function_call 的 output_item.added 与 arguments delta。
    assert!(
        frames
            .iter()
            .any(|(_, d)| d["type"] == "response.output_item.added"
                && d["item"]["type"] == "function_call"),
        "应有 function_call 的 output_item.added"
    );
    assert!(
        frames
            .iter()
            .any(|(_, d)| d["type"] == "response.function_call_arguments.delta"),
        "应有 function_call_arguments.delta"
    );
    // 关键回归：function_call 项必须被 output_item.done 收尾（携带完整 arguments）。
    let done = frames
        .iter()
        .find(|(_, d)| {
            d["type"] == "response.output_item.done" && d["item"]["type"] == "function_call"
        })
        .expect("function_call 项应被 output_item.done 收尾");
    assert_eq!(done.1["item"]["call_id"], "call_1");
    assert_eq!(done.1["item"]["name"], "get_weather");
    assert_eq!(done.1["item"]["arguments"], r#"{"city":"SF"}"#);
    // 终止帧 response.completed 在 done 之后。
    let done_idx = frames
        .iter()
        .position(|(_, d)| {
            d["type"] == "response.output_item.done" && d["item"]["type"] == "function_call"
        })
        .expect("应有 done");
    let completed_idx = frames
        .iter()
        .position(|(_, d)| d["type"] == "response.completed")
        .expect("应有 completed");
    assert!(
        done_idx < completed_idx,
        "output_item.done 应在 response.completed 之前"
    );
    // 每个 output item 独占一个 output_index（下游 SDK 按它索引进行中的项，
    // 多 item 共用同一索引会互相覆盖）。
    let indexes: Vec<u64> = frames
        .iter()
        .filter(|(_, d)| d["type"] == "response.output_item.added")
        .map(|(_, d)| d["output_index"].as_u64().expect("output_index 应存在"))
        .collect();
    assert!(indexes.len() >= 2, "应有文本与工具两个 output item");
    let unique: std::collections::HashSet<u64> = indexes.iter().copied().collect();
    assert_eq!(
        unique.len(),
        indexes.len(),
        "output_index 应按 item 唯一，实际 {indexes:?}"
    );
}

/// Responses 入站错误：下游收到 Responses 错误格式。
#[tokio::test]
async fn responses_inbound_error_uses_responses_shape() {
    let gw = TestGateway::start().await;
    let client = reqwest::Client::new();
    // 无认证 → 401，Responses（OpenAI）错误格式。
    let resp = client
        .post(format!("{}/v1/responses", gw.base_url()))
        .json(&json!({ "model": TEST_MODEL, "input": [] }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = resp.json().await.expect("错误体应可解析");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(body["error"]["message"].is_string());
    assert!(gw.upstream.received().is_empty(), "未认证不应出站");
}

/// Responses 入站 → openai_chat 渠道：多模态跨协议转换。
///
/// 入站 input_image/input_file 解码为 IR 媒体 part，出站重编码为 OpenAI chat
/// `image_url`（data URL / 远程 URL）；非图片媒体（input_file 文档）在 chat 出站
/// 丢弃并记 warning。
#[tokio::test]
async fn responses_inbound_multimodal_to_openai_chat() {
    let mut gw = TestGateway::start().await; // 默认 openai_chat 渠道。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-mm", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "ok" },
                      "logprobs": null, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/responses", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "input": [{ "type": "message", "role": "user", "content": [
                { "type": "input_text", "text": "What's in this?" },
                { "type": "input_image",
                  "image_url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==" },
                { "type": "input_file",
                  "filename": "doc.pdf",
                  "file_data": "data:application/pdf;base64,JVBERi0xLjQK" }
            ] }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 出站请求：图片映射为 image_url（data URL），文档（非图片）在 chat 丢弃。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    let content = received[0]["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "image_url");
    assert_eq!(
        content[1]["image_url"]["url"],
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg=="
    );
    assert_eq!(content.len(), 2, "文档媒体应在 chat 出站丢弃");

    // 下游响应显式 warning：文档媒体丢弃。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["gateway"]["warnings"][0]["type"], "unsupported");
    assert_eq!(body["gateway"]["warnings"][0]["feature"], "media");
}

/// Responses 入站 → Responses 渠道（同协议直通）：多模态字节级原样送达上游。
#[tokio::test]
async fn responses_multimodal_passthrough_preserves_bytes() {
    let mut gw = TestGateway::start_with(responses_channel_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "resp_mm", "object": "response", "status": "completed", "model": TEST_MODEL,
        "output": [
            { "id": "msg_1", "type": "message", "role": "assistant",
              "content": [ { "type": "output_text", "text": "ok", "annotations": [] } ] }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 2, "total_tokens": 12 }
    })));

    let client = reqwest::Client::new();
    let body = json!({
        "model": TEST_MODEL,
        "input": [{ "type": "message", "role": "user", "content": [
            { "type": "input_text", "text": "What's in this?" },
            { "type": "input_image",
              "image_url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==" }
        ] }]
    });
    let resp = client
        .post(format!("{}/v1/responses", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&body)
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 直通：出站请求体与入站字节级一致（Responses 直通无 JSON 补丁）。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0], body, "同协议直通应字节级原样送达上游");
}

/// Responses 入站 → openai_chat 渠道：多模态流式跨协议转换。
///
/// 入站 input_image 解码为 IR 媒体 part，出站流式重编码为 openai chat `image_url`
/// （data URL）；请求侧媒体转换的 warning 随 `stream-start` 首帧下发。
#[tokio::test]
async fn responses_inbound_multimodal_to_openai_chat_streaming() {
    let mut gw = TestGateway::start().await; // 默认 openai_chat 渠道。
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-mm", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "ok" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-mm", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        }))
        .unwrap(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/responses", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "stream": true,
            "input": [{ "type": "message", "role": "user", "content": [
                { "type": "input_text", "text": "What's in this?" },
                { "type": "input_image",
                  "image_url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==" }
            ] }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 出站请求：图片映射为 image_url（data URL）。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    let content = received[0]["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "image_url");
    assert_eq!(
        content[1]["image_url"]["url"],
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg=="
    );

    // 下游流式收到 Responses 事件帧，文本累积完整；图片可表达，流首不含 warning。
    let frames = collect_sse_frames(resp).await;
    let text: String = frames
        .iter()
        .filter(|(_, d)| d["type"] == "response.output_text.delta")
        .filter_map(|(_, d)| d["delta"].as_str())
        .collect();
    assert_eq!(text, "ok");
    assert!(
        frames.iter().all(|(_, d)| d.get("gateway").is_none()),
        "图片可在 openai chat 表达，不应有 warning"
    );
}

/// 构造指向 mock 上游的 Anthropic 渠道 seed（其余沿用测试默认）。
fn anthropic_channel_seed(base: &str) -> common::Seed {
    let mut seed = common::test_seed(base);
    seed.channels[0].protocol = config::Protocol::AnthropicMessages;
    seed
}
