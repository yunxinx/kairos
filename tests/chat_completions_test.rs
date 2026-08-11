//! Chat Completions 非流式垂直切片的端到端黑盒测试。
//!
//! 主接缝：测试内启动网关 + 可编程 mock 上游，断言外部可观察行为（mock 收到
//! 的出站请求、下游收到的响应与状态码、SQLite 中的请求日志）。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use serde_json::{Value, json};

/// 有效令牌 + mock 上游成功：断言出站请求体、下游响应、SQLite 日志。
#[tokio::test]
async fn valid_token_routes_request_and_logs() {
    let mut gw = TestGateway::start().await;

    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hello!" },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
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
        .expect("下游请求应能到达网关");

    assert_eq!(resp.status(), reqwest::StatusCode::OK, "有效请求应 200");

    // 下游收到入站协议格式的响应。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "Hello!");

    // mock 上游收到一条出站请求，且为 IR 重编码后的 Chat Completions 格式。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1, "mock 上游应收一条请求");
    assert_eq!(received[0]["model"], TEST_MODEL);
    assert_eq!(received[0]["messages"][0]["role"], "user");
    assert_eq!(received[0]["messages"][0]["content"], "hi");

    // SQLite 落一条请求日志。
    let rows = sqlx::query_as::<_, (String, String, String, String, i64, i64)>(
        "SELECT token_name, inbound_protocol, model, channel, status_code, latency_ms \
         FROM request_log",
    )
    .fetch_all(&gw.pool)
    .await
    .expect("应能查询请求日志");
    assert_eq!(rows.len(), 1, "应恰好落一条日志");
    assert_eq!(rows[0].0, "dev");
    assert_eq!(rows[0].1, "openai_chat");
    assert_eq!(rows[0].2, TEST_MODEL);
    assert_eq!(rows[0].3, "test-channel");
    assert_eq!(rows[0].4, 200);
    assert!(rows[0].5 >= 0);
}

/// 缺失/无效令牌 key 返回 401 + OpenAI 错误格式。
#[tokio::test]
async fn missing_or_invalid_token_is_401() {
    let gw = TestGateway::start().await;

    let client = reqwest::Client::new();
    let base = format!("{}/v1/chat/completions", gw.base_url());
    let body = json!({ "model": TEST_MODEL, "messages": [{ "role": "user", "content": "hi" }] });

    // 无认证头 → 401。
    let resp = client
        .post(&base)
        .json(&body)
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let resp_body: Value = resp.json().await.expect("401 响应应可解析");
    assert!(
        resp_body["error"]["message"].is_string(),
        "错误体应为 OpenAI 格式"
    );
    assert!(gw.upstream.received().is_empty(), "未认证不应出站");

    // 无效 key → 401，且两种头都覆盖。
    let resp = client
        .post(&base)
        .bearer_auth("sk-wrong")
        .json(&body)
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// 入站认证同时接受 `Authorization: Bearer` 与 `x-api-key` 两种头。
#[tokio::test]
async fn accepts_x_api_key_header() {
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
        .header("x-api-key", TEST_TOKEN_KEY)
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
        "x-api-key 应通过认证"
    );
}

/// 模型无任何候选渠道：准入时拒绝，503 + OpenAI 错误格式 + 可读消息。
#[tokio::test]
async fn model_without_channel_is_503() {
    let gw = TestGateway::start().await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "no-such-model",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "无渠道应 503"
    );
    let body: Value = resp.json().await.expect("503 响应应可解析");
    assert!(
        body["error"]["message"].is_string(),
        "错误体应为 OpenAI 格式"
    );
    let msg = body["error"]["message"].as_str().expect("消息应为字符串");
    assert!(msg.contains("no-such-model"), "消息应含模型名，实际 {msg}");
    assert!(gw.upstream.received().is_empty(), "无渠道不应出站");
}

/// 上游返回错误：状态码原样透传 + OpenAI 错误格式。
#[tokio::test]
async fn upstream_error_status_is_passthrough() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Status429);

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
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "429 应原样透传"
    );

    // 日志记录的是上游状态码。
    let rows = sqlx::query_as::<_, (i64,)>("SELECT status_code FROM request_log")
        .fetch_all(&gw.pool)
        .await
        .expect("应能查询请求日志");
    assert!(!rows.is_empty(), "应落一条日志");
    assert_eq!(rows[0].0, 429);
}

/// 流式请求在非流式范围内被拒绝（400）。
#[tokio::test]
async fn stream_request_is_rejected_400() {
    let gw = TestGateway::start().await;

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

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "流式应 400"
    );
}

/// 别名匹配查短名（key）：短名命中渠道，别名指向的上游真实名不命中。
#[tokio::test]
async fn alias_key_matches_but_alias_target_does_not() {
    let mut gw = TestGateway::start().await;
    let ok_body = json!({
        "id": "chatcmpl-alias", "object": "chat.completion", "model": "gpt-4o-mini",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    });

    let client = reqwest::Client::new();

    // 别名短名 fast 命中渠道。
    gw.upstream.set_behavior(UpstreamBehavior::Json(ok_body));
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "别名短名应命中渠道");

    // 别名指向的上游真实名 gpt-4o-mini 不参与候选匹配。
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "gpt-4o-mini",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "上游真实名不应绕过别名命中渠道"
    );
}
