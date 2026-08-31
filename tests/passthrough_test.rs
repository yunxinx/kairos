//! 直通快路径（#07）端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒，断言外部可观察行为——mock 上游收到的出站请求体、
//! 下游收到的响应字节流、SQLite 中的计费与日志。覆盖：同协议直通转发（请求体
//! 仅目标性补丁、响应字节级一致、逐帧嗅探 usage 计费）、跨协议/别名回落 IR 路径、
//! 快路径不免认证与计费、failover 只发生在首字节之前。

mod common;

use std::time::Duration;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior, collect_sse_frames};
use futures_util::StreamExt;
use kairos::config;
use kairos::store::resources::Channel;
use serde_json::{Value, json};

/// 发起非流式 Chat Completions 请求。
async fn send_completion(base: &str) -> reqwest::Response {
    let client = reqwest::Client::new();
    client
        .post(format!("{}/v1/chat/completions", base))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关")
}

/// 发起流式 Chat Completions 请求。
async fn send_stream(base: &str) -> reqwest::Response {
    let client = reqwest::Client::new();
    client
        .post(format!("{}/v1/chat/completions", base))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关")
}

/// 同协议且未命中别名：非流式请求走直通快路径。
///
/// mock 上游收到的出站请求体与下游请求字节级一致（非流式无任何补丁）；响应
/// 字节级透传。
#[tokio::test]
async fn non_stream_passthrough_forwards_body_and_response() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-p", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hello!"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
    })));

    let resp = send_completion(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("application/json"),
        "非流式直通应带回 Content-Type，实际 {content_type:?}"
    );

    // 下游收到的响应与 mock 上游返回的字节一致（且含 usage）。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "Hello!");
    assert_eq!(body["usage"]["prompt_tokens"], 10);

    // mock 上游收到的出站请求体：与下游 body 一致，仅目标性补丁。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1, "mock 上游应收一条请求");
    assert_eq!(received[0]["model"], TEST_MODEL);
    assert_eq!(received[0]["messages"][0]["role"], "user");
    assert_eq!(received[0]["messages"][0]["content"], "hi");
    // 非流式直通不加任何补丁：stream 保持下游原样（未发送即无此字段），也不注入
    // stream_options（include_usage 是流式计费的注入面，非流式响应自带顶层 usage）。
    assert!(
        received[0].get("stream").is_none(),
        "非流式直通不应改写 stream 字段"
    );
    assert!(
        received[0].get("stream_options").is_none(),
        "非流式直通不应注入 stream_options（include_usage 只为流式计费注入）"
    );
}

/// 同协议流式直通：响应字节流直通，逐帧嗅探 usage 计费。
///
/// mock 上游以 SSE 流返回，下游逐帧收到与上游一致的 `chat.completion.chunk`；
/// 计费金额与 IR 完整路径口径一致。
#[tokio::test]
async fn stream_passthrough_forwards_and_bills_usage() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "Hel" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "content": "lo" } }]
        }))
        .unwrap(),
        // usage 独立末帧（真实 `include_usage` 帧型）。
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [],
            "usage": {
                "prompt_tokens": 1250, "completion_tokens": 100, "total_tokens": 1350,
                "prompt_tokens_details": { "cached_tokens": 200, "cache_write_tokens": 50 }
            }
        }))
        .unwrap(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let frames = collect_sse_frames(resp).await;

    // 直通：帧与上游原样一致（含原始 finish/usage 帧）。
    assert_eq!(frames[0].data["object"], "chat.completion.chunk");
    assert_eq!(frames[0].data["choices"][0]["delta"]["content"], "Hel");
    assert_eq!(frames[1].data["choices"][0]["delta"]["content"], "lo");
    let usage_frame = frames.last().expect("应有 usage 帧");
    assert_eq!(usage_frame.data["usage"]["completion_tokens"], 100);

    // 流式直通唯一授权补丁：注入 stream_options.include_usage（usage 帧与计费的前提）。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1, "mock 上游应收一条请求");
    assert_eq!(received[0]["stream"], true, "流式请求 stream 应保持 true");
    assert_eq!(
        received[0]["stream_options"]["include_usage"], true,
        "流式直通应注入 stream_options.include_usage"
    );

    // 直通计费：usage 10o/100 等 → 与 IR 路径同一口径。
    // usage：input 1000 / output 100 / cache_read 200 / cache_write 50。
    // 费用 = 1000*2.5 + 100*10 + 200*1.25 + 50*10 = 2500+1000+250+500 = 4250。
    common::wait_for_request_persistence(&gw.pool).await;
    let row: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT ub.balance_usd_micros, tb.settled_usd_micros, input_tokens, output_tokens, cost_usd_micros \
         FROM tokens t \
         JOIN user_balance ub ON ub.user_id = t.user_id \
         JOIN token_balance tb ON tb.token_key = t.token_key \
         JOIN request_log ON request_log.token_key = t.token_key \
         WHERE t.token_key = ?",
    )
    .bind(TEST_TOKEN_KEY)
    .fetch_one(&gw.pool)
    .await
    .expect("应能查询余额与日志");
    assert_eq!(row.0, 5_000_000 - 4250, "余额应扣减 4250");
    assert_eq!(row.1, 4250, "累计结算应增加 4250");
    assert_eq!(row.2, 1000, "日志应记录 input=1000");
    assert_eq!(row.3, 100, "日志应记录 output=100");
    assert_eq!(row.4, 4250, "日志应记录费用 4250");
}

