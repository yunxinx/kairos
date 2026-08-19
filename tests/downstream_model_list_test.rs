//! 下游标准模型列表：三协议 `GET /v1/models` 按令牌分组与统一模型隐藏过滤。
//!
//! 主接缝：端到端 HTTP 黑盒。OpenAI Chat Completions / Responses 共用官方
//! `GET /v1/models`（OpenAI list 形状）；Anthropic 同一路径在带 `anthropic-version`
//! 时返回 Anthropic list 形状。未认证/坏令牌与现有协议认证一致。

mod common;

use common::{TEST_ADMIN_KEY, TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use serde_json::{Value, json};

/// 带 `TEST_ADMIN_KEY` 认证的 JSON 请求。
async fn admin_json(
    gw: &TestGateway,
    method: reqwest::Method,
    path: &str,
    body: Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .request(method, format!("{}{path}", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&body)
        .send()
        .await
        .expect("管理请求应可达")
}

async fn first_channel_id(gw: &TestGateway) -> i64 {
    let channels: Value = reqwest::Client::new()
        .get(format!("{}/channels", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("渠道列表应可达")
        .json()
        .await
        .expect("渠道列表应可解析");
    channels[0]["id"].as_i64().expect("应有渠道 id")
}

/// 以指定令牌拉 OpenAI 形状的模型列表（Chat Completions / Responses 客户端）。
async fn list_openai(gw: &TestGateway, token: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}/v1/models", gw.base_url()))
        .bearer_auth(token)
        .send()
        .await
        .expect("下游列表请求应能到达网关")
}

/// 以指定令牌拉 Anthropic 形状的模型列表。
async fn list_anthropic(gw: &TestGateway, token: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}/v1/models", gw.base_url()))
        .header("x-api-key", token)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .expect("下游列表请求应能到达网关")
}

/// 从 `{data:[{id}]}` 取出 id 列表（OpenAI / Anthropic 列表外壳都有 `data`）。
fn list_ids(body: &Value) -> Vec<&str> {
    body["data"]
        .as_array()
        .expect("data 应为数组")
        .iter()
        .map(|item| item["id"].as_str().expect("每条应有 id"))
        .collect()
}

fn completion_body() -> Value {
    json!({
        "id": "chatcmpl-list",
        "object": "chat.completion",
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "ok" },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })
}

/// 未认证 / 坏令牌：401；无 anthropic-version 为 OpenAI 错误格式。
#[tokio::test]
async fn list_models_rejects_missing_and_bad_token() {
    let gw = TestGateway::start().await;
    let client = reqwest::Client::new();
    let url = format!("{}/v1/models", gw.base_url());

    let resp = client.get(&url).send().await.expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = resp.json().await.expect("401 响应应可解析");
    assert!(
        body["error"]["message"].is_string(),
        "错误体应为 OpenAI 格式"
    );

    let resp = client
        .get(&url)
        .bearer_auth("sk-wrong")
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    let resp = client
        .get(&url)
        .header("x-api-key", "sk-wrong")
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = resp.json().await.expect("401 响应应可解析");
    assert_eq!(
        body["type"], "error",
        "Anthropic 客户端应收到 Anthropic 错误格式"
    );
}

/// default 令牌看见已登记名（渠道模型 + 别名）；OpenAI 与 Anthropic 形状都至少有 id。
#[tokio::test]
async fn default_token_lists_registered_callables_on_all_protocols() {
    let gw = TestGateway::start().await;

    let openai = list_openai(&gw, TEST_TOKEN_KEY).await;
    assert_eq!(openai.status(), reqwest::StatusCode::OK);
    let body: Value = openai.json().await.expect("列表应可解析");
    assert_eq!(body["object"], "list");
    let ids = list_ids(&body);
    assert_eq!(ids, vec!["fast", TEST_MODEL]);
    assert_eq!(body["data"][0]["object"], "model");

    // Responses 客户端走同一 OpenAI Models API（Bearer、无 anthropic-version）。
    let responses = list_openai(&gw, TEST_TOKEN_KEY).await;
    assert_eq!(responses.status(), reqwest::StatusCode::OK);
    let body: Value = responses.json().await.expect("列表应可解析");
    assert_eq!(list_ids(&body), vec!["fast", TEST_MODEL]);

    let anthropic = list_anthropic(&gw, TEST_TOKEN_KEY).await;
    assert_eq!(anthropic.status(), reqwest::StatusCode::OK);
    let body: Value = anthropic.json().await.expect("列表应可解析");
    assert_eq!(body["has_more"], false);
    assert_eq!(body["data"][0]["type"], "model");
    assert_eq!(list_ids(&body), vec!["fast", TEST_MODEL]);
}

/// 三协议列表均随令牌组过滤；组外模型不出现。
#[tokio::test]
async fn list_models_follows_token_group_on_all_protocols() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let created_group = admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "coding", "models": [{ "kind": "source", "channel_id": first_channel_id(&gw).await, "model": TEST_MODEL }] }),
    )
    .await;
    assert_eq!(created_group.status(), reqwest::StatusCode::CREATED);
    let created_token = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({
            "name": "coder",
            "limit_usd_micros": null,
            "enabled": true,
            "model_group": "coding"
        }),
    )
    .await;
    assert_eq!(created_token.status(), reqwest::StatusCode::CREATED);
    let token: Value = created_token.json().await.expect("令牌应可解析");
    let coding_key = token["token_key"].as_str().expect("应有 key");

    for (label, resp) in [
        ("openai_chat", list_openai(&gw, coding_key).await),
        ("openai_responses", list_openai(&gw, coding_key).await),
        ("anthropic", list_anthropic(&gw, coding_key).await),
    ] {
        assert_eq!(resp.status(), reqwest::StatusCode::OK, "{label}");
        let body: Value = resp.json().await.expect("列表应可解析");
        let ids = list_ids(&body);
        assert_eq!(ids, vec![TEST_MODEL], "{label} 只应看见组内模型");
        assert!(!ids.contains(&"fast"), "{label} 组外别名不应出现");
    }

    let default_body: Value = list_openai(&gw, TEST_TOKEN_KEY)
        .await
        .json()
        .await
        .expect("default 列表应可解析");
    let default_ids = list_ids(&default_body);
    assert!(
        default_ids.contains(&"fast"),
        "default 仍应看见未放入其他组的别名"
    );
    assert!(
        !default_ids.contains(&TEST_MODEL),
        "只列入 coding 的模型应离开 default 列表"
    );
}

