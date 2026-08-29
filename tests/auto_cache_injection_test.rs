//! 渠道级自动缓存断点注入（#27）端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒，断言 mock 上游收到的出站请求体。覆盖：开启时
//! 出站请求按 tools 尾 → system 尾 → 末条消息尾块被自动补缓存断点（非流式
//! 与流式同一行为）、默认关闭时出站请求保持原样（存量渠道零变化）。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use serde_json::{Value, json};

/// 构造指向 mock 上游的 Anthropic 渠道 seed（其余沿用测试默认）。
fn anthropic_channel_seed(base: &str) -> common::Seed {
    let mut seed = common::test_seed(base);
    seed.channels[0].protocol = kairos::config::Protocol::AnthropicMessages;
    seed
}

/// mock 上游的最小 Anthropic 响应。
fn anthropic_response() -> Value {
    json!({
        "id": "msg_c1", "type": "message", "role": "assistant", "model": "claude-sonnet",
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 2 }
    })
}

/// 发起一次非流式 Chat Completions 请求（含 system 与工具定义）。
async fn send_completion_with_tools(base: &str) -> reqwest::Response {
    let client = reqwest::Client::new();
    client
        .post(format!("{}/v1/chat/completions", base))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [
                { "role": "system", "content": "你是天气助手。" },
                { "role": "user", "content": "北京天气如何？" }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "parameters": { "type": "object", "properties": {} }
                }
            }]
        }))
        .send()
        .await
        .expect("应能请求网关")
}

/// 开启注入的渠道：出站请求在 tools 尾、system 尾、末条消息尾块各带一个
/// 缓存断点（chat 入站 → Anthropic 渠道恒走 IR 路径，直通不经过注入）。
#[tokio::test]
async fn enabled_channel_injects_breakpoints_into_anthropic_outbound() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = anthropic_channel_seed(base);
        seed.channels[0].injects_cache_breakpoints = true;
        seed
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(anthropic_response()));

    let resp = send_completion_with_tools(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    let tools = received[0]["tools"].as_array().expect("应有 tools");
    assert_eq!(
        tools.last().unwrap()["cache_control"],
        json!({ "type": "ephemeral" }),
        "tools 尾应带断点"
    );
    let system = received[0]["system"].as_array().expect("system 应为块数组");
    assert_eq!(
        system.last().unwrap()["cache_control"],
        json!({ "type": "ephemeral" }),
        "system 尾应带断点"
    );
    let messages = received[0]["messages"].as_array().expect("应有 messages");
    assert_eq!(messages.len(), 1, "system 上提后仅剩 user 一条消息");
    let last_blocks = messages.last().unwrap()["content"]
        .as_array()
        .expect("末条消息应为块数组");
    assert_eq!(
        last_blocks.last().unwrap()["cache_control"],
        json!({ "type": "ephemeral" }),
        "末条消息尾块应带断点"
    );
}

/// 流式与非流式同一行为：开启注入的渠道，流式出站请求同样被补断点。
#[tokio::test]
async fn enabled_channel_injects_breakpoints_when_streaming() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = anthropic_channel_seed(base);
        seed.channels[0].injects_cache_breakpoints = true;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "type": "message_start",
            "message": { "id": "msg_c2", "model": "claude-sonnet",
                         "usage": { "input_tokens": 8, "output_tokens": 0 } }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "ok" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "input_tokens": 8, "output_tokens": 1 }
        }))
        .unwrap(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "stream": true,
            "messages": [
                { "role": "system", "content": "你是天气助手。" },
                { "role": "user", "content": "北京天气如何？" }
            ]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["stream"], true);
    let system = received[0]["system"].as_array().expect("system 应为块数组");
    assert_eq!(
        system.last().unwrap()["cache_control"],
        json!({ "type": "ephemeral" }),
        "流式出站同样应补 system 尾断点"
    );
}

/// 默认关闭：出站请求不带任何注入断点（存量渠道零变化）。
#[tokio::test]
async fn default_off_channel_keeps_outbound_untouched() {
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(anthropic_response()));

    let resp = send_completion_with_tools(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    fn has_cache_control(value: &Value) -> bool {
        match value {
            Value::Object(map) => {
                map.contains_key("cache_control") || map.values().any(has_cache_control)
            }
            Value::Array(items) => items.iter().any(has_cache_control),
            _ => false,
        }
    }
    assert!(
        !has_cache_control(&received[0]),
        "默认关渠道的出站请求不应带任何断点: {received:#?}"
    );
}