/// 同协议直通在首帧前遇到上游流内错误时，应在下游收到响应头前切换渠道。
#[tokio::test]
async fn stream_passthrough_fails_over_on_pre_first_error() {
    fn anthropic_channels(bases: &[String]) -> common::Seed {
        let mut seed = common::test_seed(&bases[0]);
        seed.channels = bases
            .iter()
            .enumerate()
            .map(|(index, base)| {
                let mut channel = seed.channels[0].clone();
                channel.name = format!("anthropic-{index}");
                channel.protocol = config::Protocol::AnthropicMessages;
                channel.base_url = base.clone();
                channel
            })
            .collect();
        seed
    }

    let (gw, mut upstreams) = TestGateway::start_with_multi(2, anthropic_channels).await;
    upstreams[0].set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "type": "message_start",
            "message": { "type": "message", "role": "assistant", "id": "msg-1", "model": TEST_MODEL, "content": [] }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "error",
            "error": { "type": "overloaded_error", "message": "Overloaded" }
        }))
        .unwrap(),
    ]));
    upstreams[1].set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "type": "message_start",
            "message": { "type": "message", "role": "assistant", "id": "msg-2", "model": TEST_MODEL, "content": [] }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_start", "index": 0, "content_block": { "type": "text" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "ok" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        }))
        .unwrap(),
        serde_json::to_string(&json!({ "type": "message_stop" })).unwrap(),
    ]));

    let response = reqwest::Client::new()
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "max_tokens": 16,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let frames = collect_sse_frames(response).await;
    assert!(
        frames
            .iter()
            .any(|frame| frame.data["delta"]["text"] == json!("ok")),
        "下游应收到次渠道的正文"
    );
    assert!(
        frames
            .iter()
            .all(|frame| !serde_json::to_string(&frame.data)
                .unwrap()
                .contains("Overloaded")),
        "首渠道的流内错误不得泄漏给下游"
    );
    assert_eq!(upstreams[0].received().len(), 1);
    assert_eq!(upstreams[1].received().len(), 1);
}

