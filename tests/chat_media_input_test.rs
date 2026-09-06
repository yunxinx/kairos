//! chat 入站音频与文件 part 端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒，断言 mock 上游收到的出站请求体与下游收到的
//! warning。覆盖：chat `input_audio`/`file` part → Responses 渠道经 IR 以
//! `input_file` 出站；→ Anthropic 渠道 PDF 以 document 块出站、音频无承载
//! 丢弃且 warning 可观测。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use kairos::config;
use serde_json::{Value, json};

fn channel_seed(base: &str, protocol: config::Protocol) -> common::Seed {
    let mut seed = common::test_seed(base);
    seed.channels[0].protocol = protocol;
    seed
}

fn responses_upstream_response(model: &str) -> Value {
    json!({
        "id": "resp_1", "object": "response", "status": "completed", "model": model,
        "output": [
            { "id": "msg_1", "type": "message", "role": "assistant",
              "content": [ { "type": "output_text", "text": "ok", "annotations": [] } ] }
        ],
        "usage": { "input_tokens": 100, "output_tokens": 20, "total_tokens": 120 }
    })
}

fn anthropic_upstream_response() -> Value {
    json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet",
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 25, "output_tokens": 12 }
    })
}

/// chat 入站音频 + PDF → Responses 渠道：媒体经 IR 以 `input_file` part 出站
/// （音频 file_data 按 data URL，PDF 保留 filename），下游零告警。
#[tokio::test]
async fn chat_audio_and_file_parts_reach_responses_channel() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        channel_seed(&bases[0], config::Protocol::OpenAiResponses)
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(responses_upstream_response(
            TEST_MODEL,
        )));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "summarize" },
                    { "type": "input_audio", "input_audio": { "data": "UklGRg==", "format": "wav" } },
                    { "type": "file", "file": { "filename": "doc.pdf", "file_data": "data:application/pdf;base64,JVBERi0=" } }
                ]
            }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    let content = &received[0]["input"][0]["content"];
    assert_eq!(
        content[0],
        json!({ "type": "input_text", "text": "summarize" })
    );
    assert_eq!(
        content[1],
        json!({
            "type": "input_file",
            "filename": "data",
            "file_data": "data:audio/wav;base64,UklGRg=="
        }),
        "音频应按 input_file 通路出站（无文件名时用缺省占位）"
    );
    assert_eq!(
        content[2],
        json!({
            "type": "input_file",
            "filename": "doc.pdf",
            "file_data": "data:application/pdf;base64,JVBERi0="
        }),
        "PDF 应携带 filename 出站"
    );

    let body: Value = resp.json().await.expect("响应应可解析");
    assert!(
        body.get("gateway").is_none(),
        "responses 通路可用不应有 warning: {body}"
    );
}

/// chat 入站 PDF → Anthropic 渠道：以 document 块出站；音频无承载，丢弃且
/// warning 随响应回传下游。
#[tokio::test]
async fn chat_audio_drops_with_warning_and_pdf_reaches_anthropic() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        channel_seed(&bases[0], config::Protocol::AnthropicMessages)
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(anthropic_upstream_response()));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "read this" },
                    { "type": "file", "file": { "filename": "doc.pdf", "file_data": "data:application/pdf;base64,JVBERi0=" } },
                    { "type": "input_audio", "input_audio": { "data": "UklGRg==", "format": "wav" } }
                ]
            }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    let content = &received[0]["messages"][0]["content"];
    assert!(
        content
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|block| {
                block["type"] == "document"
                    && block["source"]["media_type"] == "application/pdf"
                    && block["source"]["data"] == "JVBERi0="
            })),
        "PDF 应以 document 块出站: {content}"
    );
    assert!(
        content
            .as_array()
            .is_none_or(|blocks| blocks.iter().all(|block| block["type"] != "image")),
        "音频无承载，不应产出任何媒体块"
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
        features.contains(&"media"),
        "音频丢弃应回传 media warning: {body}"
    );
}
