//! 计费（#04）端到端黑盒测试：请求完成后的按量扣费与准入控制。
//!
//! 主接缝：端到端 HTTP 黑盒，断言 SQLite 中余额精确扣减、请求日志含价格快照。

mod common;

use common::{
    SEED_PRICE_ATTACH_LISTING_CHANNELS, TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior,
};
use kairos::store::resources::Price;
use serde_json::{Value, json};

fn ok_response(usage: Value) -> Value {
    json!({
        "id": "chatcmpl-bill",
        "object": "chat.completion",
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "ok" },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": usage
    })
}

/// 读取令牌所属用户的钱包剩余（micro-USD）。
async fn balance_micros(gw: &TestGateway, key: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT ub.balance_usd_micros \
         FROM tokens t JOIN user_balance ub ON ub.user_id = t.user_id \
         WHERE t.token_key = ?",
    )
    .bind(key)
    .fetch_one(&gw.pool)
    .await
    .expect("用户余额应存在")
}

/// 读取令牌累计结算（micro-USD）。
async fn settled_micros(gw: &TestGateway, key: &str) -> i64 {
    sqlx::query_scalar("SELECT settled_usd_micros FROM token_balance WHERE token_key = ?")
        .bind(key)
        .fetch_one(&gw.pool)
        .await
        .expect("令牌累计结算应存在")
}

/// 发起一次 Chat Completions 请求，返回响应。
async fn send_completion(base: &str, model: &str, key: &str) -> reqwest::Response {
    let client = reqwest::Client::new();
    client
        .post(format!("{}/v1/chat/completions", base))
        .bearer_auth(key)
        .json(&json!({
            "model": model,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关")
}

/// 计费按实际 usage 四分量 × 各自价格精确扣减（整数 micro-USD）。
#[tokio::test]
async fn exact_usage_billed_and_deducted() {
    let mut gw = TestGateway::start().await;
    // usage：input 1000 / output 100 / cache_read 200 / cache_write 50。
    // wire 折算：input = prompt - cached - cache_write = 1250-200-50 = 1000。
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(json!({
            "prompt_tokens": 1250, "completion_tokens": 100, "total_tokens": 1350,
            "prompt_tokens_details": { "cached_tokens": 200, "cache_write_tokens": 50 }
        }))));
    // 期望费用 = 1000*2.5 + 100*10 + 200*1.25 + 50*10 (micro-USD) = 2500+1000+250+500 = 4250。

    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 初始余额 5 USD = 5_000_000 micro-USD，扣 4250 后余额 4_995_750，结算 4250。
    assert_eq!(balance_micros(&gw, TEST_TOKEN_KEY).await, 5_000_000 - 4250);
    assert_eq!(settled_micros(&gw, TEST_TOKEN_KEY).await, 4250);

    // 日志落 usage 四分量与费用。
    let row: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd_micros \
         FROM request_log",
    )
    .fetch_one(&gw.pool)
    .await
    .expect("应落一条日志");
    assert_eq!(row.0, 1000);
    assert_eq!(row.1, 100);
    assert_eq!(row.2, 200);
    assert_eq!(row.3, 50);
    assert_eq!(row.4, 4250);
}

/// 零输出（usage 全零）的请求不扣费。
#[tokio::test]
async fn zero_usage_is_not_charged() {
    let mut gw = TestGateway::start().await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(json!({
            "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0
        }))));

    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    assert_eq!(balance_micros(&gw, TEST_TOKEN_KEY).await, 5_000_000);
    assert_eq!(settled_micros(&gw, TEST_TOKEN_KEY).await, 0);
}

/// 成功 2xx 但上游不回报 usage：零计费，并落一条可观测的系统日志。
#[tokio::test]
async fn missing_usage_on_success_writes_system_warning() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-bill",
        "object": "chat.completion",
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "ok" },
            "logprobs": null,
            "finish_reason": "stop"
        }]
    })));

    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(balance_micros(&gw, TEST_TOKEN_KEY).await, 5_000_000);

    let (level, target, message): (String, String, String) = sqlx::query_as(
        "SELECT level, target, message FROM system_log WHERE target = 'billing' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&gw.pool)
    .await
    .expect("应落 usage 缺失的系统日志");
    assert_eq!(level, "warn");
    assert_eq!(target, "billing");
    assert!(
        message.contains("上游未回报 usage"),
        "系统日志应说明零计费原因，实际: {message}"
    );
}
#[tokio::test]
async fn failed_request_is_not_charged() {
    let mut gw = TestGateway::start().await;
    gw.upstream.set_behavior(UpstreamBehavior::Status429);

    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    assert_eq!(balance_micros(&gw, TEST_TOKEN_KEY).await, 5_000_000);
    assert_eq!(settled_micros(&gw, TEST_TOKEN_KEY).await, 0);
}

