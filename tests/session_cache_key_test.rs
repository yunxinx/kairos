//! 渠道级会话缓存键回写端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒，断言 mock 上游收到的出站请求体。覆盖回写开关
//! 三态（off 不写、auto 不覆盖下游显式键、always 无条件覆盖）、显式
//! `x-kairos-session-id` 头与前缀哈希兜底两种会话标识来源，以及直通路径与
//! IR 路径应用同一回写补丁。IR 路径用例经别名或跨协议入站强制，直通用例
//! 走同协议无别名渠道。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior, collect_sse_frames};
use kairos::config::SessionCacheKeyMode;
use serde_json::{Value, json};

/// 指定回写模式的 seed（其余沿用测试默认：openai_chat 渠道 + fast 别名强制 IR）。
fn seed_with_mode(base: &str, mode: SessionCacheKeyMode) -> common::Seed {
    let mut seed = common::test_seed(base);
    seed.channels[0].session_cache_key = mode;
    seed
}

fn chat_upstream_response() -> Value {
    json!({
        "id": "chatcmpl-1", "object": "chat.completion", "created": 0, "model": "gpt-4o-mini",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "ok" }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    })
}

/// always 渠道 + 跨协议入站（Anthropic → OpenAI chat）：显式会话头回写为
/// 仅在网关内部可复现、且不会暴露原文的上游 prompt_cache_key。
#[tokio::test]
async fn always_channel_writes_session_identity_cross_protocol() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        seed_with_mode(&bases[0], SessionCacheKeyMode::Always)
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(chat_upstream_response()));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .header("x-kairos-session-id", "conv-abc")
        .json(&json!({
            "model": TEST_MODEL,
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    let cache_key = received[0]["prompt_cache_key"]
        .as_str()
        .expect("always 渠道应写入缓存亲和键");
    assert_eq!(cache_key.len(), 64, "缓存键应为固定长度摘要");
    assert_ne!(cache_key, "conv-abc", "上游不应收到下游原始会话标识");
}

/// auto 渠道：下游显式携带的 prompt_cache_key 保留不覆盖；缺席时回写会话头。
#[tokio::test]
async fn auto_channel_keeps_explicit_key_and_fills_absent() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        seed_with_mode(&bases[0], SessionCacheKeyMode::Auto)
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    gw.upstream
        .push_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    let client = reqwest::Client::new();
    let url = format!("{}/v1/chat/completions", gw.base_url());

    // 下游显式键 + 显式会话头：auto 保留下游键。
    let resp = client
        .post(&url)
        .bearer_auth(TEST_TOKEN_KEY)
        .header("x-kairos-session-id", "sess-1")
        .json(&json!({
            "model": "fast",
            "prompt_cache_key": "downstream-key",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0]["prompt_cache_key"],
        json!("downstream-key"),
        "auto 不应覆盖下游显式携带的缓存键"
    );

    // 下游缺席：auto 回写显式会话头。
    let resp = client
        .post(&url)
        .bearer_auth(TEST_TOKEN_KEY)
        .header("x-kairos-session-id", "sess-2")
        .json(&json!({
            "model": "fast",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    assert_eq!(received.len(), 2);
    let cache_key = received[1]["prompt_cache_key"]
        .as_str()
        .expect("auto 应在下游缺席时写入缓存亲和键");
    assert_eq!(cache_key.len(), 64, "缓存键应为固定长度摘要");
    assert_ne!(cache_key, "sess-2", "上游不应收到下游原始会话标识");
}

/// off 渠道（缺省）：不出站回写；下游显式键仍经类型化字段照常透传。
#[tokio::test]
async fn off_channel_leaves_outbound_untouched_and_passes_explicit_key() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        seed_with_mode(&bases[0], SessionCacheKeyMode::Off)
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    gw.upstream
        .push_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    let client = reqwest::Client::new();

    // 显式会话头在场也不回写（缺省 off）。
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .header("x-kairos-session-id", "sess-1")
        .json(&json!({
            "model": "fast",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert!(
        received[0].get("prompt_cache_key").is_none(),
        "off 渠道不应回写缓存亲和键"
    );

    // 下游显式键不经开关、照常类型化透传。
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "prompt_cache_key": "downstream-key",
            "prompt_cache_retention": "24h",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    assert_eq!(received.len(), 2);
    assert_eq!(received[1]["prompt_cache_key"], json!("downstream-key"));
    assert_eq!(received[1]["prompt_cache_retention"], json!("24h"));
}

/// 无显式会话头：auto 渠道以前缀哈希兜底，同前缀的多轮请求得到同一亲和键。
#[tokio::test]
async fn auto_channel_falls_back_to_prefix_hash() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        seed_with_mode(&bases[0], SessionCacheKeyMode::Auto)
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    for _ in 0..2 {
        gw.upstream
            .push_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    }
    let client = reqwest::Client::new();
    let url = format!("{}/v1/chat/completions", gw.base_url());
    let post = |body: Value| {
        let client = &client;
        let url = &url;
        async move {
            client
                .post(url)
                .bearer_auth(TEST_TOKEN_KEY)
                .json(&body)
                .send()
                .await
                .expect("应能请求网关")
        }
    };

    // 同一稳定前缀（system + 首条消息），第二轮新增消息不改变亲和键。
    let first = post(json!({
        "model": "fast",
        "messages": [
            { "role": "system", "content": "be precise" },
            { "role": "user", "content": "hello" }
        ]
    }))
    .await;
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let second = post(json!({
        "model": "fast",
        "messages": [
            { "role": "system", "content": "be precise" },
            { "role": "user", "content": "hello" },
            { "role": "assistant", "content": "ok" },
            { "role": "user", "content": "turn two" }
        ]
    }))
    .await;
    assert_eq!(second.status(), reqwest::StatusCode::OK);
    // 前缀变化（system 不同）得到不同亲和键。
    let third = post(json!({
        "model": "fast",
        "messages": [
            { "role": "system", "content": "be creative" },
            { "role": "user", "content": "hello" }
        ]
    }))
    .await;
    assert_eq!(third.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 3);
    let first_key = received[0]["prompt_cache_key"]
        .as_str()
        .expect("应有亲和键");
    let second_key = received[1]["prompt_cache_key"]
        .as_str()
        .expect("应有亲和键");
    let third_key = received[2]["prompt_cache_key"]
        .as_str()
        .expect("应有亲和键");
    assert_eq!(
        first_key, second_key,
        "同前缀多轮请求应得到同一前缀哈希亲和键"
    );
    assert_ne!(first_key, third_key, "前缀变化应改变亲和键");
}

/// 流式路径同样回写：多轮流式请求不丢亲和。
#[tokio::test]
async fn streaming_path_writes_session_identity() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        seed_with_mode(&bases[0], SessionCacheKeyMode::Always)
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        json!({"id": "c1", "object": "chat.completion.chunk", "model": "gpt-4o-mini",
               "choices": [{"index": 0, "delta": {"role": "assistant", "content": "ok"}, "finish_reason": null}]})
            .to_string(),
        json!({"id": "c1", "object": "chat.completion.chunk", "model": "gpt-4o-mini",
               "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
               "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}})
            .to_string(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .header("x-kairos-session-id", "sess-stream")
        .json(&json!({
            "model": "fast",
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    let cache_key = received[0]["prompt_cache_key"]
        .as_str()
        .expect("流式路径应写入会话缓存键");
    assert_eq!(cache_key.len(), 64, "缓存键应为固定长度摘要");
    assert_ne!(cache_key, "sess-stream", "上游不应收到下游原始会话标识");
}

#[tokio::test]
async fn session_identity_is_scoped_by_user() {
    let mut gw =
        TestGateway::start_with_admin(|base| seed_with_mode(base, SessionCacheKeyMode::Always))
            .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    gw.upstream
        .push_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    let origin = gw.admin_origin();
    let admin = |method: reqwest::Method, path: &str| {
        reqwest::Client::new()
            .request(method, format!("{}{path}", gw.admin_base_url()))
            .header(reqwest::header::COOKIE, &gw.session)
            .header(reqwest::header::ORIGIN, &origin)
    };

    let created = admin(reqwest::Method::POST, "/users")
        .json(&json!({
            "email": "cache-scope@example.com",
            "display_name": "cache-scope",
            "password": "password1",
            "role": "user"
        }))
        .send()
        .await
        .expect("用户创建应可达");
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let user_id = created.json::<Value>().await.expect("用户应可解析")["id"]
        .as_i64()
        .expect("应有用户 id");
    let recharged = admin(
        reqwest::Method::POST,
        &format!("/users/{user_id}/balance-adjustments"),
    )
    .json(&json!({
        "operation_id": "cache-scope-balance",
        "delta_usd_micros": 5_000_000,
        "reason": "manual_adjustment"
    }))
    .send()
    .await
    .expect("充值应可达");
    assert_eq!(recharged.status(), reqwest::StatusCode::OK);

    let login = reqwest::Client::new()
        .post(format!("{}/login", gw.admin_base_url()))
        .json(&json!({
            "email": "cache-scope@example.com",
            "password": "password1"
        }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let user_session = common::session_cookie(&login);
    let token = reqwest::Client::new()
        .post(format!("{}/tokens", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, user_session)
        .header(reqwest::header::ORIGIN, &origin)
        .json(&json!({
            "name": "cache-scope-token",
            "model_group": "default",
            "enabled": true
        }))
        .send()
        .await
        .expect("令牌创建应可达");
    assert_eq!(token.status(), reqwest::StatusCode::CREATED);
    let user_token = token.json::<Value>().await.expect("令牌应可解析")["token_key"]
        .as_str()
        .expect("应有令牌 key")
        .to_string();

    for token in [TEST_TOKEN_KEY, user_token.as_str()] {
        let response = reqwest::Client::new()
            .post(format!("{}/v1/chat/completions", gw.base_url()))
            .bearer_auth(token)
            .header("x-kairos-session-id", "shared-signal")
            .json(&json!({
                "model": "fast",
                "messages": [{ "role": "user", "content": "hi" }]
            }))
            .send()
            .await
            .expect("下游请求应可达");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    let received = gw.upstream.received();
    assert_eq!(received.len(), 2);
    assert_ne!(
        received[0]["prompt_cache_key"], received[1]["prompt_cache_key"],
        "不同用户不能共享上游缓存亲和键"
    );
}

#[tokio::test]
async fn session_identity_survives_process_reload() {
    let mut gw =
        TestGateway::start_with(|base| seed_with_mode(base, SessionCacheKeyMode::Always)).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    gw.upstream
        .push_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    let reloaded = gw.spawn_reloaded_protocol().await;

    for base in [gw.base_url(), reloaded] {
        let response = reqwest::Client::new()
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth(TEST_TOKEN_KEY)
            .header("x-kairos-session-id", "stable-after-reload")
            .json(&json!({
                "model": "fast",
                "messages": [{ "role": "user", "content": "hi" }]
            }))
            .send()
            .await
            .expect("下游请求应可达");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    let received = gw.upstream.received();
    assert_eq!(received.len(), 2);
    assert_eq!(
        received[0]["prompt_cache_key"], received[1]["prompt_cache_key"],
        "同一数据库重载后应保持缓存亲和键稳定"
    );
}

/// 直通路径应用同一回写补丁：always 渠道上无条件覆盖下游显式键、缺席时
/// 写入；流式直通同样回写。会话标识与 IR 路径同源（显式头派生摘要）。
#[tokio::test]
async fn passthrough_path_applies_writeback_in_always_mode() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        seed_with_mode(&bases[0], SessionCacheKeyMode::Always)
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    gw.upstream
        .push_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    gw.upstream.push_behavior(UpstreamBehavior::Sse(vec![
        json!({"id": "c1", "object": "chat.completion.chunk", "model": TEST_MODEL,
               "choices": [{"index": 0, "delta": {"role": "assistant", "content": "ok"}, "finish_reason": null}]})
            .to_string(),
        json!({"id": "c1", "object": "chat.completion.chunk", "model": TEST_MODEL,
               "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
               "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}})
            .to_string(),
    ]));

    let client = reqwest::Client::new();
    let url = format!("{}/v1/chat/completions", gw.base_url());

    // 非流式：下游显式键被无条件覆盖为网关派生标识。
    let resp = client
        .post(&url)
        .bearer_auth(TEST_TOKEN_KEY)
        .header("x-kairos-session-id", "sess-1")
        .json(&json!({
            "model": TEST_MODEL,
            "prompt_cache_key": "downstream-key",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 非流式：下游缺席时写入派生标识。
    let resp = client
        .post(&url)
        .bearer_auth(TEST_TOKEN_KEY)
        .header("x-kairos-session-id", "sess-1")
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 流式直通：同样回写，且 include_usage 补丁照常注入。
    let resp = client
        .post(&url)
        .bearer_auth(TEST_TOKEN_KEY)
        .header("x-kairos-session-id", "sess-stream")
        .json(&json!({
            "model": TEST_MODEL,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let _ = collect_sse_frames(resp).await;

    let received = gw.upstream.received();
    assert_eq!(received.len(), 3);
    for body in received.iter().take(2) {
        let cache_key = body["prompt_cache_key"]
            .as_str()
            .expect("always 直通应回写缓存亲和键");
        assert_eq!(cache_key.len(), 64, "缓存键应为固定长度摘要");
        assert_ne!(cache_key, "sess-1", "上游不应收到下游原始会话标识");
    }
    assert_eq!(
        received[0]["prompt_cache_key"], received[1]["prompt_cache_key"],
        "覆盖与写入应得到同一派生标识"
    );
    let stream_cache_key = received[2]["prompt_cache_key"]
        .as_str()
        .expect("流式直通应回写缓存亲和键");
    assert_eq!(stream_cache_key.len(), 64);
    assert_eq!(
        received[2]["stream_options"]["include_usage"],
        json!(true),
        "流式直通的 include_usage 补丁应保留"
    );
}

/// 直通路径 auto 语义：下游显式键保留，缺席时回写。
#[tokio::test]
async fn passthrough_path_auto_keeps_explicit_and_fills_absent() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        seed_with_mode(&bases[0], SessionCacheKeyMode::Auto)
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(chat_upstream_response()));
    gw.upstream
        .push_behavior(UpstreamBehavior::Json(chat_upstream_response()));

    let client = reqwest::Client::new();
    let url = format!("{}/v1/chat/completions", gw.base_url());

    let resp = client
        .post(&url)
        .bearer_auth(TEST_TOKEN_KEY)
        .header("x-kairos-session-id", "sess-1")
        .json(&json!({
            "model": TEST_MODEL,
            "prompt_cache_key": "downstream-key",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let resp = client
        .post(&url)
        .bearer_auth(TEST_TOKEN_KEY)
        .header("x-kairos-session-id", "sess-2")
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 2);
    assert_eq!(
        received[0]["prompt_cache_key"],
        json!("downstream-key"),
        "auto 直通不应覆盖下游显式键"
    );
    let cache_key = received[1]["prompt_cache_key"]
        .as_str()
        .expect("auto 直通应在缺席时回写");
    assert_eq!(cache_key.len(), 64);
    assert_ne!(cache_key, "sess-2");
}

/// 直通路径 off 语义：出站体不带补丁键；下游显式键原样透传。
#[tokio::test]
async fn passthrough_path_off_leaves_body_untouched() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        seed_with_mode(&bases[0], SessionCacheKeyMode::Off)
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(chat_upstream_response()));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .header("x-kairos-session-id", "sess-1")
        .json(&json!({
            "model": TEST_MODEL,
            "prompt_cache_key": "downstream-key",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0]["prompt_cache_key"],
        json!("downstream-key"),
        "off 直通不改写下游显式键"
    );
}