/// raw chunk 直搬：跨块的大帧不参与转发决策，拼接后的下游字节与上游一致。
///
/// usage 帧也刻意跨块切分，验证旁路解析仍能完成计费；SSE 注释、CRLF 与字段
/// 空格用于区分原始字节直搬和逐帧重组。
#[tokio::test]
async fn stream_passthrough_copies_large_cross_chunk_body_and_sniffs_usage() {
    let mut gw = TestGateway::start().await;
    let large_text = "x".repeat(128 * 1024);
    let first_frame = format!(
        ": upstream-comment\r\nid: original-id\r\ndata: {{\"id\":\"chatcmpl-raw\",\"object\":\"chat.completion.chunk\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{large_text}\"}}}}]}}\r\n\r\n"
    );
    let usage_frame = concat!(
        "data:{\"id\":\"chatcmpl-raw\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,",
        "\"total_tokens\":12}}\n\n"
    );
    let upstream = [first_frame.as_bytes(), usage_frame.as_bytes()].concat();
    let split_points = [17, 64 * 1024, first_frame.len() + 43];
    let chunks = upstream.split_at(split_points[0]);
    let (second, tail) = chunks.1.split_at(split_points[1] - split_points[0]);
    let (third, fourth) = tail.split_at(split_points[2] - split_points[1]);
    gw.upstream.set_behavior(UpstreamBehavior::RawSse(vec![
        chunks.0.to_vec(),
        second.to_vec(),
        third.to_vec(),
        fourth.to_vec(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let downstream = resp.bytes().await.expect("响应流应可读");
    let expected = [upstream.as_slice(), b"data: [DONE]\n\n"].concat();
    assert_eq!(downstream.as_ref(), expected, "直通响应应原样拼接上游块");

    let mut billed = None;
    for _ in 0..100 {
        billed = sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT input_tokens, output_tokens, cost_usd_micros FROM request_log LIMIT 1",
        )
        .fetch_optional(&gw.pool)
        .await
        .expect("应能查询请求日志");
        if billed.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(billed, Some((10, 2, 45)), "跨块 usage 应完成结算");
}

/// 上游自带的 OpenAI `[DONE]` 不直搬，网关仍在结算完成后仅下发一个哨兵。
#[tokio::test]
async fn stream_passthrough_replaces_upstream_done_after_settlement() {
    let mut gw = TestGateway::start().await;
    let usage = b"data: {\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\n";
    let mixed = b"event: custom\ndata: hello\ndata:\t[DONE]\n\n";
    gw.upstream.set_behavior(UpstreamBehavior::RawSse(vec![
        usage.to_vec(),
        mixed[..19].to_vec(),
        mixed[19..].to_vec(),
        b"event: terminal\r\ndata: [DO".to_vec(),
        b"NE]\r\n\r\n".to_vec(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    let downstream = resp.bytes().await.expect("响应流应可读");
    let expected = [
        usage.as_slice(),
        b"event: custom\ndata: hello\n\n",
        b"data: [DONE]\n\n",
    ]
    .concat();
    assert_eq!(downstream.as_ref(), expected, "终止哨兵只能出现一次");

    let settled: i64 = {
        common::wait_for_request_persistence(&gw.pool).await;
        sqlx::query_scalar("SELECT settled_usd_micros FROM token_balance WHERE token_key = ?")
            .bind(TEST_TOKEN_KEY)
            .fetch_one(&gw.pool)
            .await
            .expect("读到哨兵时结算应已落库")
    };
    assert_eq!(settled, 45);
}

/// 未闭合的普通上游事件仍按已接收字节直搬，并与网关终止哨兵分隔。
#[tokio::test]
async fn stream_passthrough_separates_done_after_unclosed_event() {
    let mut gw = TestGateway::start().await;
    let upstream = b"event: custom\r\ndata: partial";
    gw.upstream.set_behavior(UpstreamBehavior::RawSse(vec![
        upstream[..12].to_vec(),
        upstream[12..].to_vec(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    let downstream = resp.bytes().await.expect("响应流应可读");
    let expected = [upstream.as_slice(), b"\n\ndata: [DONE]\n\n"].concat();
    assert_eq!(downstream.as_ref(), expected);
}

/// 混合事件中的上游哨兵行被移除时，EOF 前其余字段不得丢失。
#[tokio::test]
async fn stream_passthrough_preserves_unclosed_mixed_event_tail() {
    let mut gw = TestGateway::start().await;
    let upstream = b"event: custom\ndata: hello\ndata: [DONE]\nid: retained";
    gw.upstream.set_behavior(UpstreamBehavior::RawSse(vec![
        upstream[..7].to_vec(),
        upstream[7..35].to_vec(),
        upstream[35..].to_vec(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    let downstream = resp.bytes().await.expect("响应流应可读");
    let expected = b"event: custom\ndata: hello\nid: retained\n\ndata: [DONE]\n\n";
    assert_eq!(downstream.as_ref(), expected);
}

/// 下游读到首块后断开，直通任务仍继续消费上游尾部 usage 并完成结算。
#[tokio::test]
async fn stream_passthrough_settles_after_downstream_disconnect() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::DelayedRawSse {
        chunks: vec![
            b"data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n".to_vec(),
            b"data: {\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\n".to_vec(),
        ],
        delay_ms: 100,
    });

    let resp = send_stream(&gw.base_url()).await;
    let mut downstream = resp.bytes_stream();
    let mut prefix = Vec::new();
    while !prefix.windows(5).any(|window| window == b"first") {
        let chunk = downstream
            .next()
            .await
            .expect("正文前响应流不应结束")
            .expect("响应块应可读");
        prefix.extend_from_slice(&chunk);
    }
    drop(downstream);

    let mut settled = 0;
    for _ in 0..100 {
        settled =
            sqlx::query_scalar("SELECT settled_usd_micros FROM token_balance WHERE token_key = ?")
                .bind(TEST_TOKEN_KEY)
                .fetch_one(&gw.pool)
                .await
                .expect("应能查询余额");
        if settled > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(settled, 45, "断连后仍应消费尾部 usage 并结算");
}

/// 快路径不免认证：未认证请求不触发直通出站，返回 401。
#[tokio::test]
async fn passthrough_requires_auth() {
    let gw = TestGateway::start().await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");

    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(gw.upstream.received().is_empty(), "未认证不应出站");
}

/// 快路径不免计费：余额不足时在调用上游之前被拒绝（402），不出站。
#[tokio::test]
async fn passthrough_requires_balance_before_upstream() {
    // 用余额为 0 的令牌配置。
    let gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.tokens[0].balance_usd = 0.0;
        seed
    })
    .await;

    let resp = send_completion(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
    assert!(gw.upstream.received().is_empty(), "余额不足不应出站");
}

/// 命中别名：回落 IR 完整路径，出站请求体经 IR 重编码（非直通），响应模型名重写回入站短名。
#[tokio::test]
async fn alias_hit_falls_back_to_ir_path() {
    let mut gw = TestGateway::start().await;
    // 别名短名 `fast` → 真实模型 `gpt-4o-mini`。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-alias", "object": "chat.completion", "model": "gpt-4o-mini",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 出站请求体经 IR 重编码：模型为真实名，不含直通补丁字段。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["model"], "gpt-4o-mini", "别名应重写出站模型名");
    assert!(
        received[0].get("stream_options").is_none(),
        "IR 路径不应注入直通补丁"
    );

    // 响应模型名重写回入站短名。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["model"], "fast", "别名命中应重写响应模型名");
}

/// 跨协议通道：回落 IR 完整路径，出站请求体经 IR 重编码（非直通），不注入直通补丁。
#[tokio::test]
async fn cross_protocol_falls_back_to_ir_path() {
    // 渠道协议为 anthropic_messages（≠ 入站 openai_chat），触发 IR 完整路径。
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = config::Protocol::AnthropicMessages;
        seed
    })
    .await;
    // 上游以 Anthropic Messages 格式响应（stub 同协议，网关解码为 IR 再重编码为 openai）。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_01x", "type": "message", "role": "assistant", "model": "gpt-4o",
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    })));

    let resp = send_completion(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    // 入站协议格式：下游收到 openai chat.completion 响应。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "ok");

    // 出站请求体经 IR 重编码为 Anthropic 格式：不含直通补丁字段（非直通路径）。
    let received = gw.upstream.received();
    assert_eq!(received.len(), 1, "mock 上游应收一条请求");
    assert_eq!(received[0]["model"], TEST_MODEL);
    assert_eq!(received[0]["messages"][0]["role"], "user");
    assert!(
        received[0].get("stream_options").is_none(),
        "跨协议回落 IR 路径，不应注入直通补丁"
    );
}

/// 单密钥渠道，其余字段沿用测试默认。
fn channel_of(name: &str, protocol: config::Protocol, base_url: &str) -> Channel {
    Channel {
        name: name.to_string(),
        protocol,
        base_url: base_url.to_string(),
        keys: vec![kairos::store::resources::ChannelKey {
            name: "default".to_string(),
            api_key: "k".to_string(),
            weight: 1,
            enabled: true,
            models: None,
            blocked_models: None,
        }],
        models: vec![TEST_MODEL.to_string()],
        model_aliases: Default::default(),
        timeout_ms: 1000,
        max_retries: 0,
        enabled: true,
        model_group: kairos::store::resources::DEFAULT_MODEL_GROUP.to_string(),
        reasoning_output: Default::default(),
        session_cache_key: Default::default(),
        injects_cache_breakpoints: false,
    }
}

/// 混合协议候选（首渠道 openai_chat、次渠道 openai_responses）。
fn mixed_channel_seed(bases: &[String]) -> common::Seed {
    let mut seed = common::test_seed(&bases[0]);
    seed.channels = vec![
        channel_of("same-protocol", config::Protocol::OpenAiChat, &bases[0]),
        channel_of(
            "cross-protocol",
            config::Protocol::OpenAiResponses,
            &bases[1],
        ),
    ];
    seed
}

/// 混合协议候选（首渠道 openai_responses、次渠道 openai_chat）。
fn mixed_channel_seed_reversed(bases: &[String]) -> common::Seed {
    let mut seed = common::test_seed(&bases[0]);
    seed.channels = vec![
        channel_of(
            "cross-protocol",
            config::Protocol::OpenAiResponses,
            &bases[0],
        ),
        channel_of("same-protocol", config::Protocol::OpenAiChat, &bases[1]),
    ];
    seed
}

/// 路径哨兵：显式 `temperature: null`。字节直通原样保留该键，IR 重编码后
/// `temperature` 为 `None`、出站体不再携带——借此区分渠道实际走了哪条路径。
fn body_with_path_sentinel() -> Value {
    json!({
        "model": TEST_MODEL,
        "messages": [{ "role": "user", "content": "hi" }],
        "temperature": null,
    })
}

/// Responses 上游的非流式成功响应。
fn responses_ok() -> Value {
    json!({
        "id": "resp_01m", "object": "response", "status": "completed", "model": TEST_MODEL,
        "output": [
            { "id": "msg_1", "type": "message", "role": "assistant",
              "content": [ { "type": "output_text", "text": "ok", "annotations": [] } ] }
        ],
        "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 }
    })
}

/// 混合协议候选：同协议首渠道命中时该渠道字节直通（`temperature: null` 哨兵
/// 原样到达），异协议候选不再拖累整条路由退回 IR。
#[tokio::test]
async fn mixed_protocol_route_passthroughs_same_protocol_channel() {
    let (gw, mut upstreams) = TestGateway::start_with_multi(2, mixed_channel_seed).await;
    upstreams[0].set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-m", "object": "chat.completion", "model": TEST_MODEL,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&body_with_path_sentinel())
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["choices"][0]["message"]["content"], "ok");

    let received = upstreams[0].received();
    assert_eq!(received.len(), 1, "只有首渠道出场");
    assert_eq!(
        received[0].get("temperature"),
        Some(&Value::Null),
        "同协议渠道应走字节直通，哨兵字段原样保留"
    );
    assert!(
        upstreams[1].received().is_empty(),
        "首渠道命中时异协议候选不应被调用"
    );
}

/// 混合协议候选 failover：同协议首渠道可重试失败后，异协议候选接手走 IR
/// 编码路径，下游收到入站协议形状的成功响应。
#[tokio::test]
async fn mixed_protocol_route_falls_over_to_ir_for_cross_protocol_channel() {
    let (gw, mut upstreams) = TestGateway::start_with_multi(2, mixed_channel_seed).await;
    upstreams[0].set_behavior(UpstreamBehavior::Status429);
    upstreams[1].set_behavior(UpstreamBehavior::Json(responses_ok()));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&body_with_path_sentinel())
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "ok");

    // 首渠道收到直通字节（含哨兵），接手渠道收到 IR 重编码的 Responses 出站体。
    let first = upstreams[0].received();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].get("temperature"), Some(&Value::Null));
    let second = upstreams[1].received();
    assert_eq!(second.len(), 1);
    assert!(
        second[0].get("input").is_some() && second[0].get("messages").is_none(),
        "异协议接手渠道应收到 Responses 形状的 IR 出站体: {second:?}"
    );
    assert!(
        second[0].get("temperature").is_none(),
        "IR 路径应丢弃哨兵字段（temperature 为 None 不出站）"
    );
}

/// 反序候选：异协议渠道在前走 IR，同协议渠道在后仍字节直通——路径判定
/// 逐渠道独立，与候选顺序无关。
#[tokio::test]
async fn reversed_mixed_route_passes_through_later_same_protocol_channel() {
    let (gw, mut upstreams) = TestGateway::start_with_multi(2, mixed_channel_seed_reversed).await;
    upstreams[0].set_behavior(UpstreamBehavior::Status429);
    upstreams[1].set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-r", "object": "chat.completion", "model": TEST_MODEL,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&body_with_path_sentinel())
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let first = upstreams[0].received();
    assert_eq!(first.len(), 1);
    assert!(
        first[0].get("input").is_some(),
        "异协议首渠道应走 IR 编码路径"
    );
    let second = upstreams[1].received();
    assert_eq!(second.len(), 1);
    assert_eq!(
        second[0].get("temperature"),
        Some(&Value::Null),
        "靠后的同协议渠道仍应字节直通"
    );
}

/// 混合路径候选：同协议直通渠道在前、同协议命中别名的渠道在后（别名改写
/// 出站模型名，只能走 IR）。首渠道 429 后由别名渠道接手，出站模型重写为
/// 别名真名，且直通渠道的哨兵不被拖累。
#[tokio::test]
async fn alias_channel_in_mixed_route_takes_ir_path() {
    let (gw, mut upstreams) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        // 首渠道：名单含入站短名 fast，无别名，出站名与入站名一致 → 直通。
        let mut first = channel_of(
            "passthrough-channel",
            config::Protocol::OpenAiChat,
            &bases[0],
        );
        first.models = vec!["fast".to_string()];
        // 接手渠道：别名 fast → gpt-4o-mini，出站名与入站名不同 → IR。
        let mut second = channel_of("alias-channel", config::Protocol::OpenAiChat, &bases[1]);
        second.models = vec!["gpt-4o-mini".to_string()];
        second.model_aliases = [("fast".to_string(), "gpt-4o-mini".to_string())]
            .into_iter()
            .collect();
        seed.channels = vec![first, second];
        seed
    })
    .await;
    upstreams[0].set_behavior(UpstreamBehavior::Status429);
    upstreams[1].set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-a", "object": "chat.completion", "model": "gpt-4o-mini",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": "fast",
            "messages": [{ "role": "user", "content": "hi" }],
            "temperature": null,
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let first = upstreams[0].received();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].get("temperature"), Some(&Value::Null));
    let second = upstreams[1].received();
    assert_eq!(second.len(), 1);
    assert_eq!(
        second[0]["model"], "gpt-4o-mini",
        "别名渠道接手应重写出站模型名"
    );
    assert!(
        second[0].get("temperature").is_none(),
        "别名渠道应走 IR 路径（哨兵被重编码丢弃）"
    );
}