/// 缓存档缺省时该档不计费（用仅配置 input/output 的价格）。
#[tokio::test]
async fn unconfigured_cache_tier_is_not_charged() {
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.prices = vec![Price {
            channel_id: SEED_PRICE_ATTACH_LISTING_CHANNELS,
            model: TEST_MODEL.to_string(),
            input_micros: 2_500_000,
            output_micros: 10_000_000,
            cache_read_micros: None,
            cache_write_micros: None,
        }];
        seed
    })
    .await;
    // 有 cache token 也不按 input 计价：input=0（prompt 全是 cached），cache_read=1M → 费用 0。
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(json!({
            "prompt_tokens": 1_000_000, "completion_tokens": 0, "total_tokens": 1_000_000,
            "prompt_tokens_details": { "cached_tokens": 1_000_000 }
        }))));

    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    assert_eq!(balance_micros(&gw, TEST_TOKEN_KEY).await, 5_000_000);
    assert_eq!(settled_micros(&gw, TEST_TOKEN_KEY).await, 0);

    let row: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT cache_read_tokens, cost_usd_micros, \
                cache_read_price_usd_micros, cache_write_price_usd_micros, input_price_usd_micros \
         FROM request_log",
    )
    .fetch_one(&gw.pool)
    .await
    .expect("应落一条日志");
    assert_eq!(row.0, 1_000_000);
    assert_eq!(row.1, 0);
    assert_eq!(row.2, 0);
    assert_eq!(row.3, 0);
    assert_eq!(row.4, 2_500_000);
}

/// 余额不足（初始余额 0）准入时被拒绝：402 + OpenAI 错误格式。
#[tokio::test]
async fn zero_balance_request_is_402() {
    let gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.tokens[0].balance_usd = 0.0;
        seed
    })
    .await;

    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
    let body: Value = resp.json().await.expect("402 响应应可解析");
    assert!(body["error"]["message"].is_string());
    assert!(gw.upstream.received().is_empty(), "准入拒绝不应出站");
}

/// 在途透支：正余额准入后实际费用超出剩余余额，照常结算（余额可为负），
/// 下一次请求在准入时被拒绝。
#[tokio::test]
async fn overdraft_settles_and_blocks_next_request() {
    // 初始余额 0.000001 USD = 1 micro-USD，费用会透支。
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.tokens[0].balance_usd = 0.000001;
        seed
    })
    .await;
    // prompt=1250, cached=200, cache_write=50, completion=100 → input=1000, cache_read=200,
    // cache_write=50 → cost = 2500+1000+250+500 = 4250。
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(json!({
            "prompt_tokens": 1250, "completion_tokens": 100, "total_tokens": 1350,
            "prompt_tokens_details": { "cached_tokens": 200, "cache_write_tokens": 50 }
        }))));

    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "准入时余额为正");
    assert_eq!(balance_micros(&gw, TEST_TOKEN_KEY).await, 1 - 4250);

    // 下一次请求：余额 ≤ 0，准入拒绝（无需再设 mock 行为）。
    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
}

/// 累计结算超 limit_usd（与余额相互独立）时准入拒绝。
#[tokio::test]
async fn settled_limit_exceeded_is_402() {
    // 初始余额充足，但 limit_usd 极小（0.01 USD = 10000 micro-USD）。
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.tokens[0].balance_usd = 5.0;
        seed.tokens[0].limit_usd = Some(0.01);
        seed
    })
    .await;
    // 每次结算 4250，settled 依次 4250/8500/12750。
    let usage = json!({
        "prompt_tokens": 1250, "completion_tokens": 100, "total_tokens": 1350,
        "prompt_tokens_details": { "cached_tokens": 200, "cache_write_tokens": 50 }
    });

    // 第一次：结算 4250，settled=4250 < 10000，准入通过。
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(usage.clone())));
    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 第二次：settled=4250 < 10000，准入通过，结算后 settled=8500。
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(usage.clone())));
    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 第三次：settled=8500 < 10000，准入通过，结算后 settled=12750。
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(usage.clone())));
    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 第四次：settled=12750 ≥ 10000，准入拒绝（无需 mock 行为）。
    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
    assert!(
        gw.upstream.received().len() == 3,
        "超限请求不应出站，实际出站 {} 次",
        gw.upstream.received().len()
    );
}

