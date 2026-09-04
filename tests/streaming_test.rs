//! 流式 IR 路径（#05）端到端黑盒测试：mock 上游以 SSE 流响应，断言下游逐帧
//! 收到入站协议 SSE 事件、流式 usage 计费正确。
//!
//! 主接缝：端到端 HTTP 黑盒，断言外部可观察行为（下游收到的 SSE 帧、SQLite
//! 中的计费与日志）。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior, collect_sse_frames};
use futures_util::StreamExt;
use serde_json::json;

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

/// 流式文本：下游逐帧收到 `chat.completion.chunk`，文本累积完整。
#[tokio::test]
async fn streaming_text_delivers_chunks() {
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
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }]
        }))
        .unwrap(),
        // 真实 OpenAI `include_usage`：usage 在独立末帧，choices 为空。
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        }))
        .unwrap(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let frames = collect_sse_frames(resp).await;

    // 首帧携带 role，后续帧只带 content 增量。
    assert_eq!(frames[0].data["object"], "chat.completion.chunk");
    assert_eq!(frames[0].data["choices"][0]["delta"]["role"], "assistant");
    assert_eq!(frames[0].data["choices"][0]["delta"]["content"], "Hel");
    assert_eq!(frames[1].data["choices"][0]["delta"]["content"], "lo");
    // 有 finish 帧（带 finish_reason）与独立 usage 帧。
    let finish = frames
        .iter()
        .find(|f| f.data["choices"][0]["finish_reason"] == "stop")
        .expect("应有 finish 帧");
    assert_eq!(finish.data["choices"][0]["finish_reason"], "stop");
    let usage_frame = frames.last().expect("应有 usage 帧");
    assert_eq!(usage_frame.data["usage"]["completion_tokens"], 2);
}

/// 流式 tool-call：tool_input 跨帧累积，末帧带完整参数。
#[tokio::test]
async fn streaming_tool_call_accumulates() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "tool_calls": [{
                "index": 0, "id": "call_1", "type": "function",
                "function": { "name": "get_weather", "arguments": "" }
            }] } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "tool_calls": [{
                "index": 0, "function": { "arguments": r#"{"city":"SF"}"# }
            }] } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6 }
        }))
        .unwrap(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let frames = collect_sse_frames(resp).await;

    // 工具调用首帧带 id 与 name。
    assert_eq!(
        frames[0].data["choices"][0]["delta"]["tool_calls"][0]["id"],
        "call_1"
    );
    assert_eq!(
        frames[0].data["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    // 增量帧只带 arguments。
    assert_eq!(
        frames[1].data["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
        r#"{"city":"SF"}"#
    );
    let last = frames.last().expect("应有 finish 帧");
    assert_eq!(last.data["choices"][0]["finish_reason"], "tool_calls");
}

/// 流式 usage 计费：按实际 usage 四分量精确扣减并落日志。
#[tokio::test]
async fn streaming_usage_is_billed() {
    let mut gw = TestGateway::start().await;
    // usage：input 1000 / output 100 / cache_read 200 / cache_write 50。
    // wire 折算：input = prompt - cached - cache_write = 1250-200-50 = 1000。
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "ok" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }]
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
    // 消费完流，确保结算已落库。
    collect_sse_frames(resp).await;
    common::wait_for_request_persistence(&gw.pool).await;

    // 期望费用 = 1000*2.5 + 100*10 + 200*1.25 + 50*10 = 2500+1000+250+500 = 4250。
    let row: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT ub.balance_usd_micros, tb.settled_usd_micros, input_tokens, cost_usd_micros \
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
    assert_eq!(row.3, 4250, "日志应记录费用 4250");
}

/// 流式上游返回非 2xx：状态码原样透传，缺失 usage 的已发出尝试不产生费用。
#[tokio::test]
async fn streaming_upstream_error_releases_reservation_without_charge() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Status429);

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    common::wait_for_request_persistence(&gw.pool).await;

    let row: (i64, i64) = sqlx::query_as(
        "SELECT ub.balance_usd_micros, tb.settled_usd_micros \
         FROM tokens t \
         JOIN user_balance ub ON ub.user_id = t.user_id \
         JOIN token_balance tb ON tb.token_key = t.token_key \
         WHERE t.token_key = ?",
    )
    .bind(TEST_TOKEN_KEY)
    .fetch_one(&gw.pool)
    .await
    .expect("令牌余额应存在");
    assert_eq!(row.0, 5_000_000, "缺失 usage 的流式失败不应扣费");
    assert_eq!(row.1, 0, "缺失 usage 的流式失败不增加累计结算");
}