/// 混合协议候选的流式 failover：同协议首渠道（流式直通尝试，哨兵保留）429
/// 后，异协议渠道接手走 IR 流式路径，下游收到入站协议的 chunk 流。
#[tokio::test]
async fn mixed_protocol_stream_falls_over_to_ir() {
    let (gw, mut upstreams) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = mixed_channel_seed(bases);
        seed.channels[1].protocol = config::Protocol::AnthropicMessages;
        seed
    })
    .await;
    upstreams[0].set_behavior(UpstreamBehavior::Status429);
    upstreams[1].set_behavior(UpstreamBehavior::Sse(vec![
        json!({
            "type": "message_start",
            "message": { "id": "msg_01s", "model": TEST_MODEL, "usage": { "input_tokens": 10, "output_tokens": 0 } }
        })
        .to_string(),
        json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "text", "text": "" }
        })
        .to_string(),
        json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "ok" }
        })
        .to_string(),
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "input_tokens": 10, "output_tokens": 2 }
        })
        .to_string(),
    ]));

    let client = reqwest::Client::new();
    let mut body = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true,
            "temperature": null,
        }))
        .send()
        .await
        .expect("应能请求网关")
        .bytes_stream();
    let mut raw = Vec::new();
    while let Some(chunk) = body.next().await {
        raw.extend_from_slice(&chunk.expect("流分块应可读"));
    }
    let text = String::from_utf8(raw).expect("SSE 流应为 UTF-8");
    assert!(
        text.contains("chat.completion.chunk") && text.contains("ok"),
        "下游应收到入站协议的 chunk 流: {text}"
    );

    // 首渠道的直通尝试（哨兵保留），接手渠道收到 Anthropic 形状的流式出站体。
    let first = upstreams[0].received();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].get("temperature"), Some(&Value::Null));
    let second = upstreams[1].received();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0]["stream"], true, "IR 流式路径应强制 stream");
    assert!(
        second[0].get("stream_options").is_none(),
        "Anthropic 出站不应携带 chat 专属补丁"
    );
}

