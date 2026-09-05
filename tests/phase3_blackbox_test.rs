//! 阶段三黑盒验收：Gemini 第四协议的网关边界——入站 generateContent 的
//! model-in-path 路由与跨协议转换、直通快路径的路径段改写纪律、usage 的
//! 减法折算计费（`promptTokenCount` 含缓存）在直通与完整转换两条路径上同口径。
//!
//! 主接缝为端到端 HTTP 黑盒：断言 mock 上游收到的出站 URL 与请求体、下游收到
//! 的响应与 SSE 帧、SQLite 中的结算归因。

mod common;

use std::time::Duration;

use common::{
    TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior, collect_sse_body, parse_sse_frames,
};
use kairos::config;
use serde_json::{Value, json};

/// 发起非流式 generateContent 入站请求（模型名承载在路径上）。
async fn post_generate_content(base: &str, model: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/v1beta/models/{model}:generateContent"))
        .header("x-goog-api-key", TEST_TOKEN_KEY)
        .json(&body)
        .send()
        .await
        .expect("应能请求网关")
}

/// 发起流式 generateContent 入站请求。
async fn post_generate_content_stream(base: &str, model: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!(
            "{base}/v1beta/models/{model}:streamGenerateContent?alt=sse"
        ))
        .header("x-goog-api-key", TEST_TOKEN_KEY)
        .json(&body)
        .send()
        .await
        .expect("应能请求网关")
}

/// 发起非流式 Chat Completions 入站请求。
async fn post_chat(base: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&body)
        .send()
        .await
        .expect("应能请求网关")
}

/// 发起流式 Chat Completions 入站请求。
async fn post_chat_stream(base: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&body)
        .send()
        .await
        .expect("应能请求网关")
}

/// 指定渠道协议构造 seed（其余沿用测试默认，含 `fast` → `gpt-4o-mini` 别名）。
fn seed_with_protocol(protocol: config::Protocol) -> impl Fn(&str) -> common::Seed {
    move |base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].protocol = protocol;
        seed
    }
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

/// Anthropic 上游的非流式成功响应。
fn anthropic_ok() -> Value {
    json!({
        "id": "msg_up", "type": "message", "role": "assistant", "model": TEST_MODEL,
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 2 }
    })
}

/// Responses 上游的非流式成功响应。
fn responses_ok() -> Value {
    json!({
        "id": "resp_up", "object": "response", "status": "completed", "model": TEST_MODEL,
        "output": [
            { "id": "msg_1", "type": "message", "role": "assistant",
              "content": [ { "type": "output_text", "text": "ok", "annotations": [] } ] }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 2, "total_tokens": 12 }
    })
}

/// Gemini 上游的非流式成功响应。
fn gemini_ok() -> Value {
    json!({
        "candidates": [{
            "content": { "role": "model", "parts": [{ "text": "ok" }] },
            "finishReason": "STOP",
            "index": 0
        }],
        "usageMetadata": {
            "promptTokenCount": 10, "candidatesTokenCount": 2, "totalTokenCount": 12
        },
        "modelVersion": TEST_MODEL,
        "responseId": "resp-gem"
    })
}

/// 带缓存与思维链的 usage：prompt 含 cached 1250（缓存 200），输出由候选
/// token 与思维 token 相加得到 140。
fn billed_usage_metadata() -> Value {
    json!({
        "promptTokenCount": 1250,
        "candidatesTokenCount": 100,
        "cachedContentTokenCount": 200,
        "thoughtsTokenCount": 40
    })
}

/// 结算行：`(input, output, cache_read, cost_micros)`，按落账先后。
type BilledRow = (i64, i64, i64, i64);

