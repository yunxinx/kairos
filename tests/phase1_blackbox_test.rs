//! 阶段一黑盒验收：跨协议转换的关键场景在网关边界逐项核验——
//! developer/max_completion_tokens 入站、reasoning effort 跨族映射、
//! tool_choice 矩阵、tool id 清洗配对。流内错误的 failover 与错误帧由
//! streaming_test / chat_completions_test 覆盖。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use kairos::config;
use serde_json::{Value, json};

/// 指定渠道协议构造 seed（其余沿用测试默认）。
fn seed_with_protocol(protocol: config::Protocol) -> impl Fn(&str) -> common::Seed {
    move |base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = protocol;
        seed
    }
}

/// 发起 Chat Completions 非流式请求。
async fn post_chat(base: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", base))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&body)
        .send()
        .await
        .expect("应能请求网关")
}

/// chat 入站的 `developer` 角色与 `max_completion_tokens` 到各协议上游的
/// 出站形状：anthropic 归并进顶层 system + `max_tokens`；responses 归并进
/// instructions + `max_output_tokens`；chat 同族走直通按请求原字段保留。
#[tokio::test]
async fn developer_role_and_max_completion_tokens_reach_each_upstream() {
    let request = json!({
        "model": TEST_MODEL,
        "max_completion_tokens": 512,
        "messages": [
            { "role": "developer", "content": "以 JSON 输出" },
            { "role": "user", "content": "上海天气？" }
        ]
    });

    // anthropic 渠道：developer 归 System → 顶层 system；上限映射 max_tokens。
    let mut gw =
        TestGateway::start_with(seed_with_protocol(config::Protocol::AnthropicMessages)).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet",
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 2 }
    })));
    let resp = post_chat(&gw.base_url(), request.clone()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    assert_eq!(received[0]["system"], json!("以 JSON 输出"));
    assert_eq!(received[0]["max_tokens"], json!(512));
    assert_eq!(received[0]["messages"][0]["role"], "user");
    gw.db_dir.close().expect("临时目录应可清理");

    // responses 渠道：developer 归 instructions；上限映射 max_output_tokens。
    let mut gw =
        TestGateway::start_with(seed_with_protocol(config::Protocol::OpenAiResponses)).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "resp_1", "object": "response", "status": "completed", "model": TEST_MODEL,
        "output": [
            { "id": "msg_1", "type": "message", "role": "assistant",
              "content": [ { "type": "output_text", "text": "ok", "annotations": [] } ] }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 2, "total_tokens": 12 }
    })));
    let resp = post_chat(&gw.base_url(), request.clone()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    assert_eq!(received[0]["instructions"], json!("以 JSON 输出"));
    assert_eq!(received[0]["max_output_tokens"], json!(512));
    gw.db_dir.close().expect("临时目录应可清理");

    // chat 同族：直通转发请求字节，developer 角色与原字段名零改写。
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-1", "object": "chat.completion", "model": TEST_MODEL,
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "ok" }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    })));
    let resp = post_chat(&gw.base_url(), request).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    assert_eq!(received[0]["messages"][0]["role"], "developer");
    assert_eq!(received[0]["max_completion_tokens"], json!(512));
}

