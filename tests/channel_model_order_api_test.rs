//! 同名渠道顺序表管理 API 的 HTTP 黑盒测试。

mod common;

use common::TestGateway;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

fn admin_url(gw: &TestGateway, path: &str) -> String {
    format!("{}{path}", gw.admin_base_url())
}

async fn admin_json(
    gw: &TestGateway,
    session: &str,
    method: Method,
    path: &str,
    body: Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .request(method, admin_url(gw, path))
        .bearer_auth(session)
        .json(&body)
        .send()
        .await
        .expect("管理请求应可达")
}

async fn admin_get(gw: &TestGateway, session: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(admin_url(gw, path))
        .bearer_auth(session)
        .send()
        .await
        .expect("管理请求应可达")
}

fn channel_body(name: &str, base_url: String, models: Value) -> Value {
    json!({
        "name": name,
        "protocol": "openai_chat",
        "base_url": base_url,
        "api_key": "sk-upstream",
        "models": models,
        "model_aliases": {},
        "timeout_ms": 1000,
        "max_retries": 0,
        "enabled": true
    })
}

async fn create_channel(gw: &TestGateway, name: &str, models: Value) -> i64 {
    let response = admin_json(
        gw,
        &gw.session,
        Method::POST,
        "/channels",
        channel_body(name, gw.upstream.base_url(), models),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json::<Value>().await.expect("创建响应应可解析")["id"]
        .as_i64()
        .expect("创建渠道应返回 id")
}

async fn persisted_order(pool: &sqlx::SqlitePool, model: &str) -> Vec<(i64, i64)> {
    sqlx::query_as(
        "SELECT channel_id, position FROM channel_model_order \
         WHERE model = ? ORDER BY position ASC, channel_id ASC",
    )
    .bind(model)
    .fetch_all(pool)
    .await
    .expect("应能读取顺序表")
}

async fn create_admin_session(gw: &TestGateway) -> String {
    let response = admin_json(
        gw,
        &gw.session,
        Method::POST,
        "/users",
        json!({
            "email": "order-admin@example.com",
            "display_name": "order-admin",
            "password": "password1",
            "role": "admin"
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = reqwest::Client::new()
        .post(admin_url(gw, "/login"))
        .json(&json!({
            "email": "order-admin@example.com",
            "password": "password1"
        }))
        .send()
        .await
        .expect("管理员应能登录");
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().await.expect("登录响应应可解析")["token"]
        .as_str()
        .expect("登录应返回会话")
        .to_string()
}

/// 默认顺序按渠道 id；替换后立即读到新快照，持久化与审计均与该顺序一致。
#[tokio::test]
async fn channel_model_order_api_replaces_only_candidate_set_and_keeps_failures_atomic() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    let first_id = create_channel(&gw, "first", json!(["shared"])).await;
    let second_id = create_channel(&gw, "second", json!(["shared"])).await;
    let lone_id = create_channel(&gw, "lone", json!(["lone"])).await;

    let response = admin_get(&gw, &gw.session, "/channel-model-orders").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.expect("顺序表应可解析"),
        json!([{
            "model": "shared",
            "channel_ids": [first_id, second_id]
        }]),
        "单渠道名字不应出现在顺序表"
    );

    let replacement = json!({
        "model": "path-is-authoritative",
        "channel_ids": [second_id, first_id]
    });
    let response = admin_json(
        &gw,
        &gw.session,
        Method::PUT,
        "/channel-model-orders/shared",
        replacement.clone(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.expect("替换响应应可解析"),
        json!({ "model": "shared", "channel_ids": [second_id, first_id] })
    );
    assert_eq!(
        persisted_order(&gw.pool, "shared").await,
        vec![(second_id, 0), (first_id, 1)],
        "替换应按请求顺序从零持久化位置"
    );

    let response = admin_get(&gw, &gw.session, "/channel-model-orders").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.expect("新快照应可解析"),
        json!([{
            "model": "shared",
            "channel_ids": [second_id, first_id]
        }]),
        "提交后 GET 必须从已替换的快照读取顺序"
    );

    let audit: (String, String, Option<String>) = sqlx::query_as(
        "SELECT target, message, actor_email FROM system_log \
         WHERE target = 'channel_model_orders' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&gw.pool)
    .await
    .expect("调整顺序应留下审计事件");
    assert_eq!(audit.0, "channel_model_orders");
    assert!(
        audit.1.contains("shared") && audit.1.contains(&second_id.to_string()),
        "审计事件应标明可调用名与最终顺序：{}",
        audit.1
    );
    assert_eq!(audit.2.as_deref(), Some(common::TEST_ROOT_EMAIL));

    for invalid_order in [
        json!({ "model": "ignored", "channel_ids": [second_id, lone_id] }),
        json!({ "model": "ignored", "channel_ids": [second_id] }),
        json!({ "model": "ignored", "channel_ids": [second_id, second_id] }),
    ] {
        let response = admin_json(
            &gw,
            &gw.session,
            Method::PUT,
            "/channel-model-orders/shared",
            invalid_order,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            persisted_order(&gw.pool, "shared").await,
            vec![(second_id, 0), (first_id, 1)],
            "非法替换不得更改数据库"
        );
    }
    let response = admin_get(&gw, &gw.session, "/channel-model-orders").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.expect("快照顺序应可解析"),
        json!([{
            "model": "shared",
            "channel_ids": [second_id, first_id]
        }]),
        "非法替换不得更换运行时快照"
    );

    let response = admin_json(
        &gw,
        &gw.session,
        Method::PUT,
        "/channel-model-orders/lone",
        json!({ "model": "lone", "channel_ids": [lone_id] }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let admin_session = create_admin_session(&gw).await;
    let response = admin_get(&gw, &admin_session, "/channel-model-orders").await;
    assert_eq!(response.status(), StatusCode::OK, "管理员可以读取顺序表");
    let response = admin_json(
        &gw,
        &admin_session,
        Method::PUT,
        "/channel-model-orders/shared",
        replacement,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "只有 root 可以调整顺序"
    );
    assert_eq!(
        persisted_order(&gw.pool, "shared").await,
        vec![(second_id, 0), (first_id, 1)],
        "权限拒绝不得更改数据库"
    );
}