/// 快路径 failover：首渠道 429，切换到下一同协议渠道，字节直通成功。
#[tokio::test]
async fn passthrough_failover_happens_before_first_byte() {
    let (gw, mut upstreams) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        // 两个同协议渠道：首渠道 429（可重试），次渠道成功。
        seed.channels = vec![
            Channel {
                name: "primary".to_string(),
                protocol: config::Protocol::OpenAiChat,
                base_url: bases[0].clone(),
                keys: vec![kairos::store::resources::ChannelKey {
                    name: "default".to_string(),
                    api_key: "k".to_string(),
                    weight: 1,
                    enabled: true,
                    models: None,
                    blocked_models: None,
                }],
                models: vec![TEST_MODEL.to_string()],
                model_aliases: Default::default(),
                timeout_ms: 1000,
                max_retries: 0,
                enabled: true,
                model_group: kairos::store::resources::DEFAULT_MODEL_GROUP.to_string(),
                reasoning_output: Default::default(),
                session_cache_key: Default::default(),
                injects_cache_breakpoints: false,
            },
            Channel {
                name: "backup".to_string(),
                protocol: config::Protocol::OpenAiChat,
                base_url: bases[1].clone(),
                keys: vec![kairos::store::resources::ChannelKey {
                    name: "default".to_string(),
                    api_key: "k".to_string(),
                    weight: 1,
                    enabled: true,
                    models: None,
                    blocked_models: None,
                }],
                models: vec![TEST_MODEL.to_string()],
                model_aliases: Default::default(),
                timeout_ms: 1000,
                max_retries: 0,
                enabled: true,
                model_group: kairos::store::resources::DEFAULT_MODEL_GROUP.to_string(),
                reasoning_output: Default::default(),
                session_cache_key: Default::default(),
                injects_cache_breakpoints: false,
            },
        ];
        seed
    })
    .await;

    upstreams[0].set_behavior(UpstreamBehavior::Status429);
    upstreams[1].set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-f", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })));

    let resp = send_completion(&gw.base_url()).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "应 failover 到次渠道"
    );

    // 首渠道收到请求（直通补丁），次渠道也收到请求。
    assert_eq!(upstreams[0].received().len(), 1);
    assert_eq!(upstreams[1].received().len(), 1);
    // 次渠道收到与下游一致的非流式直通请求体（字节级原样，不注入任何补丁）。
    assert_eq!(upstreams[1].received()[0]["model"], TEST_MODEL);
    assert!(upstreams[1].received()[0].get("stream").is_none());
    assert!(
        upstreams[1].received()[0].get("stream_options").is_none(),
        "非流式直通不应注入 stream_options"
    );
}

