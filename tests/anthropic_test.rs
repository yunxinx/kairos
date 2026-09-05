//! Anthropic Messages 适配器（#08）端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒，断言外部可观察行为——mock 上游收到的出站请求体、
//! 下游收到的响应、SQLite 中的计费与日志。覆盖：OpenAI chat 入站调 Anthropic 渠道
//! （非流式/流式/tool calling）、Anthropic 入站调 OpenAI 渠道、thinking signature
//! 同协议族无损回传、跨协议族 reasoning 丢弃记 warning。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior, collect_sse_frames};
use kairos::config;
use serde_json::{Value, json};

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

    // 下游收到 openai chat.completion（tool_use → tool_calls），出站请求经 IR
    // 重编码为 Anthropic 格式。两侧 wire 形状整体快照：逐字段断言只覆盖得到
    // content/tool_calls/finish_reason，usage 换算、max_tokens 补默认、
    // stop_reason 映射这些同样属于协议契约的部分会漏在断言之外。
    let body: Value = resp.json().await.expect("响应应可解析");
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1, "mock 上游应收一条请求");
    insta::assert_json_snapshot!(json!({
        "downstream_response": body,
        "upstream_request": received[0],
    }));

    // 计费：input 25 + output 12。
    // 费用 = 25*2.5/1M + 12*10/1M = 62 + 120 = 182 微元。
    common::wait_for_request_persistence(&gw.pool).await;
    let row: (i64, i64) = sqlx::query_as(
        "SELECT settled_usd_micros, input_tokens FROM token_balance JOIN request_log \
         ON token_balance.token_key = request_log.token_key WHERE token_balance.token_key = ?",
    )
    .bind(common::fingerprint(TEST_TOKEN_KEY))
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

    // 下游收到 Anthropic Messages 格式，出站请求经 IR 重编码为 openai chat 格式。
    let body: Value = resp.json().await.expect("响应应可解析");
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    insta::assert_json_snapshot!(json!({
        "downstream_response": body,
        "upstream_request": received[0],
    }));

    // 日志 inbound_protocol 落 anthropic_messages。
    common::wait_for_request_persistence(&gw.pool).await;
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
    common::wait_for_request_persistence(&gw.pool).await;
    let row: (i64, i64) = sqlx::query_as(
        "SELECT settled_usd_micros, input_tokens FROM token_balance JOIN request_log \
         ON token_balance.token_key = request_log.token_key WHERE token_balance.token_key = ?",
    )
    .bind(common::fingerprint(TEST_TOKEN_KEY))
    .fetch_one(&gw.pool)
    .await
    .expect("应能查询计费");
    assert_eq!(row.0, 450);
    assert_eq!(row.1, 100);
}

