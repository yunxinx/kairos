//! 用户可用模型组：分配、撤组立刻失效、删组清分配。
//!
//! 主接缝：管理 API 黑盒 + 协议面请求。

mod common;

use common::{TEST_MODEL, TestGateway, UpstreamBehavior};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn admin_url(gw: &TestGateway, path: &str) -> String {
    format!("{}{path}", gw.admin_base_url())
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

async fn admin_get(gw: &TestGateway, token: &str, path: &str) -> reqwest::Response {
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
    let body: Value = resp.json().await.expect("登录应可解析");
    body["token"].as_str().expect("应有会话").to_string()
}

fn completion_body() -> Value {
    json!({
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hello!" },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3 }
    })
}

async fn chat(gw: &TestGateway, token: &str, model: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(token)
        .json(&json!({
            "model": model,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("下游请求应能到达网关")
}

async fn first_channel_id(gw: &TestGateway) -> i64 {
    let channels: Value = admin_get(gw, &gw.session, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    channels.as_array().expect("应为数组")[0]["id"]
        .as_i64()
        .expect("应有 id")
}

/// 新建用户带 `default`；整体替换可去掉 `default`；撤组后请求失败且不能新建/改绑。
#[tokio::test]
async fn assigned_groups_gate_create_rebind_and_requests() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let created = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "coder@example.com",
            "display_name": "编码",
            "password": "password1",
            "role": "user"
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let user: Value = created.json().await.expect("用户应可解析");
    let user_id = user["id"].as_i64().expect("应有 id");

    let listed = admin_get(&gw, &gw.session, &format!("/users/{user_id}/model-groups")).await;
    assert_eq!(listed.status(), StatusCode::OK);
    let groups: Value = listed.json().await.expect("可用组应可解析");
    assert_eq!(groups["groups"], json!(["default"]));

    admin_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/model-groups",
        json!({
            "name": "coding",
            "models": [{ "kind": "source", "channel_id": first_channel_id(&gw).await, "model": TEST_MODEL }]
        }),
    )
    .await;

    let replaced = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        &format!("/users/{user_id}/model-groups"),
        json!({ "groups": ["coding"] }),
    )
    .await;
    assert_eq!(replaced.status(), StatusCode::OK);
    let after: Value = replaced.json().await.expect("替换应可解析");
    assert_eq!(after["groups"], json!(["coding"]));

    let session = login(&gw, "coder@example.com", "password1").await;
    let token_resp = admin_json(
        &gw,
        &session,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "coder", "limit_usd_micros": null, "enabled": true, "model_group": "coding" }),
    )
    .await;
    assert_eq!(token_resp.status(), StatusCode::CREATED);
    let token: Value = token_resp.json().await.expect("令牌应可解析");
    let key = token["token_key"].as_str().expect("应有 key").to_string();
    let token_row_id = token["id"].as_i64().expect("应有 id");
    admin_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        &format!("/tokens/{token_row_id}/balance"),
        json!({ "delta_usd_micros": 5_000_000 }),
    )
    .await;

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let ok = chat(&gw, &key, TEST_MODEL).await;
    assert_eq!(ok.status(), StatusCode::OK);

    let withdrawn = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        &format!("/users/{user_id}/model-groups"),
        json!({ "groups": ["default"] }),
    )
    .await;
    assert_eq!(withdrawn.status(), StatusCode::OK);

    let denied = chat(&gw, &key, TEST_MODEL).await;
    assert_ne!(
        denied.status(),
        StatusCode::OK,
        "撤组后已绑令牌应立刻不能调"
    );
    let denied_body: Value = denied.json().await.expect("错误体应可解析");
    let msg = denied_body["error"]["message"].as_str().expect("应有消息");
    assert!(
        !msg.contains("组") && !msg.contains("分组") && !msg.contains("coding"),
        "不得泄露分组细节，实际 {msg}"
    );

    let recreate = admin_json(
        &gw,
        &session,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "again", "limit_usd_micros": null, "enabled": true, "model_group": "coding" }),
    )
    .await;
    assert!(
        recreate.status().is_client_error(),
        "撤组后不能新建该组令牌，实际 {}",
        recreate.status()
    );

    let rebind = admin_json(
        &gw,
        &session,
        reqwest::Method::PUT,
        &format!("/tokens/{token_row_id}"),
        json!({
            "token_key": key,
            "name": "coder",
            "limit_usd_micros": null,
            "enabled": true,
            "model_group": "coding"
        }),
    )
    .await;
    assert!(
        rebind.status().is_client_error(),
        "撤组后不能改绑回该组，实际 {}",
        rebind.status()
    );
}

/// 删组时清掉各用户身上的该组分配；root/admin 为自己操作不要求在名单内。
#[tokio::test]
async fn delete_group_clears_assignments_and_root_can_use_any_group() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    admin_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/model-groups",
        json!({
            "name": "coding",
            "models": [{ "kind": "source", "channel_id": first_channel_id(&gw).await, "model": TEST_MODEL }]
        }),
    )
    .await;
    let created = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "coder@example.com",
            "display_name": "编码",
            "password": "password1",
            "role": "user"
        }),
    )
    .await;
    let user_id = created.json::<Value>().await.expect("用户应可解析")["id"]
        .as_i64()
        .expect("应有 id");
    admin_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        &format!("/users/{user_id}/model-groups"),
        json!({ "groups": ["default", "coding"] }),
    )
    .await;

    let root_token = admin_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "root-coder", "limit_usd_micros": null, "enabled": true, "model_group": "coding" }),
    )
    .await;
    assert_eq!(
        root_token.status(),
        StatusCode::CREATED,
        "root 不要求组在自己的可用名单里"
    );

    let deleted = reqwest::Client::new()
        .delete(admin_url(&gw, "/model-groups/coding"))
        .bearer_auth(&gw.session)
        .send()
        .await
        .expect("删组应可达");
    assert_eq!(deleted.status(), StatusCode::OK);

    let listed = admin_get(&gw, &gw.session, &format!("/users/{user_id}/model-groups")).await;
    let groups: Value = listed.json().await.expect("可用组应可解析");
    assert_eq!(groups["groups"], json!(["default"]));
}
