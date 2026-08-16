//! 管理 API 端到端黑盒测试：独立管理监听 + 静态 admin key 认证 + 资源 CRUD。
//!
//! 主接缝：测试内启动网关 + mock 上游 + 独立管理监听，断言外部可观察行为
//! （管理写库后的即时生效、认证拒绝、结构化错误、SQLite 持久化状态）。

mod common;

use base64::Engine as _;
use common::{TEST_ADMIN_KEY, TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use futures_util::StreamExt;
use kairos::store;
use serde_json::{Value, json};

/// 带 `TEST_ADMIN_KEY` 认证的 GET 请求。
async fn admin_get(gw: &TestGateway, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}{path}", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("管理请求应可达")
}

/// 按名查出渠道的库生成 id，供按 id 定位的端点使用。
async fn channel_id_by_name(gw: &TestGateway, name: &str) -> i64 {
    let list: Value = admin_get(gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let channel = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("渠道 {name} 应在列表"))
        .clone();
    channel["id"].as_i64().expect("列表应回传整数 id")
}

/// 渠道完整 JSON body：openai_chat 指向给定上游，其余字段取常规默认且启用。
fn channel_body(name: &str, base_url: String, models: Value) -> Value {
    json!({
        "name": name,
        "protocol": "openai_chat",
        "base_url": base_url,
        "api_key": "sk-upstream",
        "models": models,
        "model_aliases": {},
        "priority": 1,
        "weight": 1,
        "timeout_ms": 1000,
        "max_retries": 0,
        "enabled": true
    })
}

/// 带 `TEST_ADMIN_KEY` 认证、携带 JSON body 的请求。
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

/// 让请求路径成功落一条日志：设置上游成功行为并发一条 Chat 请求断言 200。
async fn make_successful_request(gw: &mut TestGateway) {
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

/// 读最近一条日志的两份 body 列。
async fn fetch_bodies(pool: &sqlx::SqlitePool) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    sqlx::query_as("SELECT request_body, response_body FROM request_log ORDER BY id DESC LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("应有请求日志")
}

/// 以 `TEST_ADMIN_KEY` 认证、携带 JSON body 的 PUT 请求。
async fn admin_put(gw: &TestGateway, path: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!("{}{path}", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&body)
        .send()
        .await
        .expect("管理请求应可达")
}

/// 协议监听上不应出现的管理 API 与 UI 路径（含 SPA 路由）。
const PROTOCOL_FORBIDDEN_ADMIN_GETS: &[&str] = &[
    "/",
    "/overview",
    "/login",
    "/tokens",
    "/channels",
    "/prices",
    "/model-groups",
    "/settings",
    "/logs",
    "/stats",
    "/stats/lifetime",
    "/token",
    "/channel",
    "/pricing",
    "/models",
    "/unified-models",
    "/config",
    "/requests",
    "/metrics",
];

/// 未配置管理监听时管理面整体关闭：协议监听上没有任何管理路由。
#[tokio::test]
async fn admin_not_configured_means_no_admin_routes() {
    let gw = TestGateway::start().await;
    let client = reqwest::Client::new();

    // 协议监听不应有管理路由或 UI；落到 fallback（404）。覆盖读/写、探测与 SPA 路径。
    for path in PROTOCOL_FORBIDDEN_ADMIN_GETS {
        let resp = client
            .get(format!("{}{path}", gw.base_url()))
            .send()
            .await
            .expect("协议监听应可请求");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND,
            "协议监听不应注册管理路由 {path}"
        );
    }
    let resp = client
        .post(format!("{}/tokens", gw.base_url()))
        .json(&json!({ "name": "x", "limit_usd_micros": null, "enabled": true }))
        .send()
        .await
        .expect("协议监听应可请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "协议监听不应接受管理写入"
    );
    let resp = client
        .post(format!("{}/channels/1/test", gw.base_url()))
        .send()
        .await
        .expect("协议监听应可请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "协议监听不应接受渠道探测"
    );
}

