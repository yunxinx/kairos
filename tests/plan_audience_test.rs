//! 套餐受众与「新用户默认档」：管理 API 黑盒。
//!
//! 主接缝：`audience` 在创建后不可改（改档不能让已挂载用户悄悄得到/失去管理能力），
//! `is_default` 每个受众至多一档，且新建用户落到当前配置的默认档而非硬编码 id。

mod common;

use common::TestGateway;
use reqwest::StatusCode;
use serde_json::{Value, json};

fn admin_url(gw: &TestGateway, path: &str) -> String {
    format!("{}{path}", gw.admin_base_url())
}

async fn admin_get(gw: &TestGateway, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(admin_url(gw, path))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("管理请求应可达")
}

async fn admin_json(
    gw: &TestGateway,
    method: reqwest::Method,
    path: &str,
    body: Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .request(method, admin_url(gw, path))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&body)
        .send()
        .await
        .expect("管理请求应可达")
}

/// 取某一档的完整视图。
async fn plan(gw: &TestGateway, id: i64) -> Value {
    let resp = admin_get(gw, &format!("/plans/{id}")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    resp.json().await.expect("套餐应可解析")
}

/// 建一档，返回 id。
async fn create_plan(gw: &TestGateway, name: &str, extra: Value) -> i64 {
    let mut body = json!({ "display_name": name });
    let map = body.as_object_mut().expect("应为对象");
    for (key, value) in extra.as_object().expect("附加字段应为对象") {
        map.insert(key.clone(), value.clone());
    }
    let resp = admin_json(gw, reqwest::Method::POST, "/plans", body).await;
    assert_eq!(resp.status(), StatusCode::CREATED, "建档应成功");
    resp.json::<Value>().await.expect("套餐应可解析")["id"]
        .as_i64()
        .expect("套餐应有 id")
}

/// 建一个用户，返回其 `plan_id`。
async fn create_user_plan_id(gw: &TestGateway, email: &str, role: &str) -> i64 {
    let resp = admin_json(
        gw,
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
    assert_eq!(resp.status(), StatusCode::CREATED);
    resp.json::<Value>().await.expect("用户应可解析")["plan_id"]
        .as_i64()
        .expect("非 root 用户应挂档")
}

/// 迁移把内置两档标成各自受众的默认档；受众区分用户档与管理员档。
#[tokio::test]
async fn migration_marks_builtin_plans_as_audience_defaults() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let standard = plan(&gw, 1).await;
    assert_eq!(standard["audience"], "user");
    assert_eq!(standard["is_default"], true, "standard 应是用户档默认");

    let admin_plan = plan(&gw, 2).await;
    assert_eq!(admin_plan["audience"], "admin");
    assert_eq!(admin_plan["is_default"], true, "admin 档应是管理员档默认");
}

/// 每个受众至多一个默认档：新档设为默认会自动摘掉同受众旧档的标记，另一受众不受影响。
#[tokio::test]
async fn default_flag_is_exclusive_per_audience() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let new_user_plan = create_plan(
        &gw,
        "generous",
        json!({ "audience": "user", "is_default": true }),
    )
    .await;

    assert_eq!(plan(&gw, new_user_plan).await["is_default"], true);
    assert_eq!(
        plan(&gw, 1).await["is_default"],
        false,
        "同受众的旧默认档应被自动摘掉"
    );
    assert_eq!(
        plan(&gw, 2).await["is_default"],
        true,
        "管理员档的默认标记不受用户档改动影响"
    );

    // 换到第二档同样只留一个默认。
    let another = create_plan(
        &gw,
        "generous-2",
        json!({ "audience": "user", "is_default": true }),
    )
    .await;
    assert_eq!(plan(&gw, another).await["is_default"], true);
    assert_eq!(plan(&gw, new_user_plan).await["is_default"], false);
}

/// 新建用户落到「当前配置的默认档」，不是写死的内置 id。
#[tokio::test]
async fn new_users_land_on_the_configured_default_plan() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    // 改默认前：跟随迁移标好的内置档。
    assert_eq!(
        create_user_plan_id(&gw, "before@example.com", "user").await,
        1
    );

    let custom_user = create_plan(
        &gw,
        "vip",
        json!({ "audience": "user", "is_default": true }),
    )
    .await;
    let custom_admin = create_plan(
        &gw,
        "ops-lead",
        json!({ "audience": "admin", "is_default": true, "shared_with_admin": true }),
    )
    .await;

    assert_eq!(
        create_user_plan_id(&gw, "after@example.com", "user").await,
        custom_user,
        "普通用户应落到新的用户档默认"
    );
    assert_eq!(
        create_user_plan_id(&gw, "ops@example.com", "admin").await,
        custom_admin,
        "管理员应落到新的管理员档默认"
    );
}

