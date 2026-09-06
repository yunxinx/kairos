//! 统计管理面：/stats 聚合、参数校验与 /stats/lifetime。

mod common;

use common::admin::{admin_get, seed_stats_logs, utc_date};
use common::{TEST_MODEL, TestGateway};
use serde_json::Value;

/// `/stats` 汇总与逐日序列与播种数据精确一致；已结算失败行费用也计入。
#[tokio::test]
async fn stats_aggregates_seeded_logs_with_settled_attempt_cost() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let now = common::unix_millis();
    let (today_start, yesterday, _) = seed_stats_logs(&gw.pool, now).await;
    let today = utc_date(&gw.pool, today_start).await;
    let yesterday_date = utc_date(&gw.pool, yesterday).await;

    let resp = admin_get(&gw, "/stats").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("stats 应可解析");

    let summary = &body["summary"];
    assert_eq!(
        summary["request_count"], 4,
        "默认 7 天窗应含今日 3 + 昨日 1"
    );
    assert_eq!(summary["success_count"], 3);
    assert_eq!(summary["input_tokens"], 35);
    assert_eq!(summary["output_tokens"], 13);
    assert_eq!(
        summary["cost_usd_micros"], 1_003_499,
        "已结算失败尝试费用应计入财务统计"
    );
    assert_eq!(summary["token_count"], 1, "资源表令牌数");
    assert_eq!(summary["channel_count"], 1, "资源表渠道数");

    let daily = body["daily"].as_array().expect("应有逐日序列");
    assert_eq!(daily.len(), 7, "缺省 days=7");
    let today_point = daily.iter().find(|p| p["date"] == today).expect("应含今日");
    assert_eq!(today_point["request_count"], 3);
    assert_eq!(today_point["input_tokens"], 30);
    assert_eq!(today_point["output_tokens"], 12);
    assert_eq!(today_point["cost_usd_micros"], 1_002_999);
    let yesterday_point = daily
        .iter()
        .find(|p| p["date"] == yesterday_date)
        .expect("应含昨日");
    assert_eq!(yesterday_point["request_count"], 1);
    assert_eq!(yesterday_point["input_tokens"], 5);
    assert_eq!(yesterday_point["output_tokens"], 1);
    assert_eq!(yesterday_point["cost_usd_micros"], 500);
    let zero_days = daily.iter().filter(|p| p["request_count"] == 0).count();
    assert_eq!(zero_days, 5, "无流量的日历日应补零");

    let by_model = body["by_model"].as_array().expect("应有模型分布");
    let gpt4o = by_model
        .iter()
        .find(|p| p["model"] == TEST_MODEL)
        .expect("应有 gpt-4o");
    assert_eq!(gpt4o["request_count"], 3);
    assert_eq!(gpt4o["cost_usd_micros"], 1_002_999);
    let mini = by_model
        .iter()
        .find(|p| p["model"] == "gpt-4o-mini")
        .expect("应有 gpt-4o-mini");
    assert_eq!(mini["request_count"], 1);
    assert_eq!(mini["cost_usd_micros"], 500);

    let by_channel = body["by_channel"].as_array().expect("应有渠道分布");
    let test_ch = by_channel
        .iter()
        .find(|p| p["channel"] == "test-channel")
        .expect("应有 test-channel");
    assert_eq!(test_ch["request_count"], 3);
    assert_eq!(test_ch["cost_usd_micros"], 1_002_999);
    let other = by_channel
        .iter()
        .find(|p| p["channel"] == "other-channel")
        .expect("应有 other-channel");
    assert_eq!(other["request_count"], 1);
    assert_eq!(other["cost_usd_micros"], 500);
}

/// `days` 非法非数字 → 400；0 与超大值夹取；未知查询参数拒绝。
#[tokio::test]
async fn stats_clamps_days_and_rejects_invalid_query() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let now = common::unix_millis();
    let _ = seed_stats_logs(&gw.pool, now).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 非数字 → 400 结构化错误。
    let resp = client
        .get(format!("{admin}/stats?days=abc"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可请求 stats");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");

    // 未知查询参数 → 400。
    let resp = client
        .get(format!("{admin}/stats?dayz=7"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可请求 stats");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // days=0 夹取为 1：今日按 UTC 小时共 24 点。
    let resp = admin_get(&gw, "/stats?days=0").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("stats 应可解析");
    let daily = body["daily"].as_array().expect("应有趋势序列");
    assert_eq!(daily.len(), 24, "1 天窗应为 24 个小时桶");
    let first = daily[0]["date"].as_str().expect("应有小时标签");
    assert!(
        first.ends_with("T00:00:00Z"),
        "首个小时桶应为 UTC 0 点，实际 {first}"
    );
    assert_eq!(daily[0]["request_count"], 3, "今日 3 条都落在 0 点桶");
    assert_eq!(body["summary"]["request_count"], 3, "1 天窗只有今日 3 条");
    assert_eq!(body["summary"]["cost_usd_micros"], 1_002_999);

    // 超大值夹取为 90：8 天前那条进入窗口。
    let resp = admin_get(&gw, "/stats?days=99999").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("stats 应可解析");
    assert_eq!(body["daily"].as_array().unwrap().len(), 90);
    assert_eq!(body["summary"]["request_count"], 5);
    assert_eq!(body["summary"]["cost_usd_micros"], 1_003_599);
}

/// `/stats/lifetime` 为全量累计，含默认 7 天窗外的条目；已结算失败行费用也计入。
#[tokio::test]
async fn stats_lifetime_aggregates_all_seeded_logs() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let now = common::unix_millis();
    let _ = seed_stats_logs(&gw.pool, now).await;

    let resp = admin_get(&gw, "/stats/lifetime").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("lifetime stats 应可解析");
    assert_eq!(body["request_count"], 5, "全量应含 8 天前那条");
    assert_eq!(
        body["cost_usd_micros"], 1_003_599,
        "已结算失败尝试费用应计入财务统计"
    );
    assert_eq!(body["total_tokens"], 50);

    let windowed = admin_get(&gw, "/stats?days=7").await;
    assert_eq!(windowed.status(), reqwest::StatusCode::OK);
    let windowed_body: Value = windowed.json().await.expect("stats 应可解析");
    assert_eq!(windowed_body["summary"]["request_count"], 4);
    assert_eq!(body["request_count"], 5);
}
