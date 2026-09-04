//! 请求日志与只读聚合的归属隔离：普通用户只见自己的行，补扣/豁免限 admin+。
//!
//! `user` 的可见面只有「自己的令牌、余额与用量」。这些断言钉住的是
//! 「登录本身不等于可见全站」：曾经 `/logs`、`/stats`、`/logs/{id}/waive` 都只挂在
//! 「已登录」层，普通用户可读他人对话 body、也能直接豁免自己的欠账。

mod common;

use common::{TEST_MODEL, TestGateway, UpstreamBehavior};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn admin_url(gw: &TestGateway, path: &str) -> String {
    format!("{}{path}", gw.admin_base_url())
}

async fn admin_get(gw: &TestGateway, session: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(admin_url(gw, path))
        .header(reqwest::header::COOKIE, session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("管理请求应可达")
}

async fn admin_json(
    gw: &TestGateway,
    session: &str,
    method: reqwest::Method,
    path: &str,
    body: Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .request(method, admin_url(gw, path))
        .header(reqwest::header::COOKIE, session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&body)
        .send()
        .await
        .expect("管理请求应可达")
}

async fn login(gw: &TestGateway, email: &str) -> String {
    let resp = reqwest::Client::new()
        .post(admin_url(gw, "/login"))
        .json(&json!({ "email": email, "password": "password1" }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(resp.status(), StatusCode::OK);
    common::session_cookie(&resp)
}

/// 建一个指定角色的用户并登录，返回 `(id, 会话)`。
async fn create_role(gw: &TestGateway, email: &str, role: &str) -> (i64, String) {
    let created = admin_json(
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
        login(gw, email).await,
    )
}

fn completion_body() -> Value {
    json!({
        "id": "chatcmpl-scope",
        "object": "chat.completion",
        "model": TEST_MODEL,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hello!" },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3 }
    })
}

/// 给某个用户建一把有余额的令牌并跑一次成功请求，落下一条归属该用户的日志。
async fn spend_once(gw: &TestGateway, session: &str, user_id: i64, name: &str) -> String {
    let created = admin_json(
        gw,
        session,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": name, "balance_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let key = created.json::<Value>().await.expect("令牌应可解析")["token_key"]
        .as_str()
        .expect("应有 key")
        .to_string();

    // 钱包在用户身上：充值走余额调整命令，root 对谁都能充。
    let charged = admin_json(
        gw,
        &gw.session,
        reqwest::Method::POST,
        &format!("/users/{user_id}/balance-adjustments"),
        json!({ "operation_id": "log-scope-balance-1", "delta_usd_micros": 5_000_000, "reason": "manual_adjustment" }),
    )
    .await;
    assert_eq!(charged.status(), StatusCode::OK);

    let response = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(&key)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("下游请求应能到达网关");
    assert_eq!(response.status(), StatusCode::OK, "计费请求应成功");
    key
}

fn log_ids(page: &Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .expect("items 应为数组")
        .iter()
        .map(|item| item["id"].as_i64().expect("应有 id"))
        .collect()
}

/// 普通用户的 `/logs`、`/logs/{id}`、`/stats` 都只覆盖自己；admin 看全量。
#[tokio::test]
async fn request_logs_and_stats_are_scoped_to_owner() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));

    let (alice_id, alice) = create_role(&gw, "alice@example.com", "user").await;
    let (bob_id, bob) = create_role(&gw, "bob@example.com", "user").await;
    spend_once(&gw, &alice, alice_id, "alice-key").await;
    spend_once(&gw, &bob, bob_id, "bob-key").await;

    let alice_page: Value = admin_get(&gw, &alice, "/logs")
        .await
        .json()
        .await
        .expect("日志页应可解析");
    let alice_ids = log_ids(&alice_page);
    assert_eq!(alice_ids.len(), 1, "alice 只应看到自己那一条");
    assert_eq!(alice_page["total"], 1);

    let bob_page: Value = admin_get(&gw, &bob, "/logs")
        .await
        .json()
        .await
        .expect("日志页应可解析");
    let bob_ids = log_ids(&bob_page);
    assert_eq!(bob_ids.len(), 1);
    assert_ne!(alice_ids[0], bob_ids[0], "两人不应看到同一行");

    let root_page: Value = admin_get(&gw, &gw.session, "/logs")
        .await
        .json()
        .await
        .expect("日志页应可解析");
    assert_eq!(root_page["total"], 2, "root 应看到全量");

    // 详情按「不存在」拒绝越权，避免靠遍历 id 探出全站流量规模。
    let cross = admin_get(&gw, &alice, &format!("/logs/{}", bob_ids[0])).await;
    assert_eq!(cross.status(), StatusCode::NOT_FOUND);
    let own = admin_get(&gw, &alice, &format!("/logs/{}", alice_ids[0])).await;
    assert_eq!(own.status(), StatusCode::OK);
    let root_reads_bob = admin_get(&gw, &gw.session, &format!("/logs/{}", bob_ids[0])).await;
    assert_eq!(root_reads_bob.status(), StatusCode::OK);

    // 凭证不是日志查询维度；旧式精确 key 参数会被拒绝，而不是进入查询层。
    let forged = admin_get(&gw, &alice, "/logs?token_key=bob-key").await;
    assert_eq!(forged.status(), StatusCode::BAD_REQUEST);

    let alice_stats: Value = admin_get(&gw, &alice, "/stats")
        .await
        .json()
        .await
        .expect("stats 应可解析");
    assert_eq!(alice_stats["summary"]["request_count"], 1);
    assert_eq!(alice_stats["summary"]["token_count"], 1, "只数自己的令牌");
    assert!(
        alice_stats["summary"]["channel_count"].is_null(),
        "普通用户视图不返回渠道数"
    );

    let root_stats: Value = admin_get(&gw, &gw.session, "/stats")
        .await
        .json()
        .await
        .expect("stats 应可解析");
    assert_eq!(root_stats["summary"]["request_count"], 2);
    assert!(
        root_stats["summary"]["channel_count"].as_u64().is_some(),
        "运营视图仍给出渠道数"
    );

    let alice_lifetime: Value = admin_get(&gw, &alice, "/stats/lifetime")
        .await
        .json()
        .await
        .expect("lifetime 应可解析");
    assert_eq!(alice_lifetime["request_count"], 1);
    let root_lifetime: Value = admin_get(&gw, &gw.session, "/stats/lifetime")
        .await
        .json()
        .await
        .expect("lifetime 应可解析");
    assert_eq!(root_lifetime["request_count"], 2);
}

/// 补扣/豁免属运营面：普通用户一律 403，admin 不能动 root 的账。
#[tokio::test]
async fn settling_requires_admin() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));

    let (alice_id, alice) = create_role(&gw, "alice@example.com", "user").await;
    spend_once(&gw, &alice, alice_id, "alice-key").await;
    let (_admin_id, admin) = create_role(&gw, "ops@example.com", "admin").await;

    let alice_ids = log_ids(
        &admin_get(&gw, &alice, "/logs")
            .await
            .json()
            .await
            .expect("日志页应可解析"),
    );
    let target = alice_ids[0];

    // 曾经的漏洞：普通用户可以直接豁免自己的未结算行。
    let waive = admin_json(
        &gw,
        &alice,
        reqwest::Method::POST,
        &format!("/logs/{target}/waive"),
        json!({}),
    )
    .await;
    assert_eq!(waive.status(), StatusCode::FORBIDDEN);
    let settle = admin_json(
        &gw,
        &alice,
        reqwest::Method::POST,
        &format!("/logs/{target}/settle"),
        json!({}),
    )
    .await;
    assert_eq!(settle.status(), StatusCode::FORBIDDEN);

    let admin_system_logs = admin_get(&gw, &admin, "/system-logs").await;
    assert_eq!(admin_system_logs.status(), StatusCode::OK);

    // admin 能处理普通用户的行……
    let closed = admin_json(
        &gw,
        &admin,
        reqwest::Method::POST,
        &format!("/logs/{target}/waive"),
        json!({}),
    )
    .await;
    assert!(
        closed.status() == StatusCode::OK || closed.status() == StatusCode::CONFLICT,
        "admin 应能处理普通用户的行，实际 {}",
        closed.status()
    );

    // ……但不能处理 root 名下的行。
    spend_once(
        &gw,
        &gw.session,
        kairos::store::users::root_user_id(),
        "root-key",
    )
    .await;
    common::wait_for_request_persistence(&gw.pool).await;
    let root_page: Value = admin_get(&gw, &gw.session, "/logs?token_name=root-key")
        .await
        .json()
        .await
        .expect("日志页应可解析");
    let root_log = log_ids(&root_page);
    assert_eq!(root_log.len(), 1, "root 名下应恰有一条 root-key 的行");
    let cross = admin_json(
        &gw,
        &admin,
        reqwest::Method::POST,
        &format!("/logs/{}/settle", root_log[0]),
        json!({}),
    )
    .await;
    assert_eq!(
        cross.status(),
        StatusCode::FORBIDDEN,
        "admin 不能结算 root 名下的行"
    );
}

