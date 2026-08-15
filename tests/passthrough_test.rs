//! 直通快路径（#07）端到端黑盒测试。
//!
//! 主接缝：端到端 HTTP 黑盒，断言外部可观察行为——mock 上游收到的出站请求体、
//! 下游收到的响应字节流、SQLite 中的计费与日志。覆盖：同协议直通转发（请求体
//! 仅目标性补丁、响应字节级一致、逐帧嗅探 usage 计费）、跨协议/别名回落 IR 路径、
//! 快路径不免认证与计费、failover 只发生在首字节之前。

mod common;

use std::time::Duration;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use futures_util::StreamExt;
use kairos::config;
use kairos::store::resources::Channel;
use serde_json::{Value, json};

/// 解析下游 SSE 响应体，返回所有 `data:` 帧的原始 JSON 值列表。
async fn collect_sse_frames(resp: reqwest::Response) -> Vec<Value> {
    let mut frames = Vec::new();
    let mut buffer = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("响应流应可读");
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buffer.find("\n\n") {
            let frame: String = buffer.drain(..end + 2).collect();
            for line in frame.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(data) {
                        frames.push(value);
                    }
                }
            }
        }
    }
    frames
}

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
    // stream_options（非流式响应自带顶层 usage，spec 仅授权流式注入）。
    assert!(
        received[0].get("stream").is_none(),
        "非流式直通不应改写 stream 字段"
    );
    assert!(
        received[0].get("stream_options").is_none(),
        "非流式直通不应注入 stream_options（spec 仅授权流式注入）"
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
    assert_eq!(frames[0]["object"], "chat.completion.chunk");
    assert_eq!(frames[0]["choices"][0]["delta"]["content"], "Hel");
    assert_eq!(frames[1]["choices"][0]["delta"]["content"], "lo");
    let usage_frame = frames.last().expect("应有 usage 帧");
    assert_eq!(usage_frame["usage"]["completion_tokens"], 100);

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
    let row: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT balance_usd_micros, settled_usd_micros, input_tokens, output_tokens, cost_usd_micros \
         FROM token_balance JOIN request_log ON token_balance.token_key = request_log.token_key \
         WHERE token_balance.token_key = ?",
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

    let settled: i64 =
        sqlx::query_scalar("SELECT settled_usd_micros FROM token_balance WHERE token_key = ?")
            .bind(TEST_TOKEN_KEY)
            .fetch_one(&gw.pool)
            .await
            .expect("读到哨兵时结算应已落库");
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

/// 混合协议候选渠道：首渠道同协议、failover 候选为异协议时，整体回落 IR 完整
/// 路径（直通不能向异协议渠道发原生字节），出站请求体不注入直通补丁。
#[tokio::test]
async fn mixed_protocol_route_falls_back_to_ir_path() {
    let (_gw, mut upstreams) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        // 首渠道 openai_chat（同入站协议），failover 候选为 openai_responses（异协议）。
        seed.channels = vec![
            Channel {
                name: "same-protocol".to_string(),
                protocol: config::Protocol::OpenAiChat,
                base_url: bases[0].clone(),
                api_key: "k".to_string(),
                models: vec![TEST_MODEL.to_string()],
                model_aliases: Default::default(),
                priority: 1,
                weight: 1,
                timeout_ms: 1000,
                max_retries: 0,
                enabled: true,
            },
            Channel {
                name: "cross-protocol".to_string(),
                protocol: config::Protocol::OpenAiResponses,
                base_url: bases[1].clone(),
                api_key: "k".to_string(),
                models: vec![TEST_MODEL.to_string()],
                model_aliases: Default::default(),
                priority: 2,
                weight: 1,
                timeout_ms: 1000,
                max_retries: 0,
                enabled: true,
            },
        ];
        seed
    })
    .await;

    upstreams[0].set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-m", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", _gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 首渠道收到出站请求，且为 IR 完整路径（不注入直通补丁）。
    let received = upstreams[0].received();
    assert_eq!(received.len(), 1);
    assert!(
        received[0].get("stream_options").is_none(),
        "混合协议路由应回落 IR 路径，不应注入直通补丁"
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
                api_key: "k".to_string(),
                models: vec![TEST_MODEL.to_string()],
                model_aliases: Default::default(),
                priority: 1,
                weight: 1,
                timeout_ms: 1000,
                max_retries: 0,
                enabled: true,
            },
            Channel {
                name: "backup".to_string(),
                protocol: config::Protocol::OpenAiChat,
                base_url: bases[1].clone(),
                api_key: "k".to_string(),
                models: vec![TEST_MODEL.to_string()],
                model_aliases: Default::default(),
                priority: 2,
                weight: 1,
                timeout_ms: 1000,
                max_retries: 0,
                enabled: true,
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