/// 未认证或错误 admin key 一律 401，且返回结构化错误。
#[tokio::test]
async fn unauthenticated_admin_request_is_401() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();

    // 缺头。
    let resp = client
        .get(format!("{}/tokens", gw.admin_base_url()))
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "unauthorized");

    // 错误 key。
    let resp = client
        .get(format!("{}/tokens", gw.admin_base_url()))
        .bearer_auth("wrong-key")
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// 令牌 CRUD 往返 + 写后即时生效：新建立刻可用、删除立刻失效；key 由系统生成。
#[tokio::test]
async fn token_crud_roundtrip_and_immediate_effect() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    // 新建令牌：不接受指定 key，系统生成 ks- 前缀 + 64 位字母数字。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "new-dev", "limit_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建令牌");
    let new_key = created["token_key"]
        .as_str()
        .expect("应返回生成的 key")
        .to_string();
    assert!(new_key.starts_with("ks-"), "系统生成的 key 应以 ks- 开头");
    assert_eq!(new_key.len(), 67, "key 应为前缀 + 64 位随机字符");
    assert!(
        new_key[3..].chars().all(|c| c.is_ascii_alphanumeric()),
        "随机部分应为大小写字母与数字"
    );

    // 列表反映新令牌。
    let list: Value = admin_get(&gw, "/tokens")
        .await
        .json()
        .await
        .expect("令牌列表应可解析");
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|t| t["token_key"] == new_key),
        "新建令牌应出现在列表"
    );

    // 新建令牌在请求路径即时可用：充值（余额调整属 04 票，测试内用相对量原语
    // 绕过）后请求成功。新建令牌已有零额余额行，故可被 `adjust_balance` 充值。
    let mut conn = gw.pool.acquire().await.expect("应能获取连接");
    store::adjust_balance(&mut conn, &new_key, 5_000_000)
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
        .delete(format!("{}/tokens/{new_key}", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
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
        !list
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["token_key"] == new_key),
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
        json!({ "name": "life", "limit_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建令牌");
    let life_key = created["token_key"]
        .as_str()
        .expect("应返回生成的 key")
        .to_string();
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
    store::adjust_balance(&mut conn, &life_key, 5_000_000)
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
        .find(|t| t["token_key"] == life_key)
        .cloned()
        .expect("新建令牌应在列表");
    assert!(
        life["last_used_at"].as_i64().unwrap_or(0) > 0,
        "请求后最后使用时间应刷新"
    );

    // 禁用后立即在认证处拒绝（401）。
    let resp = admin_put(
        &gw,
        &format!("/tokens/{life_key}"),
        json!({ "token_key": life_key, "name": "life", "limit_usd_micros": null, "enabled": false }),
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
        &format!("/tokens/{life_key}"),
        json!({ "token_key": life_key, "name": "life", "limit_usd_micros": null, "enabled": true }),
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

/// 渠道与价格写后即时生效：渠道可路由、价格增减即时反映在计费准入。
#[tokio::test]
async fn channel_and_price_immediate_effect() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();

    // 新建一个指向 mock 上游、服务新模型的渠道。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body(
            "mini-channel",
            gw.upstream.base_url(),
            json!(["gpt-4o-mini"]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let mini_id = created["id"].as_i64().expect("创建应回传库生成的 id");

    // 渠道已建但无价格：请求被计费准入拒绝（503）。
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o-mini").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "有渠道但无价格应 503"
    );

    // 补上价格：请求立即可用。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "model": "gpt-4o-mini",
            "input_micros": 150_000,
            "output_micros": 600_000,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o-mini").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "有价格后请求应立即可用"
    );

    // 禁用渠道：模型失去可用候选，请求 503（与无渠道同等处理）。
    let mut disabled = channel_body(
        "mini-channel",
        gw.upstream.base_url(),
        json!(["gpt-4o-mini"]),
    );
    disabled["enabled"] = json!(false);
    let resp = admin_put(&gw, &format!("/channels/{mini_id}"), disabled).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let updated: Value = resp.json().await.expect("应返回变更后渠道");
    assert_eq!(updated["enabled"], false, "PUT 回显应反映禁用");
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o-mini").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "禁用的渠道不应参与路由"
    );

    // 重新启用后立即可用。
    let resp = admin_put(
        &gw,
        &format!("/channels/{mini_id}"),
        channel_body(
            "mini-channel",
            gw.upstream.base_url(),
            json!(["gpt-4o-mini"]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o-mini").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "重新启用的渠道应立即可用"
    );

    // 删除价格：请求再次 503（无价格）。
    let resp = client
        .delete(format!("{}/prices/gpt-4o-mini", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可删除价格");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o-mini").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "删除价格后应恢复无价格拒绝"
    );

    // 删除渠道：模型失去路由，请求 503（无渠道）。
    let resp = client
        .delete(format!("{}/channels/{mini_id}", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可删除渠道");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o-mini").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "删除渠道后模型应无路由"
    );
}

/// 渠道 PUT 追加模型 ID（对应编辑器手动添加并保存）：保存前该 ID 不可路由；
/// 保存后未定价仍 503；补价后可调。
#[tokio::test]
async fn channel_appended_model_unpriced_is_503_then_callable() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body("manual-add", gw.upstream.base_url(), json!(["gpt-4o-mini"])),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let channel_id = created["id"].as_i64().expect("创建应回传库生成的 id");

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "manual-only").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "未写入渠道的模型应 503"
    );
    let body: Value = resp.json().await.expect("503 响应应可解析");
    let msg = body["error"]["message"].as_str().expect("消息应为字符串");
    assert!(msg.contains("渠道"), "保存前应按无渠道拒绝，实际 {msg}");

    let resp = admin_put(
        &gw,
        &format!("/channels/{channel_id}"),
        channel_body(
            "manual-add",
            gw.upstream.base_url(),
            json!(["gpt-4o-mini", "manual-only"]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "manual-only").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "有渠道但无价格应 503"
    );
    let body: Value = resp.json().await.expect("503 响应应可解析");
    let msg = body["error"]["message"].as_str().expect("消息应为字符串");
    assert!(
        msg.contains("价格"),
        "保存后未定价应按无价格拒绝，实际 {msg}"
    );

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "model": "manual-only",
            "input_micros": 150_000,
            "output_micros": 600_000,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "manual-only").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "定价后请求应立即可用"
    );
}