/// Anthropic 直通 + 1h 分档计费：usage 的 cache_creation 明细按
/// 1h/5m 双速率拆分计价，日志记录 1h 明细与价格快照。
#[tokio::test]
async fn anthropic_passthrough_bills_1h_cache_write_tier() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = anthropic_channel_seed(base);
        seed.prices[0].cache_write_1h_micros = Some(20_000_000);
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_01t", "type": "message", "role": "assistant", "model": TEST_MODEL,
        "content": [{ "type": "text", "text": "直通" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": {
            "input_tokens": 100, "output_tokens": 20,
            "cache_creation_input_tokens": 300, "cache_read_input_tokens": 40,
            "cache_creation": { "ephemeral_5m_input_tokens": 100, "ephemeral_1h_input_tokens": 200 }
        }
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

    // 1h 写入 200 × 20.0 + 5m 写入 100 × 10.0 + input 100 × 2.5 + output 20 × 10.0
    // + read 40 × 1.25（micro-USD / 1M tokens）= 4000 + 1000 + 250 + 200 + 50 = 5500。
    common::wait_for_request_persistence(&gw.pool).await;
    let row: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT settled_usd_micros, cache_write_1h_tokens, cache_write_1h_price_usd_micros, \
         cache_write_tokens FROM token_balance JOIN request_log \
         ON token_balance.token_key = request_log.token_key WHERE token_balance.token_key = ?",
    )
    .bind(common::fingerprint(TEST_TOKEN_KEY))
    .fetch_one(&gw.pool)
    .await
    .expect("应能查询计费");
    assert_eq!(row.0, 5500);
    assert_eq!(row.1, 200);
    assert_eq!(row.2, 20_000_000);
    assert_eq!(row.3, 300);
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
    assert_eq!(frames[0].data["object"], "chat.completion.chunk");
    let text: String = frames
        .iter()
        .map(|f| {
            f.data["choices"][0]["delta"]["content"]
                .as_str()
                .unwrap_or("")
        })
        .collect();
    assert_eq!(text, "Hello");
    // finish 帧带 finish_reason 与 usage。
    let finish = frames
        .iter()
        .find(|f| f.data["choices"][0]["finish_reason"] == "stop")
        .expect("应有 finish 帧");
    assert_eq!(finish.data["usage"]["completion_tokens"], 2);

    // 计费：input 10 + output 2 = 10*2.5/1M + 2*10/1M = 25 + 20 = 45 微元。
    common::wait_for_request_persistence(&gw.pool).await;
    let row: (i64,) =
        sqlx::query_as("SELECT settled_usd_micros FROM token_balance WHERE token_key = ?")
            .bind(common::fingerprint(TEST_TOKEN_KEY))
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
    // 同协议流式直通；两处 usage 都跨原始块边界，验证旁路缓冲独立组帧。
    let raw = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{",
        "\"id\":\"msg_01\",\"usage\":{\"input_tokens\":50,\"output_tokens\":0}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,",
        "\"delta\":{\"type\":\"text_delta\",\"text\":\"直通\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",",
        "\"delta\":{\"stop_reason\":\"end_turn\"},",
        "\"usage\":{\"input_tokens\":50,\"output_tokens\":5}}\n\n"
    )
    .as_bytes();
    gw.upstream.set_behavior(UpstreamBehavior::RawSse(vec![
        raw[..73].to_vec(),
        raw[73..raw.len() - 37].to_vec(),
        raw[raw.len() - 37..].to_vec(),
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
        frames
            .iter()
            .any(|f| f.data["type"] == "content_block_delta"),
        "直通应转发 Anthropic 事件帧"
    );

    // 计费：input 50 + output 5 = 50*2.5/1M + 5*10/1M = 125 + 50 = 175 微元
    // （message_start 的 input 与 message_delta 的 output 逐分量 max 合并）。
    common::wait_for_request_persistence(&gw.pool).await;
    let row: (i64,) =
        sqlx::query_as("SELECT settled_usd_micros FROM token_balance WHERE token_key = ?")
            .bind(common::fingerprint(TEST_TOKEN_KEY))
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

/// Anthropic 入站 → OpenAI chat 渠道：多模态跨协议转换。
///
/// 入站 image/document content block 解码为 IR 媒体 part，出站重编码为 OpenAI
/// chat `image_url`（data URL / 远程 URL）；非图片媒体（文档）在 chat 出站丢弃
/// 并记 warning。
#[tokio::test]
async fn anthropic_inbound_multimodal_to_openai_chat() {
    let mut gw = TestGateway::start().await; // 默认 openai_chat 渠道。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-mm", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "ok" },
                      "logprobs": null, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "What's in this?" },
                { "type": "image",
                  "source": { "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgoAAAANSUhEUg==" } },
                { "type": "document",
                  "source": { "type": "base64", "media_type": "application/pdf", "data": "JVBERi0xLjQK" } }
            ] }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 出站请求：图片映射为 image_url（data URL），文档（非图片）在 chat 丢弃；
    // 下游响应带丢弃 warning。快照同时锁住保留部分的顺序与丢弃部分的缺席，
    // 「文档没了」和「图片还在原位」是同一条断言的两面。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    let body: Value = resp.json().await.expect("响应应可解析");
    insta::assert_json_snapshot!(json!({
        "upstream_request_content": received[0]["messages"][0]["content"],
        "downstream_warnings": body["gateway"]["warnings"],
    }));
}

