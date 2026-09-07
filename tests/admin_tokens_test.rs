//! 令牌管理面：CRUD、生命周期、余额联动与限流。

mod common;

use common::admin::{admin_get, admin_json, admin_put, chat_request, completion_body};
use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use kairos::store;
use serde_json::{Value, json};

/// 令牌 CRUD 往返 + 写后即时生效：新建立刻可用、删除立刻失效；key 由系统生成。
#[tokio::test]
async fn token_crud_roundtrip_and_immediate_effect() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    // 新建令牌：不接受指定 key，系统生成 ks- 前缀 + 64 位字母数字。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "new-dev", "balance_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建令牌");
    let new_key = created["plaintext_key"]
        .as_str()
        .expect("应返回生成的 key")
        .to_string();
    assert!(new_key.starts_with("ks-"), "系统生成的 key 应以 ks- 开头");
    assert_eq!(new_key.len(), 67, "key 应为前缀 + 64 位随机字符");
    assert!(
        new_key[3..].chars().all(|c| c.is_ascii_alphanumeric()),
        "随机部分应为大小写字母与数字"
    );
    let new_id = created["id"].as_i64().expect("应返回库生成 id");

    // 列表反映新令牌；列表一律掩码，明文经取回端点按需获得。
    let list: Value = admin_get(&gw, "/tokens")
        .await
        .json()
        .await
        .expect("令牌列表应可解析");
    let listed = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == new_id)
        .expect("新建令牌应出现在列表");
    assert_ne!(listed["token_key_masked"], new_key, "列表不得回显明文 key");
    let revealed: Value = admin_get(&gw, &format!("/tokens/{new_id}/key"))
        .await
        .json()
        .await
        .expect("明文 key 取回应可解析");
    assert_eq!(
        revealed["token_key"], new_key,
        "取回端点应返回与创建响应一致的明文 key"
    );

    // 新建令牌在请求路径即时可用：充值（余额调整属 04 票，测试内用相对量原语
    // 绕过）后请求成功。新建令牌已有零额余额行，故可被 `adjust_balance` 充值。
    let mut conn = gw.pool.acquire().await.expect("应能获取连接");
    store::settlement::adjust_user_balance(&mut conn, 1, 5_000_000)
        .await
        .expect("应能为新令牌充值");
    drop(conn);
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, &new_key, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "新令牌应立即可用于请求路径"
    );

    // 删除后立即失效：请求路径认证失败（401），列表也移除。
    let resp = reqwest::Client::new()
        .delete(format!("{}/tokens/{new_id}", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可删除令牌");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let resp = chat_request(&gw, &new_key, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "删除后的令牌应立即失效"
    );
    let list: Value = admin_get(&gw, "/tokens")
        .await
        .json()
        .await
        .expect("令牌列表应可解析");
    assert!(
        !list.as_array().unwrap().iter().any(|t| t["id"] == new_id),
        "删除后令牌应移出列表"
    );
}