/// 隐藏后列表无被收进的模型、有统一模型 ID；被隐藏成员若仍在组内则可直接调用。
#[tokio::test]
async fn hide_drops_collected_members_but_keeps_them_callable() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = first_channel_id(&gw).await;
    let unified = admin_json(
        &gw,
        reqwest::Method::POST,
        "/unified-models",
        json!({
            "id": "coding",
            "models": [{ "channel_id": channel_id, "model": TEST_MODEL }],
            "hide": true
        }),
    )
    .await;
    assert_eq!(unified.status(), reqwest::StatusCode::CREATED);
    let group = admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({
            "name": "pack",
            "models": [
                { "kind": "unified", "id": "coding" },
                { "kind": "source", "channel_id": channel_id, "model": TEST_MODEL }
            ]
        }),
    )
    .await;
    assert_eq!(group.status(), reqwest::StatusCode::CREATED);
    let created_token = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({
            "name": "packer",
            "limit_usd_micros": null,
            "enabled": true,
            "model_group": "pack"
        }),
    )
    .await;
    let token: Value = created_token.json().await.expect("令牌应可解析");
    let pack_key = token["token_key"].as_str().expect("应有 key").to_string();
    let topped = admin_json(
        &gw,
        reqwest::Method::POST,
        &format!("/tokens/{pack_key}/balance"),
        json!({ "delta_usd_micros": 5_000_000 }),
    )
    .await;
    assert_eq!(topped.status(), reqwest::StatusCode::OK);

    let body: Value = list_openai(&gw, &pack_key)
        .await
        .json()
        .await
        .expect("列表应可解析");
    let ids = list_ids(&body);
    assert_eq!(ids, vec!["coding"], "隐藏后只剩统一模型 ID");
    assert!(!ids.contains(&TEST_MODEL), "被收进的模型不应出现在列表");

    let anthropic_body: Value = list_anthropic(&gw, &pack_key)
        .await
        .json()
        .await
        .expect("Anthropic 列表应可解析");
    assert_eq!(list_ids(&anthropic_body), vec!["coding"]);

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let call = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(&pack_key)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("下游请求应能到达网关");
    assert_eq!(
        call.status(),
        reqwest::StatusCode::OK,
        "隐藏只从列表拿掉，组内成员仍可直接调用"
    );
}