/// 渠道改名：按 id 定位的 PUT 携带新 name 即改名，id 保持稳定、即时可路由；
/// 新名已被占用返回 409，id 不存在返回 404。
#[tokio::test]
async fn channel_rename_moves_definition() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    // 先按名查出 seed 渠道的 id。
    let list: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let seed_channel = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "test-channel")
        .expect("seed 渠道应在列表")
        .clone();
    let channel_id = seed_channel["id"].as_i64().expect("列表应回传库生成的 id");

    // 改名 test-channel → renamed-channel：回显为新名，id 保持不变。
    let resp = admin_put(
        &gw,
        &format!("/channels/{channel_id}"),
        channel_body(
            "renamed-channel",
            gw.upstream.base_url(),
            json!([TEST_MODEL]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let updated: Value = resp.json().await.expect("应返回变更后渠道");
    assert_eq!(updated["name"], "renamed-channel", "PUT 回显应为新名");
    assert_eq!(updated["id"], channel_id, "改名不应改变 id");

    // 列表中新名在、旧名消失；改名后立即可路由。
    let list: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"renamed-channel"), "新名应在列表");
    assert!(!names.contains(&"test-channel"), "旧名应被移除");
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "改名后模型应立即可路由"
    );

    // 改成已存在的名字 → 409，且不产生副作用。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body(
            "second-channel",
            gw.upstream.base_url(),
            json!(["other-model"]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let resp = admin_put(
        &gw,
        &format!("/channels/{channel_id}"),
        channel_body(
            "second-channel",
            gw.upstream.base_url(),
            json!([TEST_MODEL]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "冲突改名不应影响原渠道"
    );

    // id 不存在 → 404。
    let resp = admin_put(
        &gw,
        "/channels/999999",
        channel_body("whatever", gw.upstream.base_url(), json!([TEST_MODEL])),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

/// 非法输入返回结构化错误；失败与冲突的写不污染库与快照。
#[tokio::test]
async fn invalid_input_returns_structured_error_and_leaves_state() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 畸形 JSON body → 400 结构化错误。
    let resp = client
        .post(format!("{admin}/tokens"))
        .bearer_auth(TEST_ADMIN_KEY)
        .header("content-type", "application/json")
        .body("{ not json")
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");

    // 缺必填字段 → 400（serde 拒绝）。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "x" }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 指定 token_key → 400：key 只由系统生成，创建契约不接受该字段。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "token_key": "ks-custom", "name": "x", "limit_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 负单价 → 400（语义校验）。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "model": "m-neg",
            "input_micros": -1,
            "output_micros": 0,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 渠道权重为 0 → 400：权重是加权随机除数，API 直写也须被拒。
    let mut zero_weight = channel_body("zero-weight", gw.upstream.base_url(), json!([TEST_MODEL]));
    zero_weight["weight"] = json!(0);
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", zero_weight).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 重复新建 → 409，且不覆盖原资源（令牌 key 系统生成、创建不会冲突，以渠道为例）。
    let before: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let mut conflict = channel_body("test-channel", gw.upstream.base_url(), json!([TEST_MODEL]));
    conflict["api_key"] = json!("sk-other");
    conflict["priority"] = json!(9);
    conflict["weight"] = json!(9);
    conflict["timeout_ms"] = json!(1);
    conflict["max_retries"] = json!(9);
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", conflict).await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let after: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    assert_eq!(before, after, "冲突写不应改变库与快照");

    // 删除不存在的资源 → 404。
    let resp = client
        .delete(format!("{admin}/tokens/does-not-exist"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

/// 未知字段直接拒绝（deny_unknown_fields）：字段拼写错误不静默忽略。
#[tokio::test]
async fn unknown_field_is_rejected() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let mut typo = channel_body("typo-channel", gw.upstream.base_url(), json!([]));
    typo["protcol"] = json!("openai_chat");
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", typo).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");
}

/// 已配置管理面时协议监听仍不注册管理路由或 UI；管理监听不提供协议端点。
///
/// admin key 与下游令牌体系隔离：下游令牌调管理面 401，admin key 当下游令牌 401。
#[tokio::test]
async fn admin_surface_is_isolated_from_protocol_surface() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();

    // 协议监听无管理路由、无 UI（即使管理面已启动）。
    for path in PROTOCOL_FORBIDDEN_ADMIN_GETS {
        let resp = client
            .get(format!("{}{path}", gw.base_url()))
            .bearer_auth(TEST_ADMIN_KEY)
            .send()
            .await
            .expect("协议监听应可请求");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND,
            "协议监听即使已配置管理面也不应注册 {path}"
        );
    }
    let resp = client
        .post(format!("{}/channels/1/test", gw.base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("协议监听应可请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "协议监听不应接受渠道探测"
    );

    // 管理监听无协议端点。
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("管理监听应可请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "管理监听不应提供协议端点"
    );

    // 下游令牌不能调管理 API。
    let resp = client
        .get(format!("{}/tokens", gw.admin_base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "下游令牌不应能认证管理面"
    );

    // admin key 不能当下游令牌。
    let resp = chat_request(&gw, TEST_ADMIN_KEY, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "admin key 不应能作为下游令牌"
    );
}

/// 已准入的在途请求按准入时刻快照走完：流开始后改价格，在途结算仍用旧单价，新请求用新单价。
#[tokio::test]
async fn inflight_request_keeps_admission_snapshot() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    let text_frame = concat!(
        "data: {\"id\":\"chatcmpl-if\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n\n"
    );
    // 1M input tokens × 2.5 USD/1M = 2_500_000 micro-USD（准入时刻 gpt-4o 单价）。
    let usage_frame = concat!(
        "data: {\"id\":\"chatcmpl-if\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],",
        "\"usage\":{\"prompt_tokens\":1000000,\"completion_tokens\":0,\"total_tokens\":1000000}}\n\n"
    );
    gw.upstream.set_behavior(UpstreamBehavior::DelayedRawSse {
        chunks: vec![
            text_frame.as_bytes().to_vec(),
            usage_frame.as_bytes().to_vec(),
        ],
        delay_ms: 400,
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("在途请求应能发出");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let mut stream = resp.bytes_stream();
    let first_chunk = stream.next().await.expect("应有首块").expect("首块应可读");
    assert!(!first_chunk.is_empty(), "准入后应立即收到上游首块");

    // 准入后改价：在途结算必须仍按旧快照单价，证明持有的是准入时刻快照而非当前快照。
    let resp = admin_put(
        &gw,
        &format!("/prices/{TEST_MODEL}"),
        json!({
            "model": TEST_MODEL,
            "input_micros": 9_000_000,
            "output_micros": 10_000_000,
            "cache_read_micros": 1_250_000,
            "cache_write_micros": 10_000_000
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let mut rest = first_chunk.to_vec();
    while let Some(chunk) = stream.next().await {
        rest.extend_from_slice(&chunk.expect("在途流应可读"));
    }
    let body = String::from_utf8_lossy(&rest);
    assert!(body.contains("Hel"), "在途流应保留准入后的上游字节");
    assert!(body.contains("finish_reason"), "在途流不应被快照替换腰斩");

    let mut inflight_cost = None;
    for _ in 0..100 {
        inflight_cost = sqlx::query_as::<_, (i64,)>(
            "SELECT cost_usd_micros FROM request_log ORDER BY id DESC LIMIT 1",
        )
        .fetch_optional(&gw.pool)
        .await
        .expect("应能查询请求日志");
        if inflight_cost.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        inflight_cost.expect("在途请求应落结算").0,
        2_500_000,
        "在途请求应按准入时刻单价结算"
    );

    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-new", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1000000, "completion_tokens": 0, "total_tokens": 1000000}
    })));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let new_cost: (i64,) =
        sqlx::query_as("SELECT cost_usd_micros FROM request_log ORDER BY id DESC LIMIT 1")
            .fetch_one(&gw.pool)
            .await
            .expect("新请求应落结算");
    assert_eq!(new_cost.0, 9_000_000, "新请求应按替换后单价结算");
}

/// 进程重启后从数据库加载全部运行时资源：令牌/渠道/价格/设置都不丢。
#[tokio::test]
async fn runtime_resources_survive_process_restart() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    // 经管理 API 写入：新令牌 + 充值 + 收紧 body 上限。原实例保持存活以持有临时库文件。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "restart", "limit_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建令牌");
    let restart_key = created["token_key"]
        .as_str()
        .expect("应返回生成的 key")
        .to_string();
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/tokens/{restart_key}/balance",
            gw.admin_base_url()
        ))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({ "delta_usd_micros": 5_000_000 }))
        .send()
        .await
        .expect("应可充值");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = admin_put(
        &gw,
        "/settings",
        json!({ "full_body": false, "max_request_bytes": 2048 }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 单独写入的渠道+价格：重启后必须仍能路由该模型，证明渠道/价格也从库加载。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body(
            "restart-channel",
            gw.upstream.base_url(),
            json!(["restart-only"]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "model": "restart-only",
            "input_micros": 111_000,
            "output_micros": 222_000,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    let base2 = gw.spawn_reloaded_protocol().await;

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = reqwest::Client::new()
        .post(format!("{base2}/v1/chat/completions"))
        .bearer_auth(&restart_key)
        .json(&json!({
            "model": "restart-only",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("重启后应能请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "重启后应从库加载新令牌、渠道与价格并可用"
    );

    // 设置也从库加载：body 上限 2048 仍生效。
    let oversized_body = "x".repeat(3000);
    let resp = reqwest::Client::new()
        .post(format!("{base2}/v1/chat/completions"))
        .bearer_auth(&restart_key)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": oversized_body }]
        }))
        .send()
        .await
        .expect("重启后应能请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        "重启后设置上限应从库恢复"
    );
}

/// 首次部署空库：经管理 API 初始化令牌、渠道、价格后请求路径自洽可用。
#[tokio::test]
async fn empty_db_bootstraps_via_admin_api() {
    let mut gw = TestGateway::start_with_admin(common::empty_seed).await;

    let tokens: Value = admin_get(&gw, "/tokens")
        .await
        .json()
        .await
        .expect("空库令牌列表应可解析");
    assert_eq!(tokens.as_array().map(Vec::len), Some(0), "空库应无令牌");

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "boot", "limit_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建令牌");
    let boot_key = created["token_key"]
        .as_str()
        .expect("应返回生成的 key")
        .to_string();

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body("boot-channel", gw.upstream.base_url(), json!([TEST_MODEL])),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "model": TEST_MODEL,
            "input_micros": 2_500_000,
            "output_micros": 10_000_000,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    let resp = reqwest::Client::new()
        .post(format!("{}/tokens/{boot_key}/balance", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({ "delta_usd_micros": 5_000_000 }))
        .send()
        .await
        .expect("应可充值");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, &boot_key, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "空库经管理 API 初始化后请求路径应可用"
    );
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
        json!({ "name": "cycle", "limit_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建令牌");
    let cycle_key = created["token_key"]
        .as_str()
        .expect("应返回生成的 key")
        .to_string();
    let mut conn = gw.pool.acquire().await.expect("应能获取连接");
    store::adjust_balance(&mut conn, &cycle_key, 5_000_000)
        .await
        .expect("应能充值");
    drop(conn);
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, &cycle_key, TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 删除后余额行一并清理：库内不再残留该 key 的余额记录。
    let resp = client
        .delete(format!("{}/tokens/{cycle_key}", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
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

/// 设置读写：缺省读回、写后返回变更后设置、body 上限即时生效（新上限立刻拦截超限请求）。
#[tokio::test]
async fn settings_write_takes_effect_immediately() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 缺省设置：full_body 关闭、body 上限为正。
    let resp = client
        .get(format!("{admin}/settings"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可读设置");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let settings: Value = resp.json().await.expect("设置应可解析");
    assert_eq!(settings["full_body"], false);
    assert!(settings["max_request_bytes"].as_u64().unwrap() > 0);

    // 写设置：body 上限压到 100 字节，返回变更后设置。
    let resp = admin_put(
        &gw,
        "/settings",
        json!({ "full_body": false, "max_request_bytes": 100 }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let settings: Value = resp.json().await.expect("设置应可解析");
    assert_eq!(settings["max_request_bytes"], 100);

    // 新上限立即生效：超限请求被 413 拦截且不出站。
    let big = "x".repeat(2000);
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({ "model": TEST_MODEL, "messages": [{ "role": "user", "content": big }] }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        "新 body 上限应立即拦截超限请求"
    );
    assert!(gw.upstream.received().is_empty(), "超限不应出站");
}

/// 设置写入开启 full_body：后续请求的完整 body 落库，且 /logs 以 base64 返回 body。
#[tokio::test]
async fn settings_toggle_full_body_enables_body_logging() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 开启 full_body。
    let resp = admin_put(
        &gw,
        "/settings",
        json!({ "full_body": true, "max_request_bytes": 100_000_000 }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 发一条成功请求。
    make_successful_request(&mut gw).await;

    // 日志应带 body。
    let (request_body, response_body) = fetch_bodies(&gw.pool).await;
    assert!(request_body.is_some(), "开启 full_body 后应落请求 body");
    assert!(response_body.is_some(), "开启 full_body 后应落响应 body");

    // /logs 的 body 以 base64 返回。
    let resp = client
        .get(format!("{admin}/logs?page_size=1"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可查日志");
    let page: Value = resp.json().await.expect("日志应可解析");
    let entry = &page["items"][0];
    let request_b64 = entry["request_body"]
        .as_str()
        .expect("request_body 应为字符串");
    let decoded = base64::prelude::BASE64_STANDARD
        .decode(request_b64)
        .expect("request_body 应为合法 base64");
    assert!(
        String::from_utf8_lossy(&decoded).contains("model"),
        "解码后的请求体应含 model 字段"
    );
}

/// 余额调整为相对量：扣减至零余额 → 计费准入拒绝（402）；充值后恢复可用。
#[tokio::test]
async fn balance_adjustment_reflected_in_admission() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 初始余额 5 USD = 5_000_000 micros，扣减至 0。
    let resp = client
        .post(format!("{admin}/tokens/{TEST_TOKEN_KEY}/balance"))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({ "delta_usd_micros": -5_000_000 }))
        .send()
        .await
        .expect("应可调整余额");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let balance: Value = resp.json().await.expect("余额应可解析");
    assert_eq!(balance["balance_usd_micros"], 0);
    assert_eq!(balance["token_key"], TEST_TOKEN_KEY);

    // 零余额：计费准入拒绝。
    let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::PAYMENT_REQUIRED,
        "零余额应 402"
    );

    // 充值后恢复可用。
    let resp = client
        .post(format!("{admin}/tokens/{TEST_TOKEN_KEY}/balance"))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({ "delta_usd_micros": 5_000_000 }))
        .send()
        .await
        .expect("应可充值");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    make_successful_request(&mut gw).await;
}

/// 修改令牌其他属性不重置余额：充值 → 改 name → 余额保持。
#[tokio::test]
async fn token_attr_update_does_not_reset_balance() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 充值 1 USD（初始 5 USD → 6 USD）。
    let resp = client
        .post(format!("{admin}/tokens/{TEST_TOKEN_KEY}/balance"))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({ "delta_usd_micros": 1_000_000 }))
        .send()
        .await
        .expect("应可充值");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 修改令牌其他属性（name）。
    let resp = admin_put(
        &gw,
        &format!("/tokens/{TEST_TOKEN_KEY}"),
        json!({ "token_key": TEST_TOKEN_KEY, "name": "renamed", "limit_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 余额不变：以 delta 0 读回应仍为 6_000_000。
    let resp = client
        .post(format!("{admin}/tokens/{TEST_TOKEN_KEY}/balance"))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({ "delta_usd_micros": 0 }))
        .send()
        .await
        .expect("应可读余额");
    let balance: Value = resp.json().await.expect("余额应可解析");
    assert_eq!(
        balance["balance_usd_micros"], 6_000_000,
        "修改令牌属性不应重置余额"
    );
}

/// 日志分页与过滤：全量、按模型过滤、分页取数正确，时间倒序。
#[tokio::test]
async fn logs_paginate_and_filter() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 生成 3 条成功请求日志。
    for _ in 0..3 {
        make_successful_request(&mut gw).await;
    }

    // 全量：total 反映日志总数，默认每页 20 条。
    let resp = client
        .get(format!("{admin}/logs"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可查日志");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 3);
    assert_eq!(page["items"].as_array().unwrap().len(), 3);
    assert_eq!(page["items"][0]["model"], TEST_MODEL);
    assert_eq!(
        page["items"][0]["outbound_model"], TEST_MODEL,
        "无别名时出站名等于入站名"
    );

    // 按模型过滤：命中全部 3 条。
    let resp = client
        .get(format!("{admin}/logs?model={TEST_MODEL}"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可过滤日志");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 3);
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["model"] == TEST_MODEL)
    );

    // 按令牌过滤：命中全部 3 条（同一令牌）。
    let resp = client
        .get(format!("{admin}/logs?token_key={TEST_TOKEN_KEY}"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可过滤日志");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 3);

    // 综合关键字：模型子串命中全部 3 条。
    let resp = client
        .get(format!("{admin}/logs?keyword={TEST_MODEL}"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可过滤日志");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 3);

    // 综合关键字：无命中时 total 为 0。
    let resp = client
        .get(format!("{admin}/logs?keyword=no-such-keyword"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可过滤日志");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 0);

    // 分页：page_size=2 → 第一页 2 条、第二页 1 条。
    let resp = client
        .get(format!("{admin}/logs?page=1&page_size=2"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可分页");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["page_size"], 2);
    assert_eq!(page["total"], 3);

    let resp = client
        .get(format!("{admin}/logs?page=2&page_size=2"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可分页");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["items"].as_array().unwrap().len(), 1);

    // 分页 + 时间过滤：from_created_at 远在过去 → 仍命中全部。
    let resp = client
        .get(format!("{admin}/logs?from_created_at=1&page_size=2"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可过滤日志");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 3);
}

/// 别名请求：协议响应回显入站短名；管理日志列表=入站、出站字段=上游真名。
#[tokio::test]
async fn alias_logs_inbound_and_outbound_model() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-alias-log",
        "object": "chat.completion",
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "ok" },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })));

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "fast").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["model"], "fast", "协议响应 model 应回显入站名");

    let page: Value = admin_get(&gw, "/logs?page_size=1")
        .await
        .json()
        .await
        .expect("日志应可解析");
    let entry = &page["items"][0];
    assert_eq!(entry["model"], "fast", "列表字段为入站别名");
    assert_eq!(entry["outbound_model"], "gpt-4o-mini", "详情字段为出站真名");
    assert_eq!(entry["channel"], "test-channel");
}

/// 新端点的非法输入返回结构化错误：设置上限为 0、未知设置字段、余额调不存在令牌、
/// 日志非法查询参数。
#[tokio::test]
async fn new_endpoints_structured_errors() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 设置：max_request_bytes=0 → 400。
    let resp = admin_put(
        &gw,
        "/settings",
        json!({ "full_body": false, "max_request_bytes": 0 }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");

    // 设置：未知字段 → 400（deny_unknown_fields）。
    let resp = admin_put(
        &gw,
        "/settings",
        json!({ "full_body": false, "max_request_bytes": 100, "bogus": 1 }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 余额：不存在的令牌 → 404。
    let resp = client
        .post(format!("{admin}/tokens/nope/balance"))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({ "delta_usd_micros": 100 }))
        .send()
        .await
        .expect("应可调整余额");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // 日志：非法查询参数 → 400 结构化错误。
    let resp = client
        .get(format!("{admin}/logs?page=abc"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可查日志");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");

    // 日志：未知查询参数 → 400（deny_unknown_fields，拼写错误不静默返回未过滤结果）。
    let resp = client
        .get(format!("{admin}/logs?tokne_key=sk-x"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可查日志");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

// --- 01 票：stats 聚合与渠道连通性探测 ---

const MS_PER_DAY: i64 = 86_400_000;

/// 播种日志时需要变化的字段；其余列用固定测试缺省。
struct SeededLog {
    created_at: i64,
    model: &'static str,
    channel: &'static str,
    status_code: i64,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd_micros: i64,
}

/// 播种一条请求日志，字段按断言需要的口径填写。
async fn seed_log(pool: &sqlx::SqlitePool, log: SeededLog) {
    store::insert_request_log(
        pool,
        &store::RequestLog {
            id: 0,
            created_at: log.created_at,
            token_name: "dev".to_string(),
            token_key: TEST_TOKEN_KEY.to_string(),
            inbound_protocol: "openai_chat".to_string(),
            model: log.model.to_string(),
            outbound_model: None,
            channel: log.channel.to_string(),
            status_code: log.status_code,
            latency_ms: 10,
            input_tokens: log.input_tokens,
            output_tokens: log.output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            price: kairos::core::billing::PriceSnapshot::default(),
            cost_usd_micros: log.cost_usd_micros,
            request_body: None,
            response_body: None,
        },
    )
    .await
    .expect("应能播种请求日志");
}

/// 把 unix 毫秒格式化为 UTC 日历日（YYYY-MM-DD），与 SQLite `unixepoch` 口径一致。
async fn utc_date(pool: &sqlx::SqlitePool, millis: i64) -> String {
    sqlx::query_scalar("SELECT date(? / 1000, 'unixepoch')")
        .bind(millis)
        .fetch_one(pool)
        .await
        .expect("应能格式化 UTC 日期")
}

/// 播种一组跨日、含失败结算的日志，覆盖默认 7 天窗与超窗条目。
async fn seed_stats_logs(pool: &sqlx::SqlitePool, now: i64) -> (i64, i64, i64) {
    let today_start = now.div_euclid(MS_PER_DAY) * MS_PER_DAY;
    let yesterday = today_start - MS_PER_DAY;
    let eight_days_ago = today_start - 8 * MS_PER_DAY;

    // 今日两条成功：gpt-4o / test-channel。
    seed_log(
        pool,
        SeededLog {
            created_at: today_start + 1,
            model: TEST_MODEL,
            channel: "test-channel",
            status_code: 200,
            input_tokens: 10,
            output_tokens: 4,
            cost_usd_micros: 1_000,
        },
    )
    .await;
    seed_log(
        pool,
        SeededLog {
            created_at: today_start + 2,
            model: TEST_MODEL,
            channel: "test-channel",
            status_code: 200,
            input_tokens: 20,
            output_tokens: 8,
            cost_usd_micros: 2_000,
        },
    )
    .await;
    // 今日一条失败：费用不应计入（仅成功结算）。
    seed_log(
        pool,
        SeededLog {
            created_at: today_start + 3,
            model: TEST_MODEL,
            channel: "test-channel",
            status_code: 500,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd_micros: 999_999,
        },
    )
    .await;
    // 昨日一条成功：另一模型/渠道，用于分布。
    seed_log(
        pool,
        SeededLog {
            created_at: yesterday + 1,
            model: "gpt-4o-mini",
            channel: "other-channel",
            status_code: 200,
            input_tokens: 5,
            output_tokens: 1,
            cost_usd_micros: 500,
        },
    )
    .await;
    // 8 天前一条成功：默认 7 天窗应排除，夹取到 90 天后应纳入。
    seed_log(
        pool,
        SeededLog {
            created_at: eight_days_ago + 1,
            model: TEST_MODEL,
            channel: "test-channel",
            status_code: 200,
            input_tokens: 1,
            output_tokens: 1,
            cost_usd_micros: 100,
        },
    )
    .await;

    (today_start, yesterday, eight_days_ago)
}

/// `/stats` 汇总与逐日序列与播种数据精确一致；失败行费用不计入。
#[tokio::test]
async fn stats_aggregates_seeded_logs_with_success_only_cost() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let now = common::unix_millis();
    let (today_start, yesterday, _) = seed_stats_logs(&gw.pool, now).await;
    let today = utc_date(&gw.pool, today_start).await;
    let yesterday_date = utc_date(&gw.pool, yesterday).await;

    let resp = admin_get(&gw, "/stats").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("stats 应可解析");

    let summary = &body["summary"];
    assert_eq!(
        summary["request_count"], 4,
        "默认 7 天窗应含今日 3 + 昨日 1"
    );
    assert_eq!(summary["success_count"], 3);
    assert_eq!(summary["input_tokens"], 35);
    assert_eq!(summary["output_tokens"], 13);
    assert_eq!(
        summary["cost_usd_micros"], 3500,
        "失败行 999999 微元不应计入费用"
    );
    assert_eq!(summary["token_count"], 1, "资源表令牌数");
    assert_eq!(summary["channel_count"], 1, "资源表渠道数");

    let daily = body["daily"].as_array().expect("应有逐日序列");
    assert_eq!(daily.len(), 7, "缺省 days=7");
    let today_point = daily.iter().find(|p| p["date"] == today).expect("应含今日");
    assert_eq!(today_point["request_count"], 3);
    assert_eq!(today_point["input_tokens"], 30);
    assert_eq!(today_point["output_tokens"], 12);
    assert_eq!(today_point["cost_usd_micros"], 3000);
    let yesterday_point = daily
        .iter()
        .find(|p| p["date"] == yesterday_date)
        .expect("应含昨日");
    assert_eq!(yesterday_point["request_count"], 1);
    assert_eq!(yesterday_point["input_tokens"], 5);
    assert_eq!(yesterday_point["output_tokens"], 1);
    assert_eq!(yesterday_point["cost_usd_micros"], 500);
    let zero_days = daily.iter().filter(|p| p["request_count"] == 0).count();
    assert_eq!(zero_days, 5, "无流量的日历日应补零");

    let by_model = body["by_model"].as_array().expect("应有模型分布");
    let gpt4o = by_model
        .iter()
        .find(|p| p["model"] == TEST_MODEL)
        .expect("应有 gpt-4o");
    assert_eq!(gpt4o["request_count"], 3);
    assert_eq!(gpt4o["cost_usd_micros"], 3000);
    let mini = by_model
        .iter()
        .find(|p| p["model"] == "gpt-4o-mini")
        .expect("应有 gpt-4o-mini");
    assert_eq!(mini["request_count"], 1);
    assert_eq!(mini["cost_usd_micros"], 500);

    let by_channel = body["by_channel"].as_array().expect("应有渠道分布");
    let test_ch = by_channel
        .iter()
        .find(|p| p["channel"] == "test-channel")
        .expect("应有 test-channel");
    assert_eq!(test_ch["request_count"], 3);
    assert_eq!(test_ch["cost_usd_micros"], 3000);
    let other = by_channel
        .iter()
        .find(|p| p["channel"] == "other-channel")
        .expect("应有 other-channel");
    assert_eq!(other["request_count"], 1);
    assert_eq!(other["cost_usd_micros"], 500);
}

/// `days` 非法非数字 → 400；0 与超大值夹取；未知查询参数拒绝。
#[tokio::test]
async fn stats_clamps_days_and_rejects_invalid_query() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let now = common::unix_millis();
    let _ = seed_stats_logs(&gw.pool, now).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 非数字 → 400 结构化错误。
    let resp = client
        .get(format!("{admin}/stats?days=abc"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可请求 stats");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");

    // 未知查询参数 → 400。
    let resp = client
        .get(format!("{admin}/stats?dayz=7"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可请求 stats");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // days=0 夹取为 1：今日按 UTC 小时共 24 点。
    let resp = admin_get(&gw, "/stats?days=0").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("stats 应可解析");
    let daily = body["daily"].as_array().expect("应有趋势序列");
    assert_eq!(daily.len(), 24, "1 天窗应为 24 个小时桶");
    let first = daily[0]["date"].as_str().expect("应有小时标签");
    assert!(
        first.ends_with("T00:00:00Z"),
        "首个小时桶应为 UTC 0 点，实际 {first}"
    );
    assert_eq!(daily[0]["request_count"], 3, "今日 3 条都落在 0 点桶");
    assert_eq!(body["summary"]["request_count"], 3, "1 天窗只有今日 3 条");
    assert_eq!(body["summary"]["cost_usd_micros"], 3000);

    // 超大值夹取为 90：8 天前那条进入窗口。
    let resp = admin_get(&gw, "/stats?days=99999").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("stats 应可解析");
    assert_eq!(body["daily"].as_array().unwrap().len(), 90);
    assert_eq!(body["summary"]["request_count"], 5);
    assert_eq!(body["summary"]["cost_usd_micros"], 3600);
}

/// `/stats/lifetime` 为全量累计，含默认 7 天窗外的条目；失败行费用不计入。
#[tokio::test]
async fn stats_lifetime_aggregates_all_seeded_logs() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let now = common::unix_millis();
    let _ = seed_stats_logs(&gw.pool, now).await;

    let resp = admin_get(&gw, "/stats/lifetime").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("lifetime stats 应可解析");
    assert_eq!(body["request_count"], 5, "全量应含 8 天前那条");
    assert_eq!(
        body["cost_usd_micros"], 3600,
        "失败行 999999 微元不应计入费用"
    );
    assert_eq!(body["total_tokens"], 50);

    let windowed = admin_get(&gw, "/stats?days=7").await;
    assert_eq!(windowed.status(), reqwest::StatusCode::OK);
    let windowed_body: Value = windowed.json().await.expect("stats 应可解析");
    assert_eq!(windowed_body["summary"]["request_count"], 4);
    assert_eq!(body["request_count"], 5);
}

/// 渠道探测成功：可达、200、有延迟；出站为非流式极小请求；不经计费、不落日志。
#[tokio::test]
async fn channel_probe_success_skips_billing_and_logging() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));

    let balance_before: (i64,) =
        sqlx::query_as("SELECT balance_usd_micros FROM token_balance WHERE token_key = ?")
            .bind(TEST_TOKEN_KEY)
            .fetch_one(&gw.pool)
            .await
            .expect("应有余额");
    let logs_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM request_log")
        .fetch_one(&gw.pool)
        .await
        .expect("应能统计日志");

    let channel_id = channel_id_by_name(&gw, "test-channel").await;
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/channels/{channel_id}/test",
            gw.admin_base_url()
        ))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({ "model": TEST_MODEL }))
        .send()
        .await
        .expect("应可探测渠道");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("探测结果应可解析");
    assert_eq!(body["reachable"], true);
    assert_eq!(body["timed_out"], false);
    assert_eq!(body["status_code"], 200);
    assert!(
        body["latency_ms"].as_u64().unwrap() < 5_000,
        "成功探测延迟应在合理范围"
    );
    assert!(body["error"].is_null() || body.get("error").is_none());
    assert!(
        body["upstream_body"]
            .as_str()
            .map(|s| s.contains("Hello"))
            .unwrap_or(false),
        "成功应带回上游响应摘要"
    );

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1, "探测应向渠道发一条出站请求");
    assert_eq!(received[0]["model"], TEST_MODEL, "应使用 models 首个模型");
    assert_eq!(received[0]["max_tokens"], 1, "应为极小 max_tokens");
    assert!(
        received[0].get("stream").is_none() || received[0]["stream"] == false,
        "探测应为非流式"
    );

    let balance_after: (i64,) =
        sqlx::query_as("SELECT balance_usd_micros FROM token_balance WHERE token_key = ?")
            .bind(TEST_TOKEN_KEY)
            .fetch_one(&gw.pool)
            .await
            .expect("应有余额");
    assert_eq!(balance_before, balance_after, "探测不应扣减令牌余额");
    let logs_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM request_log")
        .fetch_one(&gw.pool)
        .await
        .expect("应能统计日志");
    assert_eq!(logs_before, logs_after, "探测不应落 request_log");
}

/// 渠道探测失败（上游 4xx）：可达但带状态码与错误摘要；仍不落日志。
#[tokio::test]
async fn channel_probe_upstream_error_is_reachable_with_status() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Status(401));

    let logs_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM request_log")
        .fetch_one(&gw.pool)
        .await
        .expect("应能统计日志");

    let channel_id = channel_id_by_name(&gw, "test-channel").await;
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/channels/{channel_id}/test",
            gw.admin_base_url()
        ))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({ "model": TEST_MODEL }))
        .send()
        .await
        .expect("应可探测渠道");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("探测结果应可解析");
    assert_eq!(body["reachable"], true, "拿到 HTTP 响应即可达");
    assert_eq!(body["timed_out"], false);
    assert_eq!(body["status_code"], 401);
    assert!(
        body["error"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "失败应带错误摘要"
    );

    let logs_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM request_log")
        .fetch_one(&gw.pool)
        .await
        .expect("应能统计日志");
    assert_eq!(logs_before, logs_after, "失败探测也不落 request_log");
}

/// 渠道探测超时：不可达、无状态码、错误摘要标识超时；沿用渠道 timeout_ms。
#[tokio::test]
async fn channel_probe_timeout_is_unreachable() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    let mut timeout_body = channel_body(
        "timeout-channel",
        gw.upstream.base_url(),
        json!([TEST_MODEL]),
    );
    timeout_body["timeout_ms"] = json!(200);
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", timeout_body).await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let timeout_id = created["id"].as_i64().expect("创建应回传整数 id");

    gw.upstream.set_behavior(UpstreamBehavior::Hang);

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/channels/{timeout_id}/test",
            gw.admin_base_url()
        ))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({ "model": TEST_MODEL }))
        .send()
        .await
        .expect("应可探测渠道");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("探测结果应可解析");
    assert_eq!(body["reachable"], false);
    assert_eq!(body["timed_out"], true);
    assert!(body["status_code"].is_null());
    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains("超时") || error.to_ascii_lowercase().contains("timeout"),
        "超时摘要应可识别，实际: {error}"
    );
    let latency = body["latency_ms"].as_u64().unwrap_or(0);
    assert!(
        (100..3_000).contains(&latency),
        "延迟应贴近渠道 timeout_ms=200，实际 {latency}"
    );
}

/// 探测指定模型：出站使用请求体中的 model，拒绝清单外模型。
#[tokio::test]
async fn channel_probe_uses_requested_model_and_rejects_unknown() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));

    let mut extra = channel_body(
        "multi-model",
        gw.upstream.base_url(),
        json!([TEST_MODEL, "gpt-4o-mini"]),
    );
    extra["model_aliases"] = json!({ "mini": "gpt-4o-mini" });
    extra["models"] = json!([TEST_MODEL, "gpt-4o-mini", "mini"]);
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", extra).await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let channel_id = created["id"].as_i64().expect("创建应回传整数 id");

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        &format!("/channels/{channel_id}/test"),
        json!({ "model": "gpt-4o-mini" }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    assert_eq!(received.last().unwrap()["model"], "gpt-4o-mini");

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        &format!("/channels/{channel_id}/test"),
        json!({ "model": "not-in-list" }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");
}

/// 仅别名在清单时：请求主模型名，出站仍用主模型名。
#[tokio::test]
async fn channel_probe_alias_only_uses_canonical() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));

    let mut alias_only = channel_body("alias-only", gw.upstream.base_url(), json!(["mini"]));
    alias_only["model_aliases"] = json!({ "mini": TEST_MODEL });
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", alias_only).await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let channel_id = created["id"].as_i64().expect("创建应回传整数 id");

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        &format!("/channels/{channel_id}/test"),
        json!({ "model": TEST_MODEL }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    assert_eq!(received.last().unwrap()["model"], TEST_MODEL);
}

/// 拉取上游模型列表：渠道草稿无需已保存，解析 `data[].id` 并保持上游顺序。
#[tokio::test]
async fn list_upstream_models_parses_draft_and_keeps_order() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "object": "list",
        "data": [
            { "id": "gpt-4o", "object": "model" },
            { "id": "claude-3-5-sonnet", "type": "model" },
            { "object": "model" }
        ]
    })));

    let draft = json!({
        "protocol": "openai_chat",
        "base_url": gw.upstream.base_url(),
        "api_key": "sk-upstream",
        "timeout_ms": 1000
    });
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels/models", draft).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("模型列表应可解析");
    assert_eq!(
        body["models"],
        json!(["gpt-4o", "claude-3-5-sonnet"]),
        "无 id 条目应跳过，顺序应保持上游返回顺序"
    );

    // Anthropic 协议的模型列表同为 data 数组形态，同样可解析。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "data": [{ "type": "model", "id": "claude-opus-4", "display_name": "Claude Opus 4" }],
        "has_more": false
    })));
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels/models",
        json!({
            "protocol": "anthropic_messages",
            "base_url": gw.upstream.base_url(),
            "api_key": "sk-upstream",
            "timeout_ms": 1000
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("模型列表应可解析");
    assert_eq!(body["models"], json!(["claude-opus-4"]));
}

/// 拉取上游模型列表的错误语义：上游非 2xx/不可达映射 502；非法草稿与未知字段 400。
#[tokio::test]
async fn list_upstream_models_errors_are_structured() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    let draft = json!({
        "protocol": "openai_chat",
        "base_url": gw.upstream.base_url(),
        "api_key": "sk-upstream",
        "timeout_ms": 1000
    });

    // 上游非 2xx → 502 upstream_error，错误摘要来自上游 body。
    gw.upstream.set_behavior(UpstreamBehavior::Status(500));
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels/models",
        draft.clone(),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_GATEWAY);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "upstream_error");

    // 上游不可达 → 502 upstream_error。
    let mut unreachable = draft.clone();
    unreachable["base_url"] = json!("http://127.0.0.1:1");
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels/models", unreachable).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_GATEWAY);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "upstream_error");

    // api_key 为空 → 400 invalid_body。
    let mut empty_key = draft.clone();
    empty_key["api_key"] = json!("");
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels/models", empty_key).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");

    // 未知字段 → 400。
    let mut unknown = draft.clone();
    unknown["typo"] = json!(1);
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels/models", unknown).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// 探测未知渠道 404；两端点未认证 401。
#[tokio::test]
async fn stats_and_probe_auth_and_unknown_channel() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    let resp = client
        .get(format!("{admin}/stats"))
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "unauthorized");

    let resp = client
        .post(format!("{admin}/channels/1/test"))
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    let resp = client
        .post(format!("{admin}/channels/999999/test"))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({ "model": TEST_MODEL }))
        .send()
        .await
        .expect("应可探测渠道");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "not_found");
}

