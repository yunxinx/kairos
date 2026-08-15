//! 渠道路由与 failover（#06）端到端黑盒测试。
//!
//! 主接缝：测试内启动网关 + 多个可编程 mock 上游，按渠道注入 429/5xx/断连，
//! 断言 failover 行为、下游收到的错误格式（含网关归因字段）。
//!
//! 覆盖：priority 升序优先、同级 weight 加权随机、别名重写、可重试错误自动
//! 切换下一渠道、每渠道 max_retries、不可重试 4xx 直接返回。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use kairos::config;
use kairos::store::resources::Channel;
use serde_json::{Value, json};

fn ok_response() -> Value {
    json!({
        "id": "chatcmpl-r", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

/// 发起非流式 Chat Completions 请求，返回响应。
async fn send_completion(base: &str, model: &str) -> reqwest::Response {
    let client = reqwest::Client::new();
    client
        .post(format!("{}/v1/chat/completions", base))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": model,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关")
}

/// 构造两个渠道的 seed，分别指向两个 mock 上游。默认 ch-0 更高优先级（数值更小），
/// 使 failover 顺序确定：ch-0 先试、ch-1 兜底。
fn two_channel_seed(bases: &[String]) -> common::Seed {
    let mut seed = common::test_seed(&bases[0]);
    seed.channels = vec![
        Channel {
            name: "ch-0".to_string(),
            protocol: config::Protocol::OpenAiChat,
            base_url: bases[0].clone(),
            api_key: "sk-0".to_string(),
            models: vec![TEST_MODEL.to_string()],
            model_aliases: Default::default(),
            priority: 1,
            weight: 1,
            timeout_ms: 1000,
            max_retries: 0,
            enabled: true,
        },
        Channel {
            name: "ch-1".to_string(),
            protocol: config::Protocol::OpenAiChat,
            base_url: bases[1].clone(),
            api_key: "sk-1".to_string(),
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
}

/// 首渠道 429、次渠道成功：自动 failover 到下一渠道，下游收到成功响应。
#[tokio::test]
async fn retryable_429_fails_over_to_next_channel() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_channel_seed(bases);
        seed.channels[0].max_retries = 0;
        seed.channels[1].max_retries = 0;
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Status429);
    ups[1].set_behavior(UpstreamBehavior::Json(ok_response()));

    let resp = send_completion(&gw.base_url(), TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "failover 后应成功");
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["choices"][0]["message"]["content"], "ok");

    // 两个渠道都被请求过（首渠道失败一次，次渠道成功一次）。
    assert_eq!(ups[0].received().len(), 1, "首渠道应收一次请求");
    assert_eq!(ups[1].received().len(), 1, "次渠道应收一次请求");
}

/// 首渠道 5xx、次渠道成功：failover 到下一渠道。
#[tokio::test]
async fn retryable_5xx_fails_over_to_next_channel() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_channel_seed(bases);
        seed.channels[0].max_retries = 0;
        seed.channels[1].max_retries = 0;
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Status5xx(500));
    ups[1].set_behavior(UpstreamBehavior::Json(ok_response()));

    let resp = send_completion(&gw.base_url(), TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "failover 后应成功");
    assert_eq!(ups[0].received().len(), 1);
    assert_eq!(ups[1].received().len(), 1);
}

/// 首渠道网络不可达（连接拒绝）、次渠道成功：failover。
#[tokio::test]
async fn network_error_fails_over_to_next_channel() {
    // 用一个不存在的端口模拟网络不可达。
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_channel_seed(bases);
        // 首渠道指向一个不会有服务的端口。
        seed.channels[0].base_url = "http://127.0.0.1:1".to_string();
        seed.channels[0].max_retries = 0;
        seed.channels[1].max_retries = 0;
        seed
    })
    .await;
    ups[1].set_behavior(UpstreamBehavior::Json(ok_response()));

    let resp = send_completion(&gw.base_url(), TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "网络错误应 failover"
    );
    assert_eq!(ups[1].received().len(), 1, "次渠道应收一次请求");
}

/// 每渠道 max_retries：首渠道 429 可重试 max_retries 次后仍失败才 failover。
#[tokio::test]
async fn per_channel_max_retries_retries_same_channel_then_fails_over() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_channel_seed(bases);
        seed.channels[0].max_retries = 2; // 首渠道最多尝试 3 次
        seed.channels[1].max_retries = 0;
        seed
    })
    .await;
    // 首渠道持续 429（重试预算内，最多尝试 3 次），次渠道成功。
    for _ in 0..3 {
        ups[0].push_behavior(UpstreamBehavior::Status429);
    }
    ups[1].set_behavior(UpstreamBehavior::Json(ok_response()));

    let resp = send_completion(&gw.base_url(), TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "failover 后应成功");
    // 首渠道被请求 max_retries+1 = 3 次（同渠道重试 2 次），然后 failover 到次渠道。
    assert_eq!(ups[0].received().len(), 3, "首渠道应重试 3 次");
    assert_eq!(ups[1].received().len(), 1, "次渠道应收一次请求");
}