/// 受众与默认身份不属于属性更新契约，夹带任一字段都明确返回 400。
#[tokio::test]
async fn update_rejects_immutable_audience_and_default_fields() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let plan_id = create_plan(&gw, "stays-user", json!({ "audience": "user" })).await;
    assert_eq!(plan(&gw, plan_id).await["audience"], "user");

    for immutable in [
        json!({ "audience": "admin" }),
        json!({ "is_default": true }),
    ] {
        let mut body = json!({ "display_name": "不能落库" });
        body.as_object_mut()
            .expect("应为对象")
            .extend(immutable.as_object().expect("应为对象").clone());
        let updated = admin_json(
            &gw,
            reqwest::Method::PUT,
            &format!("/plans/{plan_id}"),
            body,
        )
        .await;
        assert_eq!(updated.status(), StatusCode::BAD_REQUEST);
    }

    let after = plan(&gw, plan_id).await;
    assert_eq!(after["display_name"], "stays-user", "拒绝时不能部分更新");
    assert_eq!(after["audience"], "user");
    assert_eq!(after["is_default"], false);
}

/// 默认档是“转移”命令：只能选中另一档接任，不能把当前默认档取消成空缺。
#[tokio::test]
async fn default_plan_can_only_be_transferred_within_its_audience() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let candidate = create_plan(&gw, "candidate", json!({ "audience": "user" })).await;

    let moved = admin_json(
        &gw,
        reqwest::Method::PUT,
        &format!("/plans/{candidate}/default"),
        json!(null),
    )
    .await;
    assert_eq!(moved.status(), StatusCode::OK);
    assert_eq!(plan(&gw, candidate).await["is_default"], true);
    assert_eq!(plan(&gw, 1).await["is_default"], false);
    assert_eq!(plan(&gw, 2).await["is_default"], true);

    // 对当前默认档重复执行仍保持默认，不存在反向“关闭”语义。
    let repeated = reqwest::Client::new()
        .put(admin_url(&gw, &format!("/plans/{candidate}/default")))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("设默认命令应可达");
    assert_eq!(repeated.status(), StatusCode::OK);
    assert_eq!(plan(&gw, candidate).await["is_default"], true);
}

/// 强制删掉默认档后，被迁移的用户落到剩下的默认档，且默认标记不悬空。
#[tokio::test]
async fn force_deleting_the_default_plan_moves_users_to_a_live_plan() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let temp_default = create_plan(
        &gw,
        "temp-default",
        json!({ "audience": "user", "is_default": true }),
    )
    .await;
    let moved_user = create_user_plan_id(&gw, "moved@example.com", "user").await;
    assert_eq!(moved_user, temp_default, "该用户先挂在临时默认档上");

    let deleted = reqwest::Client::new()
        .delete(admin_url(&gw, &format!("/plans/{temp_default}?force=true")))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("删档应可达");
    assert_eq!(deleted.status(), StatusCode::OK);

    let users: Value = admin_get(&gw, "/users")
        .await
        .json()
        .await
        .expect("用户列表应可解析");
    let moved = users
        .as_array()
        .expect("应为数组")
        .iter()
        .find(|user| user["email"] == "moved@example.com")
        .expect("用户应仍在");
    let landed = moved["plan_id"].as_i64().expect("应仍挂档");
    assert_ne!(landed, temp_default, "不能仍挂在已删档上");
    assert_eq!(
        plan(&gw, landed).await["audience"],
        "user",
        "普通用户应落到用户档"
    );
}

/// 创建和后续分配都拒绝跨受众绑定，不能只靠前端过滤下拉项。
#[tokio::test]
async fn create_and_assign_reject_cross_audience_plans() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let user_plan = create_plan(&gw, "user-only", json!({"audience": "user"})).await;
    let admin_plan = create_plan(&gw, "admin-only", json!({"audience": "admin"})).await;

    let bad_create = admin_json(
        &gw,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "wrong-audience@example.com",
            "display_name": "wrong",
            "password": "password1",
            "role": "user",
            "plan_id": admin_plan
        }),
    )
    .await;
    assert_eq!(bad_create.status(), StatusCode::BAD_REQUEST);

    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "assign-audience@example.com",
            "display_name": "assign",
            "password": "password1",
            "role": "admin",
            "plan_id": admin_plan
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let user_id = created.json::<Value>().await.expect("用户应可解析")["id"]
        .as_i64()
        .expect("应有用户 id");
    let bad_assign = admin_json(
        &gw,
        reqwest::Method::PUT,
        &format!("/users/{user_id}/plan"),
        json!({"plan_id": user_plan}),
    )
    .await;
    assert_eq!(bad_assign.status(), StatusCode::BAD_REQUEST);
}

