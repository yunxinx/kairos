//! 渠道级 reasoning_content 兼容输出（#07）端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒，断言 mock 上游收到的出站请求体与下游收到的
//! 响应帧。覆盖三态开关（auto 按**出站模型名**命中厂商提示词表、always
//! 强制开启、off 恢复丢弃 + warning）在两个方向的表现：面向 chat 上游的
//! 请求历史回放（非流式），与 chat 下游的流式思维链增量转发；另覆盖 chat
//! 上游流式 `reasoning_content` 增量进 IR 的双向通路（跨族到 anthropic
//! 下游可见、同族受渠道开关约束）。

mod common;

use common::{TEST_TOKEN_KEY, TestGateway, UpstreamBehavior, collect_sse_frames};
use kairos::config::{self, ReasoningOutputMode};
use serde_json::{Value, json};

/// auto 渠道（出站模型名命中提示词表）：经别名强制走 IR 路径后，assistant
/// 历史的 reasoning_content 回放给上游。
#[tokio::test]
async fn auto_channel_replays_reasoning_content_upstream() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        // 出站模型名命中提示词表（deepseek），请求走别名强制 IR 路径。
        seed.channels[0].models = vec!["deepseek-chat".to_string()];
        seed.channels[0].model_aliases = [("fast".to_string(), "deepseek-chat".to_string())]
            .into_iter()
            .collect();
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-1", "object": "chat.completion", "created": 0, "model": "deepseek-chat",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "ok" }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "messages": [
                { "role": "user", "content": "925 ÷ 5 等于多少？" },
                { "role": "assistant", "content": "185", "reasoning_content": "先算 900 ÷ 5。" },
                { "role": "user", "content": "再除以 5 呢？" }
            ]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0]["messages"][1]["reasoning_content"],
        json!("先算 900 ÷ 5。"),
        "auto 命中时历史思维链应回放上游"
    );

    let body: Value = resp.json().await.expect("响应应可解析");
    assert!(
        body.get("gateway").is_none(),
        "回放成功不应有 warning: {body}"
    );
}

/// off 渠道：请求历史回放恢复丢弃 + warning，响应显式回传告警。
#[tokio::test]
async fn off_channel_drops_replay_with_warning() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        seed.channels[0].reasoning_output = ReasoningOutputMode::Off;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-1", "object": "chat.completion", "created": 0, "model": "gpt-4o-mini",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "ok" }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "messages": [
                { "role": "user", "content": "925 ÷ 5 等于多少？" },
                { "role": "assistant", "content": "185", "reasoning_content": "先算 900 ÷ 5。" },
                { "role": "user", "content": "再除以 5 呢？" }
            ]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert!(
        received[0]["messages"][1]
            .get("reasoning_content")
            .is_none(),
        "off 渠道不应回放思维链"
    );

    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(
        body["gateway"]["warnings"][0]["feature"],
        json!("reasoning")
    );
}

/// anthropic 渠道（always 强制开启）：上游 thinking 增量以
/// `delta.reasoning_content` 转发 chat 下游，finish 帧无告警。
#[tokio::test]
async fn always_channel_streams_reasoning_deltas_downstream() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        seed.channels[0].protocol = config::Protocol::AnthropicMessages;
        seed.channels[0].reasoning_output = ReasoningOutputMode::Always;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "type": "message_start",
            "message": { "id": "msg_01r", "model": "claude-sonnet", "usage": { "input_tokens": 8, "output_tokens": 0 } }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "thinking", "thinking": "" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "thinking_delta", "thinking": "先想一步。" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "input_tokens": 8, "output_tokens": 3 }
        }))
        .unwrap(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let frames = collect_sse_frames(resp).await;
    let reasoning: String = frames
        .iter()
        .map(|f| {
            f.data["choices"][0]["delta"]["reasoning_content"]
                .as_str()
                .unwrap_or("")
        })
        .collect();
    assert_eq!(reasoning, "先想一步。", "thinking 增量应转发下游");
    // 首个内容增量（reasoning）携带 role，且整个流只出现一次。
    let role_count = frames
        .iter()
        .filter(|f| f.data["choices"][0]["delta"]["role"] == "assistant")
        .count();
    assert_eq!(role_count, 1, "role 应恰好随首个内容增量下发一次");
    let finish = frames
        .iter()
        .find(|f| f.data["choices"][0]["finish_reason"] == "stop")
        .expect("应有 finish 帧");
    assert!(
        finish.data.get("gateway").is_none(),
        "开启开关时 finish 帧不应有 reasoning 告警"
    );
}