/// 流中途上游报错（Anthropic 200 后 `event: error`）：错误以入站协议错误帧
/// 下发、不再合成 Finish，结算按已累积 usage 落账（此处为零）。
///
/// 走跨协议路由（chat 入站 → Anthropic 渠道）以经过 IR 流式面——同协议
/// 直通路径原样转发字节。
#[tokio::test]
async fn midstream_error_delivers_error_frame_and_settles() {
    fn anthropic_channel_seed(base: &str) -> common::Seed {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = kairos::config::Protocol::AnthropicMessages;
        seed
    }
    let mut gw = TestGateway::start_with(anthropic_channel_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "type": "message_start",
            "message": { "type": "message", "role": "assistant", "id": "msg_1", "model": "claude-sonnet", "content": [] }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_start", "index": 0, "content_block": { "type": "text" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "你好" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" }
        }))
        .unwrap(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let frames = collect_sse_frames(resp).await;

    // 错误前的内容增量正常下发。
    assert!(
        frames
            .iter()
            .any(|f| f.data["choices"][0]["delta"]["content"] == json!("你好")),
        "错误前的内容帧应照常透传"
    );
    // 末帧为入站协议错误帧（chat data 帧），全程无 finish 帧（不合成完整成功）。
    let last = frames.last().expect("应有错误帧");
    assert_eq!(last.data["error"]["message"], json!("Overloaded"));
    assert!(
        frames
            .iter()
            .all(|f| f.data["choices"][0]["finish_reason"].is_null())
    );

    // 结算按已累积 usage 落账（错误前无 usage 上报 → 零费用），日志落一行。
    common::wait_for_request_persistence(&gw.pool).await;
    let row: (i64, i64) = sqlx::query_as(
        "SELECT ub.balance_usd_micros, rl.cost_usd_micros \
         FROM tokens t \
         JOIN user_balance ub ON ub.user_id = t.user_id \
         JOIN request_log rl ON rl.token_key = t.token_key \
         WHERE t.token_key = ?",
    )
    .bind(TEST_TOKEN_KEY)
    .fetch_one(&gw.pool)
    .await
    .expect("应有结算日志");
    assert_eq!(row.1, 0, "缺失 usage 时释放预留而非保守结算");
    assert_eq!(row.0, 5_000_000, "钱包不应被扣减");
}

/// anthropic 双渠道 seed（同一模型，供流式 failover 用例使用）。
fn two_anthropic_channel_seed(bases: &[String]) -> common::Seed {
    let mut seed = common::test_seed(&bases[0]);
    seed.channels = bases
        .iter()
        .enumerate()
        .map(|(index, base)| {
            let mut channel = seed.channels[0].clone();
            channel.name = format!("ch-{index}");
            channel.protocol = kairos::config::Protocol::AnthropicMessages;
            channel.base_url = base.clone();
            channel.keys = vec![kairos::store::resources::ChannelKey {
                name: "default".to_string(),
                api_key: format!("sk-{index}"),
                weight: 1,
                enabled: true,
                models: None,
                blocked_models: None,
            }];
            channel
        })
        .collect();
    seed
}

fn anthropic_message_start() -> String {
    serde_json::to_string(&json!({
        "type": "message_start",
        "message": { "type": "message", "role": "assistant", "id": "msg_1", "model": "claude-sonnet", "content": [] }
    }))
    .unwrap()
}

/// 把单个 SSE JSON 帧编码为原始字节块，供 `GappedRawSse` 使用。
fn raw_frame(value: serde_json::Value) -> Vec<u8> {
    format!("data: {value}\n\n").into_bytes()
}

fn anthropic_content_start() -> serde_json::Value {
    json!({
        "type": "content_block_start", "index": 0, "content_block": { "type": "text" }
    })
}

fn anthropic_text_delta(text: &str) -> serde_json::Value {
    json!({
        "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": text }
    })
}

fn anthropic_message_delta(usage: serde_json::Value) -> serde_json::Value {
    json!({
        "type": "message_delta",
        "delta": { "stop_reason": "end_turn", "stop_sequence": null },
        "usage": usage
    })
}

