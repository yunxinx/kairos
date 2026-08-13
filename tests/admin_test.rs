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

/// 未配置管理监听时管理面整体关闭：协议监听上没有任何管理路由。
#[tokio::test]
async fn admin_not_configured_means_no_admin_routes() {
    let gw = TestGateway::start().await;
    let client = reqwest::Client::new();

    // 协议监听不应有管理路由；落到 fallback（404）。覆盖读/写与各资源路径。
    for path in ["/tokens", "/channels", "/prices", "/settings", "/logs"] {
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
        .json(&json!({ "token_key": "sk-x", "name": "x", "limit_usd_micros": null }))
        .send()
        .await
        .expect("协议监听应可请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "协议监听不应接受管理写入"
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

/// 已配置管理面时协议监听仍不注册管理路由；管理监听不提供协议端点。
///
/// admin key 与下游令牌体系隔离：下游令牌调管理面 401，admin key 当下游令牌 401。
#[tokio::test]
async fn admin_surface_is_isolated_from_protocol_surface() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();

    // 协议监听无管理路由（即使管理面已启动）。
    let resp = client
        .get(format!("{}/tokens", gw.base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("协议监听应可请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "协议监听即使已配置管理面也不应注册管理路由"
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
        json!({ "token_key": "sk-restart", "name": "restart", "limit_usd_micros": null }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let resp = reqwest::Client::new()
        .post(format!("{}/tokens/sk-restart/balance", gw.admin_base_url()))
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
        json!({
            "name": "restart-channel",
            "protocol": "openai_chat",
            "base_url": gw.upstream.base_url(),
            "api_key": "sk-upstream",
            "models": ["restart-only"],
            "model_aliases": {},
            "priority": 1,
            "weight": 1,
            "timeout_ms": 1000,
            "max_retries": 0
        }),
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
        .bearer_auth("sk-restart")
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
        .bearer_auth("sk-restart")
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
        json!({ "token_key": "sk-boot", "name": "boot", "limit_usd_micros": null }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        json!({
            "name": "boot-channel",
            "protocol": "openai_chat",
            "base_url": gw.upstream.base_url(),
            "api_key": "sk-upstream",
            "models": [TEST_MODEL],
            "model_aliases": {},
            "priority": 1,
            "weight": 1,
            "timeout_ms": 1000,
            "max_retries": 0
        }),
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
        .post(format!("{}/tokens/sk-boot/balance", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&json!({ "delta_usd_micros": 5_000_000 }))
        .send()
        .await
        .expect("应可充值");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, "sk-boot", TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "空库经管理 API 初始化后请求路径应可用"
    );
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
        json!({ "token_key": TEST_TOKEN_KEY, "name": "renamed", "limit_usd_micros": null }),
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
