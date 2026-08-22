//! 日志维护端点：体积统计与按时间窗清理（root-only，未结算行永不清理）。

mod common;

use common::{TEST_TOKEN_KEY, TestGateway};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn admin_url(gw: &TestGateway, path: &str) -> String {
    format!("{}{path}", gw.admin_base_url())
}

async fn bearer_json(
    gw: &TestGateway,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .request(method, admin_url(gw, path))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("管理请求应可达")
}

async fn bearer_get(gw: &TestGateway, token: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(admin_url(gw, path))
        .bearer_auth(token)
        .send()
        .await
        .expect("管理请求应可达")
}

/// 落一条直写请求日志行，绕过网关链路以精确控制 created_at 与 settled。
async fn seed_request_log(gw: &TestGateway, created_at: i64, settled: bool, note: &str) {
    sqlx::query(
        "INSERT INTO request_log \
         (created_at, token_name, token_key, user_id, inbound_protocol, model, channel, \
          status_code, latency_ms, cost_usd_micros, settled) \
         VALUES (?, ?, ?, 1, 'openai_chat', 'm', 'c', 200, 10, 100, ?)",
    )
    .bind(created_at)
    .bind(note)
    .bind(TEST_TOKEN_KEY)
    .bind(settled as i64)
    .execute(&gw.pool)
    .await
    .expect("应能播种请求日志");
}

/// 清理只删时间窗外的已结算行：未结算与窗口内的行原样保留；体积统计可用；
/// 清理本身留审计行；admin 被拒。
#[tokio::test]
async fn cleanup_removes_only_old_settled_rows() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    // admin 角色用于验证非 root 被拒。
    let admin = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "cleanup-admin@example.com",
            "display_name": "管理员",
            "password": "password1",
            "role": "admin"
        }),
    )
    .await;
    assert_eq!(admin.status(), StatusCode::CREATED);
    let admin_login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({
            "email": "cleanup-admin@example.com",
            "password": "password1"
        }))
        .send()
        .await
        .expect("登录应可达");
    let admin_session = admin_login.json::<Value>().await.expect("json")["token"]
        .as_str()
        .expect("token")
        .to_string();

    let now = kairos::gateway::unix_millis();
    let day = 86_400_000i64;
    // 三类行：窗外已结算（应删）、窗外未结算（必须保留）、窗内已结算（保留）。
    seed_request_log(&gw, now - 30 * day, true, "old-settled").await;
    seed_request_log(&gw, now - 30 * day, false, "old-unsettled").await;
    seed_request_log(&gw, now - day, true, "recent-settled").await;
    // 系统日志也各来一条窗外/窗内。
    for (age, note) in [(30 * day, "old-sys"), (day, "recent-sys")] {
        sqlx::query("INSERT INTO system_log (created_at, level, target, message) VALUES (?, 'info', 'test', ?)")
            .bind(now - age)
            .bind(note)
            .execute(&gw.pool)
            .await
            .expect("应能播种系统日志");
    }

    // 非 root 被拒（读与写都拦）。
    let size_as_admin = bearer_get(&gw, &admin_session, "/logs/size").await;
    assert_eq!(size_as_admin.status(), StatusCode::FORBIDDEN);
    let cleanup_as_admin = bearer_json(
        &gw,
        &admin_session,
        reqwest::Method::POST,
        "/logs/cleanup",
        json!({ "older_than_days": 7 }),
    )
    .await;
    assert_eq!(cleanup_as_admin.status(), StatusCode::FORBIDDEN);

    // 非法窗口被拒（0 与超上限同样属于误操作防护）。
    for bad_days in [0u64, 3651] {
        let rejected = bearer_json(
            &gw,
            &gw.session,
            reqwest::Method::POST,
            "/logs/cleanup",
            json!({ "older_than_days": bad_days }),
        )
        .await;
        assert_eq!(
            rejected.status(),
            StatusCode::BAD_REQUEST,
            "days={bad_days}"
        );
    }

    // root 执行 7 天窗口清理。
    let cleanup = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/logs/cleanup",
        json!({ "older_than_days": 7 }),
    )
    .await;
    assert_eq!(cleanup.status(), StatusCode::OK);
    let result: Value = cleanup.json().await.expect("json");
    assert_eq!(result["removed_request_logs"], 1, "{result}");
    assert_eq!(result["removed_system_logs"], 1, "{result}");

    // 未结算与窗口内的行仍在。
    let (remaining,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM request_log")
        .fetch_one(&gw.pool)
        .await
        .expect("应能计数");
    assert_eq!(remaining, 2, "未结算行与窗内行必须保留");
    let (unsettled,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM request_log WHERE settled = 0")
        .fetch_one(&gw.pool)
        .await
        .expect("应能计数");
    assert_eq!(unsettled, 1, "对账队列不可被清理");

    // 体积统计反映清理后行数；system_log 含测试自身的审计行，不断言绝对值。
    let size = bearer_get(&gw, &gw.session, "/logs/size").await;
    assert_eq!(size.status(), StatusCode::OK);
    let size_body: Value = size.json().await.expect("json");
    assert_eq!(size_body["request_log_rows"], 2, "{size_body}");
    assert!(
        size_body["system_log_rows"].as_u64().unwrap_or(0) >= 1,
        "{size_body}"
    );
    assert!(
        size_body["db_size_bytes"].as_u64().unwrap_or(0) > 0,
        "主库体积应为正数"
    );
    // 清理收尾把 WAL 截断为零：审计先行、checkpoint 是最后一次写。
    assert_eq!(
        size_body["wal_size_bytes"].as_u64().unwrap_or(u64::MAX),
        0,
        "清理后 WAL 边车应已截断: {size_body}"
    );
    let (old_sys,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM system_log WHERE message = 'old-sys'")
            .fetch_one(&gw.pool)
            .await
            .expect("应能计数");
    assert_eq!(old_sys, 0, "窗外系统日志应已删除");
    let (recent_sys,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM system_log WHERE message = 'recent-sys'")
            .fetch_one(&gw.pool)
            .await
            .expect("应能计数");
    assert_eq!(recent_sys, 1, "窗内系统日志应保留");

    // 清理动作本身留了审计行。
    let (audited,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM system_log WHERE target = 'logs' AND message LIKE '清理日志%'",
    )
    .fetch_one(&gw.pool)
    .await
    .expect("应能计数");
    assert_eq!(audited, 1);
}