/// 角色变化与目标受众默认档在同一事务中迁移，来回切换均使用当前配置。
#[tokio::test]
async fn changing_role_moves_to_the_target_audience_default() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let user_default = create_plan(
        &gw,
        "role-user-default",
        json!({"audience": "user", "is_default": true}),
    )
    .await;
    let admin_default = create_plan(
        &gw,
        "role-admin-default",
        json!({"audience": "admin", "is_default": true}),
    )
    .await;
    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "role-move@example.com",
            "display_name": "role move",
            "password": "password1",
            "role": "user"
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = created.json::<Value>().await.expect("用户应可解析");
    let user_id = created["id"].as_i64().expect("应有用户 id");
    assert_eq!(created["plan_id"], user_default);

    let promoted = admin_json(
        &gw,
        reqwest::Method::PUT,
        &format!("/users/{user_id}"),
        json!({"role": "admin"}),
    )
    .await;
    assert_eq!(promoted.status(), StatusCode::OK);
    let promoted = promoted.json::<Value>().await.expect("用户应可解析");
    assert_eq!(promoted["role"], "admin");
    assert_eq!(promoted["plan_id"], admin_default);

    let demoted = admin_json(
        &gw,
        reqwest::Method::PUT,
        &format!("/users/{user_id}"),
        json!({"role": "user"}),
    )
    .await;
    assert_eq!(demoted.status(), StatusCode::OK);
    let demoted = demoted.json::<Value>().await.expect("用户应可解析");
    assert_eq!(demoted["role"], "user");
    assert_eq!(demoted["plan_id"], user_default);
}

/// 套餐迁移失败时角色也必须回滚，不能留下跨受众的中间状态。
#[tokio::test]
async fn role_and_plan_migration_is_atomic() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "role-atomic@example.com",
            "display_name": "atomic",
            "password": "password1",
            "role": "user"
        }),
    )
    .await;
    let user_id = created.json::<Value>().await.expect("用户应可解析")["id"]
        .as_i64()
        .expect("应有用户 id");
    sqlx::query(
        "CREATE TRIGGER reject_plan_move BEFORE UPDATE OF plan_id ON users \
         BEGIN SELECT RAISE(ABORT, 'reject plan move'); END",
    )
    .execute(&gw.pool)
    .await
    .expect("应能安装测试触发器");

    let failed = admin_json(
        &gw,
        reqwest::Method::PUT,
        &format!("/users/{user_id}"),
        json!({"role": "admin"}),
    )
    .await;
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let role: String = sqlx::query_scalar("SELECT role FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_one(&gw.pool)
        .await
        .expect("应能读取角色");
    assert_eq!(role, "user", "事务失败后角色必须保持原值");
}

/// 内部名由系统按自增 id 生成（`plan-{id}`），不随创建/更新请求提供，也不出现在响应里。
#[tokio::test]
async fn internal_name_is_system_generated_and_not_exposed() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let plan_id = create_plan(&gw, "系统命名档", json!({})).await;

    let view = plan(&gw, plan_id).await;
    assert!(
        view.get("internal_name").is_none(),
        "内部名是系统托管标识，不应出现在响应中"
    );

    let stored: Option<String> = sqlx::query_scalar("SELECT internal_name FROM plans WHERE id = ?")
        .bind(plan_id)
        .fetch_one(&gw.pool)
        .await
        .expect("应能读库");
    assert_eq!(stored.as_deref(), Some(format!("plan-{plan_id}").as_str()));

    // 更新不触碰内部名：系统托管标识不因显示名变化而漂移。
    let updated = admin_json(
        &gw,
        reqwest::Method::PUT,
        &format!("/plans/{plan_id}"),
        json!({ "display_name": "改名后" }),
    )
    .await;
    assert_eq!(updated.status(), StatusCode::OK, "改显示名不应失败");
    let stored_after: Option<String> =
        sqlx::query_scalar("SELECT internal_name FROM plans WHERE id = ?")
            .bind(plan_id)
            .fetch_one(&gw.pool)
            .await
            .expect("应能读库");
    assert_eq!(
        stored_after.as_deref(),
        Some(format!("plan-{plan_id}").as_str()),
        "更新后内部名必须保持稳定"
    );
}
