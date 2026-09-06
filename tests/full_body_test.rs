//! full_body 日志开关（#10）端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒。断言 `logging.full_body` 默认关闭（两列为 NULL）、
//! 开启时落「入站请求」与「入站响应」两份原始字节——入站响应指实际返回下游
//! 的字节（跨协议时为重编码结果，流式为下发 SSE 帧文本）。

mod common;

use std::time::Duration;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use futures_util::StreamExt;
use serde_json::{Value, json};

/// full_body 开启的测试 seed（其余沿用测试默认）。
fn full_body_seed(base: &str) -> common::Seed {
    let mut seed = common::test_seed(base);
    seed.settings
        .insert("full_body".to_string(), Value::Bool(true));
    seed
}

/// 读最近一条日志的两份 body 列。
async fn fetch_bodies(pool: &sqlx::SqlitePool) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    common::wait_for_request_persistence(pool).await;
    sqlx::query_as("SELECT request_body, response_body FROM request_log ORDER BY id DESC LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("应有请求日志")
}

/// 默认关闭：两列均为 NULL。
#[tokio::test]
async fn full_body_disabled_stores_null_bodies() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-1", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hello" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let (request_body, response_body) = fetch_bodies(&gw.pool).await;
    assert!(request_body.is_none(), "默认关闭时不应保存请求 body");
    assert!(response_body.is_none(), "默认关闭时不应保存响应 body");
}

/// 开启 + 同协议直通非流式：请求字节与下游收到的响应字节级一致。
#[tokio::test]
async fn full_body_passthrough_stores_inbound_request_and_response() {
    let mut gw = TestGateway::start_with(full_body_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-2", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "pong"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
    })));

    // 以原始字节发送，便于断言请求 body 字节级一致。
    let request_bytes = serde_json::to_vec(&json!({
        "model": TEST_MODEL,
        "messages": [{ "role": "user", "content": "ping" }]
    }))
    .expect("请求体应可序列化");

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .header("content-type", "application/json")
        .body(request_bytes.clone())
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let downstream_bytes = resp.bytes().await.expect("响应应可读");

    let (request_body, response_body) = fetch_bodies(&gw.pool).await;
    assert_eq!(
        request_body.as_deref(),
        Some(request_bytes.as_slice()),
        "入站请求应保存下游原始字节"
    );
    let response_body = response_body.expect("开启时应保存响应 body");
    assert_eq!(
        response_body, downstream_bytes,
        "入站响应应保存实际返回下游的字节"
    );
}

/// 开启 + 跨协议非流式：入站响应是重编码后的入站协议格式，而非上游原始响应。
#[tokio::test]
async fn full_body_cross_protocol_stores_reencoded_response() {
    let mut gw = TestGateway::start_with(full_body_seed).await; // 默认 openai_chat 渠道。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-3", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hello!"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
    })));

    let request_bytes = serde_json::to_vec(&json!({
        "model": TEST_MODEL,
        "max_tokens": 1024,
        "messages": [{ "role": "user", "content": "hi" }]
    }))
    .expect("请求体应可序列化");

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .header("content-type", "application/json")
        .body(request_bytes.clone())
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let downstream_bytes = resp.bytes().await.expect("响应应可读");

    let (request_body, response_body) = fetch_bodies(&gw.pool).await;
    assert_eq!(request_body.as_deref(), Some(request_bytes.as_slice()));
    let response_body = response_body.expect("开启时应保存响应 body");
    assert_eq!(response_body, downstream_bytes, "应与下游收到的字节一致");
    // 入站响应为 Anthropic Messages 格式（重编码结果），而非上游 openai 格式。
    let logged: Value = serde_json::from_slice(&response_body).expect("日志响应应可解析");
    assert_eq!(logged["type"], "message");
}

/// 开启 + 直通流式：入站响应为实际下发 SSE 帧文本（含 [DONE] 哨兵）。
#[tokio::test]
async fn full_body_streaming_records_forwarded_frames() {
    let mut gw = TestGateway::start_with(full_body_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        json!({
            "id": "chatcmpl-4", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hi"}, "finish_reason": null}]
        })
        .to_string(),
        json!({
            "id": "chatcmpl-4", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        })
        .to_string(),
        json!({
            "id": "chatcmpl-4", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
        })
        .to_string(),
    ]));

    let request_bytes = serde_json::to_vec(&json!({
        "model": TEST_MODEL,
        "stream": true,
        "messages": [{ "role": "user", "content": "ping" }]
    }))
    .expect("请求体应可序列化");

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .header("content-type", "application/json")
        .body(request_bytes.clone())
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 消费完整 SSE 流，确认 [DONE] 已下发（此时结算与日志必先于哨兵完成）。
    let mut stream_text = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        stream_text.push_str(&String::from_utf8_lossy(&chunk.expect("流应可读")));
    }
    assert!(stream_text.contains("data: [DONE]"), "应以 [DONE] 收尾");

    // 日志在哨兵前落库，但写入与客户端读完存在微小窗口，轮询等待。
    let mut bodies = None;
    for _ in 0..100 {
        let (request_body, response_body) = fetch_bodies(&gw.pool).await;
        if response_body.is_some() {
            bodies = Some((request_body, response_body));
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (request_body, response_body) = bodies.expect("流式日志应带响应 body");
    assert_eq!(request_body.as_deref(), Some(request_bytes.as_slice()));
    let response_text = String::from_utf8(response_body.expect("开启时应保存响应 body"))
        .expect("流式响应字节应为合法 UTF-8");
    assert!(response_text.contains("\"delta\""), "应含转发帧内容");
    assert!(
        response_text.ends_with("data: [DONE]\n\n"),
        "应以哨兵帧收尾，实际结尾: {:?}",
        &response_text[response_text.len().saturating_sub(32)..]
    );
}

/// raw chunk 直搬开启 full_body 时，日志保存成功下发的原始响应字节。
#[tokio::test]
async fn full_body_streaming_records_raw_cross_chunk_bytes() {
    let mut gw = TestGateway::start_with(full_body_seed).await;
    let upstream = b": keep-alive\r\nevent: custom\r\ndata:{\"type\":\"chunk\"}\r\n\r\ndata: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n";
    gw.upstream.set_behavior(UpstreamBehavior::RawSse(vec![
        upstream[..13].to_vec(),
        upstream[13..57].to_vec(),
        upstream[57..].to_vec(),
    ]));

    let resp = reqwest::Client::new()
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
    let downstream = resp.bytes().await.expect("响应流应可读");
    let expected = [upstream.as_slice(), b"data: [DONE]\n\n"].concat();
    assert_eq!(downstream.as_ref(), expected);

    let mut response_body = None;
    for _ in 0..100 {
        response_body = fetch_bodies(&gw.pool).await.1;
        if response_body.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(response_body.as_deref(), Some(expected.as_slice()));
}
