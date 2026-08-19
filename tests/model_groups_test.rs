//! 模型组与令牌绑定：管理 API 黑盒 + 组外调用「找不到模型」。
//!
//! 主接缝：独立管理监听上的 `/model-groups` CRUD 与令牌 `model_group`；
//! 协议面组外请求须 404（非 503），文案不提分组。

mod common;

use common::{TEST_ADMIN_KEY, TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
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

/// 带认证、无 body 的请求（删除等）。
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

/// mock 上游返回的合法 Chat Completions 成功体。
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

/// 从列表里按名取出一组。
fn group_named<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array()
        .expect("组列表应为数组")
        .iter()
        .find(|g| g["name"] == name)
        .unwrap_or_else(|| panic!("列表应含组 {name}"))
}

/// 名单条目：钉渠道的 `model` 或统一模型 `id`。
fn group_entry_name(item: &Value) -> Option<&str> {
    item.get("model")
        .and_then(Value::as_str)
        .or_else(|| item.get("id").and_then(Value::as_str))
}

fn source_entry(channel_id: i64, model: &str) -> Value {
    json!({ "kind": "source", "channel_id": channel_id, "model": model })
}

async fn first_channel_id(gw: &TestGateway) -> i64 {
    let channels: Value = admin_get(gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    channels
        .as_array()
        .expect("应为数组")
        .first()
        .expect("应有渠道")["id"]
        .as_i64()
        .expect("应有 id")
}

/// 空库即有内置 `default`，且不能删除。
#[tokio::test]
async fn default_group_always_exists_and_cannot_be_deleted() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;

    let list: Value = admin_get(&gw, "/model-groups")
        .await
        .json()
        .await
        .expect("组列表应可解析");
    let groups = list.as_array().expect("应为数组");
    assert_eq!(groups.len(), 1, "空库只有内置 default");
    assert_eq!(groups[0]["name"], "default");
    assert_eq!(groups[0]["models"], json!([]));

    let resp = admin_send(&gw, reqwest::Method::DELETE, "/model-groups/default").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "default 不能删"
    );
    let body: Value = resp.json().await.expect("错误体应可解析");
    let msg = body["error"]["message"].as_str().expect("应有消息");
    assert!(msg.contains("default"), "应点名 default，实际 {msg}");
    assert!(
        !msg.to_lowercase().contains("force"),
        "普通删除文案不必提 force"
    );

    let forced = admin_send(
        &gw,
        reqwest::Method::DELETE,
        "/model-groups/default?force=true",
    )
    .await;
    assert_eq!(
        forced.status(),
        reqwest::StatusCode::CONFLICT,
        "强制也不能删 default"
    );

    let list: Value = admin_get(&gw, "/model-groups")
        .await
        .json()
        .await
        .expect("组列表应可解析");
    assert_eq!(list.as_array().expect("应为数组").len(), 1);
}

/// CRUD：新建、列出、更新、删除无令牌的组；重名 409；拒收裸字符串名单。
#[tokio::test]
async fn model_group_crud_roundtrip() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;

    let rejected = admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "bare", "models": ["gpt-4o"] }),
    )
    .await;
    assert_eq!(
        rejected.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "裸字符串名单应拒绝"
    );

    let channel = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body(
            "crud-ch",
            "http://127.0.0.1:9",
            json!(["gpt-4o", "fast"]),
            "default",
        ),
    )
    .await;
    assert_eq!(channel.status(), reqwest::StatusCode::CREATED);
    let channel_id = channel.json::<Value>().await.expect("渠道应可解析")["id"]
        .as_i64()
        .expect("应有 id");
    let gpt = source_entry(channel_id, "gpt-4o");
    let fast = source_entry(channel_id, "fast");

    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "coding", "models": [gpt, fast] }),
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: Value = created.json().await.expect("创建响应应可解析");
    assert_eq!(body["name"], "coding");
    assert_eq!(body["models"], json!([gpt, fast]));

    let dup = admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "coding", "models": [] }),
    )
    .await;
    assert_eq!(dup.status(), reqwest::StatusCode::CONFLICT);

    let list: Value = admin_get(&gw, "/model-groups")
        .await
        .json()
        .await
        .expect("组列表应可解析");
    assert!(
        group_named(&list, "default")["models"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(group_named(&list, "coding")["models"], json!([gpt, fast]));

    let updated = admin_json(
        &gw,
        reqwest::Method::PUT,
        "/model-groups/coding",
        json!({ "name": "coding", "models": [gpt] }),
    )
    .await;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    let body: Value = updated.json().await.expect("更新响应应可解析");
    assert_eq!(body["models"], json!([gpt]));

    let missing = admin_json(
        &gw,
        reqwest::Method::PUT,
        "/model-groups/nope",
        json!({ "name": "nope", "models": [] }),
    )
    .await;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let deleted = admin_send(&gw, reqwest::Method::DELETE, "/model-groups/coding").await;
    assert_eq!(deleted.status(), reqwest::StatusCode::OK);
    let body: Value = deleted.json().await.expect("删除响应应可解析");
    assert_eq!(body["name"], "coding");

    let list: Value = admin_get(&gw, "/model-groups")
        .await
        .json()
        .await
        .expect("组列表应可解析");
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["default"]);
}

