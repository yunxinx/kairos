//! 管理监听的 Web UI 托管：SPA 回退、静态资源与 API 隔离。

mod common;

use common::TestGateway;
use serde_json::Value;

/// 响应是否为 HTML 页面（静态资源免认证）。
fn is_html(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/html"))
}

/// 管理监听托管 Web UI：GET / 免认证返回页面。
///
/// 依赖 `webui/dist`（`pnpm --dir webui build`）。产物缺失时本用例失败，
/// 与「嵌入后应能打开管理面」的验收一致；纯 API 退化见
/// [`admin_root_never_5xx_and_api_still_works`]。
#[tokio::test]
async fn admin_get_root_serves_html_without_auth() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let resp = reqwest::Client::new()
        .get(gw.admin_origin())
        .send()
        .await
        .expect("管理监听应可达");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "GET / 应返回页面");
    assert!(is_html(&resp), "GET / 应为 text/html");
    let body = resp.text().await.expect("应能读页面");
    assert!(
        body.contains("id=\"app\""),
        "页面应含 SPA 挂载点，实际: {}",
        body.chars().take(200).collect::<String>()
    );
}

/// SPA 深链刷新不 404：未匹配 API 的 GET 回退 index.html，且免认证。
#[tokio::test]
async fn admin_spa_deep_link_serves_html_without_auth() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let resp = reqwest::Client::new()
        .get(format!("{}/overview", gw.admin_origin()))
        .send()
        .await
        .expect("管理监听应可达");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "SPA 深链刷新不应 404"
    );
    assert!(is_html(&resp), "深链回退应为 text/html");
}

/// 登录页 `GET /login` 回退 SPA；登录 API 在 `/api/login`，两者不再抢同一路径。
#[tokio::test]
async fn admin_get_login_serves_html_without_auth() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let resp = reqwest::Client::new()
        .get(format!("{}/login", gw.admin_origin()))
        .send()
        .await
        .expect("管理监听应可达");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "GET /login 应返回登录页"
    );
    assert!(is_html(&resp), "GET /login 应为 text/html");
}

/// `/api` 下未匹配的路径返回结构化 404，不能把 index.html 当 API 响应回给调用方。
#[tokio::test]
async fn unknown_api_path_returns_json_404_not_spa() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let resp = reqwest::Client::new()
        .get(format!("{}/does-not-exist", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("管理监听应可达");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(!is_html(&resp), "/api 下不应回退 SPA");
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "not_found");
}

/// 静态资源免认证；资源 API 未带 key 仍 401。
#[tokio::test]
async fn admin_static_is_public_api_still_requires_key() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    let favicon = client
        .get(format!("{}/favicon.svg", gw.admin_origin()))
        .send()
        .await
        .expect("应可请求静态资源");
    assert_eq!(
        favicon.status(),
        reqwest::StatusCode::OK,
        "favicon 应免认证可加载"
    );

    let tokens = client
        .get(format!("{admin}/tokens"))
        .send()
        .await
        .expect("应可请求管理 API");
    assert_eq!(
        tokens.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "API 未带 key 仍须 401"
    );
}

/// 管理面 GET / 不得 5xx；带 key 的 API 在 UI 嵌入与否时都可用。
///
/// dist 缺失时 `allow_missing` 使编译通过、GET / 为 404 而非 5xx；本用例与
/// `admin_get_root_serves_html_without_auth` 互补（后者要求产物存在）。
#[tokio::test]
async fn admin_root_never_5xx_and_api_still_works() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    let root = client
        .get(gw.admin_origin())
        .send()
        .await
        .expect("管理监听应可达");
    assert!(
        root.status().as_u16() < 500,
        "UI 缺失或存在时 GET / 都不得 5xx，实际 {}",
        root.status()
    );

    let tokens = client
        .get(format!("{admin}/tokens"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("管理 API 应可达");
    assert_eq!(
        tokens.status(),
        reqwest::StatusCode::OK,
        "管理 API 在 UI 缺失时仍应可用"
    );
}
