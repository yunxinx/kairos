//! 阶段二黑盒验收：缓存断点全链路保真、流内错误按入站协议成形、渠道密钥的
//! 选择边界、混合协议候选的语义与结算。
//!
//! 主接缝为端到端 HTTP 黑盒：断言 mock 上游收到的出站请求、下游收到的响应与
//! SSE 帧、SQLite 中的结算归因。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior, collect_sse_frames};
use kairos::config;
use kairos::store::resources::{Channel, ChannelKey};
use serde_json::{Value, json};

/// 单密钥渠道：其余字段沿用测试默认（模型名单、超时、开关）。
fn single_key_channel(
    name: &str,
    protocol: config::Protocol,
    base_url: &str,
    api_key: &str,
) -> Channel {
    Channel {
        name: name.to_string(),
        protocol,
        base_url: base_url.to_string(),
        keys: vec![channel_key("default", api_key, true)],
        models: vec![TEST_MODEL.to_string()],
        model_aliases: Default::default(),
        timeout_ms: 1000,
        max_retries: 0,
        enabled: true,
        model_group: kairos::store::resources::DEFAULT_MODEL_GROUP.to_string(),
        reasoning_output: Default::default(),
        session_cache_key: Default::default(),
        injects_cache_breakpoints: false,
        abort_on_disconnect: true,
    }
}

fn channel_key(name: &str, api_key: &str, enabled: bool) -> ChannelKey {
    ChannelKey {
        name: name.to_string(),
        api_key: api_key.to_string(),
        weight: 1,
        enabled,
        models: None,
        blocked_models: None,
    }
}

/// Anthropic 上游的非流式成功响应。
fn anthropic_ok(model: &str) -> Value {
    json!({
        "id": "msg_up", "type": "message", "role": "assistant", "model": model,
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 2 }
    })
}

/// OpenAI Chat 上游的非流式成功响应。
fn chat_ok() -> Value {
    json!({
        "id": "chatcmpl-up", "object": "chat.completion", "model": TEST_MODEL,
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "ok" },
                     "logprobs": null, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    })
}

/// 发起 Anthropic Messages 入站请求。
async fn post_messages(base: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .header("x-api-key", TEST_TOKEN_KEY)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await
        .expect("应能请求网关")
}

/// 发起 Responses 入站流式请求。
async fn post_responses_stream(base: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/v1/responses"))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "stream": true,
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_text", "text": "hi" }] }]
        }))
        .send()
        .await
        .expect("应能请求网关")
}

/// 结算日志中成功请求的渠道序列，按落账先后。
///
/// 只取 2xx：失败尝试当前不落日志，按状态码过滤可免受该可观测性现状变化的影响。
async fn logged_channels(pool: &sqlx::SqlitePool) -> Vec<String> {
    common::wait_for_request_persistence(pool).await;
    sqlx::query_as("SELECT channel FROM request_log WHERE status_code = 200 ORDER BY id")
        .fetch_all(pool)
        .await
        .expect("应能查询结算日志")
        .into_iter()
        .map(|(channel,): (String,)| channel)
        .collect()
}