/// 流建立后上游沉默超过渠道空闲超时：空闲超时仍会终止流（流不再受总时限
/// 约束的语义不放松空闲约束），缺失 usage 的尝试释放预留。
#[tokio::test]
async fn idle_timeout_terminates_stream_after_established() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = kairos::config::Protocol::AnthropicMessages;
        seed.channels[0].timeout_ms = 50;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::GappedRawSse {
        prefix: vec![
            raw_frame(serde_json::from_str(&anthropic_message_start()).unwrap()),
            raw_frame(anthropic_content_start()),
            raw_frame(anthropic_text_delta("你好")),
        ],
        gap_ms: 200,
        tail: vec![raw_frame(anthropic_message_delta(
            json!({ "input_tokens": 25, "output_tokens": 12 }),
        ))],
    });

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let frames = collect_sse_frames(resp).await;
    assert!(
        frames
            .iter()
            .any(|f| f.data["choices"][0]["delta"]["content"] == json!("你好")),
        "流首内容帧应照常下发"
    );
    let last = frames.last().expect("应有终止帧");
    assert_eq!(
        last.data["error"]["message"],
        json!("上游流未正常收尾，已中断"),
        "空闲超时中断应以错误帧收场"
    );

    common::wait_for_request_persistence(&gw.pool).await;
    let row: (i64, i64) = sqlx::query_as(
        "SELECT cost_usd_micros, usage_reported FROM request_log WHERE token_key = ?",
    )
    .bind(TEST_TOKEN_KEY)
    .fetch_one(&gw.pool)
    .await
    .expect("应有结算日志");
    assert_eq!(row.0, 0, "缺失 usage 的空闲中断应释放预留");
    assert_eq!(row.1, 0, "空闲中断时上游未回报 usage");
}

/// 长流回归：流建立后累计时长超过请求总时限（120s）仍继续可读并正常收尾。
///
/// 下游读到首块后暂停虚拟时钟——此刻请求路径的数据库操作已全部完成；此后
/// 时钟自动推进驱动上游两段各 100s 的沉默（均小于渠道空闲超时 120s），
/// 累计越过总时限。结算依赖 SQLite 真实时钟，暂停期间无法落库，故本用例
/// 断言转发与收尾语义；结算语义由空闲中断用例与常规流式用例覆盖。
#[tokio::test]
async fn stream_continues_past_request_total_deadline() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = kairos::config::Protocol::AnthropicMessages;
        seed.channels[0].timeout_ms = 120_000;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::GappedRawSse {
        prefix: vec![
            raw_frame(serde_json::from_str(&anthropic_message_start()).unwrap()),
            raw_frame(anthropic_content_start()),
            raw_frame(anthropic_text_delta("你")),
        ],
        gap_ms: 100_000,
        tail: vec![
            raw_frame(anthropic_text_delta("好")),
            raw_frame(anthropic_message_delta(json!({
                "input_tokens": 25, "output_tokens": 12
            }))),
            raw_frame(json!({ "type": "message_stop" })),
        ],
    });

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    // 读到首块内容后暂停时钟，请求路径已结束，流内不再有数据库操作。
    let mut downstream = resp.bytes_stream();
    let mut seen = Vec::new();
    while !seen.windows(3).any(|window| window == "你".as_bytes()) {
        let chunk = downstream
            .next()
            .await
            .expect("正文前响应流不应结束")
            .expect("响应块应可读");
        seen.extend_from_slice(&chunk);
    }
    tokio::time::pause();

    // 总时限过后的块照常读取与下发，流正常收尾而非被截断。
    let mut tail = seen;
    while let Some(chunk) = downstream.next().await {
        tail.extend_from_slice(&chunk.expect("响应块应可读"));
    }
    let text = String::from_utf8_lossy(&tail);
    assert!(
        text.contains("\"finish_reason\":\"stop\""),
        "流应在总时限过后正常收尾: {text}"
    );
    assert!(!text.contains("\"error\""), "长流不应被总时限截断出错误帧");
}