/// 响应头已返回后，上游块间隔超过渠道 `timeout_ms` 则按空闲超时结束流并结算。
#[tokio::test]
async fn stream_passthrough_idle_timeout_ends_stream() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].timeout_ms = 80;
        seed
    })
    .await;
    let first = concat!(
        "data: {\"id\":\"chatcmpl-idle\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"}}]}\n\n"
    );
    gw.upstream.set_behavior(UpstreamBehavior::DelayedRawSse {
        chunks: vec![first.as_bytes().to_vec(), b"data: never\n\n".to_vec()],
        delay_ms: 400,
    });

    let started = std::time::Instant::now();
    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let _frames = collect_sse_frames(resp).await;
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "空闲超时应在第二块到达前结束流，实际 {:?}",
        started.elapsed()
    );
}

/// 未形成完整 SSE 帧的重装缓冲超过上限时向下游发错误事件，避免当成正常结束。
#[tokio::test]
async fn stream_passthrough_caps_reassembly_buffer() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.settings
            .insert("sse_reassembly_max_bytes".to_string(), json!(64));
        seed
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::RawSse(vec![vec![b'x'; 128]]));
    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let bytes = resp.bytes().await.expect("超限后流应结束而非挂起");
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        body.contains("SSE 重装缓冲超过上限"),
        "下游应看到截断错误，实际: {body}"
    );
}