/// 显式缓存断点经别名（强制完整转换路径）后仍锚在上游请求体的同一位置。
///
/// 别名让出站模型名改写，直通路径无法承载，请求必过中间表示重编码；断点挂在
/// system 尾块、工具定义与消息内容块三处，重编码后须逐一还原。
#[tokio::test]
async fn explicit_cache_breakpoints_survive_alias_forced_conversion() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = config::Protocol::AnthropicMessages;
        seed
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(anthropic_ok("gpt-4o-mini")));

    let resp = post_messages(
        &gw.base_url(),
        json!({
            "model": "fast",
            "max_tokens": 1024,
            "system": [
                { "type": "text", "text": "你是助手。" },
                { "type": "text", "text": "长系统提示。",
                  "cache_control": { "type": "ephemeral", "ttl": "1h" } }
            ],
            "tools": [{
                "name": "get_weather",
                "input_schema": { "type": "object", "properties": {} },
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "上海天气？",
                      "cache_control": { "type": "ephemeral" } }
                ]
            }]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    assert_eq!(received.len(), 1);
    // 别名改写出站模型名，即该请求经中间表示重编码而非字节直通。
    assert_eq!(received[0]["model"], json!("gpt-4o-mini"));
    // 带断点的 system 以块数组出站，断点连 ttl 一起挂尾块（system 归并成一块
    // 是中间表示的既有语义，这里只锁定断点落点）。
    let system_blocks = received[0]["system"]
        .as_array()
        .expect("带断点时 system 应为块数组");
    let system_tail = system_blocks.last().expect("system 块数组不应为空");
    assert_eq!(
        system_tail["cache_control"],
        json!({ "type": "ephemeral", "ttl": "1h" })
    );
    assert_eq!(system_tail["text"], json!("你是助手。长系统提示。"));
    assert_eq!(
        received[0]["tools"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert_eq!(
        received[0]["messages"][0]["content"][0],
        json!({ "type": "text", "text": "上海天气？",
                "cache_control": { "type": "ephemeral" } })
    );

    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["model"], json!("fast"));
    // 断点未超预算，无信息损失告警。用 get 逐层取值：直接下标会在键缺失时
    // 拿到 Null，把「压根没有告警字段」误判成「没有告警」。
    let warnings = body
        .get("gateway")
        .and_then(|gateway| gateway.get("warnings"));
    assert!(warnings.is_none(), "预算内不应告警，实际 {warnings:?}");
}

/// 断点超出单请求预算时按渲染顺序保后弃前，并在响应面暴露告警。
#[tokio::test]
async fn cache_breakpoints_beyond_budget_are_clamped_and_reported() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = config::Protocol::AnthropicMessages;
        seed
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(anthropic_ok("gpt-4o-mini")));

    let ephemeral = json!({ "type": "ephemeral" });
    let resp = post_messages(
        &gw.base_url(),
        json!({
            "model": "fast",
            "max_tokens": 1024,
            "system": [
                { "type": "text", "text": "长系统提示。", "cache_control": ephemeral }
            ],
            "tools": [
                { "name": "get_weather",
                  "input_schema": { "type": "object", "properties": {} },
                  "cache_control": ephemeral },
                { "name": "get_time",
                  "input_schema": { "type": "object", "properties": {} },
                  "cache_control": ephemeral }
            ],
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "上海天气？", "cache_control": ephemeral }
                ] },
                { "role": "assistant", "content": [
                    { "type": "text", "text": "好的。" }
                ] },
                { "role": "user", "content": [
                    { "type": "text", "text": "明天呢？", "cache_control": ephemeral }
                ] }
            ]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let received = gw.upstream.received();
    // 渲染顺序为工具 → system → 消息，五处断点超限一处，最早的工具断点被剥。
    assert!(
        received[0]["tools"][0].get("cache_control").is_none(),
        "超预算应牺牲最早的工具断点"
    );
    // 钳制只摘掉断点，工具定义本身须完好。
    assert_eq!(received[0]["tools"][0]["name"], json!("get_weather"));
    assert_eq!(received[0]["tools"][1]["cache_control"], ephemeral);
    assert_eq!(received[0]["system"][0]["cache_control"], ephemeral);
    assert_eq!(
        received[0]["messages"][0]["content"][0]["cache_control"],
        ephemeral
    );
    assert_eq!(
        received[0]["messages"][2]["content"][0]["cache_control"],
        ephemeral
    );

    let body: Value = resp.json().await.expect("响应应可解析");
    let warnings = body["gateway"]["warnings"].as_array().expect("钳制应告警");
    assert_eq!(warnings.len(), 1, "一次钳制只记一条告警");
    assert_eq!(warnings[0]["feature"], json!("cache_breakpoint"));
    assert_eq!(warnings[0]["type"], json!("unsupported"));
    assert!(
        warnings[0]["details"]
            .as_str()
            .is_some_and(|details| details.contains("丢弃最早的 1 个")),
        "告警应说明丢弃数量: {}",
        warnings[0]["details"]
    );
}