/// 断连即取消（渠道开关缺省开）：下游断开后立即停止上游消费，usage 载荷
/// 在更后的块里、取消时未被嗅探，预留全额释放（计费宽容原语）。
#[tokio::test]
async fn downstream_disconnect_aborts_upstream_and_releases_reservation() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = kairos::config::Protocol::AnthropicMessages;
        seed
    })
    .await;
    // 前缀内容让下游读到正文；随后一块大体积无 usage 填充帧承担断连检测，
    // 最后 usage 帧在取消发生后才到达，不应被消费或计费。
    let padding = " ".repeat(256 * 1024);
    gw.upstream.set_behavior(UpstreamBehavior::GappedRawSse {
        prefix: vec![
            raw_frame(serde_json::from_str(&anthropic_message_start()).unwrap()),
            raw_frame(anthropic_content_start()),
            raw_frame(anthropic_text_delta("你好")),
        ],
        gap_ms: 100,
        tail: vec![
            raw_frame(anthropic_text_delta(&padding)),
            raw_frame(anthropic_message_delta(json!({
                "input_tokens": 25, "output_tokens": 12
            }))),
            raw_frame(json!({ "type": "message_stop" })),
        ],
    });

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let mut downstream = resp.bytes_stream();
    let mut seen = Vec::new();
    while !seen.windows(6).any(|window| window == "你好".as_bytes()) {
        let chunk = downstream
            .next()
            .await
            .expect("正文前响应流不应结束")
            .expect("响应块应可读");
        seen.extend_from_slice(&chunk);
    }
    drop(downstream);

    // 结算行随断连取消入队：直接轮询行出现（outbox 计数在入队前本就为 0）。
    let mut settled: Option<(i64, i64)> = None;
    for _ in 0..250 {
        settled = sqlx::query_as(
            "SELECT cost_usd_micros, usage_reported FROM request_log WHERE token_key = ?",
        )
        .bind(TEST_TOKEN_KEY)
        .fetch_optional(&gw.pool)
        .await
        .expect("应能查询日志");
        if settled.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let row = settled.expect("断连取消后应落结算日志");
    assert_eq!(row.0, 0, "断连取消后未嗅探到 usage，应释放预留");
    assert_eq!(row.1, 0, "断连取消时 usage 帧不应被消费");
}

/// 断连止损开关关闭：维持原语义，继续消费上游至收尾，按实际 usage 结算。
#[tokio::test]
async fn downstream_disconnect_without_abort_keeps_consuming() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = kairos::config::Protocol::AnthropicMessages;
        seed.channels[0].abort_on_disconnect = false;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::GappedRawSse {
        prefix: vec![
            raw_frame(serde_json::from_str(&anthropic_message_start()).unwrap()),
            raw_frame(anthropic_content_start()),
            raw_frame(anthropic_text_delta("你好")),
        ],
        gap_ms: 100,
        tail: vec![raw_frame(anthropic_message_delta(json!({
            "input_tokens": 25, "output_tokens": 12
        })))],
    });

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let mut downstream = resp.bytes_stream();
    let mut seen = Vec::new();
    while !seen.windows(6).any(|window| window == "你好".as_bytes()) {
        let chunk = downstream
            .next()
            .await
            .expect("正文前响应流不应结束")
            .expect("响应块应可读");
        seen.extend_from_slice(&chunk);
    }
    drop(downstream);

    // 上游尾部 usage 被继续消费：按实际 usage 结算，费用落在日志与余额。
    let mut cost = 0;
    for _ in 0..200 {
        let rows: Vec<i64> =
            sqlx::query_scalar("SELECT cost_usd_micros FROM request_log WHERE token_key = ?")
                .bind(TEST_TOKEN_KEY)
                .fetch_all(&gw.pool)
                .await
                .expect("应能查询日志");
        if let Some(value) = rows.into_iter().find(|cost| *cost > 0) {
            cost = value;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(cost > 0, "开关关闭时断连后仍应消费尾部 usage 并结算");
}

fn anthropic_text_stream(text: &str) -> Vec<String> {
    vec![
        anthropic_message_start(),
        serde_json::to_string(&json!({
            "type": "content_block_start", "index": 0, "content_block": { "type": "text" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": text }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "input_tokens": 25, "output_tokens": 12 }
        }))
        .unwrap(),
        serde_json::to_string(&json!({ "type": "message_stop" })).unwrap(),
    ]
}

/// 首块前流内错误（200 后立即 `event: error`）触发 failover：下游收到的是
/// 下一渠道的完整流，无任何残留帧，结算只按次渠道一次落账。
#[tokio::test]
async fn pre_first_chunk_error_fails_over_to_next_channel() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, two_anthropic_channel_seed).await;
    ups[0].set_behavior(UpstreamBehavior::Sse(vec![
        anthropic_message_start(),
        serde_json::to_string(&json!({
            "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" }
        }))
        .unwrap(),
    ]));
    ups[1].set_behavior(UpstreamBehavior::Sse(anthropic_text_stream("ok")));

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "failover 后应成功");
    let frames = collect_sse_frames(resp).await;
    assert!(
        frames
            .iter()
            .any(|f| f.data["choices"][0]["delta"]["content"] == json!("ok")),
        "下游应收次渠道的内容帧"
    );
    assert!(
        frames.iter().all(|f| !serde_json::to_string(&f.data)
            .unwrap()
            .contains("Overloaded")),
        "首渠道的错误帧不得泄漏给下游"
    );
    assert!(
        frames
            .iter()
            .any(|f| f.data["choices"][0]["finish_reason"] == json!("stop")),
        "下游应收次渠道的正常收尾"
    );

    // 两个渠道都被请求过；每个出站尝试都保留独立结算记录。
    assert_eq!(ups[0].received().len(), 1);
    assert_eq!(ups[1].received().len(), 1);
    common::wait_for_request_persistence(&gw.pool).await;
    let rows: Vec<(String, i64, i64)> =
        sqlx::query_as("SELECT channel, status_code, cost_usd_micros FROM request_log ORDER BY id")
            .fetch_all(&gw.pool)
            .await
            .expect("应有结算日志");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "ch-0");
    assert_ne!(rows[0].1, 200);
    assert!(rows[0].2 == 0, "无 usage 的首渠道不产生费用");
    assert_eq!(rows[1].0, "ch-1");
    assert_eq!(rows[1].1, 200);
    assert!(rows[1].2 > 0, "次渠道按 usage 计费");
}

