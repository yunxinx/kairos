//! 统一模型：管理 API CRUD、同组未隐藏撞名、有序 failover 与按实际模型计价。
//!
//! 主接缝：端到端 HTTP 黑盒。统一 ID 本身无价格行；失败跳不扣费；响应 `model`
//! 回显下游请求名。

mod common;

use common::{
    SEED_PRICE_ATTACH_LISTING_CHANNELS, TEST_ADMIN_KEY, TEST_MODEL, TEST_TOKEN_KEY, TestGateway,
    UpstreamBehavior,
};
use kairos::config;
use kairos::store::resources::{Channel, Price, UnifiedMember, UnifiedModel};
use serde_json::{Value, json};

/// 带 `TEST_ADMIN_KEY` 认证的 GET。
async fn admin_get(gw: &TestGateway, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}{path}", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("管理请求应可达")
}

/// 带认证的 JSON 请求。
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

/// 带认证、无 body 的请求。
async fn admin_send(gw: &TestGateway, method: reqwest::Method, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .request(method, format!("{}{path}", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("管理请求应可达")
}

/// 以指定令牌向网关发一条 Chat Completions 请求。
async fn chat_request(gw: &TestGateway, token: &str, model: &str) -> reqwest::Response {
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

fn completion_body(model: &str, prompt: u64, completion: u64) -> Value {
    json!({
        "id": "chatcmpl-u",
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "ok" },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": prompt + completion
        }
    })
}

async fn balance_micros(gw: &TestGateway, key: &str) -> i64 {
    let row: (i64,) =
        sqlx::query_as("SELECT balance_usd_micros FROM token_balance WHERE token_key = ?")
            .bind(key)
            .fetch_one(&gw.pool)
            .await
            .expect("令牌余额应存在");
    row.0
}

fn member(channel_id: i64, model: &str) -> UnifiedMember {
    UnifiedMember {
        channel_id,
        model: model.to_string(),
    }
}

fn member_json(channel_id: i64, model: &str) -> Value {
    json!({ "channel_id": channel_id, "model": model })
}

async fn first_channel_id(gw: &TestGateway) -> i64 {
    let channels: Value = admin_get(gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    channels[0]["id"].as_i64().expect("应有渠道 id")
}

/// CRUD：新建、列出、更新、删除；重名 409；空成员/未登记成员 400。
#[tokio::test]
async fn unified_model_crud_roundtrip() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = first_channel_id(&gw).await;

    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/unified-models",
        json!({
            "id": "coding",
            "models": [member_json(channel_id, "gpt-4o"), member_json(channel_id, "fast")],
            "hide": false
        }),
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: Value = created.json().await.expect("创建响应应可解析");
    assert_eq!(body["id"], "coding");
    assert_eq!(
        body["models"],
        json!([
            member_json(channel_id, "gpt-4o"),
            member_json(channel_id, "fast")
        ])
    );
    assert_eq!(body["hide"], false);

    let dup = admin_json(
        &gw,
        reqwest::Method::POST,
        "/unified-models",
        json!({ "id": "coding", "models": [member_json(channel_id, "gpt-4o")] }),
    )
    .await;
    assert_eq!(dup.status(), reqwest::StatusCode::CONFLICT);

    let empty = admin_json(
        &gw,
        reqwest::Method::POST,
        "/unified-models",
        json!({ "id": "empty", "models": [] }),
    )
    .await;
    assert_eq!(empty.status(), reqwest::StatusCode::BAD_REQUEST);

    let unknown = admin_json(
        &gw,
        reqwest::Method::POST,
        "/unified-models",
        json!({ "id": "ghost", "models": [member_json(channel_id, "not-registered")] }),
    )
    .await;
    assert_eq!(unknown.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = unknown.json().await.expect("错误体应可解析");
    let msg = body["error"]["message"].as_str().expect("应有消息");
    assert!(msg.contains("已登记"), "应提示未登记，实际 {msg}");

    let missing_channel = admin_json(
        &gw,
        reqwest::Method::POST,
        "/unified-models",
        json!({ "id": "ghost-ch", "models": [member_json(9_999, "gpt-4o")] }),
    )
    .await;
    assert_eq!(missing_channel.status(), reqwest::StatusCode::BAD_REQUEST);

    let list: Value = admin_get(&gw, "/unified-models")
        .await
        .json()
        .await
        .expect("列表应可解析");
    assert_eq!(list.as_array().expect("应为数组").len(), 1);
    assert_eq!(list[0]["id"], "coding");

    let updated = admin_json(
        &gw,
        reqwest::Method::PUT,
        "/unified-models/coding",
        json!({
            "id": "ignored",
            "models": [member_json(channel_id, "fast"), member_json(channel_id, "gpt-4o")],
            "hide": true
        }),
    )
    .await;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    let body: Value = updated.json().await.expect("更新应可解析");
    assert_eq!(body["id"], "coding", "路径权威");
    assert_eq!(
        body["models"],
        json!([
            member_json(channel_id, "fast"),
            member_json(channel_id, "gpt-4o")
        ])
    );
    assert_eq!(body["hide"], true);

    let missing = admin_json(
        &gw,
        reqwest::Method::PUT,
        "/unified-models/nope",
        json!({ "id": "nope", "models": [member_json(channel_id, "gpt-4o")] }),
    )
    .await;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let deleted = admin_send(&gw, reqwest::Method::DELETE, "/unified-models/coding").await;
    assert_eq!(deleted.status(), reqwest::StatusCode::OK);
    let list: Value = admin_get(&gw, "/unified-models")
        .await
        .json()
        .await
        .expect("列表应可解析");
    assert!(list.as_array().expect("应为数组").is_empty());
}

/// 未隐藏且 ID 等于已登记模型/别名 → 409；开隐藏则允许。
#[tokio::test]
async fn unhidden_id_colliding_with_registered_name_is_rejected() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = first_channel_id(&gw).await;

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/unified-models",
        json!({ "id": TEST_MODEL, "models": [member_json(channel_id, TEST_MODEL)], "hide": false }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: Value = resp.json().await.expect("错误体应可解析");
    let msg = body["error"]["message"].as_str().expect("应有消息");
    assert!(msg.contains("隐藏"), "应提示开隐藏，实际 {msg}");

    let alias = admin_json(
        &gw,
        reqwest::Method::POST,
        "/unified-models",
        json!({ "id": "fast", "models": [member_json(channel_id, "gpt-4o")], "hide": false }),
    )
    .await;
    assert_eq!(alias.status(), reqwest::StatusCode::CONFLICT);

    let hidden = admin_json(
        &gw,
        reqwest::Method::POST,
        "/unified-models",
        json!({
            "id": TEST_MODEL,
            "models": [member_json(channel_id, TEST_MODEL), member_json(channel_id, "fast")],
            "hide": true
        }),
    )
    .await;
    assert_eq!(hidden.status(), reqwest::StatusCode::CREATED);
}

/// 令牌名 / 分组名 / 统一模型 ID 可以同时为 `coding`。
#[tokio::test]
async fn token_group_and_unified_id_may_share_the_same_string() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let group = admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "coding", "models": ["coding", "gpt-4o"] }),
    )
    .await;
    assert_eq!(group.status(), reqwest::StatusCode::CREATED);

    let token = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "coding", "limit_usd_micros": null, "enabled": true, "model_group": "coding" }),
    )
    .await;
    assert_eq!(token.status(), reqwest::StatusCode::CREATED);
    let token_body: Value = token.json().await.expect("令牌");
    assert_eq!(token_body["name"], "coding");

    let unified = admin_json(
        &gw,
        reqwest::Method::POST,
        "/unified-models",
        json!({ "id": "coding", "models": [member_json(first_channel_id(&gw).await, "gpt-4o")], "hide": false }),
    )
    .await;
    assert_eq!(unified.status(), reqwest::StatusCode::CREATED);
}

