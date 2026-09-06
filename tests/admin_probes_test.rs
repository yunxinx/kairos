//! 渠道探测与上游模型列表：连通性、密钥筛选、超时与错误语义。

mod common;

use common::admin::{admin_json, channel_body, channel_id_by_name, completion_body};
use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use serde_json::{Value, json};

/// 渠道探测成功：可达、200、有延迟；出站为非流式极小请求；不经计费、不落日志。
#[tokio::test]
async fn channel_probe_success_skips_billing_and_logging() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));

    let balance_before: (i64,) =
        sqlx::query_as("SELECT ub.balance_usd_micros FROM tokens t JOIN user_balance ub ON ub.user_id = t.user_id WHERE t.token_key = ?")
            .bind(common::fingerprint(TEST_TOKEN_KEY))
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
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
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
        sqlx::query_as("SELECT ub.balance_usd_micros FROM tokens t JOIN user_balance ub ON ub.user_id = t.user_id WHERE t.token_key = ?")
            .bind(common::fingerprint(TEST_TOKEN_KEY))
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

/// 渠道探测同样按模型筛选密钥，并使用适用密钥发起请求。
#[tokio::test]
async fn channel_probe_uses_model_eligible_key() {
    let mut gw = TestGateway::start_with_admin(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].keys = vec![
            kairos::store::resources::ChannelKey {
                name: "blocked".to_string(),
                api_key: "sk-blocked".to_string(),
                weight: 100,
                enabled: true,
                models: Some(vec!["other".to_string()]),
                blocked_models: None,
            },
            kairos::store::resources::ChannelKey {
                name: "usable".to_string(),
                api_key: "sk-probe".to_string(),
                weight: 1,
                enabled: true,
                models: Some(vec![TEST_MODEL.to_string()]),
                blocked_models: None,
            },
        ];
        seed
    })
    .await;
    let channel_id = channel_id_by_name(&gw, "test-channel").await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(json!({"ok": true})));

    let response = admin_json(
        &gw,
        reqwest::Method::POST,
        &format!("/channels/{channel_id}/test"),
        json!({"model": TEST_MODEL}),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        gw.upstream.received_api_keys(),
        vec![Some("Bearer sk-probe".to_string())]
    );
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
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
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
    timeout_body["timeout_ms"] = json!(1000);
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
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
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
        (900..3_000).contains(&latency),
        "延迟应贴近渠道 timeout_ms=1000，实际 {latency}"
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
    extra["models"] = json!([TEST_MODEL, "gpt-4o-mini"]);
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

    // 非 http(s) scheme → 400，避免 file:// 等探测本机。
    let mut file_url = draft.clone();
    file_url["base_url"] = json!("file:///etc/passwd");
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels/models", file_url).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");
    let msg = body["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("http"), "应提示仅支持 http/https，实际 {msg}");

    // 未知字段 → 400。
    let mut unknown = draft.clone();
    unknown["typo"] = json!(1);
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels/models", unknown).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}
