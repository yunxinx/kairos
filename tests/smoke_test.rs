//! 端到端冒烟测试：axum SSE 端点 → reqwest 流式出站（连 mock 上游）→ sqlx 落库。
//!
//! 验证技术栈全链路，并沉淀后续票复用的测试基建。

mod common;

use common::{TestGateway, UpstreamBehavior};
use futures_util::StreamExt;
use serde_json::json;

/// axum SSE 端点 → reqwest 流式转发 mock 上游 → sqlx 写入一行，断言全绿。
#[tokio::test]
async fn smoke_axum_sse_reqwest_stream_sqlx_persist() {
    let mut gw = TestGateway::start().await;

    // mock 上游返回三段 SSE 文本。
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        "hello".into(),
        " ".into(),
        "world".into(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .json(&json!({ "model": "gpt-4o", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .expect("下游请求应能到达网关");

    assert_eq!(resp.status(), reqwest::StatusCode::OK, "网关应透传 200");

    // 读取 SSE 事件流，累积原始字节后解析。
    let mut raw = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("SSE 分片应可读");
        raw.extend_from_slice(&chunk);
    }
    let body = String::from_utf8_lossy(&raw);

    // 按空行分隔事件，提取每个事件的 `data:` 载荷，去掉 SSE 字段与内容之间的单个空格。
    let collected: String = body
        .split("\n\n")
        .filter_map(|event| {
            event
                .lines()
                .find_map(|line| line.strip_prefix("data:"))
                .map(|data| data.strip_prefix(' ').unwrap_or(data))
        })
        .collect();
    assert_eq!(collected, "hello world", "下游应收齐全部 SSE 帧");

    // 断言 mock 上游收到一条出站请求，且 body 与入站一致。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1, "mock 上游应收一条请求");
    assert_eq!(received[0]["model"], "gpt-4o", "出站请求应携带模型字段");

    // 断言 SQLite 落了一行冒烟记录（流结束后写入）。
    let rows = sqlx::query_as::<_, (String,)>("SELECT note FROM smoke_probe")
        .fetch_all(&gw.pool)
        .await
        .expect("应能查询冒烟记录");

    assert_eq!(rows.len(), 1, "应恰好落一行冒烟记录");
    assert_eq!(rows[0].0, "relayed status 200", "冒烟记录应反映上游状态码");
}

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