/// 同协议 Anthropic 直通：下游 `anthropic-version` 原样转发，缺省才钉官方默认。
#[tokio::test]
async fn anthropic_passthrough_forwards_inbound_version_header() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = config::Protocol::AnthropicMessages;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_01x", "type": "message", "role": "assistant", "model": "gpt-4o",
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    })));

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .header("anthropic-version", "2024-10-22")
        .header("anthropic-beta", "prompt-caching-2024-07-31")
        .json(&json!({
            "model": TEST_MODEL,
            "max_tokens": 16,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        gw.upstream.received_anthropic_versions(),
        vec![Some("2024-10-22".to_string())],
        "直通应转发下游版本头"
    );
    assert_eq!(
        gw.upstream.received_anthropic_betas(),
        vec![Some("prompt-caching-2024-07-31".to_string())],
        "直通应转发 anthropic-beta"
    );
}

/// 同协议 Anthropic 直通：下游未带版本头时出站钉官方默认。
#[tokio::test]
async fn anthropic_passthrough_defaults_version_when_inbound_omits_it() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = config::Protocol::AnthropicMessages;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_01x", "type": "message", "role": "assistant", "model": "gpt-4o",
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    })));

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages", gw.base_url()))
        .header("x-api-key", TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "max_tokens": 16,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        gw.upstream.received_anthropic_versions(),
        vec![Some("2023-06-01".to_string())],
        "直通缺省版本头应钉官方默认"
    );
}