/// 生命周期字段与启用开关：读响应带创建/最后使用时间，请求后刷新最后使用时间，
/// 禁用立即在认证处拒绝（401），重新启用立即可用。
#[tokio::test]
async fn token_lifecycle_fields_and_disable_take_effect() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    // 新建：响应含生命周期字段，未使用前最后使用时间为空。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "life", "balance_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建令牌");
    let life_key = created["plaintext_key"]
        .as_str()
        .expect("应返回生成的 key")
        .to_string();
    let life_id = created["id"].as_i64().expect("应返回库生成 id");
    assert_eq!(created["enabled"], true);
    assert!(
        created["created_at"].as_i64().unwrap_or(0) > 0,
        "创建时间应落库并回传"
    );
    assert!(
        created["last_used_at"].is_null(),
        "未使用前最后使用时间应为空"
    );

    // 充值后成功请求一次：列表中的最后使用时间被刷新。
    let mut conn = gw.pool.acquire().await.expect("应能获取连接");
    store::settlement::adjust_user_balance(&mut conn, 1, 5_000_000)
        .await
        .expect("应能充值");
    drop(conn);
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, &life_key, TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let list: Value = admin_get(&gw, "/tokens")
        .await
        .json()
        .await
        .expect("令牌列表应可解析");
    let life = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == life_id)
        .cloned()
        .expect("新建令牌应在列表");
    assert!(
        life["last_used_at"].as_i64().unwrap_or(0) > 0,
        "请求后最后使用时间应刷新"
    );

    // 禁用后立即在认证处拒绝（401）。
    let resp = admin_put(
        &gw,
        &format!("/tokens/{life_id}"),
        json!({ "name": "life", "enabled": false }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let updated: Value = resp.json().await.expect("应返回变更后令牌");
    assert_eq!(updated["enabled"], false, "PUT 回显应反映禁用");
    let resp = chat_request(&gw, &life_key, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "禁用的令牌应被拒绝"
    );

    // 重新启用后立即可用。
    let resp = admin_put(
        &gw,
        &format!("/tokens/{life_id}"),
        json!({ "name": "life", "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = chat_request(&gw, &life_key, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "重新启用的令牌应立即可用"
    );
}

/// PUT 不存在的令牌返回 404，不隐式创建；非整数 id 同样按不存在处理。
#[tokio::test]
async fn update_missing_token_is_404_and_non_numeric_id_is_404() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let resp = admin_put(
        &gw,
        "/tokens/999999",
        json!({
            "name": "x",
            "enabled": true
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    let resp = admin_put(
        &gw,
        "/tokens/sk-bad!key",
        json!({
            "name": "x",
            "enabled": true
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "not_found");
}

/// 删除令牌同事务清理余额行：不留孤儿余额，杜绝任何途径复活旧余额。
#[tokio::test]
async fn deleting_token_clears_balance_row() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();

    // 建令牌并充值，确认请求可用（200）。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "cycle", "balance_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建令牌");
    let cycle_key = created["plaintext_key"]
        .as_str()
        .expect("应返回生成的 key")
        .to_string();
    let mut conn = gw.pool.acquire().await.expect("应能获取连接");
    store::settlement::adjust_user_balance(&mut conn, 1, 5_000_000)
        .await
        .expect("应能充值");
    drop(conn);
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, &cycle_key, TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 删除后余额行一并清理：库内不再残留该 key 的余额记录。
    let cycle_id = common::token_id(&gw.pool, &cycle_key).await;
    let resp = client
        .delete(format!("{}/tokens/{cycle_id}", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可删除令牌");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let leftover: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM token_balance WHERE token_key = ?")
        .bind(&cycle_key)
        .fetch_one(&gw.pool)
        .await
        .expect("应能查询余额行");
    assert_eq!(leftover.0, 0, "删除令牌应同事务清理余额行");
}

// --- 04 票：设置、余额调整与日志查询 ---

/// 修改令牌其他属性不重置余额：充值 → 改 name → 余额保持。
#[tokio::test]
async fn token_attr_update_does_not_reset_balance() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();
    let seeded_id = common::token_id(&gw.pool, TEST_TOKEN_KEY).await;

    // 充值 1 USD（初始 5 USD → 6 USD）。
    let resp = client
        .post(format!("{admin}/users/1/balance-adjustments"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({ "operation_id": "admin-balance-5", "delta_usd_micros": 1_000_000, "reason": "manual_adjustment" }))
        .send()
        .await
        .expect("应可充值");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 修改令牌其他属性（name）。
    let resp = admin_put(
        &gw,
        &format!("/tokens/{seeded_id}"),
        json!({ "name": "renamed", "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 余额不变：从用户读视图确认仍为 6_000_000。
    let resp = admin_get(&gw, "/users/1").await;
    let balance: Value = resp.json().await.expect("余额应可解析");
    assert_eq!(
        balance["balance_usd_micros"], 6_000_000,
        "修改令牌属性不应重置余额"
    );
}

/// 全局 RPM 兜底拦住未单独配置的令牌；令牌显式 `0` 可超过全局上限。
#[tokio::test]
async fn token_rate_limit_uses_global_fallback_and_token_override() {
    let mut gw = TestGateway::start_with_admin(|base| {
        let mut seed = common::test_seed(base);
        seed.settings.insert("rate_limit_rpm".to_string(), json!(1));
        seed
    })
    .await;
    let seeded_id = common::token_id(&gw.pool, TEST_TOKEN_KEY).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));

    let first = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let second = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(second.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert!(
        second.headers().get("retry-after").is_some(),
        "超限应带 Retry-After"
    );

    let resp = admin_put(
        &gw,
        &format!("/tokens/{seeded_id}"),
        json!({
            "name": "dev",
            "rate_limit_rpm": 0,
            "enabled": true,
            "model_group": "default"
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let third = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(
        third.status(),
        reqwest::StatusCode::OK,
        "令牌显式 0 应覆盖全局兜底"
    );
}
