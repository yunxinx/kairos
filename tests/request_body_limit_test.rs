//! 入站请求体上限（运行时开关）端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒。断言 `max_request_bytes` 开关控制入站请求体上限：
//! 超限返回 413 + 入站协议错误格式（且不出站）；缺省（未配置开关）用默认值，
//! 常规请求不受影响。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use serde_json::{Value, json};

/// 设置一个很小的 `max_request_bytes` 的 seed（其余沿用测试默认）。
fn tiny_body_seed(base: &str) -> common::Seed {
    let mut seed = common::test_seed(base);
    seed.settings
        .insert("max_request_bytes".to_string(), Value::from(100u64));
    seed
}

/// 超限请求返回 413 + 入站协议错误格式，且不出站。
#[tokio::test]
async fn oversized_request_returns_413() {
    let gw = TestGateway::start_with(tiny_body_seed).await;
    // 构造一个远超 100 字节的请求体。
    let big_content = "x".repeat(2000);
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": big_content }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        "超限应返回 413"
    );
    let body: Value = resp.json().await.expect("413 响应应可解析");
    assert!(body["error"]["message"].is_string(), "应为入站协议错误格式");
    assert!(gw.upstream.received().is_empty(), "超限不应出站");
}

/// 缺省（未配置开关）用默认值，axum 默认的 2MB 上限已被禁用：超过 2MB 的常规
/// 请求仍到达处理器，由运行时 `max_request_bytes`（默认 100MB）裁决，而非被 axum
/// 提前以通用 413 拒绝。
#[tokio::test]
async fn large_body_over_axum_default_is_allowed() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-2m", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })));
    // 3MB 请求体 > axum 默认 2MB 上限：应放行并返回入站协议成功响应。
    let big_content = "y".repeat(3 * 1024 * 1024);
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": big_content }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "超过 axum 默认 2MB 的请求应由处理器按运行时上限裁决，而非被 axum 拒绝"
    );
}

/// 缺省（未配置开关）用默认值，常规请求不受影响。
#[tokio::test]
async fn default_limit_allows_normal_requests() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-1", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
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
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "缺省上限应放行常规请求"
    );
}