/// Anthropic 入站流式中途报错：错误以入站协议的 `error` 事件收场，不合成成功收尾。
///
/// 经别名命中完整转换路径——同名字节直通会把上游帧原样转发，不经过错误语义。
#[tokio::test]
async fn anthropic_inbound_midstream_error_uses_anthropic_error_event() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = config::Protocol::AnthropicMessages;
        seed
    })
    .await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "type": "message_start",
            "message": { "type": "message", "role": "assistant", "id": "msg_1",
                         "model": "gpt-4o-mini", "content": [] }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "text" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "你好" }
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" }
        }))
        .unwrap(),
    ]));

    let resp = post_messages(
        &gw.base_url(),
        json!({
            "model": "fast",
            "max_tokens": 1024,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let frames = collect_sse_frames(resp).await;

    // 别名改写出站模型名，即该流经过错误语义而非字节直通。
    assert_eq!(
        gw.upstream.received()[0]["model"],
        json!("gpt-4o-mini"),
        "直通路径原样转发帧，无法承载流内错误语义"
    );
    // 错误前的内容增量照常下发。
    assert!(
        frames
            .iter()
            .any(|frame| frame.data["delta"]["text"] == json!("你好")),
        "错误前的内容帧应下发"
    );
    // 末帧为入站协议错误事件，且没有 message_stop 合成的成功收尾。
    let last = frames.last().expect("应有错误帧");
    assert_eq!(last.event.as_deref(), Some("error"));
    assert_eq!(last.data["type"], json!("error"));
    assert_eq!(last.data["error"]["message"], json!("Overloaded"));
    assert!(
        frames
            .iter()
            .all(|frame| frame.data["type"] != json!("message_stop")),
        "中途错误不应合成成功收尾"
    );
    // 整条帧序列以快照锁定：错误前各帧的形状与顺序是抽查单帧覆盖不到的契约。
    insta::assert_json_snapshot!(frames);
}

/// Responses 入站的异常流以入站协议错误事件收场：帧名与载荷形状都随入站协议。
#[tokio::test]
async fn responses_inbound_unterminated_stream_uses_responses_error_event() {
    let mut gw = TestGateway::start().await; // 默认 openai_chat 渠道。
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-9", "object": "chat.completion.chunk", "model": TEST_MODEL,
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "Hel" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-9", "object": "chat.completion.chunk", "model": TEST_MODEL,
            "choices": [{ "index": 0, "delta": { "content": "lo" } }]
        }))
        .unwrap(),
    ]));

    let resp = post_responses_stream(&gw.base_url()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let frames = collect_sse_frames(resp).await;

    // 缺收尾前的内容增量照常下发，否则「异常流」与「空流直接报错」无从区分。
    assert!(
        frames
            .iter()
            .any(|frame| frame.data["delta"] == json!("Hel")),
        "错误前的内容增量应下发"
    );
    let last = frames.last().expect("应有错误帧");
    assert_eq!(last.event.as_deref(), Some("error"));
    assert_eq!(
        last.data["error"]["message"],
        json!("上游流未正常收尾，已中断"),
        "缺收尾应报错误而非合成完成事件"
    );
    assert!(
        frames
            .iter()
            .all(|frame| frame.data["type"] != json!("response.completed")),
        "异常流不应合成终端完成事件"
    );
    insta::assert_json_snapshot!(frames);
}

/// 渠道密钥按渠道各自选取：失效渠道与接手渠道各用自己的启用密钥，禁用密钥
/// 不参与出站认证。
#[tokio::test]
async fn channel_keys_are_selected_per_channel_and_skip_disabled() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        seed.channels = vec![
            {
                let mut channel =
                    single_key_channel("ch-0", config::Protocol::OpenAiChat, &bases[0], "sk-0-a");
                channel.keys.push(channel_key("disabled", "sk-0-b", false));
                channel
            },
            {
                let mut channel =
                    single_key_channel("ch-1", config::Protocol::OpenAiChat, &bases[1], "sk-1-a");
                channel.keys.push(channel_key("b", "sk-1-b", true));
                channel
            },
        ];
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Status429);
    ups[1].set_behavior(UpstreamBehavior::Json(chat_ok()));

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
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "failover 后应成功");

    let first_keys = ups[0].received_api_keys();
    let handed_off_keys = ups[1].received_api_keys();
    // 首渠道只剩一把启用密钥：不锁出站次数（那是重试语义），只锁取值域。
    assert!(!first_keys.is_empty(), "首渠道应有出站尝试");
    assert!(
        first_keys
            .iter()
            .all(|key| key.as_deref() == Some("Bearer sk-0-a")),
        "首渠道只用自己的启用密钥: {first_keys:?}"
    );
    let handed_off = handed_off_keys
        .first()
        .and_then(|key| key.as_deref())
        .expect("接手渠道应有出站尝试");
    assert!(
        handed_off == "Bearer sk-1-a" || handed_off == "Bearer sk-1-b",
        "接手渠道应在自己的启用密钥内选择，不沿用首渠道的结果: {handed_off}"
    );
    assert!(
        !first_keys
            .iter()
            .chain(handed_off_keys.iter())
            .any(|key| key.as_deref() == Some("Bearer sk-0-b")),
        "禁用密钥不得出站"
    );
    assert_eq!(
        logged_channels(&gw.pool).await,
        vec!["ch-1".to_string()],
        "结算只归接手渠道"
    );
}