/// 响应是否为 HTML 页面（静态资源免认证）。
fn is_html(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/html"))
}

/// 管理监听托管 Web UI：GET / 免认证返回页面。
///
/// 依赖 `webui/dist`（`pnpm --dir webui build`）。产物缺失时本用例失败，
/// 与「嵌入后应能打开管理面」的验收一致；纯 API 退化见
/// [`admin_root_never_5xx_and_api_still_works`]。
#[tokio::test]
async fn admin_get_root_serves_html_without_auth() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let resp = reqwest::Client::new()
        .get(gw.admin_base_url())
        .send()
        .await
        .expect("管理监听应可达");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "GET / 应返回页面");
    assert!(is_html(&resp), "GET / 应为 text/html");
    let body = resp.text().await.expect("应能读页面");
    assert!(
        body.contains("id=\"app\""),
        "页面应含 SPA 挂载点，实际: {}",
        body.chars().take(200).collect::<String>()
    );
}

/// SPA 深链刷新不 404：未匹配 API 的 GET 回退 index.html，且免认证。
#[tokio::test]
async fn admin_spa_deep_link_serves_html_without_auth() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let resp = reqwest::Client::new()
        .get(format!("{}/overview", gw.admin_base_url()))
        .send()
        .await
        .expect("管理监听应可达");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "SPA 深链刷新不应 404"
    );
    assert!(is_html(&resp), "深链回退应为 text/html");
}

