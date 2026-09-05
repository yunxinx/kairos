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
    sqlx::query(
        "UPDATE user_balance SET balance_usd_micros = 10_000_000_000 \
         WHERE user_id = (SELECT user_id FROM tokens WHERE token_key = ?)",
    )
    .bind(common::fingerprint(TEST_TOKEN_KEY))
    .execute(&gw.pool)
    .await
    .expect("应能为大请求准备充足余额");
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

/// `Content-Length` 超过上限时在读 body 之前 413，调用方不必真的送齐声明长度。
#[tokio::test]
async fn oversized_content_length_is_rejected_before_body() {
    let gw = TestGateway::start_with(tiny_body_seed).await;
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\n\
         Host: 127.0.0.1:{}\r\n\
         Authorization: Bearer {TEST_TOKEN_KEY}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: 1000000\r\n\
         Connection: close\r\n\
         \r\n",
        gw.addr.port()
    );
    let response = raw_http_exchange(gw.addr, request.into_bytes()).await;
    assert!(
        response.contains("413"),
        "声明超大 Content-Length 应 413，实际 {response:?}"
    );
    assert!(
        response.contains("请求体超过上限"),
        "应为入站协议超限错误，实际 {response:?}"
    );
    assert!(gw.upstream.received().is_empty(), "超限不应出站");
}

/// chunked 入站按实际读取字节封顶，超过 `max_request_bytes` 即 413。
#[tokio::test]
async fn oversized_chunked_body_returns_413() {
    let gw = TestGateway::start_with(tiny_body_seed).await;
    let chunk = "x".repeat(32);
    let mut body = String::new();
    for _ in 0..8 {
        body.push_str("20\r\n");
        body.push_str(&chunk);
        body.push_str("\r\n");
    }
    body.push_str("0\r\n\r\n");
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\n\
         Host: 127.0.0.1:{}\r\n\
         Authorization: Bearer {TEST_TOKEN_KEY}\r\n\
         Content-Type: application/json\r\n\
         Transfer-Encoding: chunked\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        gw.addr.port()
    );
    let response = raw_http_exchange(gw.addr, request.into_bytes()).await;
    assert!(
        response.contains("413"),
        "chunked 超限应 413，实际 {response:?}"
    );
    assert!(
        response.contains("请求体超过上限"),
        "应为入站协议超限错误，实际 {response:?}"
    );
    assert!(gw.upstream.received().is_empty(), "超限不应出站");
}

/// 向网关写原始 HTTP 并读响应；用于断言「未送齐声明 body 也能 413」。
///
/// 必须在 `spawn_blocking` 里做阻塞 IO：`#[tokio::test]` 默认单线程，
/// 若在同一线程阻塞读，网关任务无法推进。
async fn raw_http_exchange(addr: std::net::SocketAddr, request: Vec<u8>) -> String {
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        use std::time::Duration;

        let mut stream = TcpStream::connect(addr).expect("应能连接网关");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("应能设置读超时");
        stream.write_all(&request).expect("应能写请求");
        stream.flush().expect("应能刷新请求");
        let mut buf = Vec::new();
        match stream.read_to_end(&mut buf) {
            Ok(_) => {}
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(err) => panic!("读响应失败: {err}"),
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
    .await
    .expect("原始 HTTP 交换不应被取消")
}
