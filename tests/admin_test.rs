//! 管理 API 端到端黑盒测试：独立管理监听 + 静态 admin key 认证 + 资源 CRUD。
//!
//! 主接缝：测试内启动网关 + mock 上游 + 独立管理监听，断言外部可观察行为
//! （管理写库后的即时生效、认证拒绝、结构化错误、SQLite 持久化状态）。

mod common;

use common::{TEST_ADMIN_KEY, TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
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

/// 未配置管理监听时管理面整体关闭：协议监听上没有任何管理路由。
#[tokio::test]
async fn admin_not_configured_means_no_admin_routes() {
    let gw = TestGateway::start().await;

    // 协议监听不应有管理路由；`tokens` 落到 fallback（404）。
    let resp = reqwest::Client::new()
        .get(format!("{}/tokens", gw.base_url()))
        .send()
        .await
        .expect("协议监听应可请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "协议监听不应注册管理路由"
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

/// 令牌 CRUD 往返 + 写后即时生效：新建立刻可用、删除立刻失效。
#[tokio::test]
async fn token_crud_roundtrip_and_immediate_effect() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    // 新建令牌。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "token_key": "sk-new", "name": "new-dev", "limit_usd_micros": null }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建令牌");
    assert_eq!(created["token_key"], "sk-new");

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
            .any(|t| t["token_key"] == "sk-new"),
        "新建令牌应出现在列表"
    );

    // 新建令牌在请求路径即时可用：充值（余额调整属 04 票，测试内用相对量原语
    // 绕过）后请求成功。新建令牌已有零额余额行，故可被 `adjust_balance` 充值。
    let mut conn = gw.pool.acquire().await.expect("应能获取连接");
    store::adjust_balance(&mut conn, "sk-new", 5_000_000)
        .await
        .expect("应能为新令牌充值");
    drop(conn);
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, "sk-new", TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "新令牌应立即可用于请求路径"
    );

    // 删除后立即失效：请求路径认证失败（401），列表也移除。
    let resp = reqwest::Client::new()
        .delete(format!("{}/tokens/sk-new", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可删除令牌");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let resp = chat_request(&gw, "sk-new", TEST_MODEL).await;
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
            .any(|t| t["token_key"] == "sk-new"),
        "删除后令牌应移出列表"
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
        json!({
            "name": "mini-channel",
            "protocol": "openai_chat",
            "base_url": gw.upstream.base_url(),
            "api_key": "sk-upstream",
            "models": ["gpt-4o-mini"],
            "model_aliases": {},
            "priority": 1,
            "weight": 1,
            "timeout_ms": 1000,
            "max_retries": 0
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

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
        .delete(format!("{}/channels/mini-channel", gw.admin_base_url()))
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

    // 缺必填字段（token_key）→ 400（serde 拒绝）。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "x" }),
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

    // 重复新建 → 409，且不覆盖原资源。
    let before: Value = admin_get(&gw, "/tokens")
        .await
        .json()
        .await
        .expect("令牌列表应可解析");
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "token_key": TEST_TOKEN_KEY, "name": "overwrite", "limit_usd_micros": null }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let after: Value = admin_get(&gw, "/tokens")
        .await
        .json()
        .await
        .expect("令牌列表应可解析");
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
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        json!({
            "name": "typo-channel",
            "protcol": "openai_chat",
            "protocol": "openai_chat",
            "base_url": gw.upstream.base_url(),
            "api_key": "sk-upstream",
            "models": [],
            "model_aliases": {},
            "priority": 1,
            "weight": 1,
            "timeout_ms": 1000,
            "max_retries": 0
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");
}

/// 删除令牌后同 key 重建：余额从零开始，不复活删除前的旧余额。
#[tokio::test]
async fn recreated_token_does_not_resurrect_balance() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();

    // 建令牌并充值，确认请求可用（200）。
    admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "token_key": "sk-cycle", "name": "cycle", "limit_usd_micros": null }),
    )
    .await;
    let mut conn = gw.pool.acquire().await.expect("应能获取连接");
    store::adjust_balance(&mut conn, "sk-cycle", 5_000_000)
        .await
        .expect("应能充值");
    drop(conn);
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, "sk-cycle", TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 删除后同 key 重建：余额行已随删除清理，重建播种零额。
    let resp = client
        .delete(format!("{}/tokens/sk-cycle", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("应可删除令牌");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "token_key": "sk-cycle", "name": "cycle-again", "limit_usd_micros": null }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    // 重建的令牌余额为零：计费准入拒绝（402），旧余额没有复活。
    let resp = chat_request(&gw, "sk-cycle", TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::PAYMENT_REQUIRED,
        "重建令牌应为零余额，不复活旧余额"
    );
}