/// 跨协议回落 IR：即使下游带了更新的版本头，出站仍钉适配器默认。
#[tokio::test]
async fn ir_path_keeps_default_anthropic_version() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = config::Protocol::AnthropicMessages;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "msg_01x", "type": "message", "role": "assistant", "model": "gpt-4o",
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    })));

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .header("anthropic-version", "2024-10-22")
        .header("anthropic-beta", "prompt-caching-2024-07-31")
        .header("openai-organization", "org-ir")
        .header("openai-project", "proj-ir")
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        gw.upstream.received_anthropic_versions(),
        vec![Some("2023-06-01".to_string())],
        "IR 路径应钉适配器默认版本"
    );
    assert_eq!(
        gw.upstream.received_anthropic_betas(),
        vec![Some("prompt-caching-2024-07-31".to_string())],
        "IR 路径仍应转发功能头"
    );
    assert_eq!(
        gw.upstream.received_openai_organizations(),
        vec![Some("org-ir".to_string())],
        "IR 路径应转发 openai-organization"
    );
    assert_eq!(
        gw.upstream.received_openai_projects(),
        vec![Some("proj-ir".to_string())],
        "IR 路径应转发 openai-project"
    );
}

/// 同协议 OpenAI 直通：白名单功能头原样转发。
#[tokio::test]
async fn openai_passthrough_forwards_org_and_project_headers() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-h", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })));

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .header("openai-organization", "org-pt")
        .header("openai-project", "proj-pt")
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        gw.upstream.received_openai_organizations(),
        vec![Some("org-pt".to_string())],
        "直通应转发 openai-organization"
    );
    assert_eq!(
        gw.upstream.received_openai_projects(),
        vec![Some("proj-pt".to_string())],
        "直通应转发 openai-project"
    );
}