/// 所有候选渠道均 429：返回最后一次可重试错误，状态码原样 + 归因字段。
#[tokio::test]
async fn all_channels_retryable_fail_returns_attributed_error() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_channel_seed(bases);
        seed.channels[0].max_retries = 0;
        seed.channels[1].max_retries = 0;
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Status429);
    ups[1].set_behavior(UpstreamBehavior::Status429);

    let resp = send_completion(&gw.base_url(), TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "429 应原样透传"
    );
    let body: Value = resp.json().await.expect("错误体应可解析");
    assert!(body["error"]["message"].is_string(), "应为 OpenAI 错误格式");
    // 归因字段标识出错渠道与已 failover。
    assert_eq!(body["error"]["gateway"]["failover"], true);
    assert!(body["error"]["gateway"]["channel"].is_string());
}

/// 不可重试 4xx（如 400）不重试，直接返回，状态码原样 + 归因（未 failover）。
#[tokio::test]
async fn non_retryable_4xx_returns_immediately() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_channel_seed(bases);
        seed.channels[0].max_retries = 2; // 即使可重试 budget 大，4xx 也不重试
        seed.channels[1].max_retries = 0;
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::for_status(400));
    ups[1].set_behavior(UpstreamBehavior::Json(ok_response()));

    let resp = send_completion(&gw.base_url(), TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "400 不重试，原样返回"
    );
    // 只请求了首渠道一次，未 failover 到次渠道。
    assert_eq!(ups[0].received().len(), 1, "首渠道只请求一次");
    assert_eq!(ups[1].received().len(), 0, "4xx 不应 failover");
    let body: Value = resp.json().await.expect("错误体应可解析");
    assert_eq!(body["error"]["gateway"]["failover"], false);
}

/// 别名命中：出站请求用真实模型名，响应重写回入站短名。
#[tokio::test]
async fn alias_rewrites_outbound_and_response_model() {
    let (gw, mut ups) = TestGateway::start_with_multi(1, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        seed.channels[0]
            .model_aliases
            .insert("fast".to_string(), "gpt-4o-mini".to_string());
        seed
    })
    .await;
    // 上游返回真实模型名。
    ups[0].set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-alias", "object": "chat.completion", "model": "gpt-4o-mini",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })));

    let resp = send_completion(&gw.base_url(), "fast").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    // 出站请求模型被重写为真实名。
    assert_eq!(ups[0].received()[0]["model"], "gpt-4o-mini");
    // 响应模型名重写回入站短名。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["model"], "fast");
}

/// 别名命中 + 遇 429 自动 failover 到另一渠道（同模型多渠道，别名在渠道级）。
#[tokio::test]
async fn alias_fails_over_across_channels() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_channel_seed(bases);
        // 两个渠道都服务短名 `fast`，都映射到真实名 gpt-4o-mini。
        for ch in &mut seed.channels {
            ch.model_aliases
                .insert("fast".to_string(), "gpt-4o-mini".to_string());
        }
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Status429);
    ups[1].set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-a", "object": "chat.completion", "model": "gpt-4o-mini",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })));

    let resp = send_completion(&gw.base_url(), "fast").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "别名请求应 failover"
    );
    // 响应模型名重写回短名。
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["model"], "fast");
    assert_eq!(ups[1].received()[0]["model"], "gpt-4o-mini");
}

/// 流式请求遇 429 自动 failover：首渠道 429，次渠道以 SSE 流响应成功。
#[tokio::test]
async fn stream_fails_over_on_429() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_channel_seed(bases);
        seed.channels[0].max_retries = 0;
        seed.channels[1].max_retries = 0;
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Status429);
    ups[1].set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "Hi" } }]
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
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
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "流式应 failover");
    assert_eq!(ups[0].received().len(), 1);
    assert_eq!(ups[1].received().len(), 1);
}

/// 流式首渠道中途断连（已发首字节）：不 failover（spec：failover 只在首字节前），
/// 下游收到已累积的部分流后结束。
#[tokio::test]
async fn stream_disconnect_after_first_byte_does_not_failover() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_channel_seed(bases);
        seed.channels[0].max_retries = 3; // 即使可重试 budget 大，首字节后断连也不 failover
        seed.channels[1].max_retries = 0;
        seed
    })
    .await;
    // 首渠道发一个帧后断连；次渠道本应能成功，但不应被调用。
    ups[0].set_behavior(UpstreamBehavior::Disconnect);
    ups[1].set_behavior(UpstreamBehavior::Sse(vec![
        serde_json::to_string(&json!({
            "id": "ok", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "content": "should-not" } }]
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
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "首字节后断连仍返回 200"
    );
    // 只请求了首渠道一次，未 failover 到次渠道。
    assert_eq!(ups[0].received().len(), 1, "首渠道只请求一次");
    assert_eq!(ups[1].received().len(), 0, "首字节后断连不应 failover");
}
