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
    let disabled_view: Value = disable.json().await.expect("禁用响应应可解析");
    let disabled_key = disabled_view["token_key"]
        .as_str()
        .expect("禁用响应应有 key");
    assert_ne!(disabled_key, mine_key, "跨归属操作不应回显明文 key");
    assert!(
        disabled_key.contains("******"),
        "跨归属操作应返回脱敏 key，实际 {disabled_key}"
    );

    let enable = json_req(
        &gw,
        &admin_token,
        reqwest::Method::PUT,
        &format!("/tokens/{mine_id}"),
        json!({
            "token_key": mine_key,
            "name": "mine",
            "limit_usd_micros": null,
            "rate_limit_rpm": null,
            "enabled": true,
            "model_group": "default"
        }),
    )
    .await;
    assert_eq!(
        enable.status(),
        StatusCode::FORBIDDEN,
        "运营只能禁用普通用户令牌，不能重新启用"
    );

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

    // 更新契约不允许通过 body 改变令牌归属；路径对应的库记录才是权威。
    let attempted_rebind = json_req(
        &gw,
        &admin_token,
        reqwest::Method::PUT,
        &format!("/tokens/{admin_token_id}"),
        json!({
            "token_key": admin_token_key,
            "name": "admin-renamed",
            "limit_usd_micros": null,
            "enabled": true,
            "user_id": user_id
        }),
    )
    .await;
    assert_eq!(attempted_rebind.status(), StatusCode::OK);
    let owner_after: (i64,) = sqlx::query_as("SELECT user_id FROM tokens WHERE id = ?")
        .bind(admin_token_id)
        .fetch_one(&gw.pool)
        .await
        .expect("令牌应仍存在");
    assert_eq!(owner_after.0, owner.0, "更新不得改令牌归属");

    let recharge_root = json_req(
        &gw,
        &admin_token,
        reqwest::Method::POST,
        "/users/1/balance",
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

/// `PUT /users/{id}` 的 `rate_limit_rpm` 三态：缺省不改、`null` 清空、数值设值。
///
/// 界面清空输入框时发的是 `null`；曾因 serde 的 `Option<Option<T>>` 语义被当成
/// 「字段缺省」而静默忽略——保存成功，刷新后旧值原样回来。
#[tokio::test]
async fn user_rate_limit_rpm_can_be_cleared_with_null() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    let (user_id, _) = create_role(&gw, "rpm@example.com", "user").await;

    let set = json_req(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        &format!("/users/{user_id}"),
        json!({ "rate_limit_rpm": 42 }),
    )
    .await;
    assert_eq!(set.status(), StatusCode::OK);
    assert_eq!(
        set.json::<Value>().await.expect("应可解析")["rate_limit_rpm"],
        42
    );

    // 字段缺省：不改。
    let untouched = json_req(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        &format!("/users/{user_id}"),
        json!({ "display_name": "改个名" }),
    )
    .await;
    assert_eq!(untouched.status(), StatusCode::OK);
    assert_eq!(
        untouched.json::<Value>().await.expect("应可解析")["rate_limit_rpm"],
        42,
        "字段缺省不应改动 RPM"
    );

    // 显式 null：清空。
    let cleared = json_req(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        &format!("/users/{user_id}"),
        json!({ "rate_limit_rpm": null }),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    assert!(
        cleared.json::<Value>().await.expect("应可解析")["rate_limit_rpm"].is_null(),
        "null 应清空 RPM"
    );
    let reread = get_req(&gw, &gw.session, &format!("/users/{user_id}")).await;
    assert!(
        reread.json::<Value>().await.expect("应可解析")["rate_limit_rpm"].is_null(),
        "回读也应为空，而不是旧值复活"
    );
}

/// 改密码后吊销该用户的其他会话，留下当前这条。
#[tokio::test]
async fn changing_password_revokes_other_sessions() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    let (user_id, session1) = create_role(&gw, "pwd@example.com", "user").await;

    // 再登录一次，拿到第二条会话。
    let login2 = reqwest::Client::new()
        .post(format!("{}/login", gw.admin_base_url()))
        .json(&json!({ "email": "pwd@example.com", "password": "password1" }))
        .send()
        .await
        .expect("应可登录");
    assert_eq!(login2.status(), StatusCode::OK);
    let session2 = login2.json::<Value>().await.expect("应可解析")["token"]
        .as_str()
        .expect("应有 token")
        .to_string();

    // 两条会话都能访问 /me。
    assert_eq!(
        get_req(&gw, &session1, "/me").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        get_req(&gw, &session2, "/me").await.status(),
        StatusCode::OK
    );

    // 用自助端点改密码：必须校验当前密码；session1 保留，session2 被吊销。
    let changed = json_req(
        &gw,
        &session1,
        reqwest::Method::PUT,
        "/me",
        json!({
            "password": "new-password",
            "current_password": "password1"
        }),
    )
    .await;
    assert_eq!(changed.status(), StatusCode::OK);

    assert_eq!(
        get_req(&gw, &session1, "/me").await.status(),
        StatusCode::OK,
        "当前会话应保留"
    );
    assert_eq!(
        get_req(&gw, &session2, "/me").await.status(),
        StatusCode::UNAUTHORIZED,
        "其他会话应被吊销"
    );

    // 邮箱变更同样吊销随后签发的其它会话，并留下当前会话。
    let session3 = login(&gw, "pwd@example.com", "new-password").await;
    let renamed = json_req(
        &gw,
        &session1,
        reqwest::Method::PUT,
        "/me",
        json!({ "email": "pwd-renamed@example.com" }),
    )
    .await;
    assert_eq!(renamed.status(), StatusCode::OK);
    assert_eq!(
        get_req(&gw, &session1, "/me").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        get_req(&gw, &session3, "/me").await.status(),
        StatusCode::UNAUTHORIZED,
        "改邮箱也应吊销其它会话"
    );
    let audit: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM system_log WHERE target = 'users' AND actor_user_id = ? \
         AND message LIKE '%修改自己的账户%' AND message LIKE '%email%'",
    )
    .bind(user_id)
    .fetch_one(&gw.pool)
    .await
    .expect("自助身份变更应写审计");
    assert_eq!(audit.0, 1);
}