/// 混合协议候选：无论命中同协议还是异协议渠道，下游都收到入站协议形状，出站
/// 请求按命中渠道的协议成形，结算归命中的渠道。
///
/// 用例只锁定跨阶段稳定的语义与归因，不断言内部走哪条转换路径。
#[tokio::test]
async fn mixed_protocol_candidates_keep_downstream_shape_and_attribution() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        seed.channels = vec![
            single_key_channel(
                "ch-anthropic",
                config::Protocol::AnthropicMessages,
                &bases[0],
                "sk-anthropic",
            ),
            single_key_channel(
                "ch-chat",
                config::Protocol::OpenAiChat,
                &bases[1],
                "sk-chat",
            ),
        ];
        seed
    })
    .await;
    // 首次请求命中同协议渠道；第二次该渠道 429，由异协议渠道接手。
    ups[0].set_behavior(UpstreamBehavior::Json(anthropic_ok(TEST_MODEL)));
    ups[0].push_behavior(UpstreamBehavior::Status429);
    ups[1].set_behavior(UpstreamBehavior::Json(chat_ok()));

    let client = reqwest::Client::new();
    let request = || {
        client
            .post(format!("{}/v1/messages", gw.base_url()))
            .header("x-api-key", TEST_TOKEN_KEY)
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model": TEST_MODEL,
                "max_tokens": 1024,
                "messages": [{ "role": "user", "content": "hi" }]
            }))
            .send()
    };

    let first = request().await.expect("首请求应到达网关");
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let first_body: Value = first.json().await.expect("响应应可解析");
    assert_eq!(first_body["type"], json!("message"));
    assert_eq!(first_body["content"][0]["text"], json!("ok"));
    assert_eq!(first_body["stop_reason"], json!("end_turn"));

    let second = request().await.expect("次请求应到达网关");
    assert_eq!(second.status(), reqwest::StatusCode::OK);
    let second_body: Value = second.json().await.expect("响应应可解析");
    assert_eq!(second_body["type"], json!("message"), "下游形状随入站协议");
    assert_eq!(second_body["content"][0]["text"], json!("ok"));
    assert_eq!(second_body["stop_reason"], json!("end_turn"));
    assert_eq!(second_body["usage"]["output_tokens"], 2, "usage 随响应回传");
    common::wait_for_request_persistence(&gw.pool).await;

    let to_anthropic = ups[0].received();
    assert_eq!(to_anthropic.len(), 2, "两次请求都先打同一渠道");
    assert_eq!(to_anthropic[0]["max_tokens"], 1024, "出站按目标协议成形");
    assert_eq!(to_anthropic[0]["messages"][0]["role"], "user");
    // 异协议渠道按自己的协议收到出站请求。
    let to_chat = ups[1].received();
    assert_eq!(to_chat.len(), 1);
    assert_eq!(to_chat[0]["model"], json!(TEST_MODEL));
    assert_eq!(to_chat[0]["messages"][0]["content"], json!("hi"));

    assert_eq!(
        logged_channels(&gw.pool).await,
        vec!["ch-anthropic".to_string(), "ch-chat".to_string()],
        "结算按实际命中的渠道归因"
    );
}
