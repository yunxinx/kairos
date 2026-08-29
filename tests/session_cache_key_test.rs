//! 渠道级会话缓存键回写端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒，断言 mock 上游收到的出站请求体。覆盖回写开关
//! 三态（off 不写、auto 不覆盖下游显式键、always 无条件覆盖）、显式
//! `x-kairos-session-id` 头与前缀哈希兜底两种会话标识来源，以及直通路径
//! 不经过回写。回写只发生在 IR 路径，用例统一经别名或跨协议入站强制。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
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
/// 上游 prompt_cache_key，多轮跨协议请求获得上游自动缓存亲和。
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
    assert_eq!(
        received[0]["prompt_cache_key"],
        json!("conv-abc"),
        "always 渠道应把显式会话头回写为上游缓存亲和键"
    );
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
    assert_eq!(
        received[1]["prompt_cache_key"],
        json!("sess-2"),
        "auto 应在下游缺席时回写会话头标识"
    );
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
    assert_eq!(
        received[0]["prompt_cache_key"],
        json!("sess-stream"),
        "流式路径应同样回写会话缓存键"
    );
}

/// 直通快路径（同协议无别名）字节直搬：开关在场也不改写请求体。
#[tokio::test]
async fn passthrough_path_skips_writeback() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        seed_with_mode(&bases[0], SessionCacheKeyMode::Always)
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
        "直通路径不应改写请求体"
    );
}
