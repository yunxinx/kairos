//! 审计日志：写入与认证事件带操作者落入 system_log，读取操作不落。
//!
//! 此前用户增删改、余额调整、结算/豁免完全无记录——谁在什么时候给谁加了多少钱，
//! 事后查不出来。参考 one-api 的 LogTypeTopup / LogTypeManage 补上。

mod common;

use common::{TestGateway, UpstreamBehavior};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn admin_url(gw: &TestGateway, path: &str) -> String {
    format!("{}{path}", gw.admin_base_url())
}

async fn admin_get(gw: &TestGateway, token: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(admin_url(gw, path))
        .bearer_auth(token)
        .send()
        .await
        .expect("管理请求应可达")
}

async fn admin_json(
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

/// 读出全部审计行（带操作者的 info 级系统日志）。
async fn audit_rows(gw: &TestGateway) -> Vec<(String, String, Option<String>)> {
    sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT target, message, actor_email FROM system_log \
         WHERE actor_user_id IS NOT NULL ORDER BY id",
    )
    .fetch_all(&gw.pool)
    .await
    .expect("应能读审计行")
}

/// 用户增删改与余额调整各留一条带操作者的审计行；纯读取不留。
#[tokio::test]
async fn user_and_balance_mutations_are_audited() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;

    let created = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "audited@example.com",
            "display_name": "被审计",
            "password": "password1",
            "role": "user"
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let user_id = created.json::<Value>().await.expect("应可解析")["id"]
        .as_i64()
        .expect("应有 id");

    let charged = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        &format!("/users/{user_id}/balance"),
        json!({ "delta_usd_micros": 25_000_000 }),
    )
    .await;
    assert_eq!(charged.status(), StatusCode::OK);

    let disabled = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        &format!("/users/{user_id}"),
        json!({ "enabled": false }),
    )
    .await;
    assert_eq!(disabled.status(), StatusCode::OK);

    // 重复提交已经生效的值不应制造伪变更或新的审计行。
    let before_noop = audit_rows(&gw).await.len();
    let noop = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        &format!("/users/{user_id}"),
        json!({ "enabled": false }),
    )
    .await;
    assert_eq!(noop.status(), StatusCode::OK);
    assert_eq!(audit_rows(&gw).await.len(), before_noop);

    // 纯读取：不应产生审计行。
    let before_reads = audit_rows(&gw).await.len();
    admin_get(&gw, &gw.session, "/users").await;
    admin_get(&gw, &gw.session, &format!("/users/{user_id}")).await;
    admin_get(&gw, &gw.session, "/logs").await;
    assert_eq!(
        audit_rows(&gw).await.len(),
        before_reads,
        "读取不该进审计表：/users 被导航 hover 预取反复触发，逐次落库会淹掉这张表"
    );

    let rows = audit_rows(&gw).await;
    let joined: String = rows
        .iter()
        .map(|(target, message, _)| format!("{target}|{message}"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        rows.iter()
            .all(|(_, _, actor)| actor.as_deref() == Some(common::TEST_ROOT_EMAIL)),
        "审计行都应带操作者邮箱，实际 {rows:?}"
    );
    assert!(
        joined.contains("users|创建用户") && joined.contains("audited@example.com"),
        "应记下创建用户，实际:\n{joined}"
    );
    assert!(
        joined.contains("billing|")
            && joined.contains("+25.00 USD")
            && joined.contains("0.00 → 25.00"),
        "余额审计应记 delta 与前后值，实际:\n{joined}"
    );
    assert!(
        joined.contains("users|修改用户") && joined.contains("enabled true → false"),
        "改动审计应记字段前后值，实际:\n{joined}"
    );
}