/// 会话过期不计入认证失败限流：这是每 8 小时必然发生一次的正常事件。
#[tokio::test]
async fn expired_session_does_not_count_toward_rate_limit() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    let (_, session) = create_role(&gw, "expire@example.com", "user").await;

    // 手动把会话改成 1 秒后过期。
    sqlx::query("UPDATE management_sessions SET expires_at = ? WHERE token_hash = ?")
        .bind(kairos::gateway::unix_millis() + 1000)
        .bind(kairos::store::users::hash_session_token(&session))
        .execute(&gw.pool)
        .await
        .expect("应能改过期时间");

    std::thread::sleep(std::time::Duration::from_millis(1100));

    // 普通 GC 只能清理超过保留窗口的失效行；当前这条仍需保留，以便旧 token
    // 继续被识别为 Inactive，而不是退化成会计入限流的 Unknown。
    let removed_early =
        kairos::store::users::purge_expired_sessions(&gw.pool, kairos::gateway::unix_millis())
            .await
            .expect("维护清理应成功");
    assert_eq!(removed_early, 0);

    // 连续用过期会话访问 10 次（认证失败上限是 5），每次都 401，但不触发限流。
    for _ in 0..10 {
        assert_eq!(
            get_req(&gw, &session, "/me").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    // 正常登录仍可达，说明 IP 未被限流。
    let login = reqwest::Client::new()
        .post(format!("{}/login", gw.admin_base_url()))
        .json(&json!({ "email": "expire@example.com", "password": "password1" }))
        .send()
        .await
        .expect("应可登录");
    assert_eq!(login.status(), StatusCode::OK, "过期会话不应计入限流");

    let (expires_at,): (i64,) =
        sqlx::query_as("SELECT expires_at FROM management_sessions WHERE token_hash = ?")
            .bind(kairos::store::users::hash_session_token(&session))
            .fetch_one(&gw.pool)
            .await
            .expect("失效会话在保留窗口内应仍存在");
    let removed_late = kairos::store::users::purge_expired_sessions(
        &gw.pool,
        expires_at + kairos::store::users::SESSION_TTL_MS + 1,
    )
    .await
    .expect("保留窗口结束后应能清理");
    assert_eq!(removed_late, 1);
}

/// 维护任务可独立清理已过期或已吊销的会话行，不依赖后续登录。
#[tokio::test]
async fn expired_and_revoked_sessions_can_be_purged_independently() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    let (user_id, _) = create_role(&gw, "gc@example.com", "user").await;

    // 手写几条已经超过保留窗口的失效会话，以及一条正常会话。
    let now = kairos::gateway::unix_millis();
    let old_offset = -(kairos::store::users::SESSION_TTL_MS + 3600 * 1000);
    for (suffix, offset, revoked) in [
        ("expired", old_offset, 0),
        ("revoked", old_offset, 1),
        ("normal", 3600 * 1000, 0),
    ] {
        sqlx::query(
            "INSERT INTO management_sessions (user_id, token_hash, expires_at, revoked, created_at)              VALUES (?, 'hash-' || ?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(suffix)
        .bind(now + offset)
        .bind(revoked)
        .bind(now)
        .execute(&gw.pool)
        .await
        .expect("应能写");
    }

    let before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM management_sessions")
        .fetch_one(&gw.pool)
        .await
        .expect("应能统计");
    assert_eq!(before.0, 5, "3 条手写 + root 的 + create_role 时登录的");

    let removed = kairos::store::users::purge_expired_sessions(&gw.pool, now)
        .await
        .expect("维护清理应成功");
    assert_eq!(removed, 2);

    let after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM management_sessions")
        .fetch_one(&gw.pool)
        .await
        .expect("应能统计");
    assert_eq!(
        after.0, 3,
        "超过保留窗口的 1 条过期 + 1 条吊销会话被清理，留下正常的 1 条 + root 与用户的现有会话"
    );
}

/// 用户批量视图不能把缺失钱包伪装成零余额。
#[tokio::test]
async fn users_list_reports_missing_wallet_as_corruption() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    let (user_id, _) = create_role(&gw, "broken-wallet@example.com", "user").await;
    sqlx::query("DELETE FROM user_balance WHERE user_id = ?")
        .bind(user_id)
        .execute(&gw.pool)
        .await
        .expect("应能构造缺失钱包");

    let response = get_req(&gw, &gw.session, "/users").await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body: Value = response.json().await.expect("错误应可解析");
    assert_eq!(body["error"]["code"], "store_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("缺少钱包"))
    );
}