/// 模型未配置价格：准入时拒绝并返回可读错误。
#[tokio::test]
async fn missing_price_is_rejected() {
    // 渠道服务于 `no-price` 模型（有渠道），但价格表无该模型项。
    let gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.channels[0].models.push("no-price".to_string());
        seed
    })
    .await;

    let resp = send_completion(&gw.base_url(), "no-price", TEST_TOKEN_KEY).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "价格缺失应拒绝"
    );
    let body: Value = resp.json().await.expect("503 响应应可解析");
    let msg = body["error"]["message"].as_str().expect("消息应为字符串");
    assert!(msg.contains("价格"), "消息应提示价格缺失，实际 {msg}");
    assert!(gw.upstream.received().is_empty(), "价格缺失不应出站");
}

/// 每条日志保存计费时的四档价格快照，调价后历史账单可复核。
#[tokio::test]
async fn log_records_price_snapshot() {
    let mut gw = TestGateway::start().await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(json!({
            "prompt_tokens": 1000, "completion_tokens": 100, "total_tokens": 1100,
            "prompt_tokens_details": { "cached_tokens": 200 }
        }))));

    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let row: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT input_price_usd_micros, output_price_usd_micros, \
                cache_read_price_usd_micros, cache_write_price_usd_micros \
         FROM request_log",
    )
    .fetch_one(&gw.pool)
    .await
    .expect("应落一条日志");
    // 2.5 / 10.0 / 1.25 / 10.0 USD 每 1M tokens → micro-USD 每 1M tokens。
    assert_eq!(row.0, 2_500_000);
    assert_eq!(row.1, 10_000_000);
    assert_eq!(row.2, 1_250_000);
    assert_eq!(row.3, 10_000_000);
}

/// 令牌余额存库；重启从库加载快照，不重置已存在的余额，请求路径仍可用。
#[tokio::test]
async fn balance_persists_across_restart() {
    let mut gw = TestGateway::start().await;
    // prompt=1250, cached=200, cache_write=50, completion=100 → cost 4250。
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(json!({
            "prompt_tokens": 1250, "completion_tokens": 100, "total_tokens": 1350,
            "prompt_tokens_details": { "cached_tokens": 200, "cache_write_tokens": 50 }
        }))));

    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(balance_micros(&gw, TEST_TOKEN_KEY).await, 5_000_000 - 4250);

    // 用同一数据库文件重启网关（模拟重启），余额不应被重置回初始 5 USD。
    // 资源也存库中，重启从库加载快照即可，无需再注入配置。
    assert_eq!(balance_micros(&gw, TEST_TOKEN_KEY).await, 5_000_000 - 4250);
    let base2 = gw.spawn_reloaded_protocol().await;

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(json!({
            "prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11
        }))));
    let resp = send_completion(&base2, TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "重启后应从库加载令牌与渠道，请求路径仍可用"
    );
    let after_reload = balance_micros(&gw, TEST_TOKEN_KEY).await;
    assert!(
        after_reload < 5_000_000 - 4250,
        "重启后继续结算，余额不应被重置，实际 {after_reload}"
    );
}