/// off 渠道的流式：思维链增量不下发，finish 帧以告警显式回传信息损失。
#[tokio::test]
async fn off_channel_streams_finish_warning_instead() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        seed.channels[0].protocol = config::Protocol::AnthropicMessages;
        seed.channels[0].reasoning_output = ReasoningOutputMode::Off;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "type": "message_start",
            "message": { "id": "msg_01r", "model": "claude-sonnet", "usage": { "input_tokens": 8, "output_tokens": 0 } }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "thinking", "thinking": "" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "thinking_delta", "thinking": "先想一步。" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "input_tokens": 8, "output_tokens": 3 }
        }))
        .unwrap(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let frames = collect_sse_frames(resp).await;
    let has_reasoning = frames.iter().any(|f| {
        f.data["choices"][0]["delta"]
            .get("reasoning_content")
            .is_some()
    });
    assert!(!has_reasoning, "off 渠道不应下发思维链增量");
    let finish = frames
        .iter()
        .find(|f| f.data["choices"][0]["finish_reason"] == "stop")
        .expect("应有 finish 帧");
    assert_eq!(
        finish.data["gateway"]["warnings"][0]["feature"],
        json!("reasoning")
    );
}

/// chat 上游流式 `reasoning_content` 增量 → anthropic 下游 thinking 块。
///
/// 思维链先行（DeepSeek 首帧 role + 空 reasoning_content）经 IR
/// ReasoningDelta 通路跨族可见：thinking 块在文本块之前开启，块切换先
/// stop 再 start，全流无信息损失告警。
#[tokio::test]
async fn chat_upstream_reasoning_streams_to_anthropic_downstream() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        // 经别名强制 IR 路径（同协议直通不经 IR，思维链无从转换）。
        seed.channels[0].model_aliases = [("fast".to_string(), "deepseek-chat".to_string())]
            .into_iter()
            .collect();
        seed
    })
    .await;
    // DeepSeek 形状：首帧 role + 空 reasoning_content，随后思维链增量，再文本。
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-r1", "object": "chat.completion.chunk", "model": "deepseek-chat",
            "choices": [{ "index": 0, "delta": {
                "role": "assistant", "content": null, "reasoning_content": ""
            } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-r1", "object": "chat.completion.chunk", "model": "deepseek-chat",
            "choices": [{ "index": 0, "delta": { "reasoning_content": "先想一步。" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-r1", "object": "chat.completion.chunk", "model": "deepseek-chat",
            "choices": [{ "index": 0, "delta": { "content": "185" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-r1", "object": "chat.completion.chunk", "model": "deepseek-chat",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        }))
        .unwrap(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/messages", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "max_tokens": 1024,
            "stream": true,
            "messages": [{ "role": "user", "content": "925 ÷ 5 等于多少？" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let frames = collect_sse_frames(resp).await;
    let thinking: String = frames
        .iter()
        .filter(|f| f.data["delta"]["type"] == "thinking_delta")
        .filter_map(|f| f.data["delta"]["thinking"].as_str())
        .collect();
    assert_eq!(
        thinking, "先想一步。",
        "chat 上游思维链应对 anthropic 下游可见"
    );
    let text: String = frames
        .iter()
        .filter(|f| f.data["delta"]["type"] == "text_delta")
        .filter_map(|f| f.data["delta"]["text"].as_str())
        .collect();
    assert_eq!(text, "185");

    // 块切换纪律：thinking 块 stop 之后才开 text 块。
    let thinking_block = frames
        .iter()
        .find(|f| f.data["content_block"]["type"] == "thinking")
        .expect("应有 thinking 块");
    let thinking_block_index = thinking_block.data["index"].as_i64().expect("块应带 index");
    let thinking_stop = frames
        .iter()
        .position(|f| {
            f.data["type"] == "content_block_stop" && f.data["index"] == thinking_block_index
        })
        .expect("thinking 块应被 stop 收尾");
    let text_start = frames
        .iter()
        .position(|f| f.data["content_block"]["type"] == "text")
        .expect("应有 text 块");
    assert!(
        thinking_stop < text_start,
        "thinking 块应先 stop 再开 text 块"
    );
    // 思维链完整转发：无信息损失告警（anthropic 下游以 ping 携带 warnings）。
    assert!(
        frames.iter().all(|f| f.event.as_deref() != Some("ping")),
        "跨族转发成功不应有告警"
    );
}

/// chat 上游思维链 → chat 下游（always 强制开启）：以 `delta.reasoning_content`
/// 增量下发，role 恰好一次，finish 帧无告警。
#[tokio::test]
async fn chat_upstream_reasoning_streams_to_chat_downstream_when_enabled() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        seed.channels[0].reasoning_output = ReasoningOutputMode::Always;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-r2", "object": "chat.completion.chunk", "model": "gpt-4o-mini",
            "choices": [{ "index": 0, "delta": {
                "role": "assistant", "reasoning_content": "先想一步。"
            } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-r2", "object": "chat.completion.chunk", "model": "gpt-4o-mini",
            "choices": [{ "index": 0, "delta": { "content": "185" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-r2", "object": "chat.completion.chunk", "model": "gpt-4o-mini",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        }))
        .unwrap(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "stream": true,
            "messages": [{ "role": "user", "content": "925 ÷ 5 等于多少？" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let frames = collect_sse_frames(resp).await;
    let reasoning: String = frames
        .iter()
        .filter_map(|f| f.data["choices"][0]["delta"]["reasoning_content"].as_str())
        .collect();
    assert_eq!(reasoning, "先想一步。", "思维链增量应转发下游");
    let content: String = frames
        .iter()
        .filter_map(|f| f.data["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(content, "185");
    let role_count = frames
        .iter()
        .filter(|f| f.data["choices"][0]["delta"]["role"] == "assistant")
        .count();
    assert_eq!(role_count, 1, "role 应恰好随首个内容增量下发一次");
    let finish = frames
        .iter()
        .find(|f| f.data["choices"][0]["finish_reason"] == "stop")
        .expect("应有 finish 帧");
    assert!(
        finish.data.get("gateway").is_none(),
        "开启开关时 finish 帧不应有 reasoning 告警"
    );
}

/// chat 上游思维链 → chat 下游（off）：增量不下发，finish 帧显式告警。
#[tokio::test]
async fn chat_upstream_reasoning_dropped_when_disabled() {
    let (mut gw, _upstreams) = TestGateway::start_with_multi(1, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        seed.channels[0].reasoning_output = ReasoningOutputMode::Off;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-r3", "object": "chat.completion.chunk", "model": "gpt-4o-mini",
            "choices": [{ "index": 0, "delta": {
                "role": "assistant", "reasoning_content": "先想一步。"
            } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-r3", "object": "chat.completion.chunk", "model": "gpt-4o-mini",
            "choices": [{ "index": 0, "delta": { "content": "185" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-r3", "object": "chat.completion.chunk", "model": "gpt-4o-mini",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        }))
        .unwrap(),
    ]));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "stream": true,
            "messages": [{ "role": "user", "content": "925 ÷ 5 等于多少？" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let frames = collect_sse_frames(resp).await;
    assert!(
        frames.iter().all(|f| f.data["choices"][0]["delta"]
            .get("reasoning_content")
            .is_none()),
        "off 渠道不应下发思维链增量"
    );
    let finish = frames
        .iter()
        .find(|f| f.data["choices"][0]["finish_reason"] == "stop")
        .expect("应有 finish 帧");
    assert_eq!(
        finish.data["gateway"]["warnings"][0]["feature"],
        json!("reasoning")
    );
}