/// 系统日志对普通用户开放，但只到「自己的审计行」这条线上。
///
/// 归属由身份注入：普通用户既看不到他人的审计行，也看不到 actor 为 NULL 的运维事件
/// （失败登录、结算失败等含内部细节）。`actor_user_id` 参数不能用来把这条线挪开。
#[tokio::test]
async fn system_logs_show_users_only_their_own_audit_rows() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let (alice_id, alice) = create_role(&gw, "alice@example.com", "user").await;
    let (bob_id, bob) = create_role(&gw, "bob@example.com", "user").await;

    // 无 actor 的运维事件：登录失败只记邮箱，认不出是谁。
    let failed = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({ "email": "alice@example.com", "password": "wrong-password" }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(failed.status(), StatusCode::UNAUTHORIZED);

    let page: Value = admin_get(&gw, &alice, "/system-logs")
        .await
        .json()
        .await
        .expect("系统日志页应可解析");
    let rows = page["items"].as_array().expect("items 应为数组");
    assert!(!rows.is_empty(), "alice 至少应看到自己的登录审计行");
    for row in rows {
        assert_eq!(
            row["actor_user_id"].as_i64(),
            Some(alice_id),
            "只应出现 alice 自己的审计行，实际 {row}"
        );
    }

    // 他人的审计行不可见：bob 登录过，alice 的视图里不应有 bob。
    let bob_page: Value = admin_get(&gw, &bob, "/system-logs")
        .await
        .json()
        .await
        .expect("系统日志页应可解析");
    for row in bob_page["items"].as_array().expect("items 应为数组") {
        assert_eq!(row["actor_user_id"].as_i64(), Some(bob_id));
    }

    // 参数不能解除归属边界：指定他人 actor 仍只返回自己的行。
    let forged: Value = admin_get(&gw, &alice, &format!("/system-logs?actor_user_id={bob_id}"))
        .await
        .json()
        .await
        .expect("系统日志页应可解析");
    for row in forged["items"].as_array().expect("items 应为数组") {
        assert_eq!(
            row["actor_user_id"].as_i64(),
            Some(alice_id),
            "actor_user_id 参数不应让 alice 看到他人的行"
        );
    }

    // root 仍看全量，包括那条没有 actor 的失败登录。
    let root_page: Value = admin_get(&gw, &gw.session, "/system-logs")
        .await
        .json()
        .await
        .expect("系统日志页应可解析");
    let has_systemic = root_page["items"]
        .as_array()
        .expect("items 应为数组")
        .iter()
        .any(|row| row["actor_user_id"].is_null());
    assert!(has_systemic, "root 视图应包含无操作者的运维事件");
}