/// 带极大 `max_tokens` 时按 output 单价粗估，挡住极端输出上限。
#[tokio::test]
async fn huge_max_tokens_is_rejected_before_upstream() {
    let gw = TestGateway::start().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "max_tokens": 600_000,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关");
    assert_eq!(resp.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
    assert!(gw.upstream.received().is_empty(), "预估超余额不应出站");
    assert_eq!(
        balance_micros(&gw, TEST_TOKEN_KEY).await,
        5_000_000,
        "被门槛挡住时不应扣费"
    );
}

/// 同一用户两把令牌共用钱包；各令牌 `settled` 分开计。
#[tokio::test]
async fn sibling_tokens_share_user_wallet() {
    const SIBLING_KEY: &str = "sk-test-token-b";
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.tokens.push(common::SeedToken {
            token_key: SIBLING_KEY.to_string(),
            name: "dev-b".to_string(),
            limit_usd: None,
            balance_usd: 0.0,
            rate_limit_rpm: None,
        });
        seed
    })
    .await;
    let usage = json!({
        "prompt_tokens": 1250, "completion_tokens": 100, "total_tokens": 1350,
        "prompt_tokens_details": { "cached_tokens": 200, "cache_write_tokens": 50 }
    });
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(usage.clone())));
    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(balance_micros(&gw, TEST_TOKEN_KEY).await, 5_000_000 - 4250);
    assert_eq!(balance_micros(&gw, SIBLING_KEY).await, 5_000_000 - 4250);
    assert_eq!(settled_micros(&gw, TEST_TOKEN_KEY).await, 4250);
    assert_eq!(settled_micros(&gw, SIBLING_KEY).await, 0);

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(usage)));
    let resp = send_completion(&gw.base_url(), TEST_MODEL, SIBLING_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(balance_micros(&gw, TEST_TOKEN_KEY).await, 5_000_000 - 8500);
    assert_eq!(balance_micros(&gw, SIBLING_KEY).await, 5_000_000 - 8500);
    assert_eq!(settled_micros(&gw, TEST_TOKEN_KEY).await, 4250);
    assert_eq!(settled_micros(&gw, SIBLING_KEY).await, 4250);
}

/// 一把令牌在途透支后，同用户另一把令牌的新请求也被挡住。
#[tokio::test]
async fn overdraft_on_one_token_blocks_sibling() {
    const SIBLING_KEY: &str = "sk-test-token-b";
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.tokens[0].balance_usd = 0.000001;
        seed.tokens.push(common::SeedToken {
            token_key: SIBLING_KEY.to_string(),
            name: "dev-b".to_string(),
            limit_usd: None,
            balance_usd: 0.0,
            rate_limit_rpm: None,
        });
        seed
    })
    .await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(json!({
            "prompt_tokens": 1250, "completion_tokens": 100, "total_tokens": 1350,
            "prompt_tokens_details": { "cached_tokens": 200, "cache_write_tokens": 50 }
        }))));
    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(balance_micros(&gw, SIBLING_KEY).await, 1 - 4250);

    let resp = send_completion(&gw.base_url(), TEST_MODEL, SIBLING_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
    assert_eq!(
        gw.upstream.received().len(),
        1,
        "第二把令牌被挡住时不应再出站"
    );
}

/// 令牌 `limit_usd_micros` 按该令牌 settled，不与同用户另一把令牌共享。
#[tokio::test]
async fn token_limit_is_per_token_not_shared_wallet() {
    const SIBLING_KEY: &str = "sk-test-token-b";
    let mut gw = TestGateway::start_with(|base| {
        let mut seed = common::test_seed(base);
        seed.tokens[0].limit_usd = Some(0.01);
        seed.tokens.push(common::SeedToken {
            token_key: SIBLING_KEY.to_string(),
            name: "dev-b".to_string(),
            limit_usd: None,
            balance_usd: 0.0,
            rate_limit_rpm: None,
        });
        seed
    })
    .await;
    let usage = json!({
        "prompt_tokens": 1250, "completion_tokens": 100, "total_tokens": 1350,
        "prompt_tokens_details": { "cached_tokens": 200, "cache_write_tokens": 50 }
    });
    for _ in 0..3 {
        gw.upstream
            .set_behavior(UpstreamBehavior::Json(ok_response(usage.clone())));
        let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }
    let blocked = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(blocked.status(), reqwest::StatusCode::PAYMENT_REQUIRED);

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(usage)));
    let sibling = send_completion(&gw.base_url(), TEST_MODEL, SIBLING_KEY).await;
    assert_eq!(sibling.status(), reqwest::StatusCode::OK);
    assert_eq!(settled_micros(&gw, TEST_TOKEN_KEY).await, 12_750);
    assert_eq!(settled_micros(&gw, SIBLING_KEY).await, 4250);
    assert_eq!(
        balance_micros(&gw, SIBLING_KEY).await,
        5_000_000 - 12_750 - 4250
    );
}

/// 未带 `max_tokens` 时不做输出粗估，沿用余额门槛。
#[tokio::test]
async fn omitted_max_tokens_does_not_use_estimate_gate() {
    let mut gw = TestGateway::start().await;
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(ok_response(json!({
            "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2
        }))));
    let resp = send_completion(&gw.base_url(), TEST_MODEL, TEST_TOKEN_KEY).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}