/// 渠道新增已登记名若撞上未隐藏统一 ID → 409。
#[tokio::test]
async fn channel_save_rejects_unhidden_unified_id_collision() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = first_channel_id(&gw).await;
    admin_json(
        &gw,
        reqwest::Method::POST,
        "/unified-models",
        json!({ "id": "bundle", "models": [member_json(channel_id, "gpt-4o")], "hide": false }),
    )
    .await;

    let channels: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表");
    let id = channels[0]["id"].as_i64().expect("应有 id");
    let mut body = channels[0].clone();
    body.as_object_mut().expect("对象").remove("id");
    body["models"] = json!(["gpt-4o", "bundle"]);

    let resp = admin_json(&gw, reqwest::Method::PUT, &format!("/channels/{id}"), body).await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
}

fn two_member_seed(bases: &[String]) -> common::Seed {
    let mut seed = common::test_seed(&bases[0]);
    seed.channels = vec![
        Channel {
            name: "ch-cheap".to_string(),
            protocol: config::Protocol::OpenAiChat,
            base_url: bases[0].clone(),
            api_key: "sk-0".to_string(),
            models: vec!["cheap".to_string()],
            model_aliases: Default::default(),
            priority: 1,
            weight: 1,
            timeout_ms: 1000,
            max_retries: 0,
            enabled: true,
            model_group: kairos::store::resources::DEFAULT_MODEL_GROUP.to_string(),
        },
        Channel {
            name: "ch-pricey".to_string(),
            protocol: config::Protocol::OpenAiChat,
            base_url: bases[1].clone(),
            api_key: "sk-1".to_string(),
            models: vec!["pricey".to_string()],
            model_aliases: Default::default(),
            priority: 1,
            weight: 1,
            timeout_ms: 1000,
            max_retries: 0,
            enabled: true,
            model_group: kairos::store::resources::DEFAULT_MODEL_GROUP.to_string(),
        },
    ];
    seed.prices = vec![
        Price {
            channel_id: SEED_PRICE_ATTACH_LISTING_CHANNELS,
            model: "cheap".to_string(),
            input_micros: 1_000_000,
            output_micros: 1_000_000,
            cache_read_micros: None,
            cache_write_micros: None,
        },
        Price {
            channel_id: SEED_PRICE_ATTACH_LISTING_CHANNELS,
            model: "pricey".to_string(),
            input_micros: 10_000_000,
            output_micros: 10_000_000,
            cache_read_micros: None,
            cache_write_micros: None,
        },
    ];
    seed.unified_models = vec![UnifiedModel {
        id: "bundle".to_string(),
        models: vec![member(1, "cheap"), member(2, "pricey")],
        hide: false,
    }];
    seed
}