async fn billed_rows(pool: &sqlx::SqlitePool) -> Vec<BilledRow> {
    sqlx::query_as(
        "SELECT input_tokens, output_tokens, cache_read_tokens, cost_usd_micros \
         FROM request_log ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .expect("应能查询结算日志")
}

/// 轮询直至落账行数达标（流式结算在流尾任务中完成，与下游读完无先后保证）。
async fn wait_for_billed_rows(pool: &sqlx::SqlitePool, count: usize) -> Vec<BilledRow> {
    for _ in 0..100 {
        let rows = billed_rows(pool).await;
        if rows.len() >= count {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    billed_rows(pool).await
}

/// 带系统提示与输出上限的入站 generateContent 请求体。
fn generate_content_request() -> Value {
    json!({
        "contents": [{ "role": "user", "parts": [{ "text": "上海天气？" }] }],
        "systemInstruction": { "parts": [{ "text": "以 JSON 输出" }] },
        "generationConfig": { "maxOutputTokens": 512 }
    })
}

/// 断言下游 SSE 字节流不含 `[DONE]` 哨兵（Gemini 入站流的收尾约定）。
fn assert_no_done_sentinel(body: &[u8], context: &str) {
    assert!(
        !body
            .windows(b"[DONE]".len())
            .any(|window| window == b"[DONE]"),
        "{context}"
    );
}

/// 入站 generateContent 到三种异协议上游的出站形状：systemInstruction 与
/// maxOutputTokens 按各协议归位，下游统一收到 Gemini 响应形状。
#[tokio::test]
async fn generate_content_inbound_converts_to_each_upstream_protocol() {
    let downstream_ok = |body: &Value| {
        assert_eq!(
            body["candidates"][0]["content"]["parts"][0]["text"],
            json!("ok")
        );
        assert_eq!(body["candidates"][0]["finishReason"], json!("STOP"));
        assert_eq!(body["usageMetadata"]["promptTokenCount"], json!(10));
        assert_eq!(body["usageMetadata"]["candidatesTokenCount"], json!(2));
        assert_eq!(body["modelVersion"], json!(TEST_MODEL));
    };

    // chat 渠道：systemInstruction 归 system 消息，maxOutputTokens 归 max_tokens。
    let mut gw = TestGateway::start_with(seed_with_protocol(config::Protocol::OpenAiChat)).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(chat_ok()));
    let resp = post_generate_content(&gw.base_url(), TEST_MODEL, generate_content_request()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    // 路径中的模型名是权威来源：出站体回写同名模型。
    assert_eq!(received[0]["model"], json!(TEST_MODEL));
    assert_eq!(received[0]["messages"][0]["role"], "system");
    assert_eq!(received[0]["messages"][0]["content"], "以 JSON 输出");
    assert_eq!(received[0]["messages"][1]["content"], "上海天气？");
    assert_eq!(received[0]["max_tokens"], json!(512));
    downstream_ok(&resp.json().await.expect("响应应可解析"));
    gw.db_dir.close().expect("临时目录应可清理");

    // anthropic 渠道：systemInstruction 归顶层 system。
    let mut gw =
        TestGateway::start_with(seed_with_protocol(config::Protocol::AnthropicMessages)).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(anthropic_ok()));
    let resp = post_generate_content(&gw.base_url(), TEST_MODEL, generate_content_request()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    assert_eq!(received[0]["system"], json!("以 JSON 输出"));
    assert_eq!(received[0]["max_tokens"], json!(512));
    assert_eq!(received[0]["messages"][0]["role"], "user");
    downstream_ok(&resp.json().await.expect("响应应可解析"));
    gw.db_dir.close().expect("临时目录应可清理");

    // responses 渠道：systemInstruction 归 instructions。
    let mut gw =
        TestGateway::start_with(seed_with_protocol(config::Protocol::OpenAiResponses)).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(responses_ok()));
    let resp = post_generate_content(&gw.base_url(), TEST_MODEL, generate_content_request()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let received = gw.upstream.received();
    assert_eq!(received[0]["instructions"], json!("以 JSON 输出"));
    assert_eq!(received[0]["max_output_tokens"], json!(512));
    downstream_ok(&resp.json().await.expect("响应应可解析"));
    gw.db_dir.close().expect("临时目录应可清理");
}

/// 入站流式 generateContent：上游帧翻译为 Gemini 形状逐 chunk 下发，末 chunk
/// 携带 `finishReason` 与 `usageMetadata`，无 `[DONE]` 哨兵。
///
/// 出站按目标协议强制流式并注入 `include_usage`（模型名回写请求体是其余协议
/// 的出站形状，与 Gemini 的 model-in-path 互斥）。
#[tokio::test]
async fn stream_generate_content_inbound_emits_gemini_frames_without_sentinel() {
    let mut gw = TestGateway::start().await; // 默认 openai_chat 渠道。
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": TEST_MODEL,
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "Hel" } }]
        })
        .to_string(),
        json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": TEST_MODEL,
            "choices": [{ "index": 0, "delta": { "content": "lo" } }]
        })
        .to_string(),
        json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": TEST_MODEL,
            "choices": [],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        })
        .to_string(),
    ]));

    let resp = post_generate_content_stream(
        &gw.base_url(),
        TEST_MODEL,
        json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 出站按目标协议成形：体上强制 stream 与 include_usage，模型名回写请求体。
    let received = gw.upstream.received();
    assert_eq!(received[0]["stream"], json!(true));
    assert_eq!(received[0]["stream_options"]["include_usage"], json!(true));
    assert_eq!(received[0]["model"], json!(TEST_MODEL));

    let body = collect_sse_body(resp).await;
    assert_no_done_sentinel(&body, "Gemini 入站流不以 [DONE] 哨兵收尾");
    let frames = parse_sse_frames(&body);
    assert_eq!(
        frames[0].data["candidates"][0]["content"]["parts"][0]["text"],
        json!("Hel")
    );
    assert_eq!(
        frames[1].data["candidates"][0]["content"]["parts"][0]["text"],
        json!("lo")
    );
    let last = frames.last().expect("应有收尾帧");
    assert_eq!(last.data["candidates"][0]["finishReason"], json!("STOP"));
    assert_eq!(last.data["usageMetadata"]["promptTokenCount"], json!(10));
    assert_eq!(last.data["usageMetadata"]["candidatesTokenCount"], json!(2));
    // 整条帧序列以快照锁定：chunk 的形状与顺序是单帧抽查覆盖不到的契约。
    insta::assert_json_snapshot!(frames);
}