/// 钉渠道须已登记且渠道存在；统一 ID 须已存在；无 kind 的对象拒收。
#[tokio::test]
async fn model_group_rejects_invalid_members() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    let channel = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body(
            "gate-ch",
            "http://127.0.0.1:9",
            json!(["gpt-4o"]),
            "default",
        ),
    )
    .await;
    assert_eq!(channel.status(), reqwest::StatusCode::CREATED);
    let channel_id = channel.json::<Value>().await.expect("渠道应可解析")["id"]
        .as_i64()
        .expect("应有 id");

    let unregistered = admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "g", "models": [source_entry(channel_id, "nope")] }),
    )
    .await;
    assert_eq!(unregistered.status(), reqwest::StatusCode::BAD_REQUEST);

    let missing_channel = admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "g", "models": [source_entry(9_999, "gpt-4o")] }),
    )
    .await;
    assert_eq!(missing_channel.status(), reqwest::StatusCode::BAD_REQUEST);

    let unknown_unified = admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "g", "models": [{ "kind": "unified", "id": "ghost" }] }),
    )
    .await;
    assert_eq!(unknown_unified.status(), reqwest::StatusCode::BAD_REQUEST);

    let untagged = admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "g", "models": [{ "channel_id": channel_id, "model": "gpt-4o" }] }),
    )
    .await;
    assert_eq!(untagged.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// 新建令牌未指定组则绑 `default`；可改绑到已有组；不存在的组拒绝。
#[tokio::test]
async fn token_binds_exactly_one_group() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "coding", "models": [] }),
    )
    .await;

    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "anon", "limit_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let token: Value = created.json().await.expect("令牌应可解析");
    assert_eq!(token["model_group"], "default", "未指定则 default");
    let default_key = token["token_key"].as_str().expect("应有 key");

    let coded = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "coder", "limit_usd_micros": null, "enabled": true, "model_group": "coding" }),
    )
    .await;
    assert_eq!(coded.status(), reqwest::StatusCode::CREATED);
    let token: Value = coded.json().await.expect("令牌应可解析");
    assert_eq!(token["model_group"], "coding");
    let coding_key = token["token_key"].as_str().expect("应有 key").to_string();

    let rebound = admin_json(
        &gw,
        reqwest::Method::PUT,
        &format!("/tokens/{default_key}"),
        json!({
            "token_key": default_key,
            "name": "anon",
            "limit_usd_micros": null,
            "enabled": true,
            "model_group": "coding"
        }),
    )
    .await;
    assert_eq!(rebound.status(), reqwest::StatusCode::OK);
    let token: Value = rebound.json().await.expect("令牌应可解析");
    assert_eq!(token["model_group"], "coding");

    let missing = admin_json(
        &gw,
        reqwest::Method::PUT,
        &format!("/tokens/{coding_key}"),
        json!({
            "token_key": coding_key,
            "name": "coder",
            "limit_usd_micros": null,
            "enabled": true,
            "model_group": "ghost"
        }),
    )
    .await;
    assert_eq!(
        missing.status(),
        reqwest::StatusCode::NOT_FOUND,
        "绑不存在的组应 404"
    );

    let listed: Value = admin_get(&gw, "/tokens")
        .await
        .json()
        .await
        .expect("令牌列表应可解析");
    let coder = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["token_key"] == coding_key)
        .expect("coder 应仍在列表");
    assert_eq!(coder["model_group"], "coding", "失败的改绑不应落库");
}

