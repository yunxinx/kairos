//! response_format ↔ text.format 双向互认端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒，断言 mock 上游收到的出站请求体与下游收到的
//! warning。覆盖：chat 入站 JSON 结构化输出 → Responses 渠道以 text.format
//! 出站；chat 入站 → Anthropic 渠道无请求侧承载，warning 随响应回传。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use kairos::config;
use serde_json::{Value, json};

fn channel_seed(base: &str, protocol: config::Protocol) -> common::Seed {
    let mut seed = common::test_seed(base);
    seed.channels[0].protocol = protocol;
    seed
}

/// chat 入站 json_schema → Responses 渠道：以等价的 text.format 出站，下游
/// 不应收到 response_format 丢弃告警。
#[tokio::test]
async fn chat_response_format_maps_to_responses_text_format() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        channel_seed(&bases[0], config::Protocol::OpenAiResponses)
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "resp_01f", "object": "response", "status": "completed", "model": TEST_MODEL,
        "output": [
            { "id": "msg_1", "type": "message", "role": "assistant",
              "content": [ { "type": "output_text", "text": "ok", "annotations": [] } ] }
        ],
        "usage": { "input_tokens": 100, "output_tokens": 20, "total_tokens": 120 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": "answer", "schema": { "type": "object" }, "strict": true }
            },
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0]["text"],
        json!({ "format": {
            "type": "json_schema",
            "name": "answer",
            "schema": { "type": "object" },
            "strict": true
        }}),
        "json_schema 应摊平为 text.format 顶层字段出站"
    );
    assert!(
        received[0].get("response_format").is_none(),
        "Responses 出站不应保留 chat 字段名"
    );

    let body: Value = resp.json().await.expect("响应应可解析");
    assert!(
        body.get("gateway").is_none(),
        "等价映射成功不应有 warning: {body}"
    );
}

/// chat 入站 json_object → Anthropic 渠道：无请求侧承载，warning 随响应回传。
#[tokio::test]
async fn chat_response_format_to_anthropic_warns() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        channel_seed(&bases[0], config::Protocol::AnthropicMessages)
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_01f", "type": "message", "role": "assistant", "model": "claude-sonnet",
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 25, "output_tokens": 12 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "response_format": { "type": "json_object" },
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert!(
        received[0].get("response_format").is_none(),
        "Anthropic 出站不应带 response_format"
    );

    let body: Value = resp.json().await.expect("响应应可解析");
    let features: Vec<&str> = body["gateway"]["warnings"]
        .as_array()
        .map(|warnings| {
            warnings
                .iter()
                .filter_map(|w| w["feature"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        features.contains(&"response_format"),
        "结构化输出丢弃应回传 response_format warning: {body}"
    );
}