/// Anthropic 入站 → Anthropic 渠道（同协议直通）：多模态字节级原样送达上游。
#[tokio::test]
async fn anthropic_multimodal_passthrough_preserves_bytes() {
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_mm", "type": "message", "role": "assistant", "model": TEST_MODEL,
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 2 }
    })));

    let client = reqwest::Client::new();
    let body = json!({
        "model": TEST_MODEL,
        "max_tokens": 1024,
        "messages": [{ "role": "user", "content": [
            { "type": "text", "text": "What's in this?" },
            { "type": "image",
              "source": { "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgoAAAANSUhEUg==" } }
        ] }]
    });
    let resp = client
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .json(&body)
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 直通：出站请求体与入站字节级一致（Anthropic 直通无 JSON 补丁）。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0], body, "同协议直通应字节级原样送达上游");
}

/// Anthropic 入站 → openai_chat 渠道：多模态流式跨协议转换。
///
/// 入站 image/document content block 解码为 IR 媒体 part，出站流式重编码为
/// openai chat `image_url`（data URL）；文档在 chat 出站丢弃，其 warning 随
/// `stream-start` 首帧（`ping` 事件）下发。
#[tokio::test]
async fn anthropic_inbound_multimodal_to_openai_chat_streaming() {
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
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "max_tokens": 1024,
            "stream": true,
            "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "What's in this?" },
                { "type": "image",
                  "source": { "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgoAAAANSUhEUg==" } },
                { "type": "document",
                  "source": { "type": "base64", "media_type": "application/pdf", "data": "JVBERi0xLjQK" } }
            ] }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 出站请求：图片映射为 image_url（data URL），文档在 chat 出站丢弃。
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

    // 下游流式收到 Anthropic 事件帧（入站协议为 anthropic），文本累积完整。
    let frames = collect_sse_frames(resp).await;
    let text: String = frames
        .iter()
        .filter(|f| {
            f.data["type"] == "content_block_delta" && f.data["delta"]["type"] == "text_delta"
        })
        .filter_map(|f| f.data["delta"]["text"].as_str())
        .collect();
    assert_eq!(text, "ok");
    // 文档丢弃的 warning 随流首 ping 帧下发（Anthropic 无标准 warnings 通道）。
    let warning_frame = frames
        .iter()
        .find(|f| f.data.get("warnings").is_some())
        .expect("流首应有携带 warnings 的 ping 帧");
    assert_eq!(warning_frame.data["warnings"][0]["type"], "unsupported");
    assert_eq!(warning_frame.data["warnings"][0]["feature"], "media");
}

/// OpenAI chat 入站 → Anthropic 渠道：多模态跨协议映射（data URL ↔ base64 source）。
///
/// 入站 `image_url`（data URL + 远程 URL）与文本混排，出站编码为 Anthropic
/// `image` content block（base64 source / URL source），顺序与语义保持。
#[tokio::test]
async fn openai_inbound_multimodal_to_anthropic() {
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_mm", "type": "message", "role": "assistant", "model": TEST_MODEL,
        "content": [{ "type": "text", "text": "two images" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 2 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "What's in these images?" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==" } },
                    { "type": "text", "text": "and" },
                    { "type": "image_url", "image_url": { "url": "https://example.com/image.png" } }
                ]
            }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 文本与两种 image source 的混排：快照锁住 4 个 block 的顺序与各自的
    // source 形状（base64 三元组 / url 二元组）。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    insta::assert_json_snapshot!(received[0]["messages"][0]["content"]);
}
