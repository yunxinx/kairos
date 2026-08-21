//! 用户管理 API 与令牌只本人建：列表、充值、自有令牌过滤、admin 禁用。

mod common;

use common::{TEST_TOKEN_KEY, TestGateway};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn admin_url(gw: &TestGateway, path: &str) -> String {
    format!("{}{path}", gw.admin_base_url())
}

async fn json_req(
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

async fn get_req(gw: &TestGateway, token: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(admin_url(gw, path))
        .bearer_auth(token)
        .send()
        .await
        .expect("管理请求应可达")
}

async fn login(gw: &TestGateway, email: &str, password: &str) -> String {
    let resp = reqwest::Client::new()
        .post(admin_url(gw, "/login"))
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(resp.status(), StatusCode::OK);
    resp.json::<Value>().await.expect("登录应可解析")["token"]
        .as_str()
        .expect("应有会话")
        .to_string()
}

async fn create_role(gw: &TestGateway, email: &str, role: &str) -> (i64, String) {
    let created = json_req(
        gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": email,
            "display_name": email,
            "password": "password1",
            "role": role
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let body: Value = created.json().await.expect("用户应可解析");
    (
        body["id"].as_i64().expect("应有 id"),
        login(gw, email, "password1").await,
    )
}

/// 用户列表与充值；user 看不见列表。
#[tokio::test]
async fn users_list_and_recharge_are_admin_plus() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    let (user_id, user_token) = create_role(&gw, "user@example.com", "user").await;

    let forbidden = get_req(&gw, &user_token, "/users").await;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let listed = get_req(&gw, &gw.session, "/users").await;
    assert_eq!(listed.status(), StatusCode::OK);
    let users: Value = listed.json().await.expect("用户列表应可解析");
    assert!(
        users
            .as_array()
            .expect("应为数组")
            .iter()
            .any(|u| u["email"] == "user@example.com")
    );

    let charged = json_req(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        &format!("/users/{user_id}/balance"),
        json!({ "delta_usd_micros": 1_000_000 }),
    )
    .await;
    assert_eq!(charged.status(), StatusCode::OK);
    let wallet: Value = charged.json().await.expect("钱包应可解析");
    assert_eq!(wallet["balance_usd_micros"], 1_000_000);

    let detail = get_req(&gw, &gw.session, &format!("/users/{user_id}")).await;
    assert_eq!(detail.status(), StatusCode::OK);
    let body: Value = detail.json().await.expect("详情应可解析");
    assert_eq!(body["balance_usd_micros"], 1_000_000);
}

/// POST /tokens 属当前用户；user 列表只含自己的；admin 可禁用普通用户令牌但不能代建。
#[tokio::test]
async fn tokens_are_owned_by_session_user_and_admin_can_disable() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let (user_id, user_token) = create_role(&gw, "user@example.com", "user").await;
    let (_admin_id, admin_token) = create_role(&gw, "admin@example.com", "admin").await;

    let created = json_req(
        &gw,
        &user_token,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "mine", "limit_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let mine: Value = created.json().await.expect("令牌应可解析");
    let mine_key = mine["token_key"].as_str().expect("应有 key").to_string();

    let user_list: Value = get_req(&gw, &user_token, "/tokens")
        .await
        .json()
        .await
        .expect("令牌列表应可解析");
    let user_keys: Vec<&str> = user_list
        .as_array()
        .expect("应为数组")
        .iter()
        .map(|t| t["token_key"].as_str().unwrap())
        .collect();
    assert_eq!(user_keys, vec![mine_key.as_str()]);
    assert!(!user_keys.contains(&TEST_TOKEN_KEY));

    let admin_creates = json_req(
        &gw,
        &admin_token,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "admin-own", "limit_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(admin_creates.status(), StatusCode::CREATED);
    let admin_token_key = admin_creates.json::<Value>().await.expect("应可解析")["token_key"]
        .as_str()
        .expect("应有 key")
        .to_string();
    let owner: (i64,) = sqlx::query_as("SELECT user_id FROM tokens WHERE token_key = ?")
        .bind(&admin_token_key)
        .fetch_one(&gw.pool)
        .await
        .expect("应有归属");
    assert_ne!(owner.0, user_id, "admin 不能把令牌建成别人的");

    let disable = json_req(
        &gw,
        &admin_token,
        reqwest::Method::PUT,
        &format!("/tokens/{mine_key}"),
        json!({
            "token_key": mine_key,
            "name": "mine",
            "limit_usd_micros": null,
            "rate_limit_rpm": null,
            "enabled": false,
            "model_group": "default"
        }),
    )
    .await;
    assert_eq!(disable.status(), StatusCode::OK);

    let rename = json_req(
        &gw,
        &admin_token,
        reqwest::Method::PUT,
        &format!("/tokens/{mine_key}"),
        json!({
            "token_key": mine_key,
            "name": "hijacked",
            "limit_usd_micros": null,
            "rate_limit_rpm": null,
            "enabled": false,
            "model_group": "default"
        }),
    )
    .await;
    assert_eq!(rename.status(), StatusCode::FORBIDDEN);

    let delete_others = reqwest::Client::new()
        .delete(admin_url(&gw, &format!("/tokens/{mine_key}")))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("删除应可达");
    assert_eq!(delete_others.status(), StatusCode::FORBIDDEN);

    let own_update = json_req(
        &gw,
        &admin_token,
        reqwest::Method::PUT,
        &format!("/tokens/{admin_token_key}"),
        json!({
            "token_key": admin_token_key,
            "name": "admin-renamed",
            "limit_usd_micros": null,
            "enabled": true
        }),
    )
    .await;
    assert_eq!(own_update.status(), StatusCode::OK);

    let recharge_root = json_req(
        &gw,
        &admin_token,
        reqwest::Method::POST,
        &format!("/tokens/{TEST_TOKEN_KEY}/balance"),
        json!({ "delta_usd_micros": 1_000_000 }),
    )
    .await;
    assert_eq!(recharge_root.status(), StatusCode::FORBIDDEN);

    let with_user_id = json_req(
        &gw,
        &admin_token,
        reqwest::Method::POST,
        "/tokens",
        json!({
            "name": "steal",
            "limit_usd_micros": null,
            "enabled": true,
            "user_id": user_id
        }),
    )
    .await;
    assert_eq!(with_user_id.status(), StatusCode::BAD_REQUEST);

    let admin_list: Value = get_req(&gw, &admin_token, "/tokens")
        .await
        .json()
        .await
        .expect("令牌列表应可解析");
    let admin_keys: Vec<&str> = admin_list
        .as_array()
        .expect("应为数组")
        .iter()
        .map(|t| t["token_key"].as_str().unwrap())
        .collect();
    assert_eq!(admin_keys, vec![admin_token_key.as_str()]);

    let user_touch_seed = json_req(
        &gw,
        &user_token,
        reqwest::Method::PUT,
        &format!("/tokens/{TEST_TOKEN_KEY}"),
        json!({
            "token_key": TEST_TOKEN_KEY,
            "name": "hijack",
            "limit_usd_micros": null,
            "enabled": false
        }),
    )
    .await;
    assert_eq!(user_touch_seed.status(), StatusCode::FORBIDDEN);

    let others = get_req(&gw, &gw.session, &format!("/users/{user_id}/tokens")).await;
    assert_eq!(others.status(), StatusCode::OK);
    let listed: Value = others.json().await.expect("应可解析");
    assert!(
        listed
            .as_array()
            .expect("应为数组")
            .iter()
            .any(|t| t["token_key"] == mine_key && t["enabled"] == false)
    );
}
