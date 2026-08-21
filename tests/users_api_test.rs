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
    let seeded_id = common::token_id(&gw.pool, TEST_TOKEN_KEY).await;
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
    let mine_id = mine["id"].as_i64().expect("应有 id");

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
    let admin_created: Value = admin_creates.json().await.expect("应可解析");
    let admin_token_key = admin_created["token_key"]
        .as_str()
        .expect("应有 key")
        .to_string();
    let admin_token_id = admin_created["id"].as_i64().expect("应有 id");
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
        &format!("/tokens/{mine_id}"),
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
        &format!("/tokens/{mine_id}"),
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
        .delete(admin_url(&gw, &format!("/tokens/{mine_id}")))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("删除应可达");
    assert_eq!(delete_others.status(), StatusCode::FORBIDDEN);

    let own_update = json_req(
        &gw,
        &admin_token,
        reqwest::Method::PUT,
        &format!("/tokens/{admin_token_id}"),
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
        &format!("/tokens/{seeded_id}/balance"),
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
        &format!("/tokens/{seeded_id}"),
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
    let row = listed
        .as_array()
        .expect("应为数组")
        .iter()
        .find(|t| t["id"] == mine_id)
        .expect("应能按 id 找到该令牌");
    assert_eq!(row["enabled"], false, "admin 应已禁用该令牌");
    // 他人令牌的 key 只给脱敏形态：运营按 id 操作，拿不到明文去花别人的余额。
    let shown = row["token_key"].as_str().expect("应有 key 字段");
    assert_ne!(shown, mine_key, "不应回显明文 key");
    assert!(shown.contains("******"), "应为脱敏形态，实际 {shown}");
}

/// 软删除：账号停用归档、令牌立即失效、消费记录保留、原邮箱可重新注册。
///
/// 曾经是硬删除：`tokens.user_id` 是无级联外键，删有令牌的用户直接撞 FOREIGN KEY
/// 返 500，而正常使用下用户都有令牌，所以「删除用户」对活跃用户必然失败。
#[tokio::test]
async fn deleting_user_archives_and_keeps_usage_history() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let (user_id, user_token) = create_role(&gw, "archive-me@example.com", "user").await;

    let created = json_req(
        &gw,
        &user_token,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "doomed", "limit_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let owned: Value = created.json().await.expect("令牌应可解析");
    let owned_key = owned["token_key"].as_str().expect("应有 key").to_string();

    // 手写一条归属该用户的请求日志，模拟已产生的消费。
    sqlx::query(
        "INSERT INTO request_log \
         (created_at, token_name, token_key, user_id, inbound_protocol, model, channel, \
          status_code, latency_ms, cost_usd_micros, settled) \
         VALUES (1, 'doomed', ?, ?, 'openai_chat', 'gpt-4o', 'c', 200, 5, 1234, 1)",
    )
    .bind(&owned_key)
    .bind(user_id)
    .execute(&gw.pool)
    .await
    .expect("应能写日志");

    let deleted = reqwest::Client::new()
        .delete(admin_url(&gw, &format!("/users/{user_id}")))
        .bearer_auth(&gw.session)
        .send()
        .await
        .expect("归档应可达");
    assert_eq!(
        deleted.status(),
        StatusCode::NO_CONTENT,
        "有令牌的用户也应能归档（硬删除会撞外键返 500）"
    );

    // 列表里消失，但库内行仍在。
    let listed: Value = get_req(&gw, &gw.session, "/users")
        .await
        .json()
        .await
        .expect("列表应可解析");
    assert!(
        !listed
            .as_array()
            .expect("应为数组")
            .iter()
            .any(|u| u["id"] == user_id),
        "归档用户不应出现在列表"
    );
    let archived: (Option<i64>, String, i64) =
        sqlx::query_as("SELECT deleted_at, email, enabled FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_one(&gw.pool)
            .await
            .expect("行应仍在");
    assert!(archived.0.is_some(), "应打上归档时刻");
    assert_eq!(archived.2, 0, "应同时停用");
    assert_eq!(
        archived.1,
        format!("deleted.{user_id}.archive-me@example.com"),
        "邮箱应改写以释放原地址"
    );

    // 消费记录保留。
    let kept: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM request_log WHERE user_id = ?")
        .bind(user_id)
        .fetch_one(&gw.pool)
        .await
        .expect("应能统计");
    assert_eq!(kept.0, 1, "历史消费记录必须保留");

    // 令牌行仍在（供日志归属），但入站请求立即失效。
    let token_rows: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tokens WHERE user_id = ?")
        .bind(user_id)
        .fetch_one(&gw.pool)
        .await
        .expect("应能统计");
    assert_eq!(token_rows.0, 1, "令牌行保留，token_key → user_id 关联不丢");
    let denied = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(&owned_key)
        .json(&json!({ "model": "gpt-4o", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .expect("下游请求应可达");
    assert_eq!(
        denied.status(),
        StatusCode::UNAUTHORIZED,
        "归档用户的令牌应立即失效"
    );

    // 会话立即失效。
    let stale = get_req(&gw, &user_token, "/me").await;
    assert_eq!(stale.status(), StatusCode::UNAUTHORIZED, "会话应被吊销");

    // 原邮箱可重新注册。
    let recreated = json_req(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "archive-me@example.com",
            "display_name": "再来一个",
            "password": "password1",
            "role": "user"
        }),
    )
    .await;
    assert_eq!(
        recreated.status(),
        StatusCode::CREATED,
        "归档后原邮箱应可重新注册"
    );
}

/// 最后一个启用 root 不能归档。
#[tokio::test]
async fn last_root_cannot_be_archived() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let resp = reqwest::Client::new()
        .delete(admin_url(&gw, "/users/1"))
        .bearer_auth(&gw.session)
        .send()
        .await
        .expect("请求应可达");
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.expect("应可解析");
    assert_eq!(body["error"]["code"], "last_root_protected");
}