/// 归档用户、撤组、设置变更各留痕。
#[tokio::test]
async fn archive_groups_and_settings_are_audited() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let created = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "gone@example.com",
            "display_name": "待归档",
            "password": "password1",
            "role": "user"
        }),
    )
    .await;
    let user_id = created.json::<Value>().await.expect("应可解析")["id"]
        .as_i64()
        .expect("应有 id");

    // 撤掉全部可用组：会让该用户已绑组的令牌立即失效，值得留痕。
    let withdrawn = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        &format!("/users/{user_id}/model-groups"),
        json!({ "groups": [] }),
    )
    .await;
    assert_eq!(withdrawn.status(), StatusCode::OK);

    let settings: Value = admin_get(&gw, &gw.session, "/settings")
        .await
        .json()
        .await
        .expect("设置应可解析");
    let mut next = settings.clone();
    next["rate_limit_rpm"] = json!(120);
    let saved = admin_json(&gw, &gw.session, reqwest::Method::PUT, "/settings", next).await;
    assert_eq!(saved.status(), StatusCode::OK);

    let archived = reqwest::Client::new()
        .delete(admin_url(&gw, &format!("/users/{user_id}")))
        .bearer_auth(&gw.session)
        .send()
        .await
        .expect("归档应可达");
    assert_eq!(archived.status(), StatusCode::NO_CONTENT);

    let joined: String = audit_rows(&gw)
        .await
        .iter()
        .map(|(target, message, _)| format!("{target}|{message}"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        joined.contains("可用模型组：[default] → []"),
        "撤组应记前后名单，实际:\n{joined}"
    );
    assert!(
        joined.contains("settings|修改设置") && joined.contains("rate_limit_rpm"),
        "设置变更应留痕，实际:\n{joined}"
    );
    assert!(
        joined.contains("users|归档用户"),
        "归档应留痕，实际:\n{joined}"
    );
}

/// 登录成功记 info、失败记 warn 且不带操作者（此刻还没认出是谁）。
#[tokio::test]
async fn login_success_and_failure_are_audited() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;

    let failed = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({ "email": common::TEST_ROOT_EMAIL, "password": "wrong-password" }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(failed.status(), StatusCode::UNAUTHORIZED);

    let rows = sqlx::query_as::<_, (String, String, String, Option<i64>)>(
        "SELECT level, target, message, actor_user_id FROM system_log \
         WHERE target = 'auth' ORDER BY id",
    )
    .fetch_all(&gw.pool)
    .await
    .expect("应能读");

    let success = rows.iter().find(|(_, _, m, _)| m.contains("登录成功"));
    let failure = rows.iter().find(|(_, _, m, _)| m.contains("登录失败"));

    let (level, _, _, actor) = success.expect("建库时的 root 登录应留痕");
    assert_eq!(level, "info");
    assert!(actor.is_some(), "成功登录应带操作者");

    let (level, _, message, actor) = failure.expect("失败登录应留痕");
    assert_eq!(level, "warn");
    assert!(
        actor.is_none(),
        "失败时还没认出是谁，邮箱只是对方声称的，不该当作操作者"
    );
    assert!(message.contains(common::TEST_ROOT_EMAIL));
}

/// 结算/豁免留痕，且记下费用与归属用户。
#[tokio::test]
async fn settling_unsettled_log_is_audited() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-audit",
        "object": "chat.completion",
        "model": common::TEST_MODEL,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "hi" },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3 }
    })));

    // 手写一条未结算日志（归 root）。
    sqlx::query(
        "INSERT INTO request_log \
         (created_at, token_name, token_key, user_id, inbound_protocol, model, channel, \
          status_code, latency_ms, cost_usd_micros, settled) \
         VALUES (1, 'seed', ?, 1, 'openai_chat', 'gpt-4o', 'c', 200, 5, 2_500_000, 0)",
    )
    .bind(common::TEST_TOKEN_KEY)
    .execute(&gw.pool)
    .await
    .expect("应能写日志");
    let log_id: (i64,) = sqlx::query_as("SELECT id FROM request_log WHERE settled = 0")
        .fetch_one(&gw.pool)
        .await
        .expect("应有未结算行");

    let settled = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        &format!("/logs/{}/settle", log_id.0),
        json!({}),
    )
    .await;
    assert_eq!(settled.status(), StatusCode::OK);

    let joined: String = audit_rows(&gw)
        .await
        .iter()
        .map(|(target, message, _)| format!("{target}|{message}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("billing|补扣未结算日志") && joined.contains("2.50 USD"),
        "补扣应记费用，实际:\n{joined}"
    );
}
