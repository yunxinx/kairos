//! 端到端冒烟测试：验证 SQLite 迁移与可编程 mock 上游基建。
//!
//! 原来的透传 relay 端点已被 #03 的 Chat Completions 真实 handler 取代，
//! 其端到端覆盖由 `chat_completions_test.rs` 承接。

mod common;

use common::TestGateway;
use futures_util::StreamExt;
use serde_json::json;

/// 冒烟表也应建出，验证迁移机制可用。
#[tokio::test]
async fn migration_builds_smoke_probe_table() {
    let gw = TestGateway::start().await;

    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM smoke_probe")
        .fetch_one(&gw.pool)
        .await
        .expect("冒烟表应存在且可查询");
    assert_eq!(row.0, 0, "冒烟表初始为空");
}

/// 可编程 mock 上游的 JSON 响应行为。
#[tokio::test]
async fn mock_upstream_returns_json() {
    let mut upstream = common::MockUpstream::start().await;
    upstream.set_behavior(common::UpstreamBehavior::Json(
        serde_json::json!({ "ok": true }),
    ));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/chat/completions", upstream.base_url()))
        .json(&json!({ "model": "gpt-4o" }))
        .send()
        .await
        .expect("应能请求 mock 上游");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("应能解析 JSON");
    assert_eq!(body["ok"], true);
}

/// 可编程 mock 上游的 429 行为（可重试），供后续路由/failover 票复用。
#[tokio::test]
async fn mock_upstream_returns_429() {
    let mut upstream = common::MockUpstream::start().await;
    upstream.set_behavior(common::UpstreamBehavior::Status429);

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/chat/completions", upstream.base_url()))
        .json(&json!({ "model": "gpt-4o" }))
        .send()
        .await
        .expect("应能请求 mock 上游");
    assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
}

/// 可编程 mock 上游的 5xx 行为（可重试），供后续 failover 票复用。
#[tokio::test]
async fn mock_upstream_returns_5xx() {
    let mut upstream = common::MockUpstream::start().await;
    upstream.set_behavior(common::UpstreamBehavior::Status5xx(502));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/chat/completions", upstream.base_url()))
        .json(&json!({ "model": "gpt-4o" }))
        .send()
        .await
        .expect("应能请求 mock 上游");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_GATEWAY);
}

/// 可编程 mock 上游的断连行为：发送部分字节后结束连接。
#[tokio::test]
async fn mock_upstream_disconnects_mid_stream() {
    let mut upstream = common::MockUpstream::start().await;
    upstream.set_behavior(common::UpstreamBehavior::Disconnect);

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/chat/completions", upstream.base_url()))
        .json(&json!({ "model": "gpt-4o" }))
        .send()
        .await
        .expect("应能请求 mock 上游");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let mut raw = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("分片应可读");
        raw.extend_from_slice(&chunk);
    }
    let body = String::from_utf8_lossy(&raw);
    assert!(body.contains("partial"), "上游应下发部分内容后断连");
}
