//! 渠道路由与 failover（#06）端到端黑盒测试。
//!
//! 主接缝：测试内启动网关 + 多个可编程 mock 上游，按渠道注入 429/5xx/断连，
//! 断言 failover 行为、下游收到的错误格式（含网关归因字段）。
//!
//! 覆盖：创建顺序默认选路、别名重写、可重试错误自动切换下一渠道、每渠道
//! max_retries、不可重试 4xx 直接返回。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use kairos::config;
use kairos::store::resources::{Channel, GroupModel, ModelGroup, Price};
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

/// 构造两个渠道的 seed，分别指向两个 mock 上游。未设显式顺序时按创建先后：
/// ch-0 先试、ch-1 兜底。
fn two_channel_seed(bases: &[String]) -> common::Seed {
    let mut seed = common::test_seed(&bases[0]);
    seed.channels = vec![
        Channel {
            name: "ch-0".to_string(),
            protocol: config::Protocol::OpenAiChat,
            base_url: bases[0].clone(),
            keys: vec![kairos::store::resources::ChannelKey {
                name: "default".to_string(),
                api_key: "sk-0".to_string(),
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
        },
        Channel {
            name: "ch-1".to_string(),
            protocol: config::Protocol::OpenAiChat,
            base_url: bases[1].clone(),
            keys: vec![kairos::store::resources::ChannelKey {
                name: "default".to_string(),
                api_key: "sk-1".to_string(),
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
        },
    ];
    seed
}

/// 三条同名渠道：创建顺序与 mock 上游下标一致，便于验证显式顺序再过滤的结果。
fn three_channel_seed(bases: &[String]) -> common::Seed {
    let mut seed = two_channel_seed(bases);
    seed.channels.push(Channel {
        name: "ch-2".to_string(),
        protocol: config::Protocol::OpenAiChat,
        base_url: bases[2].clone(),
        keys: vec![kairos::store::resources::ChannelKey {
            name: "default".to_string(),
            api_key: "sk-2".to_string(),
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
    });
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

/// 渠道内启用密钥按模型名单筛选，并把选中的密钥用于出站认证。
#[tokio::test]
async fn channel_key_model_filter_selects_usable_auth_key() {
    let (gw, mut ups) = TestGateway::start_with_multi(1, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        seed.channels[0].keys = vec![
            kairos::store::resources::ChannelKey {
                name: "blocked".to_string(),
                api_key: "sk-blocked".to_string(),
                weight: 100,
                enabled: true,
                models: Some(vec!["other".to_string()]),
                blocked_models: None,
            },
            kairos::store::resources::ChannelKey {
                name: "usable".to_string(),
                api_key: "sk-usable".to_string(),
                weight: 1,
                enabled: true,
                models: Some(vec![TEST_MODEL.to_string()]),
                blocked_models: None,
            },
        ];
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Json(ok_response()));

    let response = send_completion(&gw.base_url(), TEST_MODEL).await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        ups[0].received_api_keys(),
        vec![Some("Bearer sk-usable".to_string())]
    );
}

/// 同一会话的首试、429 重试与后续请求均复用同一把渠道密钥。
#[tokio::test]
async fn session_stickiness_keeps_key_across_retry_and_requests() {
    let (gw, mut ups) = TestGateway::start_with_multi(1, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        seed.channels[0].max_retries = 1;
        seed.channels[0].keys = vec![
            kairos::store::resources::ChannelKey {
                name: "a".to_string(),
                api_key: "sk-a".to_string(),
                weight: 1,
                enabled: true,
                models: None,
                blocked_models: None,
            },
            kairos::store::resources::ChannelKey {
                name: "b".to_string(),
                api_key: "sk-b".to_string(),
                weight: 1,
                enabled: true,
                models: None,
                blocked_models: None,
            },
        ];
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Status429);
    ups[0].set_behavior(UpstreamBehavior::Json(ok_response()));
    ups[0].set_behavior(UpstreamBehavior::Json(ok_response()));

    let client = reqwest::Client::new();
    let request = || {
        client
            .post(format!("{}/v1/chat/completions", gw.base_url()))
            .bearer_auth(TEST_TOKEN_KEY)
            .header("x-kairos-session-id", "session-1")
            .json(&json!({
                "model": TEST_MODEL,
                "messages": [{ "role": "user", "content": "hi" }]
            }))
    };
    assert_eq!(request().send().await.expect("首请求应成功").status(), 200);
    assert_eq!(
        request().send().await.expect("后续请求应成功").status(),
        200
    );

    let keys = ups[0].received_api_keys();
    assert_eq!(keys.len(), 3, "首试、429 重试与后续请求都应到达上游");
    assert!(keys.iter().all(|key| key == &keys[0]));
}

/// 不带会话头时，IR 前缀相同的请求也复用同一把密钥。
#[tokio::test]
async fn session_prefix_stickiness_without_header() {
    let (gw, mut ups) = TestGateway::start_with_multi(1, |bases| {
        let mut seed = common::test_seed(&bases[0]);
        seed.channels[0].keys = vec![
            kairos::store::resources::ChannelKey {
                name: "a".to_string(),
                api_key: "sk-a".to_string(),
                weight: 1,
                enabled: true,
                models: None,
                blocked_models: None,
            },
            kairos::store::resources::ChannelKey {
                name: "b".to_string(),
                api_key: "sk-b".to_string(),
                weight: 1,
                enabled: true,
                models: None,
                blocked_models: None,
            },
        ];
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Json(ok_response()));
    ups[0].set_behavior(UpstreamBehavior::Json(ok_response()));

    let client = reqwest::Client::new();
    for _ in 0..2 {
        let response = client
            .post(format!("{}/v1/chat/completions", gw.base_url()))
            .bearer_auth(TEST_TOKEN_KEY)
            .json(&json!({
                "model": TEST_MODEL,
                "messages": [
                    { "role": "system", "content": "be precise" },
                    { "role": "user", "content": "hello" }
                ]
            }))
            .send()
            .await
            .expect("请求应成功");
        assert_eq!(response.status(), 200);
    }
    let keys = ups[0].received_api_keys();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
}

/// 顺序表先给全部候选排序；模型组钉渠道随后只做稳定过滤，不能让未钉渠道
/// 的排位改变剩余候选的相对次序。
#[tokio::test]
async fn pinned_group_filters_after_ordering_without_reordering() {
    let (gw, mut ups) = TestGateway::start_with_multi(3, three_channel_seed).await;
    let mut conn = gw.pool.acquire().await.expect("应能获取连接");
    kairos::store::resources::upsert_model_group(
        &mut conn,
        &ModelGroup {
            name: "pinned".to_string(),
            models: vec![
                GroupModel::Source {
                    channel_id: 1,
                    model: TEST_MODEL.to_string(),
                },
                GroupModel::Source {
                    channel_id: 2,
                    model: TEST_MODEL.to_string(),
                },
            ],
        },
    )
    .await
    .expect("应能写模型组");
    sqlx::query("UPDATE tokens SET model_group = 'pinned' WHERE token_key = ?")
        .bind(TEST_TOKEN_KEY)
        .execute(&mut *conn)
        .await
        .expect("应能改测试令牌模型组");
    sqlx::query(
        "INSERT INTO channel_model_order (model, channel_id, position) VALUES \
         (?, 3, 0), (?, 2, 1), (?, 1, 2)",
    )
    .bind(TEST_MODEL)
    .bind(TEST_MODEL)
    .bind(TEST_MODEL)
    .execute(&mut *conn)
    .await
    .expect("应能写顺序表");
    drop(conn);

    let base = gw.spawn_reloaded_protocol().await;
    ups[1].set_behavior(UpstreamBehavior::Status429);
    ups[0].set_behavior(UpstreamBehavior::Json(ok_response()));
    ups[2].set_behavior(UpstreamBehavior::Json(ok_response()));

    let resp = send_completion(&base, TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(ups[2].received().len(), 0, "未钉渠道不得被调用");
    assert_eq!(ups[1].received().len(), 1, "钉渠道中顺序靠前的应先试");
    assert_eq!(
        ups[0].received().len(),
        1,
        "可重试失败后应保序切到下一钉渠道"
    );
}

/// 未定价渠道也只能在顺序表排序后稳定滤掉，不能使后续有价候选重排。
#[tokio::test]
async fn unpriced_channel_is_filtered_after_ordering_without_reordering() {
    let (gw, mut ups) = TestGateway::start_with_multi(3, three_channel_seed).await;
    let mut conn = gw.pool.acquire().await.expect("应能获取连接");
    sqlx::query("DELETE FROM prices WHERE channel_id = 3 AND model = ?")
        .bind(TEST_MODEL)
        .execute(&mut *conn)
        .await
        .expect("应能删未定价渠道价格");
    sqlx::query(
        "INSERT INTO channel_model_order (model, channel_id, position) VALUES \
         (?, 3, 0), (?, 2, 1), (?, 1, 2)",
    )
    .bind(TEST_MODEL)
    .bind(TEST_MODEL)
    .bind(TEST_MODEL)
    .execute(&mut *conn)
    .await
    .expect("应能写顺序表");
    drop(conn);

    let base = gw.spawn_reloaded_protocol().await;
    ups[1].set_behavior(UpstreamBehavior::Status429);
    ups[0].set_behavior(UpstreamBehavior::Json(ok_response()));
    ups[2].set_behavior(UpstreamBehavior::Json(ok_response()));

    let resp = send_completion(&base, TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(ups[2].received().len(), 0, "未定价渠道不得出站");
    assert_eq!(ups[1].received().len(), 1, "有价候选应保持原顺序先试");
    assert_eq!(ups[0].received().len(), 1, "可重试失败后应保序切换");
}

/// 首渠道 429、次渠道成功：只按成功渠道单价结算一次。
#[tokio::test]
async fn failover_bills_succeeding_channel_price_once() {
    const PRICE_A: i64 = 1_000_000;
    const PRICE_B: i64 = 3_000_000;
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_channel_seed(bases);
        seed.channels[0].max_retries = 0;
        seed.channels[1].max_retries = 0;
        seed.prices = vec![
            Price {
                channel_id: 1,
                model: TEST_MODEL.to_string(),
                input_micros: PRICE_A,
                output_micros: 0,
                cache_read_micros: None,
                cache_write_micros: None,
            },
            Price {
                channel_id: 2,
                model: TEST_MODEL.to_string(),
                input_micros: PRICE_B,
                output_micros: 0,
                cache_read_micros: None,
                cache_write_micros: None,
            },
        ];
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Status429);
    ups[1].set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-bill-b", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1_000_000, "completion_tokens": 0, "total_tokens": 1_000_000}
    })));

    let resp = send_completion(&gw.base_url(), TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "failover 后应成功");
    assert_eq!(ups[0].received().len(), 1, "首渠道应收一次请求");
    assert_eq!(ups[1].received().len(), 1, "次渠道应收一次请求");

    let cost: (i64,) =
        sqlx::query_as("SELECT cost_usd_micros FROM request_log ORDER BY id DESC LIMIT 1")
            .fetch_one(&gw.pool)
            .await
            .expect("应落结算");
    assert_eq!(cost.0, PRICE_B, "应按成功渠道单价结算，不得用失败渠道价");

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM request_log")
        .fetch_one(&gw.pool)
        .await
        .expect("应能统计日志");
    assert_eq!(count.0, 1, "失败 hop 不落账，只应有一条成功日志");

    let balance: (i64, i64) = sqlx::query_as(
        "SELECT ub.balance_usd_micros, tb.settled_usd_micros FROM tokens t JOIN user_balance ub ON ub.user_id = t.user_id JOIN token_balance tb ON tb.token_key = t.token_key WHERE t.token_key = ?",
    )
    .bind(TEST_TOKEN_KEY)
    .fetch_one(&gw.pool)
    .await
    .expect("令牌余额应存在");
    assert_eq!(balance.0, 5_000_000 - PRICE_B, "余额只应按成功渠道扣一次");
    assert_eq!(balance.1, PRICE_B, "累计结算应等于成功渠道费用");
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

/// 别名按渠道改写：各候选用自己的表，不共用切片里第一个候选的出站名。
///
/// 首渠道的别名不得泄露给后续渠道。无别名渠道应原样发送入站名；轮到别名渠道
/// 时才改写为该渠道自己的真名。
#[tokio::test]
async fn alias_rewrites_per_channel_not_shared_from_first_candidate() {
    let (gw, mut ups) = TestGateway::start_with_multi(2, |bases| {
        let mut seed = two_channel_seed(bases);
        // channels[0] → ups[0]：别名渠道，按默认创建顺序先试。
        seed.channels[0].name = "alias-ch".to_string();
        seed.channels[0]
            .model_aliases
            .insert("fast".to_string(), "gpt-4o-mini".to_string());
        // channels[1] → ups[1]：无别名，清单含短名，作为 failover 渠道。
        seed.channels[1].name = "plain-ch".to_string();
        seed.channels[1].models = vec!["fast".to_string()];
        seed.channels[1].model_aliases.clear();
        seed
    })
    .await;
    ups[0].set_behavior(UpstreamBehavior::Status429);
    ups[1].set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-per-ch", "object": "chat.completion", "model": "fast",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })));

    let resp = send_completion(&gw.base_url(), "fast").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "别名渠道 429 后应 failover 到无别名渠道"
    );
    assert_eq!(
        ups[0].received()[0]["model"],
        "gpt-4o-mini",
        "别名渠道应按自己的表改写"
    );
    assert_eq!(
        ups[1].received()[0]["model"],
        "fast",
        "无别名渠道应按入站名出站，不得套用其它渠道的别名"
    );
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["model"], "fast", "响应模型名应回显入站短名");
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

/// 流式首渠道中途断连（已发首字节）：不 failover（failover 只保证首字节前的
/// 请求完整性，首字节后重试会让下游收到重复内容），下游收到已累积的部分流后结束。
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
