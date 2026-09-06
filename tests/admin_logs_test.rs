//! 日志管理面：分页过滤、脱敏、系统日志与未结算补扣/豁免。

mod common;

use common::admin::{admin_get, admin_json, chat_request, make_successful_request};
use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use kairos::store;
use serde_json::{Value, json};

/// 不存在的日志 id 返回 404。
#[tokio::test]
async fn get_log_unknown_id_is_404() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let resp = admin_get(&gw, "/logs/999999").await;
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "not_found");
}

/// 日志分页与过滤：全量、按模型过滤、分页取数正确，时间倒序。
#[tokio::test]
async fn logs_paginate_and_filter() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 生成 3 条成功请求日志。
    for _ in 0..3 {
        make_successful_request(&mut gw).await;
    }

    // 全量：total 反映日志总数，默认每页 20 条。
    let resp = client
        .get(format!("{admin}/logs"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可查日志");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 3);
    assert_eq!(page["unsettled_total"], 0);
    assert_eq!(page["items"].as_array().unwrap().len(), 3);
    assert_eq!(page["items"][0]["model"], TEST_MODEL);
    assert_eq!(
        page["items"][0]["outbound_model"], TEST_MODEL,
        "无别名时出站名等于入站名"
    );

    // 按模型过滤：命中全部 3 条。
    let resp = client
        .get(format!("{admin}/logs?model={TEST_MODEL}"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可过滤日志");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 3);
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["model"] == TEST_MODEL)
    );

    // 令牌 key 不是日志查询维度；携带明文 key 的查询应被拒绝。
    let resp = client
        .get(format!("{admin}/logs?token_key={TEST_TOKEN_KEY}"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可过滤日志");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 按令牌名精确过滤：子串不命中。
    let resp = client
        .get(format!("{admin}/logs?token_name=dev"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可按令牌名过滤");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 3);
    let resp = client
        .get(format!("{admin}/logs?token_name=de"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可按令牌名精确过滤");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 0);

    // 按渠道精确过滤：子串不命中。
    let resp = client
        .get(format!("{admin}/logs?channel=test-channel"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可按渠道过滤");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 3);
    let resp = client
        .get(format!("{admin}/logs?channel=test"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可按渠道精确过滤");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 0);

    // 综合关键字：模型子串命中全部 3 条。
    let resp = client
        .get(format!("{admin}/logs?keyword={TEST_MODEL}"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可过滤日志");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 3);

    // 综合关键字：无命中时 total 为 0。
    let resp = client
        .get(format!("{admin}/logs?keyword=no-such-keyword"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可过滤日志");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 0);

    // 分页：page_size=2 → 第一页 2 条、第二页 1 条。
    let resp = client
        .get(format!("{admin}/logs?page=1&page_size=2"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可分页");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["page_size"], 2);
    assert_eq!(page["total"], 3);

    let resp = client
        .get(format!("{admin}/logs?page=2&page_size=2"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可分页");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["items"].as_array().unwrap().len(), 1);

    // 分页 + 时间过滤：from_created_at 远在过去 → 仍命中全部。
    let resp = client
        .get(format!("{admin}/logs?from_created_at=1&page_size=2"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可过滤日志");
    let page: Value = resp.json().await.expect("日志应可解析");
    assert_eq!(page["total"], 3);

    let resp = client
        .get(format!("{admin}/logs?page_size=10"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可查日志");
    let page: Value = resp.json().await.expect("日志应可解析");
    let desc_ids: Vec<i64> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_i64().unwrap())
        .collect();

    let resp = client
        .get(format!(
            "{admin}/logs?sort_by=created&sort_dir=asc&page_size=10"
        ))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可按时间正序查日志");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let page: Value = resp.json().await.expect("日志应可解析");
    let mut asc_ids: Vec<i64> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_i64().unwrap())
        .collect();
    asc_ids.reverse();
    assert_eq!(asc_ids, desc_ids, "时间正序应是缺省倒序的逆序");
}

/// 别名请求：协议响应回显入站短名；管理日志列表=入站、出站字段=上游真名。
#[tokio::test]
async fn alias_logs_inbound_and_outbound_model() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-alias-log",
        "object": "chat.completion",
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "ok" },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })));

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "fast").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("响应应可解析");
    assert_eq!(body["model"], "fast", "协议响应 model 应回显入站名");

    let page: Value = admin_get(&gw, "/logs?page_size=1")
        .await
        .json()
        .await
        .expect("日志应可解析");
    let entry = &page["items"][0];
    assert_eq!(entry["model"], "fast", "列表字段为入站别名");
    assert_eq!(entry["outbound_model"], "gpt-4o-mini", "详情字段为出站真名");
    assert_eq!(entry["channel"], "test-channel");
}

/// GET `/logs` 对长令牌 key 按前 8 + `******` + 后 8 脱敏，短 key 也全量掩码。
#[tokio::test]
async fn logs_redact_long_token_keys() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let long_key = format!("ks-{}", "a".repeat(64));
    store::request_log::insert_request_log(
        &gw.pool,
        &store::request_log::RequestLog {
            id: 0,
            created_at: 1,
            token_name: "long".to_string(),
            token_key: long_key.clone(),
            user_id: 1,
            inbound_protocol: "openai_chat".to_string(),
            model: "m".to_string(),
            outbound_model: None,
            channel_key: None,
            channel: "c".to_string(),
            status_code: 200,
            latency_ms: 1,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_write_1h_tokens: 0,
            usage_reported: false,
            price: kairos::core::billing::PriceSnapshot::default(),
            cost_usd_micros: 0,
            base_cost_usd_micros: 0,
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
    .expect("应能写长 key 日志");

    let page: Value = admin_get(&gw, "/logs?page_size=10")
        .await
        .json()
        .await
        .expect("日志应可解析");
    let items = page["items"].as_array().expect("应有 items");
    let long_entry = items
        .iter()
        .find(|item| item["token_name"] == "long")
        .expect("应有长 key 行");
    let masked = long_entry["token_key_fingerprint"]
        .as_str()
        .expect("token_key 应为字符串");
    assert_eq!(
        masked,
        format!(
            "{}******{}",
            &long_key[..8],
            &long_key[long_key.len() - 8..]
        )
    );
    assert!(!masked.contains(&"a".repeat(20)), "中间明文不应出现");
    assert_eq!(long_entry["settled"], true);

    let short_key = "sk-short";
    store::request_log::insert_request_log(
        &gw.pool,
        &store::request_log::RequestLog {
            id: 0,
            created_at: 2,
            token_name: "short".to_string(),
            token_key: short_key.to_string(),
            user_id: 1,
            inbound_protocol: "openai_chat".to_string(),
            model: "m".to_string(),
            outbound_model: None,
            channel_key: None,
            channel: "c".to_string(),
            status_code: 200,
            latency_ms: 1,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_write_1h_tokens: 0,
            usage_reported: false,
            price: kairos::core::billing::PriceSnapshot::default(),
            cost_usd_micros: 0,
            base_cost_usd_micros: 0,
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
    .expect("应能写短 key 日志");
    let page: Value = admin_get(&gw, "/logs?page_size=10")
        .await
        .json()
        .await
        .expect("日志应可解析");
    let short_entry = page["items"]
        .as_array()
        .expect("应有 items")
        .iter()
        .find(|item| item["token_name"] == "short")
        .expect("应有短 key 行");
    assert_eq!(short_entry["token_key_fingerprint"], "******");
}

/// GET `/logs` 按 Unicode 标量掩码多字节 token_key，不会按字节切片 panic。
#[tokio::test]
async fn logs_mask_multibyte_token_keys_without_panic() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let long_key = "测".repeat(20);
    store::request_log::insert_request_log(
        &gw.pool,
        &store::request_log::RequestLog {
            id: 0,
            created_at: 1,
            token_name: "mb".to_string(),
            token_key: long_key.clone(),
            user_id: 1,
            inbound_protocol: "openai_chat".to_string(),
            model: "m".to_string(),
            outbound_model: None,
            channel_key: None,
            channel: "c".to_string(),
            status_code: 200,
            latency_ms: 1,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_write_1h_tokens: 0,
            usage_reported: false,
            price: kairos::core::billing::PriceSnapshot::default(),
            cost_usd_micros: 0,
            base_cost_usd_micros: 0,
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
    .expect("应能写多字节 key 日志");

    let page: Value = admin_get(&gw, "/logs?page_size=10")
        .await
        .json()
        .await
        .expect("日志应可解析");
    let items = page["items"].as_array().expect("应有 items");
    let entry = items
        .iter()
        .find(|item| item["token_name"] == "mb")
        .expect("应有多字节 key 行");
    let chars: Vec<char> = long_key.chars().collect();
    let expected: String = chars[..8]
        .iter()
        .chain(['*', '*', '*', '*', '*', '*'].iter())
        .chain(chars[chars.len() - 8..].iter())
        .collect();
    assert_eq!(entry["token_key_fingerprint"], expected);
}

/// GET `/logs?settled=` 过滤，且 `unsettled_total` 忽略 settled 维。
#[tokio::test]
async fn logs_filter_settled_and_report_unsettled_total() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let mut row = store::request_log::RequestLog {
        id: 0,
        created_at: 1,
        token_name: "u".to_string(),
        token_key: "sk-unsettled".to_string(),
        user_id: 1,
        inbound_protocol: "openai_chat".to_string(),
        model: "m".to_string(),
        outbound_model: None,
        channel_key: None,

        channel: "c".to_string(),
        status_code: 200,
        latency_ms: 1,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_write_1h_tokens: 0,
        usage_reported: false,
        price: kairos::core::billing::PriceSnapshot::default(),
        cost_usd_micros: 9,
        base_cost_usd_micros: 0,
        discount_bp: 10_000,
        settled: false,
        request_id: None,
        billing_attempt_id: None,
        dispatched: true,
        request_body: None,
        response_body: None,
    };
    store::request_log::insert_request_log(&gw.pool, &row)
        .await
        .expect("应能写未结算日志");
    row.created_at = 2;
    row.token_name = "s".to_string();
    row.token_key = "sk-settled".to_string();
    row.settled = true;
    store::request_log::insert_request_log(&gw.pool, &row)
        .await
        .expect("应能写已结算日志");

    let all: Value = admin_get(&gw, "/logs?page_size=10")
        .await
        .json()
        .await
        .expect("日志应可解析");
    assert_eq!(all["total"], 2);
    assert_eq!(all["unsettled_total"], 1);

    let open: Value = admin_get(&gw, "/logs?settled=false&page_size=10")
        .await
        .json()
        .await
        .expect("日志应可解析");
    assert_eq!(open["total"], 1);
    assert_eq!(open["unsettled_total"], 1);
    assert_eq!(open["items"][0]["settled"], false);
    assert_eq!(open["items"][0]["token_name"], "u");

    let closed: Value = admin_get(&gw, "/logs?settled=true&page_size=10")
        .await
        .json()
        .await
        .expect("日志应可解析");
    assert_eq!(closed["total"], 1);
    assert_eq!(closed["unsettled_total"], 1);
    assert_eq!(closed["items"][0]["settled"], true);
}

/// 未结算日志可补扣（允许透支）或豁免（不改余额）；已结算再操作 409。
#[tokio::test]
async fn unsettled_log_can_be_settled_or_waived() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let mut row = store::request_log::RequestLog {
        id: 0,
        created_at: 1,
        token_name: "dev".to_string(),
        token_key: TEST_TOKEN_KEY.to_string(),
        user_id: 1,
        inbound_protocol: "openai_chat".to_string(),
        model: "m".to_string(),
        outbound_model: None,
        channel_key: None,

        channel: "c".to_string(),
        status_code: 200,
        latency_ms: 1,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_write_1h_tokens: 0,
        usage_reported: false,
        price: kairos::core::billing::PriceSnapshot::default(),
        cost_usd_micros: 1_000_000,
        base_cost_usd_micros: 0,
        discount_bp: 10_000,
        settled: false,
        request_id: None,
        billing_attempt_id: None,
        dispatched: true,
        request_body: None,
        response_body: None,
    };
    let settle_id = store::request_log::insert_request_log(&gw.pool, &row)
        .await
        .expect("应能写待补扣日志");
    row.created_at = 2;
    let waive_id = store::request_log::insert_request_log(&gw.pool, &row)
        .await
        .expect("应能写待豁免日志");

    let before: (i64,) =
        sqlx::query_as("SELECT ub.balance_usd_micros FROM tokens t JOIN user_balance ub ON ub.user_id = t.user_id WHERE t.token_key = ?")
            .bind(common::fingerprint(TEST_TOKEN_KEY))
            .fetch_one(&gw.pool)
            .await
            .expect("应能读余额");
    assert_eq!(before.0, 5_000_000);

    let resp = reqwest::Client::new()
        .post(format!("{}/logs/{settle_id}/settle", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应能补扣");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let settled: Value = resp.json().await.expect("补扣响应应可解析");
    assert_eq!(settled["settled"], true);
    let after_settle: (i64,) =
        sqlx::query_as("SELECT ub.balance_usd_micros FROM tokens t JOIN user_balance ub ON ub.user_id = t.user_id WHERE t.token_key = ?")
            .bind(common::fingerprint(TEST_TOKEN_KEY))
            .fetch_one(&gw.pool)
            .await
            .expect("应能读余额");
    assert_eq!(after_settle.0, 4_000_000);

    let again = reqwest::Client::new()
        .post(format!("{}/logs/{settle_id}/settle", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应能再请求");
    assert_eq!(again.status(), reqwest::StatusCode::CONFLICT);

    let resp = reqwest::Client::new()
        .post(format!("{}/logs/{waive_id}/waive", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应能豁免");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let waived: Value = resp.json().await.expect("豁免响应应可解析");
    assert_eq!(waived["settled"], true);
    let after_waive: (i64,) =
        sqlx::query_as("SELECT ub.balance_usd_micros FROM tokens t JOIN user_balance ub ON ub.user_id = t.user_id WHERE t.token_key = ?")
            .bind(common::fingerprint(TEST_TOKEN_KEY))
            .fetch_one(&gw.pool)
            .await
            .expect("应能读余额");
    assert_eq!(after_waive.0, 4_000_000, "豁免不应再扣余额");
}

/// 历史补扣使用日志冻结的用户归属：令牌删除、用户归档都不能抹掉待结算费用。
#[tokio::test]
async fn unsettled_log_survives_token_deletion_and_user_archival() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    let created = admin_json(
        &gw,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "historical-debt@example.com",
            "display_name": "历史欠费",
            "password": "password1",
            "role": "user"
        }),
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let user_id = created.json::<Value>().await.expect("用户应可解析")["id"]
        .as_i64()
        .expect("应有用户 id");
    let recharge = admin_json(
        &gw,
        reqwest::Method::POST,
        &format!("/users/{user_id}/balance-adjustments"),
        json!({ "operation_id": "admin-balance-6", "delta_usd_micros": 5_000_000, "reason": "manual_adjustment" }),
    )
    .await;
    assert_eq!(recharge.status(), reqwest::StatusCode::OK);

    let login = reqwest::Client::new()
        .post(format!("{}/login", gw.admin_base_url()))
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({
            "email": "historical-debt@example.com",
            "password": "password1"
        }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session = common::session_cookie(&login);
    let token = reqwest::Client::new()
        .post(format!("{}/tokens", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({
            "name": "temporary",
            "model_group": "default",
            "enabled": true
        }))
        .send()
        .await
        .expect("令牌创建应可达");
    assert_eq!(token.status(), reqwest::StatusCode::CREATED);
    let token: Value = token.json().await.expect("令牌应可解析");
    let token_id = token["id"].as_i64().expect("应有令牌 id");
    let token_key = token["token_key_fingerprint"]
        .as_str()
        .expect("应有令牌 key");

    let mut log = store::request_log::RequestLog {
        id: 0,
        created_at: 1,
        token_name: "temporary".to_string(),
        token_key: token_key.to_string(),
        user_id,
        inbound_protocol: "openai_chat".to_string(),
        model: "m".to_string(),
        outbound_model: None,
        channel_key: None,

        channel: "c".to_string(),
        status_code: 200,
        latency_ms: 1,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_write_1h_tokens: 0,
        usage_reported: false,
        price: kairos::core::billing::PriceSnapshot::default(),
        cost_usd_micros: 1_000_000,
        base_cost_usd_micros: 0,
        discount_bp: 10_000,
        settled: false,
        request_id: None,
        billing_attempt_id: None,
        dispatched: true,
        request_body: None,
        response_body: None,
    };
    let deleted_token_log = store::request_log::insert_request_log(&gw.pool, &log)
        .await
        .expect("应能写日志");
    log.created_at = 2;
    let archived_user_log = store::request_log::insert_request_log(&gw.pool, &log)
        .await
        .expect("应能写日志");

    let deleted = reqwest::Client::new()
        .delete(format!("{}/tokens/{token_id}", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("令牌删除应可达");
    assert_eq!(deleted.status(), reqwest::StatusCode::OK);

    for log_id in [deleted_token_log, archived_user_log] {
        if log_id == archived_user_log {
            let archived = reqwest::Client::new()
                .delete(format!("{}/users/{user_id}", gw.admin_base_url()))
                .header(reqwest::header::COOKIE, &gw.session)
                .header(reqwest::header::ORIGIN, gw.admin_origin())
                .send()
                .await
                .expect("归档应可达");
            assert_eq!(archived.status(), reqwest::StatusCode::NO_CONTENT);
        }
        let settled = reqwest::Client::new()
            .post(format!("{}/logs/{log_id}/settle", gw.admin_base_url()))
            .header(reqwest::header::COOKIE, &gw.session)
            .header(reqwest::header::ORIGIN, gw.admin_origin())
            .send()
            .await
            .expect("补扣应可达");
        assert_eq!(settled.status(), reqwest::StatusCode::OK);
    }

    let wallet: (i64, i64) = sqlx::query_as(
        "SELECT balance_usd_micros, settled_usd_micros FROM user_balance WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_one(&gw.pool)
    .await
    .expect("归档钱包应保留");
    assert_eq!(wallet, (3_000_000, 2_000_000));
}

/// GET `/system-logs` 分页返回运维事件。
#[tokio::test]
async fn system_logs_list_inserted_rows() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    store::insert_system_log(&gw.pool, "error", "billing", "结算失败")
        .await
        .expect("应能写系统日志");
    store::insert_system_log(&gw.pool, "error", "catalog", "目录同步失败")
        .await
        .expect("应能写系统日志");
    store::insert_system_log(&gw.pool, "warn", "throttle", "限流触发")
        .await
        .expect("应能写系统日志");
    store::record_audit_detached(
        &gw.pool,
        None,
        "info",
        "events",
        &store::SystemLogEvent::new(
            "catalog.sync_failed",
            json!({ "error": "timeout" }),
            "目录同步失败: timeout",
        ),
    )
    .await;

    let page: Value = admin_get(&gw, "/system-logs?keyword=billing")
        .await
        .json()
        .await
        .expect("系统日志应可解析");
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["target"], "billing");
    assert_eq!(page["items"][0]["message"], "结算失败");
    assert_eq!(page["targets"], json!(["billing"]));

    let event_page: Value = admin_get(&gw, "/system-logs?target=events")
        .await
        .json()
        .await
        .expect("结构化系统日志应可解析");
    assert_eq!(event_page["items"][0]["event_code"], "catalog.sync_failed");
    assert_eq!(event_page["items"][0]["event_params"]["error"], "timeout");

    let by_level: Value = admin_get(&gw, "/system-logs?level=error")
        .await
        .json()
        .await
        .expect("系统日志应可解析");
    assert_eq!(by_level["total"], 2);
    assert_eq!(by_level["targets"], json!(["billing", "catalog"]));

    let by_warn: Value = admin_get(&gw, "/system-logs?level=warn")
        .await
        .json()
        .await
        .expect("系统日志应可解析");
    assert_eq!(by_warn["total"], 1);
    assert_eq!(by_warn["items"][0]["target"], "throttle");

    let by_target: Value = admin_get(&gw, "/system-logs?target=catalog")
        .await
        .json()
        .await
        .expect("系统日志应可解析");
    assert_eq!(by_target["total"], 1);
    assert_eq!(by_target["items"][0]["target"], "catalog");
}