/// 静态资源免认证；资源 API 未带 key 仍 401。
#[tokio::test]
async fn admin_static_is_public_api_still_requires_key() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    let favicon = client
        .get(format!("{admin}/favicon.svg"))
        .send()
        .await
        .expect("应可请求静态资源");
    assert_eq!(
        favicon.status(),
        reqwest::StatusCode::OK,
        "favicon 应免认证可加载"
    );

    let tokens = client
        .get(format!("{admin}/tokens"))
        .send()
        .await
        .expect("应可请求管理 API");
    assert_eq!(
        tokens.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "API 未带 key 仍须 401"
    );
}

/// 管理面 GET / 不得 5xx；带 key 的 API 在 UI 嵌入与否时都可用。
///
/// dist 缺失时 `allow_missing` 使编译通过、GET / 为 404 而非 5xx；本用例与
/// `admin_get_root_serves_html_without_auth` 互补（后者要求产物存在）。
#[tokio::test]
async fn admin_root_never_5xx_and_api_still_works() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    let root = client.get(&admin).send().await.expect("管理监听应可达");
    assert!(
        root.status().as_u16() < 500,
        "UI 缺失或存在时 GET / 都不得 5xx，实际 {}",
        root.status()
    );

    let tokens = client
        .get(format!("{admin}/tokens"))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("管理 API 应可达");
    assert_eq!(
        tokens.status(),
        reqwest::StatusCode::OK,
        "管理 API 在 UI 缺失时仍应可用"
    );
}