/// 删除仍有令牌的组被拒；强制删除把那些令牌改回 `default`。
#[tokio::test]
async fn delete_group_with_tokens_is_blocked_until_forced() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "coding", "models": [] }),
    )
    .await;
    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "coder", "limit_usd_micros": null, "enabled": true, "model_group": "coding" }),
    )
    .await;
    let token: Value = created.json().await.expect("令牌应可解析");
    let key = token["token_key"].as_str().expect("应有 key").to_string();

    let blocked = admin_send(&gw, reqwest::Method::DELETE, "/model-groups/coding").await;
    assert_eq!(blocked.status(), reqwest::StatusCode::CONFLICT);
    let body: Value = blocked.json().await.expect("错误体应可解析");
    let msg = body["error"]["message"].as_str().expect("应有消息");
    assert!(
        msg.contains("令牌") || msg.contains("绑定"),
        "应说明仍有令牌，实际 {msg}"
    );

    let list: Value = admin_get(&gw, "/model-groups")
        .await
        .json()
        .await
        .expect("组列表应可解析");
    group_named(&list, "coding");

    let forced = admin_send(
        &gw,
        reqwest::Method::DELETE,
        "/model-groups/coding?force=true",
    )
    .await;
    assert_eq!(forced.status(), reqwest::StatusCode::OK);

    let list: Value = admin_get(&gw, "/model-groups")
        .await
        .json()
        .await
        .expect("组列表应可解析");
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["default"]);

    let tokens: Value = admin_get(&gw, "/tokens")
        .await
        .json()
        .await
        .expect("令牌列表应可解析");
    let rebound = tokens
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["token_key"] == key)
        .expect("令牌应仍在");
    assert_eq!(rebound["model_group"], "default");
}

/// 组外调用：404「不存在该模型」，不提分组，不是 503；组内仍可调。
#[tokio::test]
async fn out_of_group_model_is_not_found_not_503() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));

    admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "coding", "models": [source_entry(first_channel_id(&gw).await, TEST_MODEL)] }),
    )
    .await;
    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "coder", "limit_usd_micros": null, "enabled": true, "model_group": "coding" }),
    )
    .await;
    let token: Value = created.json().await.expect("令牌应可解析");
    let coding_key = token["token_key"].as_str().expect("应有 key");
    admin_json(
        &gw,
        reqwest::Method::POST,
        &format!("/tokens/{coding_key}/balance"),
        json!({ "delta_usd_micros": 5_000_000 }),
    )
    .await;

    // 组内已登记模型可调。
    let ok = chat_request(&gw, coding_key, TEST_MODEL).await;
    assert_eq!(ok.status(), reqwest::StatusCode::OK, "组内模型应可调");

    // 渠道上有别名 `fast` 但不在 coding 组：404，不是无渠道 503。
    let denied = chat_request(&gw, coding_key, "fast").await;
    assert_eq!(
        denied.status(),
        reqwest::StatusCode::NOT_FOUND,
        "组外应 404 而非 503，实际 {}",
        denied.status()
    );
    let body: Value = denied.json().await.expect("错误体应可解析");
    let msg = body["error"]["message"].as_str().expect("应有消息");
    assert!(msg.contains("fast"), "应含模型名，实际 {msg}");
    assert!(
        msg.contains("不存在") || msg.contains("找不到"),
        "文案应为找不到/不存在，实际 {msg}"
    );
    assert!(
        !msg.contains("组") && !msg.contains("分组") && !msg.contains("coding"),
        "不得泄露分组细节，实际 {msg}"
    );

    // 默认令牌：`fast` 未放入其他组，仍视为 default，可调。
    let default_ok = chat_request(&gw, TEST_TOKEN_KEY, "fast").await;
    assert_eq!(
        default_ok.status(),
        reqwest::StatusCode::OK,
        "未放入其他组的可调用名对 default 令牌仍可用"
    );
}

/// 可调用名只放入自定义组后，不再隐式属于 default。
#[tokio::test]
async fn name_only_in_custom_group_leaves_default() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "coding", "models": [source_entry(first_channel_id(&gw).await, TEST_MODEL)] }),
    )
    .await;

    let denied = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(
        denied.status(),
        reqwest::StatusCode::NOT_FOUND,
        "只在 coding 的模型对 default 令牌应找不到"
    );
    let body: Value = denied.json().await.expect("错误体应可解析");
    let msg = body["error"]["message"].as_str().expect("应有消息");
    assert!(
        msg.contains("不存在") || msg.contains("找不到"),
        "文案应为找不到/不存在，实际 {msg}"
    );
    assert!(!msg.contains("组"), "不得提分组，实际 {msg}");

    // 显式放进 default 后两边都能调。
    admin_json(
        &gw,
        reqwest::Method::PUT,
        "/model-groups/default",
        json!({ "name": "default", "models": [source_entry(first_channel_id(&gw).await, TEST_MODEL)] }),
    )
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let ok = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(
        ok.status(),
        reqwest::StatusCode::OK,
        "显式列入 default 后应可调"
    );
}

