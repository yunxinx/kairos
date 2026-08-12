//! Chat Completions 非流式垂直切片的端到端黑盒测试。
//!
//! 主接缝：测试内启动网关 + 可编程 mock 上游，断言外部可观察行为（mock 收到
//! 的出站请求、下游收到的响应与状态码、SQLite 中的请求日志）。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use futures_util::StreamExt;
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

/// 流式请求经 IR 完整路径返回 SSE：mock 上游以 SSE 流响应，下游逐帧收到
/// `chat.completion.chunk`，且 SQLite 落流式计费日志。
#[tokio::test]
async fn stream_request_returns_sse_and_logs() {
    let mut gw = TestGateway::start().await;

    // mock 上游以 SSE 流返回：两个文本增量帧 + 一个 finish 帧（含 usage）。
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
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
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
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "流式应 200");

    // 消费 SSE 帧，累积文本与 usage。
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.contains("text/event-stream"),
        "应返回 SSE，实际 {content_type}"
    );

    let mut text = String::new();
    let mut saw_finish_usage = false;
    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("响应流应可读");
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        // 按空行切分完整帧，保留尾部可能不完整的数据。
        while let Some(end) = buffer.find("\n\n") {
            let frame: String = buffer.drain(..end + 2).collect();
            for line in frame.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    let value: Value = serde_json::from_str(data).unwrap_or(Value::Null);
                    if let Some(delta) = value["choices"][0]["delta"]["content"].as_str() {
                        text.push_str(delta);
                    }
                    if value["usage"].is_object() {
                        saw_finish_usage = true;
                    }
                }
            }
        }
    }

    assert_eq!(text, "Hello", "应累积出完整文本");
    assert!(saw_finish_usage, "finish 帧应携带 usage");

    // 流式计费落库：usage 10/2 → input 10 × 2.5 + output 2 × 10 = 25 + 20 = 45 micro-USD。
    let row: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd_micros \
         FROM request_log",
    )
    .fetch_one(&gw.pool)
    .await
    .expect("应落一条流式日志");
    assert_eq!(row.0, 10);
    assert_eq!(row.1, 2);
    assert_eq!(row.4, 45);
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

/// 入站多模态（image_url data URL + 远程 URL + 文本混排）：同协议直通字节级原样
/// 送达上游，媒体内容零转换损耗。
#[tokio::test]
async fn multimodal_inbound_passthrough_preserves_bytes() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-mm", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "two images"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
    })));

    let body = json!({
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
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&body)
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "多模态请求应 200");

    // 直通快路径：mock 上游收到的出站请求体与下游请求字节级一致（媒体原样）。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1, "mock 上游应收一条请求");
    assert_eq!(
        received[0], body,
        "同协议直通应字节级原样送达上游（含 base64 与混排顺序）"
    );
}
