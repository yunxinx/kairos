//! 集合级删除契约：输入校验、授权检查与所有资源变更必须原子完成。

mod common;

use common::{TEST_TOKEN_KEY, TestGateway};
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

async fn create_user(gw: &TestGateway, email: &str, role: &str) -> i64 {
    let response = admin_json(
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
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json::<Value>().await.expect("用户应可解析")["id"]
        .as_i64()
        .expect("应有用户 id")
}

async fn login(gw: &TestGateway, email: &str, password: &str) -> String {
    let response = reqwest::Client::new()
        .post(admin_url(gw, "/login"))
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(response.status(), StatusCode::OK);
    common::session_cookie(&response)
}

async fn create_plan(gw: &TestGateway, name: &str, is_default: bool) -> i64 {
    let response = admin_json(
        gw,
        &gw.session,
        reqwest::Method::POST,
        "/plans",
        json!({
            "display_name": name,
            "audience": "user",
            "is_default": is_default
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json::<Value>().await.expect("套餐应可解析")["id"]
        .as_i64()
        .expect("应有套餐 id")
}

async fn token_exists(gw: &TestGateway, id: i64) -> bool {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tokens WHERE id = ?")
        .bind(id)
        .fetch_one(&gw.pool)
        .await
        .expect("令牌存在性应可查询");
    count == 1
}

async fn active_user_exists(gw: &TestGateway, id: i64) -> bool {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = ? AND deleted_at IS NULL")
            .bind(id)
            .fetch_one(&gw.pool)
            .await
            .expect("用户存在性应可查询");
    count == 1
}

/// 目标为空、重复、含不存在项或内置组时均不产生部分删除。
#[tokio::test]
async fn model_group_bulk_delete_validates_before_writing() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let created = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/model-groups",
        json!({"name": "bulk-keep", "models": []}),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    for targets in [json!([]), json!(["bulk-keep", "bulk-keep"])] {
        let rejected = admin_json(
            &gw,
            &gw.session,
            reqwest::Method::DELETE,
            "/model-groups",
            json!({"targets": targets}),
        )
        .await;
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    }
    let missing = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::DELETE,
        "/model-groups",
        json!({"targets": ["bulk-keep", "missing-group"]}),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let builtin = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::DELETE,
        "/model-groups",
        json!({"targets": ["bulk-keep", "default"]}),
    )
    .await;
    assert_eq!(builtin.status(), StatusCode::CONFLICT);

    let groups: Value = admin_get(&gw, &gw.session, "/model-groups")
        .await
        .json()
        .await
        .expect("模型组列表应可解析");
    assert!(
        groups
            .as_array()
            .expect("模型组列表应为数组")
            .iter()
            .any(|group| group["name"] == "bulk-keep"),
        "任一校验失败后合法目标必须仍存在"
    );
}

/// 令牌批量删除严格按所有者授权：混入他人令牌时自己的令牌也不能先被删除。
#[tokio::test]
async fn token_bulk_delete_cannot_cross_owner() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    create_user(&gw, "bulk-token-owner@example.com", "user").await;
    let owner_session = login(&gw, "bulk-token-owner@example.com", "password1").await;
    let created = admin_json(
        &gw,
        &owner_session,
        reqwest::Method::POST,
        "/tokens",
        json!({"name": "owned", "balance_usd_micros": null, "enabled": true}),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let owned_id = created.json::<Value>().await.expect("令牌应可解析")["id"]
        .as_i64()
        .expect("应有令牌 id");
    let root_id = common::token_id(&gw.pool, TEST_TOKEN_KEY).await;

    let rejected = admin_json(
        &gw,
        &owner_session,
        reqwest::Method::DELETE,
        "/tokens",
        json!({"targets": [owned_id, root_id]}),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    assert!(token_exists(&gw, owned_id).await);
    assert!(token_exists(&gw, root_id).await);
}

/// admin 的批量归档混入 admin/root 时整批拒绝，前面的普通用户保持启用。
#[tokio::test]
async fn user_bulk_archive_rejects_higher_roles_atomically() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    create_user(&gw, "bulk-operator@example.com", "admin").await;
    let target_id = create_user(&gw, "bulk-user-target@example.com", "user").await;
    let protected_id = create_user(&gw, "bulk-admin-target@example.com", "admin").await;
    let operator_session = login(&gw, "bulk-operator@example.com", "password1").await;

    let rejected = admin_json(
        &gw,
        &operator_session,
        reqwest::Method::DELETE,
        "/users",
        json!({"targets": [target_id, protected_id]}),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    assert!(active_user_exists(&gw, target_id).await);
    assert!(active_user_exists(&gw, protected_id).await);
}

/// 强制批量删档不能把用户迁到同批目标；删除自定义默认档后每个受众仍恰好一个默认档。
#[tokio::test]
async fn plan_bulk_delete_chooses_a_live_default_outside_the_batch() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let deleted_default = create_plan(&gw, "bulk-default", true).await;
    let also_deleted = create_plan(&gw, "bulk-other", false).await;
    let user_id = create_user(&gw, "bulk-plan-user@example.com", "user").await;

    let deleted = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::DELETE,
        "/plans?force=true",
        json!({"targets": [deleted_default, also_deleted]}),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::OK);

    let plan_id: i64 = sqlx::query_scalar("SELECT plan_id FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_one(&gw.pool)
        .await
        .expect("用户套餐应可查询");
    assert_ne!(plan_id, deleted_default);
    assert_ne!(plan_id, also_deleted, "迁移目标不能属于同批删除目标");

    let plans: Value = admin_get(&gw, &gw.session, "/plans")
        .await
        .json()
        .await
        .expect("套餐列表应可解析");
    for audience in ["user", "admin"] {
        let defaults = plans
            .as_array()
            .expect("套餐列表应为数组")
            .iter()
            .filter(|plan| plan["audience"] == audience && plan["is_default"] == true)
            .count();
        assert_eq!(defaults, 1, "{audience} 受众必须恰好保留一个默认档");
    }
}

/// 渠道模型批量删除先验证全部目标；一项非法时模型、别名和价格均保持原状。
#[tokio::test]
async fn channel_model_bulk_delete_is_atomic() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channels: Value = admin_get(&gw, &gw.session, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let channel_id = channels[0]["id"].as_i64().expect("应有渠道 id");

    let rejected = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::DELETE,
        "/channel-models",
        json!({
            "targets": [
                {"channel_id": channel_id, "model": "gpt-4o"},
                {"channel_id": channel_id, "model": "missing-model"}
            ]
        }),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::NOT_FOUND);

    let channels: Value = admin_get(&gw, &gw.session, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    assert_eq!(channels[0]["models"], json!(["gpt-4o"]));
    assert_eq!(channels[0]["model_aliases"]["fast"], "gpt-4o-mini");
    let prices: Value = admin_get(&gw, &gw.session, "/prices")
        .await
        .json()
        .await
        .expect("价格列表应可解析");
    assert!(
        prices
            .as_array()
            .expect("价格列表应为数组")
            .iter()
            .any(|price| price["channel_id"] == channel_id && price["model"] == "gpt-4o")
    );
}
