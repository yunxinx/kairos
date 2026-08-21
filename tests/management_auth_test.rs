//! 管理会话与 RBAC 端到端黑盒：登录、越权 403、最后 root 保护。
//!
//! 登录口令不能当作管理 API 的 Bearer；管理面只认 `POST /login` 签发的 `ksess_…`。

mod common;

use common::{TEST_MODEL, TEST_ROOT_PASSWORD, TEST_TOKEN_KEY, TestGateway};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn admin_url(gw: &TestGateway, path: &str) -> String {
    format!("{}{path}", gw.admin_base_url())
}

async fn bearer_get(gw: &TestGateway, token: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(admin_url(gw, path))
        .bearer_auth(token)
        .send()
        .await
        .expect("管理请求应可达")
}

async fn bearer_json(
    gw: &TestGateway,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .request(method, admin_url(gw, path))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("管理请求应可达")
}

/// 播种后的 root 可用邮箱密码换会话；错密码失败；登出后会话失效。
#[tokio::test]
async fn seeded_root_can_login_and_logout() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let denied = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({ "email": "root@localhost", "password": "password1" }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({
            "email": "root@localhost",
            "password": TEST_ROOT_PASSWORD
        }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(login.status(), StatusCode::OK);
    let body: Value = login.json().await.expect("登录响应应可解析");
    let token = body["token"].as_str().expect("应有会话令牌");
    assert!(token.starts_with("ksess_"));
    assert_eq!(body["user"]["role"], "root");

    let me = bearer_get(&gw, token, "/me").await;
    assert_eq!(me.status(), StatusCode::OK);
    let me_body: Value = me.json().await.expect("me 应可解析");
    assert_eq!(me_body["email"], "root@localhost");

    let tokens = bearer_get(&gw, token, "/tokens").await;
    assert_eq!(tokens.status(), StatusCode::OK);

    let logout = reqwest::Client::new()
        .post(admin_url(&gw, "/logout"))
        .bearer_auth(token)
        .send()
        .await
        .expect("登出应可达");
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    let after = bearer_get(&gw, token, "/tokens").await;
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
}

/// 登录口令不能当管理 Bearer，也不能当模型令牌；会话同样不能调模型路由。
#[tokio::test]
async fn login_password_is_not_a_bearer_and_cannot_call_models() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let as_admin = bearer_get(&gw, TEST_ROOT_PASSWORD, "/tokens").await;
    assert_eq!(
        as_admin.status(),
        StatusCode::UNAUTHORIZED,
        "登录口令不能作为管理 API Bearer"
    );

    for key in [gw.session.as_str(), TEST_ROOT_PASSWORD] {
        let resp = reqwest::Client::new()
            .post(format!("{}/v1/chat/completions", gw.base_url()))
            .bearer_auth(key)
            .json(&json!({
                "model": TEST_MODEL,
                "messages": [{ "role": "user", "content": "hi" }]
            }))
            .send()
            .await
            .expect("模型请求应可达");
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{key} 不应能调模型路由"
        );
    }

    let ok = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("令牌请求应可达");
    assert_ne!(ok.status(), StatusCode::UNAUTHORIZED);
}

/// 账户页：改邮箱不需当前密码；改密码必须带对的当前密码。
#[tokio::test]
async fn update_me_email_and_password_with_current() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let email = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({ "email": "root-renamed@example.com" }),
    )
    .await;
    assert_eq!(email.status(), StatusCode::OK);
    let body: Value = email.json().await.expect("应可解析");
    assert_eq!(body["email"], "root-renamed@example.com");

    let missing_current = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({ "password": "password1" }),
    )
    .await;
    assert_eq!(missing_current.status(), StatusCode::BAD_REQUEST);

    let wrong_current = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({
            "password": "password1",
            "current_password": "not-the-password"
        }),
    )
    .await;
    assert_eq!(wrong_current.status(), StatusCode::BAD_REQUEST);

    let changed = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({
            "password": "password1",
            "current_password": TEST_ROOT_PASSWORD
        }),
    )
    .await;
    assert_eq!(changed.status(), StatusCode::OK);

    let old_login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({
            "email": "root-renamed@example.com",
            "password": TEST_ROOT_PASSWORD
        }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(old_login.status(), StatusCode::UNAUTHORIZED);

    let new_login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({
            "email": "root-renamed@example.com",
            "password": "password1"
        }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(new_login.status(), StatusCode::OK);
}

/// user 不能写渠道/模型组；admin 不能写渠道；最后 root 不能降级。
#[tokio::test]
async fn rbac_forbids_cross_role_writes_and_protects_last_root() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let created = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "user@example.com",
            "display_name": "普通",
            "password": "password1",
            "role": "user"
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let admin_created = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "admin@example.com",
            "display_name": "管理",
            "password": "password1",
            "role": "admin"
        }),
    )
    .await;
    assert_eq!(admin_created.status(), StatusCode::CREATED);

    let user_login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({ "email": "user@example.com", "password": "password1" }))
        .send()
        .await
        .expect("登录应可达");
    let user_body: Value = user_login.json().await.expect("登录应可解析");
    let user_token = user_body["token"].as_str().expect("应有会话");

    let admin_login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({ "email": "admin@example.com", "password": "password1" }))
        .send()
        .await
        .expect("登录应可达");
    let admin_body: Value = admin_login.json().await.expect("登录应可解析");
    let admin_token = admin_body["token"].as_str().expect("应有会话");

    let user_channels = bearer_get(&gw, user_token, "/channels").await;
    assert_eq!(user_channels.status(), StatusCode::FORBIDDEN);
    let user_groups = bearer_get(&gw, user_token, "/model-groups").await;
    assert_eq!(user_groups.status(), StatusCode::FORBIDDEN);

    let admin_channels = bearer_get(&gw, admin_token, "/channels").await;
    assert_eq!(admin_channels.status(), StatusCode::FORBIDDEN);
    let admin_groups = bearer_get(&gw, admin_token, "/model-groups").await;
    assert_eq!(admin_groups.status(), StatusCode::OK);
    let admin_settings = bearer_get(&gw, admin_token, "/settings").await;
    assert_eq!(admin_settings.status(), StatusCode::FORBIDDEN);

    let user_creates = bearer_json(
        &gw,
        user_token,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "another@example.com",
            "display_name": "越权",
            "password": "password1",
            "role": "user"
        }),
    )
    .await;
    assert_eq!(user_creates.status(), StatusCode::FORBIDDEN);

    let admin_creates_admin = bearer_json(
        &gw,
        admin_token,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "other-admin@example.com",
            "display_name": "越权管理",
            "password": "password1",
            "role": "admin"
        }),
    )
    .await;
    assert_eq!(admin_creates_admin.status(), StatusCode::FORBIDDEN);

    let demote = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/users/1",
        json!({ "role": "admin" }),
    )
    .await;
    assert_eq!(demote.status(), StatusCode::CONFLICT);
    let body: Value = demote.json().await.expect("应可解析");
    assert_eq!(body["error"]["code"], "last_root_protected");

    let delete_last = reqwest::Client::new()
        .delete(admin_url(&gw, "/users/1"))
        .bearer_auth(&gw.session)
        .send()
        .await
        .expect("删除应可达");
    assert_eq!(delete_last.status(), StatusCode::CONFLICT);
    let delete_body: Value = delete_last.json().await.expect("应可解析");
    assert_eq!(delete_body["error"]["code"], "last_root_protected");
}
