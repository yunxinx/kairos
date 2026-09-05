//! 出站响应体上限（运行时开关）端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒。断言 `max_response_bytes` 控制上游非流式响应体：
//! 超限视为可换渠道错误（单渠道时最终 502），不把超大体当作成功 200 透传。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use kairos::config;
use kairos::store::resources::Channel;
use serde_json::{Value, json};

fn tiny_ok_json() -> Value {
    json!({
        "id": "chatcmpl-ok", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

fn oversized_json() -> Value {
    json!({
        "id": "chatcmpl-big", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "x".repeat(2000)},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

fn tiny_response_seed(base: &str) -> common::Seed {
    let mut seed = common::test_seed(base);
    seed.settings
        .insert("max_response_bytes".to_string(), Value::from(200u64));
    seed
}

/// 上游 JSON 超过 `max_response_bytes`：单渠道耗尽后 502，不是 200。
#[tokio::test]
async fn oversized_upstream_body_returns_502() {
    let mut gw = TestGateway::start_with(tiny_response_seed).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(oversized_json()));

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_GATEWAY,
        "超限应走 failover 耗尽后的 502，实际 {}",
        resp.status()
    );
    let body: Value = resp.json().await.expect("502 响应应可解析");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|msg| msg.contains("上游响应超过上限")),
        "应说明超限，实际 {body:?}"
    );
}

/// 超限是确定性策略结果：不得在其它渠道重新下载另一份可能同样超限的响应。
#[tokio::test]
async fn oversized_upstream_body_does_not_retry_another_channel() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        // 成功响应 JSON 本身超过 200 字节，这里用 500 才能区分「大体超限」和「小体成功」。
        seed.settings
            .insert("max_response_bytes".to_string(), Value::from(500u64));
        seed.channels = vec![
            Channel {
                name: "ch-0".to_string(),
                protocol: config::Protocol::OpenAiChat,
                base_url: bases[0].clone(),
                keys: vec![kairos::store::resources::ChannelKey {
                    name: "default".to_string(),
                    api_key: "sk-0".to_string(),
                    weight: 1,
                    enabled: true,
                    models: None,
                    blocked_models: None,
                }],
                models: vec![TEST_MODEL.to_string()],
                model_aliases: Default::default(),
                timeout_ms: 1000,
                request_timeout_ms: 120_000,
                max_retries: 0,
                enabled: true,
                model_group: kairos::store::resources::DEFAULT_MODEL_GROUP.to_string(),
                reasoning_output: Default::default(),
                session_cache_key: Default::default(),
                injects_cache_breakpoints: false,
                abort_on_disconnect: true,
            },
            Channel {
                name: "ch-1".to_string(),
                protocol: config::Protocol::OpenAiChat,
                base_url: bases[1].clone(),
                keys: vec![kairos::store::resources::ChannelKey {
                    name: "default".to_string(),
                    api_key: "sk-1".to_string(),
                    weight: 1,
                    enabled: true,
                    models: None,
                    blocked_models: None,
                }],
                models: vec![TEST_MODEL.to_string()],
                model_aliases: Default::default(),
                timeout_ms: 1000,
                request_timeout_ms: 120_000,
                max_retries: 0,
                enabled: true,
                model_group: kairos::store::resources::DEFAULT_MODEL_GROUP.to_string(),
                reasoning_output: Default::default(),
                session_cache_key: Default::default(),
                injects_cache_breakpoints: false,
                abort_on_disconnect: true,
            },
        ];
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Json(oversized_json()));
    ups[1].set_behavior(UpstreamBehavior::Json(tiny_ok_json()));

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_GATEWAY);
    assert_eq!(ups[0].received().len(), 1, "首渠道应收一次");
    assert_eq!(ups[1].received().len(), 0, "响应超限不得切换渠道重试");
}

/// 跨协议 IR 路径同样封顶：超限不把大体解码成 200。
#[tokio::test]
async fn oversized_ir_upstream_body_returns_502() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = tiny_response_seed(base);
        seed.channels[0].protocol = config::Protocol::AnthropicMessages;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_big", "type": "message", "role": "assistant", "model": "gpt-4o",
        "content": [{ "type": "text", "text": "x".repeat(2000) }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    })));

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_GATEWAY,
        "IR 超限应 502，实际 {}",
        resp.status()
    );
}