fn channel_body(name: &str, base_url: &str, models: Value, group: &str) -> Value {
    json!({
        "name": name,
        "protocol": "openai_chat",
        "base_url": base_url,
        "api_key": "sk-upstream",
        "models": models,
        "model_aliases": { "fast": "gpt-4o-mini" },
        "priority": 1,
        "weight": 1,
        "timeout_ms": 1000,
        "max_retries": 0,
        "enabled": true,
        "model_group": group
    })
}

/// 新建渠道时把清单与别名 key 并入自定义组；绑 default 不入组。
#[tokio::test]
async fn creating_channel_enrolls_callable_names_into_custom_group() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    let created_group = admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "coding", "models": [] }),
    )
    .await;
    assert_eq!(created_group.status(), reqwest::StatusCode::CREATED);

    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body(
            "enroll-ch",
            "http://127.0.0.1:9",
            json!(["gpt-4o", "gpt-4o-mini"]),
            "coding",
        ),
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);

    let list: Value = admin_get(&gw, "/model-groups")
        .await
        .json()
        .await
        .expect("组列表应可解析");
    let coding = group_named(&list, "coding");
    let models = coding["models"].as_array().expect("名单应为数组");
    let names: Vec<&str> = models.iter().filter_map(group_entry_name).collect();
    assert!(names.contains(&"gpt-4o"), "清单名应入组: {names:?}");
    assert!(names.contains(&"gpt-4o-mini"), "清单名应入组: {names:?}");
    assert!(names.contains(&"fast"), "别名 key 应入组: {names:?}");
    let channel_id = created.json::<Value>().await.expect("创建响应应可解析")["id"]
        .as_i64()
        .expect("应有 id");
    assert!(
        models
            .iter()
            .all(|item| { item["kind"] == "source" && item["channel_id"] == channel_id }),
        "入组条目应钉在该渠道: {models:?}"
    );

    let leftover = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body(
            "default-ch",
            "http://127.0.0.1:9",
            json!(["orphan"]),
            "default",
        ),
    )
    .await;
    assert_eq!(leftover.status(), reqwest::StatusCode::CREATED);
    let list: Value = admin_get(&gw, "/model-groups")
        .await
        .json()
        .await
        .expect("组列表应可解析");
    assert_eq!(group_named(&list, "default")["models"], json!([]));
}

/// 更新渠道只把新出现的可调用名入组；从渠道删模型不退组。
#[tokio::test]
async fn updating_channel_enrolls_only_new_names_and_keeps_removed() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "coding", "models": [] }),
    )
    .await;
    let keeper = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        json!({
            "name": "keeper",
            "protocol": "openai_chat",
            "base_url": "http://127.0.0.1:9",
            "api_key": "sk-upstream",
            "models": ["kept"],
            "model_aliases": {},
            "priority": 1,
            "weight": 1,
            "timeout_ms": 1000,
            "max_retries": 0,
            "enabled": true,
            "model_group": "coding"
        }),
    )
    .await;
    assert_eq!(keeper.status(), reqwest::StatusCode::CREATED);

    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        json!({
            "name": "enroll-upd",
            "protocol": "openai_chat",
            "base_url": "http://127.0.0.1:9",
            "api_key": "sk-upstream",
            "models": ["alpha"],
            "model_aliases": {},
            "priority": 1,
            "weight": 1,
            "timeout_ms": 1000,
            "max_retries": 0,
            "enabled": true,
            "model_group": "coding"
        }),
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let id = created.json::<Value>().await.expect("创建响应应可解析")["id"]
        .as_i64()
        .expect("应有 id");

    let updated = admin_json(
        &gw,
        reqwest::Method::PUT,
        &format!("/channels/{id}"),
        json!({
            "name": "enroll-upd",
            "protocol": "openai_chat",
            "base_url": "http://127.0.0.1:9",
            "api_key": "sk-upstream",
            "models": ["alpha", "beta"],
            "model_aliases": {},
            "priority": 1,
            "weight": 1,
            "timeout_ms": 1000,
            "max_retries": 0,
            "enabled": true,
            "model_group": "coding"
        }),
    )
    .await;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);

    let stripped = admin_json(
        &gw,
        reqwest::Method::PUT,
        &format!("/channels/{id}"),
        json!({
            "name": "enroll-upd",
            "protocol": "openai_chat",
            "base_url": "http://127.0.0.1:9",
            "api_key": "sk-upstream",
            "models": ["beta"],
            "model_aliases": {},
            "priority": 1,
            "weight": 1,
            "timeout_ms": 1000,
            "max_retries": 0,
            "enabled": true,
            "model_group": "coding"
        }),
    )
    .await;
    assert_eq!(stripped.status(), reqwest::StatusCode::OK);

    let list: Value = admin_get(&gw, "/model-groups")
        .await
        .json()
        .await
        .expect("组列表应可解析");
    let names: Vec<&str> = group_named(&list, "coding")["models"]
        .as_array()
        .expect("名单应为数组")
        .iter()
        .filter_map(group_entry_name)
        .collect();
    assert!(names.contains(&"kept"));
    assert!(names.contains(&"alpha"), "从渠道删模型不应退组: {names:?}");
    assert!(names.contains(&"beta"), "新增名应入组: {names:?}");
}