/// chat `reasoning_effort: high` 经 legacy 模型形态兜底为 anthropic budget
/// 阶梯（24576）。
#[tokio::test]
async fn chat_reasoning_effort_maps_to_anthropic_budget() {
    let mut gw =
        TestGateway::start_with(seed_with_protocol(config::Protocol::AnthropicMessages)).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet",
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 2 }
    })));
    let resp = post_chat(
        &gw.base_url(),
        json!({
            "model": TEST_MODEL,
            "reasoning_effort": "high",
            "messages": [{ "role": "user", "content": "难题" }]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    assert_eq!(
        received[0]["thinking"],
        json!({ "type": "enabled", "budget_tokens": 24576 })
    );
}

/// anthropic 入站 thinking budget → responses 渠道出 `reasoning.effort`。
#[tokio::test]
async fn anthropic_budget_maps_to_responses_effort() {
    let mut gw =
        TestGateway::start_with(seed_with_protocol(config::Protocol::OpenAiResponses)).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "resp_1", "object": "response", "status": "completed", "model": TEST_MODEL,
        "output": [
            { "id": "msg_1", "type": "message", "role": "assistant",
              "content": [ { "type": "output_text", "text": "ok", "annotations": [] } ] }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 2, "total_tokens": 12 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": TEST_MODEL,
            "max_tokens": 1024,
            "thinking": { "type": "enabled", "budget_tokens": 24576 },
            "messages": [{ "role": "user", "content": "难题" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received[0]["reasoning"], json!({ "effort": "high" }));
}

/// tool_choice 矩阵：chat `required` ↔ anthropic `any`，指名工具跨族保形。
#[tokio::test]
async fn tool_choice_maps_across_protocols() {
    let tool = json!({
        "type": "function",
        "function": { "name": "get_weather", "parameters": { "type": "object", "properties": {} } }
    });
    let anthropic_response = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet",
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 2 }
    });

    // chat required → anthropic any。
    let mut gw =
        TestGateway::start_with(seed_with_protocol(config::Protocol::AnthropicMessages)).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(anthropic_response.clone()));
    let resp = post_chat(
        &gw.base_url(),
        json!({
            "model": TEST_MODEL,
            "tool_choice": "required",
            "tools": [tool],
            "messages": [{ "role": "user", "content": "天气？" }]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        gw.upstream.received()[0]["tool_choice"],
        json!({ "type": "any" })
    );
    gw.db_dir.close().expect("临时目录应可清理");

    // chat 指名工具 → anthropic tool。
    let mut gw =
        TestGateway::start_with(seed_with_protocol(config::Protocol::AnthropicMessages)).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(anthropic_response));
    let resp = post_chat(
        &gw.base_url(),
        json!({
            "model": TEST_MODEL,
            "tool_choice": { "type": "function", "function": { "name": "get_weather" } },
            "tools": [tool],
            "messages": [{ "role": "user", "content": "天气？" }]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        gw.upstream.received()[0]["tool_choice"],
        json!({ "type": "tool", "name": "get_weather" })
    );
    gw.db_dir.close().expect("临时目录应可清理");

    // anthropic any → chat required。
    let mut gw = TestGateway::start_with(seed_with_protocol(config::Protocol::OpenAiChat)).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-1", "object": "chat.completion", "model": TEST_MODEL,
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "ok" }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    })));
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": TEST_MODEL,
            "max_tokens": 1024,
            "tool_choice": { "type": "any" },
            "tools": [ { "name": "get_weather", "input_schema": { "type": "object", "properties": {} } } ],
            "messages": [{ "role": "user", "content": "天气？" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(gw.upstream.received()[0]["tool_choice"], json!("required"));
}

/// tool id 非法字符清洗与空 id 生成：anthropic 出站 tool_use 与 tool_result
/// 经同一映射保持配对。
#[tokio::test]
async fn illegal_tool_ids_sanitized_and_paired() {
    let mut gw =
        TestGateway::start_with(seed_with_protocol(config::Protocol::AnthropicMessages)).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet",
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 2 }
    })));

    let resp = post_chat(
        &gw.base_url(),
        json!({
            "model": TEST_MODEL,
            "messages": [
                { "role": "user", "content": "天气？" },
                { "role": "assistant", "content": "", "tool_calls": [ {
                    "id": "we!rd@id", "type": "function",
                    "function": { "name": "get_weather", "arguments": "{\"city\":\"上海\"}" }
                } ] },
                { "role": "tool", "tool_call_id": "we!rd@id", "content": "晴" },
                { "role": "assistant", "content": "", "tool_calls": [ {
                    "id": "", "type": "function",
                    "function": { "name": "get_time", "arguments": "{}" }
                } ] },
                { "role": "tool", "tool_call_id": "", "content": "正午" }
            ]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    let messages = received[0]["messages"].as_array().expect("应有消息序列");

    // 非法 id 清洗后 tool_use 与 tool_result 配对一致。
    let first_assistant = &messages[1];
    assert_eq!(first_assistant["content"][0]["type"], "tool_use");
    assert_eq!(first_assistant["content"][0]["id"], json!("we_rd_id"));
    let tool_result_user = &messages[2];
    assert_eq!(tool_result_user["content"][0]["type"], "tool_result");
    assert_eq!(
        tool_result_user["content"][0]["tool_use_id"],
        json!("we_rd_id")
    );

    // 空 id 生成一次并两侧配对。
    let second_assistant = &messages[3];
    assert_eq!(second_assistant["content"][0]["type"], "tool_use");
    let generated = second_assistant["content"][0]["id"]
        .as_str()
        .expect("生成 id 应为字符串");
    assert!(
        generated.starts_with("toolu_") && generated.len() > "toolu_".len(),
        "空 id 应生成 toolu_ 前缀兜底: {generated}"
    );
    let second_result = &messages[4];
    assert_eq!(
        second_result["content"][0]["tool_use_id"],
        json!(generated),
        "同一原始空 id 的 tool_result 应配对同一生成 id"
    );
}