/// 空流（200 后零帧即断）按可重试归类：failover 到下一渠道成功。
#[tokio::test]
async fn empty_stream_fails_over_to_next_channel() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, two_anthropic_channel_seed).await;
    ups[0].set_behavior(UpstreamBehavior::Sse(vec![]));
    ups[1].set_behavior(UpstreamBehavior::Sse(anthropic_text_stream("ok")));

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "空流应 failover 而非当成成功"
    );
    let frames = collect_sse_frames(resp).await;
    assert!(
        frames
            .iter()
            .any(|f| f.data["choices"][0]["delta"]["content"] == json!("ok"))
    );
    assert_eq!(ups[0].received().len(), 1);
    assert_eq!(ups[1].received().len(), 1);
}

/// 上游流缺收尾事件（内容后即断）归类为异常：下游收到错误帧而非合成
/// 成功 Finish；缺失 usage 的尝试释放预留、不产生费用。
#[tokio::test]
async fn unterminated_stream_delivers_error_frame_and_settles() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = kairos::config::Protocol::AnthropicMessages;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        anthropic_message_start(),
        serde_json::to_string(&json!({
            "type": "content_block_start", "index": 0, "content_block": { "type": "text" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "你好" }
        }))
        .unwrap(),
    ]));

    let resp = send_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let frames = collect_sse_frames(resp).await;
    assert!(
        frames
            .iter()
            .any(|f| f.data["choices"][0]["delta"]["content"] == json!("你好"))
    );
    let last = frames.last().expect("应有错误帧");
    assert_eq!(
        last.data["error"]["message"],
        json!("上游流未正常收尾，已中断"),
        "缺收尾应以错误帧收场而非合成成功 Finish"
    );
    assert!(
        frames
            .iter()
            .all(|f| f.data["choices"][0]["finish_reason"].is_null())
    );

    common::wait_for_request_persistence(&gw.pool).await;
    let row: (i64,) = sqlx::query_as(
        "SELECT rl.cost_usd_micros FROM tokens t \
         JOIN request_log rl ON rl.token_key = t.token_key WHERE t.token_key = ?",
    )
    .bind(TEST_TOKEN_KEY)
    .fetch_one(&gw.pool)
    .await
    .expect("应有结算日志");
    assert_eq!(row.0, 0, "缺失 usage 时释放预留而非保守结算");
}
