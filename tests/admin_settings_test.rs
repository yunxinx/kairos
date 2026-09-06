//! 设置管理面：读写即时生效、校验与完整 body 日志开关。

mod common;

use base64::Engine as _;
use common::admin::{
    admin_get, admin_json, admin_put, channel_id_by_name, chat_request, make_successful_request,
};
use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway};
use serde_json::{Value, json};

/// 设置读写：缺省读回、写后返回变更后设置、body 上限即时生效（新上限立刻拦截超限请求）。
#[tokio::test]
async fn settings_write_takes_effect_immediately() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 缺省设置：full_body 关闭、body 上限为正。
    let resp = client
        .get(format!("{admin}/settings"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可读设置");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let settings: Value = resp.json().await.expect("设置应可解析");
    assert_eq!(settings["full_body"], false);
    assert!(settings["max_request_bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        settings["max_response_bytes"],
        kairos::store::resources::DEFAULT_MAX_RESPONSE_BYTES
    );
    assert_eq!(settings["log_body_max_bytes"], 1024 * 1024);
    assert_eq!(settings["auth_throttle_max_failures"], 30);
    assert_eq!(settings["auth_throttle_window_secs"], 60);
    assert_eq!(settings["sse_reassembly_max_bytes"], 8 * 1024 * 1024);
    assert_eq!(settings["retry_backoff_ms"], 200);
    assert_eq!(settings["retry_backoff_cap_ms"], 5_000);
    assert_eq!(settings["retry_after_cap_secs"], 60);
    assert_eq!(settings["rate_limit_rpm"], 0);

    // 写设置：body 上限压到 100 字节，返回变更后设置。
    let resp = admin_put(
        &gw,
        "/settings",
        json!({ "full_body": false, "max_request_bytes": 100 }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let settings: Value = resp.json().await.expect("设置应可解析");
    assert_eq!(settings["max_request_bytes"], 100);

    // 新上限立即生效：超限请求被 413 拦截且不出站。
    let big = "x".repeat(2000);
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({ "model": TEST_MODEL, "messages": [{ "role": "user", "content": big }] }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        "新 body 上限应立即拦截超限请求"
    );
    assert!(gw.upstream.received().is_empty(), "超限不应出站");
}

/// 网关保护类设置读写往返；非法阈值返回 400。
#[tokio::test]
async fn settings_gateway_knobs_roundtrip_and_validation() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let resp = admin_put(
        &gw,
        "/settings",
        json!({
            "full_body": false,
            "max_request_bytes": 1_000_000,
            "auth_throttle_max_failures": 2,
            "auth_throttle_window_secs": 30,
            "sse_reassembly_max_bytes": 4_000_000,
            "retry_backoff_ms": 100,
            "retry_backoff_cap_ms": 1_000,
            "retry_after_cap_secs": 10,
            "rate_limit_rpm": 90
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let settings: Value = resp.json().await.expect("设置应可解析");
    assert_eq!(settings["auth_throttle_max_failures"], 2);
    assert_eq!(settings["auth_throttle_window_secs"], 30);
    assert_eq!(settings["sse_reassembly_max_bytes"], 4_000_000);
    assert_eq!(settings["retry_backoff_ms"], 100);
    assert_eq!(settings["retry_backoff_cap_ms"], 1_000);
    assert_eq!(settings["retry_after_cap_secs"], 10);
    assert_eq!(settings["rate_limit_rpm"], 90);

    let got: Value = admin_get(&gw, "/settings")
        .await
        .json()
        .await
        .expect("设置应可解析");
    assert_eq!(got["auth_throttle_max_failures"], 2);
    assert_eq!(got["sse_reassembly_max_bytes"], 4_000_000);

    let resp = admin_put(
        &gw,
        "/settings",
        json!({
            "full_body": false,
            "max_request_bytes": 1_000_000,
            "max_response_bytes": 2_000_000,
            "sse_reassembly_max_bytes": 4_000_000,
            "retry_backoff_ms": 100,
            "retry_backoff_cap_ms": 1_000,
            "retry_after_cap_secs": 10,
            "rate_limit_rpm": 90
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let settings: Value = resp.json().await.expect("设置应可解析");
    assert_eq!(settings["max_response_bytes"], 2_000_000);

    let resp = admin_put(
        &gw,
        "/settings",
        json!({
            "full_body": false,
            "max_request_bytes": 1_000_000,
            "sse_reassembly_max_bytes": 0
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    let resp = admin_put(
        &gw,
        "/settings",
        json!({
            "full_body": false,
            "max_request_bytes": 1_000_000,
            "retry_backoff_ms": 500,
            "retry_backoff_cap_ms": 100
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn private_network_policy_takes_effect_immediately() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let disabled = admin_put(
        &gw,
        "/settings",
        json!({
            "full_body": false,
            "max_request_bytes": 1_000_000,
            "allow_private_networks": false
        }),
    )
    .await;
    assert_eq!(disabled.status(), reqwest::StatusCode::OK);
    let channel_id = channel_id_by_name(&gw, "test-channel").await;

    let probe = admin_json(
        &gw,
        reqwest::Method::POST,
        &format!("/channels/{channel_id}/test"),
        json!({ "model": TEST_MODEL }),
    )
    .await;
    assert_eq!(probe.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = probe.json().await.expect("拒绝响应应可解析");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("安全策略"))
    );

    let response = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
    assert!(
        gw.upstream.received().is_empty(),
        "策略拒绝不应建立上游连接"
    );
}

/// 认证失败限流次数写入后对新请求即时生效。
#[tokio::test]
async fn settings_auth_throttle_takes_effect_immediately() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let resp = admin_put(
        &gw,
        "/settings",
        json!({
            "full_body": false,
            "max_request_bytes": 1_000_000,
            "auth_throttle_max_failures": 1
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body = json!({
        "model": TEST_MODEL,
        "messages": [{ "role": "user", "content": "hi" }]
    });
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth("sk-wrong")
        .json(&body)
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth("sk-wrong")
        .json(&body)
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "次数上限 1 时第二次认证失败应 429"
    );
}

/// 读最近一条日志的两份 body 列。
async fn fetch_bodies(pool: &sqlx::SqlitePool) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    sqlx::query_as("SELECT request_body, response_body FROM request_log ORDER BY id DESC LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("应有请求日志")
}

/// 设置写入开启 full_body：后续请求的完整 body 落库，且 /logs 以 base64 返回 body。
#[tokio::test]
async fn settings_toggle_full_body_enables_body_logging() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 开启 full_body。
    let resp = admin_put(
        &gw,
        "/settings",
        json!({
            "full_body": true,
            "max_request_bytes": 100_000_000,
            "allow_private_networks": true
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 发一条成功请求。
    make_successful_request(&mut gw).await;

    // 日志应带 body。
    let (request_body, response_body) = fetch_bodies(&gw.pool).await;
    assert!(request_body.is_some(), "开启 full_body 后应落请求 body");
    assert!(response_body.is_some(), "开启 full_body 后应落响应 body");

    // 列表不带 body；详情按 id 以 base64 返回。
    let resp = client
        .get(format!("{admin}/logs?page_size=1"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可查日志");
    let page: Value = resp.json().await.expect("日志应可解析");
    let entry = &page["items"][0];
    assert!(entry["request_body"].is_null(), "列表不应返回 request_body");
    let id = entry["id"].as_i64().expect("应有日志 id");
    let resp = client
        .get(format!("{admin}/logs/{id}"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可查日志详情");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let detail: Value = resp.json().await.expect("详情应可解析");
    let request_b64 = detail["request_body"]
        .as_str()
        .expect("详情 request_body 应为字符串");
    let decoded = base64::prelude::BASE64_STANDARD
        .decode(request_b64)
        .expect("request_body 应为合法 base64");
    assert!(
        String::from_utf8_lossy(&decoded).contains("model"),
        "解码后的请求体应含 model 字段"
    );
}