/// 顺序 failover：首成员失败后打第二条；一次只出站一条；响应回显统一 ID。
#[tokio::test]
async fn ordered_failover_tries_one_member_at_a_time() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, two_member_seed).await;
    ups[0].set_behavior(UpstreamBehavior::Status429);
    ups[1].set_behavior(UpstreamBehavior::Json(completion_body("pricey", 1000, 0)));

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "bundle").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["model"], "bundle", "响应 model 回显下游请求名");
    assert_eq!(ups[0].received().len(), 1, "先打 cheap");
    assert_eq!(ups[1].received().len(), 1, "cheap 失败后再打 pricey");
    assert_eq!(ups[0].received()[0]["model"], "cheap");
    assert_eq!(ups[1].received()[0]["model"], "pricey");
}

/// 首成员返回 400（请求本身有问题）时不再 hop 到下一成员。
#[tokio::test]
async fn client_error_400_does_not_try_next_member() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, two_member_seed).await;
    ups[0].set_behavior(UpstreamBehavior::Status(400));
    ups[1].set_behavior(UpstreamBehavior::Json(completion_body("pricey", 1000, 0)));

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "bundle").await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(ups[0].received().len(), 1, "先打 cheap");
    assert_eq!(ups[1].received().len(), 0, "400 不应 hop 到下一成员");
}

/// 同名挂两条渠道是两条独立成员：只打钉死的渠道，失败不扩到同名另一条。
#[tokio::test]
async fn same_name_on_two_channels_are_independent_members() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_member_seed(bases);
        seed.channels[0].models = vec!["gpt-4o".to_string()];
        seed.channels[1].models = vec!["gpt-4o".to_string()];
        seed.prices[0].model = "gpt-4o".to_string();
        seed.prices[1].model = "gpt-4o".to_string();
        seed.unified_models = vec![UnifiedModel {
            id: "bundle".to_string(),
            models: vec![member(1, "gpt-4o"), member(2, "gpt-4o")],
            hide: false,
        }];
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Status429);
    ups[1].set_behavior(UpstreamBehavior::Json(completion_body("gpt-4o", 1000, 0)));

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "bundle").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(ups[0].received().len(), 1, "先打渠道 1");
    assert_eq!(ups[1].received().len(), 1, "渠道 1 失败后再打渠道 2");
    assert_eq!(ups[0].received()[0]["model"], "gpt-4o");
    assert_eq!(ups[1].received()[0]["model"], "gpt-4o");
    assert_eq!(
        balance_micros(&gw, TEST_TOKEN_KEY).await,
        5_000_000 - 10_000,
        "应按渠道 2 的单价扣费"
    );
}

/// 按实际打到的成员计价；统一 ID 无价格行不 503；失败跳不扣费。
#[tokio::test]
async fn bills_served_member_and_does_not_charge_failed_hops() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, two_member_seed).await;
    ups[0].set_behavior(UpstreamBehavior::Status429);
    ups[1].set_behavior(UpstreamBehavior::Json(completion_body("pricey", 1000, 0)));

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "bundle").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 1000 input × 10 USD/1M = 10_000 micros；失败的 cheap 跳不扣。
    assert_eq!(
        balance_micros(&gw, TEST_TOKEN_KEY).await,
        5_000_000 - 10_000
    );

    let row: (String, Option<String>, i64, String) = sqlx::query_as(
        "SELECT model, outbound_model, cost_usd_micros, channel FROM request_log \
         WHERE status_code BETWEEN 200 AND 299",
    )
    .fetch_one(&gw.pool)
    .await
    .expect("应有成功日志");
    assert_eq!(row.0, "bundle", "入站名为统一 ID");
    assert_eq!(row.1.as_deref(), Some("pricey"), "出站名为实际成员");
    assert_eq!(row.2, 10_000);
    assert_eq!(row.3, "ch-pricey");
}