/// chat 入站到 Gemini 渠道：模型名只在出站 URL 路径上，请求体不带
/// `model`/`stream`；下游收到 chat 形状响应。
#[tokio::test]
async fn chat_inbound_reaches_gemini_upstream_model_in_path() {
    let mut gw = TestGateway::start_with(seed_with_protocol(config::Protocol::Gemini)).await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(gemini_ok()));

    let resp = post_chat(
        &gw.base_url(),
        json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    assert_eq!(
        gw.upstream.received_paths()[0],
        "/v1beta/models/gpt-4o:generateContent"
    );
    assert_eq!(
        gw.upstream.received_api_keys()[0].as_deref(),
        Some("sk-upstream"),
        "Gemini 渠道出站认证走 x-goog-api-key（裸密钥）"
    );
    let received = gw.upstream.received();
    assert!(
        received[0].get("model").is_none(),
        "Gemini 出站体不写 model（与 URL 路径冲突）"
    );
    assert!(
        received[0].get("stream").is_none(),
        "Gemini 出站体不写 stream（流式与否由路径端点决定）"
    );
    assert_eq!(received[0]["contents"][0]["role"], "user");
    assert_eq!(received[0]["contents"][0]["parts"][0]["text"], "hi");

    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
    assert_eq!(body["usage"]["prompt_tokens"], 10);
    assert_eq!(body["usage"]["completion_tokens"], 2);
    // 非流式完整转换的计费对账：input 10*2.5 + output 2*10 = 45。
    common::wait_for_request_persistence(&gw.pool).await;
    assert_eq!(billed_rows(&gw.pool).await[0], (10, 2, 0, 45));
}

