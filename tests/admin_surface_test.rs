//! 管理面横切行为：监听隔离、认证边界与结构化错误。

mod common;

use common::admin::{admin_put, chat_request};
use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway};
use serde_json::{Value, json};

/// 协议监听上不应出现的管理 API 与 UI 路径（含 SPA 路由）。
const PROTOCOL_FORBIDDEN_ADMIN_GETS: &[&str] = &[
    "/",
    "/overview",
    "/login",
    "/tokens",
    "/channels",
    "/prices",
    "/model-groups",
    "/settings",
    "/logs",
    "/system-logs",
    "/stats",
    "/stats/lifetime",
    "/token",
    "/channel",
    "/pricing",
    "/models",
    "/unified-models",
    "/catalog",
    "/catalog/meta",
    "/config",
    "/requests",
    "/metrics",
];

/// 未配置管理监听时管理面整体关闭：协议监听上没有任何管理路由。
#[tokio::test]
async fn admin_not_configured_means_no_admin_routes() {
    let gw = TestGateway::start().await;
    let client = reqwest::Client::new();

    // 协议监听不应有管理路由或 UI；落到 fallback（404）。覆盖读/写、探测与 SPA 路径。
    for path in PROTOCOL_FORBIDDEN_ADMIN_GETS {
        let resp = client
            .get(format!("{}{path}", gw.base_url()))
            .send()
            .await
            .expect("协议监听应可请求");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND,
            "协议监听不应注册管理路由 {path}"
        );
    }
    let resp = client
        .post(format!("{}/tokens", gw.base_url()))
        .json(&json!({ "name": "x", "balance_usd_micros": null, "enabled": true }))
        .send()
        .await
        .expect("协议监听应可请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "协议监听不应接受管理写入"
    );
    let resp = client
        .post(format!("{}/channels/1/test", gw.base_url()))
        .send()
        .await
        .expect("协议监听应可请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "协议监听不应接受渠道探测"
    );
}

/// 未认证或错误 admin key 一律 401，且返回结构化错误。
#[tokio::test]
async fn unauthenticated_admin_request_is_401() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();

    // 缺头。
    let resp = client
        .get(format!("{}/tokens", gw.admin_base_url()))
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "unauthorized");

    // 错误 key。
    let resp = client
        .get(format!("{}/tokens", gw.admin_base_url()))
        .bearer_auth("wrong-key")
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// 管理面不接受 Authorization 中的会话值。
#[tokio::test]
async fn admin_rejects_bearer_session() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/tokens", gw.admin_base_url()))
        .header("Authorization", "Bearer ksess_not_a_cookie")
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// 已配置管理面时协议监听仍不注册管理路由或 UI；管理监听不提供协议端点。
///
/// admin key 与下游令牌体系隔离：下游令牌调管理面 401，admin key 当下游令牌 401。
#[tokio::test]
async fn admin_surface_is_isolated_from_protocol_surface() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();

    // 协议监听无管理路由、无 UI（即使管理面已启动）。
    for path in PROTOCOL_FORBIDDEN_ADMIN_GETS {
        let resp = client
            .get(format!("{}{path}", gw.base_url()))
            .header(reqwest::header::COOKIE, &gw.session)
            .header(reqwest::header::ORIGIN, gw.admin_origin())
            .send()
            .await
            .expect("协议监听应可请求");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND,
            "协议监听即使已配置管理面也不应注册 {path}"
        );
    }
    let resp = client
        .post(format!("{}/channels/1/test", gw.base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("协议监听应可请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "协议监听不应接受渠道探测"
    );

    // 管理监听无协议端点。
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("管理监听应可请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "管理监听不应提供协议端点"
    );

    // 下游令牌不能调管理 API。
    let resp = client
        .get(format!("{}/tokens", gw.admin_base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "下游令牌不应能认证管理面"
    );

    // 管理会话不能当下游令牌。
    let resp = chat_request(&gw, &gw.session, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "管理会话不应能作为下游令牌"
    );
}

/// 新端点的非法输入返回结构化错误：设置上限为 0、未知设置字段、余额调不存在令牌、
/// 日志非法查询参数。
#[tokio::test]
async fn new_endpoints_structured_errors() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 设置：max_request_bytes=0 → 400。
    let resp = admin_put(
        &gw,
        "/settings",
        json!({ "full_body": false, "max_request_bytes": 0 }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");

    // 设置：max_response_bytes=0 → 400。
    let resp = admin_put(
        &gw,
        "/settings",
        json!({ "full_body": false, "max_request_bytes": 100, "max_response_bytes": 0 }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 设置：log_body_max_bytes=0 → 400。
    let resp = admin_put(
        &gw,
        "/settings",
        json!({ "full_body": false, "max_request_bytes": 100, "log_body_max_bytes": 0 }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 设置：未知字段 → 400（deny_unknown_fields）。
    let resp = admin_put(
        &gw,
        "/settings",
        json!({ "full_body": false, "max_request_bytes": 100, "bogus": 1 }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 余额：不存在的令牌 → 404。
    let resp = client
        .post(format!("{admin}/users/999999/balance-adjustments"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({ "operation_id": "admin-balance-7", "delta_usd_micros": 100, "reason": "manual_adjustment" }))
        .send()
        .await
        .expect("应可调整余额");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // 日志：非法查询参数 → 400 结构化错误。
    let resp = client
        .get(format!("{admin}/logs?page=abc"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可查日志");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");

    // 日志：未知查询参数 → 400（deny_unknown_fields，拼写错误不静默返回未过滤结果）。
    let resp = client
        .get(format!("{admin}/logs?tokne_key=sk-x"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可查日志");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 日志：未知排序列 → 400（只接受白名单）。
    let resp = client
        .get(format!("{admin}/logs?sort_by=nope"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可查日志");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// 探测未知渠道 404；两端点未认证 401。
#[tokio::test]
async fn stats_and_probe_auth_and_unknown_channel() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    let resp = client
        .get(format!("{admin}/stats"))
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "unauthorized");

    let resp = client
        .post(format!("{admin}/channels/1/test"))
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    let resp = client
        .post(format!("{admin}/channels/999999/test"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({ "model": TEST_MODEL }))
        .send()
        .await
        .expect("应可探测渠道");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "not_found");
}
