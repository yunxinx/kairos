//! 未出站即终局的请求不再隐形：全部渠道冷却 / 无可用密钥、本地计费拒绝、
//! 出站安全策略拒绝与准入前置失败都落一条 `dispatched = 0` 的零费用行；
//! 已派发失败仍只有逐尝试行，不重复落终局行。统计侧 `not_dispatched` 单列
//! 计数，不并入 `request_count` 等出站口径指标。

mod common;

use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn admin_url(gw: &TestGateway, path: &str) -> String {
    format!("{}{path}", gw.admin_base_url())
}

async fn admin_json(
    gw: &TestGateway,
    method: reqwest::Method,
    path: &str,
    body: Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .request(method, admin_url(gw, path))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&body)
        .send()
        .await
        .expect("管理请求应可达")
}

async fn admin_get(gw: &TestGateway, path: &str) -> Value {
    let resp = reqwest::Client::new()
        .get(admin_url(gw, path))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("管理请求应可达");
    assert_eq!(resp.status(), StatusCode::OK);
    resp.json().await.expect("管理响应应可解析")
}

async fn send_completion(gw: &TestGateway, model: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": model,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("应能请求网关")
}

/// 请求日志的 `(status_code, channel, dispatched)` 形态，按 id 升序。
async fn log_rows(gw: &TestGateway) -> Vec<(i64, String, bool)> {
    common::wait_for_request_persistence(&gw.pool).await;
    let rows: Vec<(i64, String, i64)> =
        sqlx::query_as("SELECT status_code, channel, dispatched FROM request_log ORDER BY id")
            .fetch_all(&gw.pool)
            .await
            .expect("应能查询请求日志");
    rows.into_iter()
        .map(|(status, channel, d)| (status, channel, d != 0))
        .collect()
}

/// 零余额的本地计费拒绝（402）从未出站：落一条 dispatched=0 的零费用行，
/// 统计单列 not_dispatched，不并入出站请求口径。
#[tokio::test]
async fn billing_denied_402_leaves_undispatched_log_row() {
    let gw = TestGateway::start_with_admin(|base| {
        let mut seed = common::test_seed(base);
        seed.tokens[0].balance_usd = 0.0;
        seed
    })
    .await;

    let resp = send_completion(&gw, TEST_MODEL).await;
    assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);

    assert_eq!(
        log_rows(&gw).await,
        vec![(402, String::new(), false)],
        "计费拒绝应落一条无渠道、未出站的失败行"
    );

    // 管理面日志列表暴露 dispatched 布尔，供 UI 呈现「未出站」徽标。
    let page = admin_get(&gw, "/logs").await;
    let item = &page["items"][0];
    assert_eq!(item["status_code"], 402);
    assert_eq!(item["dispatched"], false, "列表应暴露未出站标记");

    let stats = admin_get(&gw, "/stats").await;
    assert_eq!(
        stats["summary"]["request_count"], 0,
        "未出站请求不进入出站请求口径"
    );
    assert_eq!(stats["summary"]["not_dispatched"], 1);
    let lifetime = admin_get(&gw, "/stats/lifetime").await;
    assert_eq!(lifetime["request_count"], 0);
}

/// 上游 402 冷却唯一渠道后，后续请求在没有任何出站尝试的情况下以 502 终局：
/// 落 dispatched=0 行；此前已派发的 402 尝试行保持 dispatched=1。
#[tokio::test]
async fn cooled_out_channel_502_leaves_undispatched_log_row() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Status(402));

    let first = send_completion(&gw, TEST_MODEL).await;
    assert_eq!(first.status(), StatusCode::PAYMENT_REQUIRED);
    assert_eq!(gw.upstream.received().len(), 1, "首次请求应出站");

    // 渠道已被上游 402 冷却：本次请求不会再建立任何上游连接。
    let second = send_completion(&gw, TEST_MODEL).await;
    assert_eq!(second.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(gw.upstream.received().len(), 1, "冷却中的渠道不应再次出站");

    assert_eq!(
        log_rows(&gw).await,
        vec![
            (402, "test-channel".to_string(), true),
            (502, String::new(), false)
        ],
        "已派发 402 尝试行保持出站标记；冷却终局落未出站行"
    );

    let stats = admin_get(&gw, "/stats").await;
    assert_eq!(stats["summary"]["request_count"], 1, "只有首次请求出过站");
    assert_eq!(stats["summary"]["not_dispatched"], 1);
}

/// 出站安全策略拒绝（私网目标）从未建立上游连接：落 dispatched=0 行。
#[tokio::test]
async fn network_policy_rejection_leaves_undispatched_log_row() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let disabled = admin_json(
        &gw,
        reqwest::Method::PUT,
        "/settings",
        json!({
            "full_body": false,
            "max_request_bytes": 1_000_000,
            "allow_private_networks": false
        }),
    )
    .await;
    assert_eq!(disabled.status(), StatusCode::OK);

    let resp = send_completion(&gw, TEST_MODEL).await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert!(
        gw.upstream.received().is_empty(),
        "策略拒绝不应建立上游连接"
    );

    assert_eq!(
        log_rows(&gw).await,
        vec![(502, String::new(), false)],
        "安全策略拒绝应落未出站失败行"
    );
    let stats = admin_get(&gw, "/stats").await;
    assert_eq!(stats["summary"]["request_count"], 0);
    assert_eq!(stats["summary"]["not_dispatched"], 1);
}

/// 准入前置失败（模型无任何可路由渠道）同样按未出站终局落行。
#[tokio::test]
async fn unroutable_model_503_leaves_undispatched_log_row() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let resp = send_completion(&gw, "no-such-model").await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    assert_eq!(
        log_rows(&gw).await,
        vec![(503, String::new(), false)],
        "无路由模型应落未出站失败行"
    );
    let stats = admin_get(&gw, "/stats").await;
    assert_eq!(stats["summary"]["request_count"], 0);
    assert_eq!(stats["summary"]["not_dispatched"], 1);
}

/// 已派发的失败只有逐尝试行：耗尽候选后的终局响应不重复落 dispatched=0 行。
#[tokio::test]
async fn dispatched_failure_does_not_duplicate_terminal_row() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Status5xx(500));

    let resp = send_completion(&gw, TEST_MODEL).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(gw.upstream.received().len(), 1, "失败尝试已出站");

    assert_eq!(
        log_rows(&gw).await,
        vec![(500, "test-channel".to_string(), true)],
        "已派发失败不追加未出站终局行"
    );
    let stats = admin_get(&gw, "/stats").await;
    assert_eq!(stats["summary"]["request_count"], 1);
    assert_eq!(stats["summary"]["not_dispatched"], 0);
}