/// Gemini 同族直通与别名回落：同名请求字节直搬（URL 路径段零改写），别名命中
/// 走完整转换（URL 路径段携带出站名，体不带 `model`）；两条路径的计费同口径。
#[tokio::test]
async fn gemini_passthrough_zero_rewrite_and_alias_forces_ir() {
    let mut gw = TestGateway::start_with(seed_with_protocol(config::Protocol::Gemini)).await;
    let request = json!({
        "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }]
    });

    // 同名请求：字节直通，URL 路径段零改写，usage 按减法约定折算。
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "candidates": [{
            "content": { "role": "model", "parts": [{ "text": "ok" }] },
            "finishReason": "STOP",
            "index": 0
        }],
        "usageMetadata": billed_usage_metadata(),
        "modelVersion": TEST_MODEL,
        "responseId": "resp-gem"
    })));
    let resp = post_generate_content(&gw.base_url(), TEST_MODEL, request.clone()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    assert_eq!(
        gw.upstream.received_paths()[0],
        "/v1beta/models/gpt-4o:generateContent",
        "同名直通的路径段零改写"
    );
    assert_eq!(
        gw.upstream.received()[0],
        request,
        "直通请求体与下游字节级一致"
    );
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["usageMetadata"]["promptTokenCount"], json!(1250));
    // input = 1250 - 200（缓存）= 1050；output = 100 + 40 = 140。
    // 费用 = 1050*2.5 + 140*10 + 200*1.25 = 4275。
    common::wait_for_request_persistence(&gw.pool).await;
    assert_eq!(
        billed_rows(&gw.pool).await[0],
        (1050, 140, 200, 4275),
        "非流式直通按减法约定折算计费"
    );

    // 别名命中：出站名改写，回落完整转换；URL 路径段携带出站名，体不带 model。
    gw.upstream
        .push_behavior(UpstreamBehavior::Json(gemini_ok()));
    let resp = post_generate_content(&gw.base_url(), "fast", request).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    assert_eq!(
        gw.upstream.received_paths()[1],
        "/v1beta/models/gpt-4o-mini:generateContent",
        "别名命中后 URL 路径段按出站名构造"
    );
    let received = gw.upstream.received();
    assert!(received[1].get("model").is_none(), "完整转换同样不写 model");
    assert_eq!(received[1]["contents"][0]["parts"][0]["text"], "hi");
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(
        body["modelVersion"],
        json!("fast"),
        "响应模型名重写回入站短名"
    );

    // 别名请求按 request.model（fast）计价；微元按分量整数截断：
    common::wait_for_request_persistence(&gw.pool).await;
    // input 150_000*10/1M = 1，output 600_000*2/1M = 1，合计 2。
    assert_eq!(billed_rows(&gw.pool).await[1], (10, 2, 0, 2));

    // 结算同步入账：钱包扣减两笔费用，令牌累计结算等额增加。
    let (balance, settled): (i64, i64) = sqlx::query_as(
        "SELECT ub.balance_usd_micros, tb.settled_usd_micros \
         FROM tokens t \
         JOIN user_balance ub ON ub.user_id = t.user_id \
         JOIN token_balance tb ON tb.token_key = t.token_key \
         WHERE t.token_key = ?",
    )
    .bind(common::fingerprint(TEST_TOKEN_KEY))
    .fetch_one(&gw.pool)
    .await
    .expect("应能查询余额与结算");
    assert_eq!(balance, 5_000_000 - 4275 - 2, "余额应扣减两笔费用");
    assert_eq!(settled, 4277, "累计结算应等于两笔费用之和");
}