/// 首成员成功则不再打后续；按该成员单价扣费。
#[tokio::test]
async fn first_successful_member_stops_and_is_billed() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, two_member_seed).await;
    ups[0].set_behavior(UpstreamBehavior::Json(completion_body("cheap", 1000, 0)));
    ups[1].set_behavior(UpstreamBehavior::Json(completion_body("pricey", 1000, 0)));

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "bundle").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(ups[0].received().len(), 1);
    assert_eq!(ups[1].received().len(), 0, "成功后不应再打下一条");
    // 1000 × 1 USD/1M = 1_000 micros。
    assert_eq!(balance_micros(&gw, TEST_TOKEN_KEY).await, 5_000_000 - 1_000);
}

/// 开隐藏时请求名走统一模型（与同名已登记模型并存）。
#[tokio::test]
async fn hidden_colliding_id_is_served_as_unified_model() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_member_seed(bases);
        seed.channels[0].models = vec!["gpt-4o".to_string()];
        seed.prices[0].model = "gpt-4o".to_string();
        seed.unified_models = vec![UnifiedModel {
            id: "gpt-4o".to_string(),
            models: vec![member(1, "gpt-4o"), member(2, "pricey")],
            hide: true,
        }];
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Status429);
    ups[1].set_behavior(UpstreamBehavior::Json(completion_body("pricey", 1000, 0)));

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["model"], "gpt-4o");
    assert_eq!(ups[1].received().len(), 1, "应 failover 到统一模型下一成员");
}

/// 成员渠道全失效：503 说明原因，不是「模型不存在」。
#[tokio::test]
async fn invalid_members_return_gateway_reason_not_acl() {
    let gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.unified_models = vec![UnifiedModel {
            id: "bundle".to_string(),
            models: vec![member(1, "missing")],
            hide: false,
        }];
        seed
    })
    .await;

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "bundle").await;
    assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = resp.json().await.expect("错误体应可解析");
    let msg = body["error"]["message"].as_str().expect("应有消息");
    assert!(msg.contains("统一模型"), "应点名统一模型，实际 {msg}");
    assert!(
        msg.contains("渠道") || msg.contains("价格"),
        "应说明成员失效原因，实际 {msg}"
    );
    assert!(
        !msg.contains("不存在"),
        "不应使用 ACL「不存在」口吻，实际 {msg}"
    );
    assert!(gw.upstream.received().is_empty());
}

/// 顺序中失效成员被跳过，后续已定价可路由成员仍可打。
#[tokio::test]
async fn stale_member_is_skipped_then_next_serves() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_member_seed(bases);
        seed.unified_models[0].models =
            vec![member(1, "gone"), member(1, "cheap"), member(2, "pricey")];
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Json(completion_body("cheap", 1000, 0)));

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "bundle").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(ups[0].received().len(), 1);
    assert_eq!(ups[0].received()[0]["model"], "cheap");
    assert_eq!(ups[1].received().len(), 0);
}

/// 删除钉住的渠道后，GET 统一模型把该成员标为 `available: false`。
#[tokio::test]
async fn deleted_channel_marks_unified_member_unavailable() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = first_channel_id(&gw).await;
    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/unified-models",
        json!({
            "id": "bundle",
            "models": [member_json(channel_id, TEST_MODEL)],
            "hide": false
        }),
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);

    let listed: Value = admin_get(&gw, "/unified-models")
        .await
        .json()
        .await
        .expect("列表应可解析");
    assert_eq!(listed[0]["models"][0]["available"], true);

    let deleted = admin_send(
        &gw,
        reqwest::Method::DELETE,
        &format!("/channels/{channel_id}"),
    )
    .await;
    assert_eq!(deleted.status(), reqwest::StatusCode::OK);

    let listed: Value = admin_get(&gw, "/unified-models")
        .await
        .json()
        .await
        .expect("列表应可解析");
    assert_eq!(listed[0]["id"], "bundle");
    assert_eq!(listed[0]["models"][0]["channel_id"], channel_id);
    assert_eq!(
        listed[0]["models"][0]["available"], false,
        "渠道删除后成员应标为不可用"
    );
}
