//! 管理面测试共享辅助：带会话认证的请求、渠道构造、请求路径驱动与日志播种。

use kairos::store;
use serde_json::{Value, json};

use super::{
    TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior, wait_for_request_persistence,
};

/// 带管理会话认证的 GET 请求。
pub async fn admin_get(gw: &TestGateway, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}{path}", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("管理请求应可达")
}

/// 按名查出渠道的库生成 id，供按 id 定位的端点使用。
pub async fn channel_id_by_name(gw: &TestGateway, name: &str) -> i64 {
    let list: Value = admin_get(gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let channel = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("渠道 {name} 应在列表"))
        .clone();
    channel["id"].as_i64().expect("列表应回传整数 id")
}

/// 渠道完整 JSON body：openai_chat 指向给定上游，其余字段取常规默认且启用。
pub fn channel_body(name: &str, base_url: String, models: Value) -> Value {
    json!({
        "name": name,
        "protocol": "openai_chat",
        "base_url": base_url,
        "keys": [{
            "name": "default",
            "api_key": "sk-upstream",
            "weight": 1,
            "enabled": true,
            "models": null,
            "blocked_models": null
        }],
        "models": models,
        "model_aliases": {},
        "timeout_ms": 1000,
        "max_retries": 0,
        "enabled": true
    })
}

/// 带管理会话认证、携带 JSON body 的请求。
pub async fn admin_json(
    gw: &TestGateway,
    method: reqwest::Method,
    path: &str,
    body: Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .request(method, format!("{}{path}", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&body)
        .send()
        .await
        .expect("管理请求应可达")
}

/// 以指定令牌向网关发一条 Chat Completions 请求。
pub async fn chat_request(gw: &TestGateway, token: &str, model: &str) -> reqwest::Response {
    let response = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(token)
        .json(&json!({
            "model": model,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("下游请求应能到达网关");
    wait_for_request_persistence(&gw.pool).await;
    response
}

/// mock 上游返回的合法 Chat Completions 成功体。
pub fn completion_body() -> Value {
    json!({
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hello!" },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3 }
    })
}

/// 让请求路径成功落一条日志：设置上游成功行为并发一条 Chat 请求断言 200。
pub async fn make_successful_request(gw: &mut TestGateway) {
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

/// 以管理会话认证、携带 JSON body 的 PUT 请求。
pub async fn admin_put(gw: &TestGateway, path: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!("{}{path}", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&body)
        .send()
        .await
        .expect("管理请求应可达")
}

const MS_PER_DAY: i64 = 86_400_000;

/// 播种日志时需要变化的字段；其余列用固定测试缺省。
pub struct SeededLog {
    pub created_at: i64,
    pub model: &'static str,
    pub channel: &'static str,
    pub status_code: i64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd_micros: i64,
}

/// 播种一条请求日志，字段按断言需要的口径填写。
pub async fn seed_log(pool: &sqlx::SqlitePool, log: SeededLog) {
    store::request_log::insert_request_log(
        pool,
        &store::request_log::RequestLog {
            id: 0,
            created_at: log.created_at,
            token_name: "dev".to_string(),
            token_key: TEST_TOKEN_KEY.to_string(),
            user_id: 1,
            inbound_protocol: "openai_chat".to_string(),
            model: log.model.to_string(),
            outbound_model: None,
            channel_key: None,
            channel: log.channel.to_string(),
            status_code: log.status_code,
            latency_ms: 10,
            input_tokens: log.input_tokens,
            output_tokens: log.output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_write_1h_tokens: 0,
            usage_reported: false,
            price: kairos::core::billing::PriceSnapshot::default(),
            cost_usd_micros: log.cost_usd_micros,
            base_cost_usd_micros: log.cost_usd_micros,
            discount_bp: 10_000,
            settled: true,
            request_id: None,
            billing_attempt_id: None,
            dispatched: true,
            request_body: None,
            response_body: None,
        },
    )
    .await
    .expect("应能播种请求日志");
}

/// 把 unix 毫秒格式化为 UTC 日历日（YYYY-MM-DD），与 SQLite `unixepoch` 口径一致。
pub async fn utc_date(pool: &sqlx::SqlitePool, millis: i64) -> String {
    sqlx::query_scalar("SELECT date(? / 1000, 'unixepoch')")
        .bind(millis)
        .fetch_one(pool)
        .await
        .expect("应能格式化 UTC 日期")
}

/// 播种一组跨日、含失败结算的日志，覆盖默认 7 天窗与超窗条目。
pub async fn seed_stats_logs(pool: &sqlx::SqlitePool, now: i64) -> (i64, i64, i64) {
    let today_start = now.div_euclid(MS_PER_DAY) * MS_PER_DAY;
    let yesterday = today_start - MS_PER_DAY;
    let eight_days_ago = today_start - 8 * MS_PER_DAY;

    // 今日两条成功：gpt-4o / test-channel。
    seed_log(
        pool,
        SeededLog {
            created_at: today_start + 1,
            model: TEST_MODEL,

            channel: "test-channel",
            status_code: 200,
            input_tokens: 10,
            output_tokens: 4,
            cost_usd_micros: 1_000,
        },
    )
    .await;
    seed_log(
        pool,
        SeededLog {
            created_at: today_start + 2,
            model: TEST_MODEL,

            channel: "test-channel",
            status_code: 200,
            input_tokens: 20,
            output_tokens: 8,
            cost_usd_micros: 2_000,
        },
    )
    .await;
    // 今日一条失败：已完成结算，费用应进入财务统计。
    seed_log(
        pool,
        SeededLog {
            created_at: today_start + 3,
            model: TEST_MODEL,

            channel: "test-channel",
            status_code: 500,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd_micros: 999_999,
        },
    )
    .await;
    // 昨日一条成功：另一模型/渠道，用于分布。
    seed_log(
        pool,
        SeededLog {
            created_at: yesterday + 1,
            model: "gpt-4o-mini",

            channel: "other-channel",
            status_code: 200,
            input_tokens: 5,
            output_tokens: 1,
            cost_usd_micros: 500,
        },
    )
    .await;
    // 8 天前一条成功：默认 7 天窗应排除，夹取到 90 天后应纳入。
    seed_log(
        pool,
        SeededLog {
            created_at: eight_days_ago + 1,
            model: TEST_MODEL,

            channel: "test-channel",
            status_code: 200,
            input_tokens: 1,
            output_tokens: 1,
            cost_usd_micros: 100,
        },
    )
    .await;

    (today_start, yesterday, eight_days_ago)
}