/// Gemini 同族流式直通：上游帧字节直搬，末 chunk 的 usageMetadata 旁路嗅探
/// 计费，无哨兵追加。
#[tokio::test]
async fn gemini_stream_passthrough_bills_final_chunk_usage() {
    let mut gw = TestGateway::start_with(seed_with_protocol(config::Protocol::Gemini)).await;
    let content_chunk = json!({
        "candidates": [{
            "content": { "role": "model", "parts": [{ "text": "你好" }] },
            "index": 0
        }],
        "modelVersion": TEST_MODEL
    });
    let finish_chunk = json!({
        "candidates": [{ "finishReason": "STOP", "index": 0 }],
        "usageMetadata": billed_usage_metadata()
    });
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        content_chunk.to_string(),
        finish_chunk.to_string(),
    ]));

    let resp = post_generate_content_stream(
        &gw.base_url(),
        TEST_MODEL,
        json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    assert_eq!(
        gw.upstream.received_paths()[0],
        "/v1beta/models/gpt-4o:streamGenerateContent?alt=sse"
    );
    let body = collect_sse_body(resp).await;
    assert_no_done_sentinel(&body, "Gemini 流式直通不追加哨兵");
    let frames = parse_sse_frames(&body);
    assert_eq!(frames.len(), 2, "直通帧与上游一一对应");
    assert_eq!(frames[0].data, content_chunk);
    assert_eq!(frames[1].data, finish_chunk);

    // input = 1250 - 200 = 1050；output = 100 + 40 = 140；费用 = 4275。
    assert_eq!(
        wait_for_billed_rows(&gw.pool, 1).await[0],
        (1050, 140, 200, 4275),
        "流式直通按末 chunk 的 usageMetadata 计费"
    );
}

/// chat 流式入站到 Gemini 渠道：上游逐 chunk 累计的 usage 在收尾折算计费，
/// 下游 chat 帧的 usage 回写为加法约定（prompt 含缓存）。
#[tokio::test]
async fn chat_inbound_stream_bills_gemini_upstream_usage() {
    let mut gw = TestGateway::start_with(seed_with_protocol(config::Protocol::Gemini)).await;
    gw.upstream.set_behavior(UpstreamBehavior::Sse(vec![
        json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": "你" }] },
                "index": 0
            }],
            "modelVersion": TEST_MODEL
        })
        .to_string(),
        json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": "好" }] },
                "index": 0
            }],
            "modelVersion": TEST_MODEL
        })
        .to_string(),
        json!({
            "candidates": [{ "finishReason": "STOP", "index": 0 }],
            "usageMetadata": billed_usage_metadata()
        })
        .to_string(),
    ]));

    let resp = post_chat_stream(
        &gw.base_url(),
        json!({
            "model": TEST_MODEL,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    assert_eq!(
        gw.upstream.received_paths()[0],
        "/v1beta/models/gpt-4o:streamGenerateContent?alt=sse"
    );
    let received = gw.upstream.received();
    assert!(
        received[0].get("model").is_none() && received[0].get("stream").is_none(),
        "流式出站体同样不带 model/stream"
    );

    let body = collect_sse_body(resp).await;
    let frames = parse_sse_frames(&body);
    let last = frames.last().expect("应有收尾帧");
    assert_eq!(last.data["choices"][0]["finish_reason"], json!("stop"));
    assert_eq!(last.data["usage"]["prompt_tokens"], json!(1250));
    assert_eq!(last.data["usage"]["completion_tokens"], json!(140));
    assert_eq!(
        last.data["usage"]["prompt_tokens_details"]["cached_tokens"],
        json!(200)
    );

    assert_eq!(
        wait_for_billed_rows(&gw.pool, 1).await[0],
        (1050, 140, 200, 4275),
        "完整转换路径与直通同口径计费"
    );
}

/// Gemini 客户端的模型列表端点：`models[].name` 带 `models/` 前缀的官方形状。
#[tokio::test]
async fn gemini_model_list_uses_official_shape() {
    let gw = TestGateway::start().await;

    let resp = reqwest::Client::new()
        .get(format!("{}/v1beta/models", gw.base_url()))
        .header("x-goog-api-key", TEST_TOKEN_KEY)
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: Value = resp.json().await.expect("响应应可解析");
    let models = body["models"].as_array().expect("应有 models 数组");
    assert!(
        models
            .iter()
            .any(|model| model["name"] == json!(format!("models/{TEST_MODEL}"))),
        "模型名应带 models/ 前缀: {models:?}"
    );
    assert!(models[0].get("supportedGenerationMethods").is_some());
}