/// 删自定义组前把仍绑该组的渠道改回 default，避免外键挡住删除。
#[tokio::test]
async fn deleting_group_rebinds_channels_to_default() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": "coding", "models": [] }),
    )
    .await;
    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body("rebind-ch", "http://127.0.0.1:9", json!(["m"]), "coding"),
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let id = created.json::<Value>().await.expect("创建响应应可解析")["id"]
        .as_i64()
        .expect("应有 id");

    let deleted = admin_send(&gw, reqwest::Method::DELETE, "/model-groups/coding").await;
    assert_eq!(deleted.status(), reqwest::StatusCode::OK);

    let channels: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let channel = channels
        .as_array()
        .expect("应为数组")
        .iter()
        .find(|item| item["id"] == id)
        .expect("渠道应仍在");
    assert_eq!(channel["model_group"], "default");
}

/// 自定义组只钉某一渠道时，同名其它渠道不参与路由。
#[tokio::test]
async fn pinned_group_source_routes_only_that_channel() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let channels: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let first_id = channels
        .as_array()
        .expect("应为数组")
        .iter()
        .find(|item| item["name"] == "test-channel")
        .expect("应有 test-channel")["id"]
        .as_i64()
        .expect("应有 id");

    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        json!({
            "name": "pinned-ch",
            "protocol": "openai_chat",
            "base_url": gw.upstream.base_url(),
            "api_key": "sk-pinned",
            "models": [TEST_MODEL],
            "model_aliases": {},
            "priority": 2,
            "weight": 1,
            "timeout_ms": 1000,
            "max_retries": 0,
            "enabled": true,
            "model_group": "default"
        }),
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let pinned_id = created.json::<Value>().await.expect("创建响应应可解析")["id"]
        .as_i64()
        .expect("应有 id");
    let priced = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "channel_id": pinned_id,
            "model": TEST_MODEL,
            "input_micros": 1_000_000,
            "output_micros": 1_000_000,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(priced.status(), reqwest::StatusCode::CREATED);

    let grouped = admin_json(
        &gw,
        reqwest::Method::POST,
        "/model-groups",
        json!({
            "name": "coding",
            "models": [source_entry(pinned_id, TEST_MODEL)]
        }),
    )
    .await;
    assert_eq!(grouped.status(), reqwest::StatusCode::CREATED);

    let token_resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "coder", "limit_usd_micros": null, "enabled": true, "model_group": "coding" }),
    )
    .await;
    let token: Value = token_resp.json().await.expect("令牌应可解析");
    let coding_key = token["token_key"].as_str().expect("应有 key");
    admin_json(
        &gw,
        reqwest::Method::POST,
        &format!("/tokens/{coding_key}/balance"),
        json!({ "delta_usd_micros": 5_000_000 }),
    )
    .await;

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let ok = chat_request(&gw, coding_key, TEST_MODEL).await;
    assert_eq!(ok.status(), reqwest::StatusCode::OK, "钉渠道应可调");

    let logs: Value = admin_get(&gw, "/logs?page_size=1")
        .await
        .json()
        .await
        .expect("日志应可解析");
    assert_eq!(
        logs["items"][0]["channel"], "pinned-ch",
        "应打到钉死的渠道而非优先级更高的 test-channel"
    );
    assert_ne!(first_id, pinned_id);
}