/// 两条启用渠道将同一别名指到不同真名：创建/更新 409，文案提示用统一模型。
/// 指向同一真名允许；禁用渠道不参与冲突，启用时再拦。
#[tokio::test]
async fn enabled_channels_reject_divergent_alias_values() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    // seed 渠道已有 fast → gpt-4o-mini。

    let mut divergent = channel_body("divergent", gw.upstream.base_url(), json!([TEST_MODEL]));
    divergent["model_aliases"] = json!({ "fast": TEST_MODEL });
    let before: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", divergent.clone()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: Value = resp.json().await.expect("冲突体应可解析");
    assert_eq!(body["error"]["code"], "conflict");
    let message = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("统一模型"),
        "冲突文案应提示用统一模型，实际: {message}"
    );
    assert!(
        message.contains("fast"),
        "冲突文案应点名别名 key，实际: {message}"
    );
    let after: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    assert_eq!(before, after, "别名冲突写不应改变库与快照");

    let mut same_value = channel_body("same-alias", gw.upstream.base_url(), json!([TEST_MODEL]));
    same_value["model_aliases"] = json!({ "fast": "gpt-4o-mini" });
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", same_value.clone()).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "同一别名指向同一真名应允许（多渠道 failover）"
    );
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let same_id = created["id"].as_i64().expect("创建应回传整数 id");

    let mut disabled = channel_body(
        "disabled-divergent",
        gw.upstream.base_url(),
        json!([TEST_MODEL]),
    );
    disabled["model_aliases"] = json!({ "fast": TEST_MODEL });
    disabled["enabled"] = json!(false);
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", disabled.clone()).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "禁用渠道不参与别名冲突"
    );
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let disabled_id = created["id"].as_i64().expect("创建应回传整数 id");

    disabled["enabled"] = json!(true);
    let resp = admin_put(&gw, &format!("/channels/{disabled_id}"), disabled).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "启用后与已有别名冲突应拒绝"
    );
    let listed: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let still_disabled = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == disabled_id)
        .expect("禁用渠道应仍在列表");
    assert_eq!(still_disabled["enabled"], false, "冲突启用不应落地");

    same_value["model_aliases"] = json!({ "fast": TEST_MODEL });
    let resp = admin_put(&gw, &format!("/channels/{same_id}"), same_value).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "更新为不同真名应拒绝"
    );
}
