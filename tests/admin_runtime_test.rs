//! 运行时资源生效：准入快照、进程重启重建与空库引导。

mod common;

use common::admin::{
    admin_get, admin_json, admin_put, channel_body, channel_id_by_name, chat_request,
    completion_body,
};
use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use futures_util::StreamExt;
use serde_json::{Value, json};

/// 已准入的在途请求按准入时刻快照走完：流开始后改价格，在途结算仍用旧单价，新请求用新单价。
#[tokio::test]
async fn inflight_request_keeps_admission_snapshot() {
    let mut gw = TestGateway::start_with_admin(|base| {
        let mut seed = common::test_seed(base);
        seed.tokens[0].balance_usd = 50.0;
        seed
    })
    .await;

    let text_frame = concat!(
        "data: {\"id\":\"chatcmpl-if\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n\n"
    );
    // 1M input tokens × 2.5 USD/1M = 2_500_000 micro-USD（准入时刻 gpt-4o 单价）。
    let usage_frame = concat!(
        "data: {\"id\":\"chatcmpl-if\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],",
        "\"usage\":{\"prompt_tokens\":1000000,\"completion_tokens\":0,\"total_tokens\":1000000}}\n\n"
    );
    gw.upstream.set_behavior(UpstreamBehavior::DelayedRawSse {
        chunks: vec![
            text_frame.as_bytes().to_vec(),
            usage_frame.as_bytes().to_vec(),
        ],
        delay_ms: 400,
    });

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
        .expect("在途请求应能发出");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let mut stream = resp.bytes_stream();
    let first_chunk = stream.next().await.expect("应有首块").expect("首块应可读");
    assert!(!first_chunk.is_empty(), "准入后应立即收到上游首块");

    // 准入后改价：在途结算必须仍按旧快照单价，证明持有的是准入时刻快照而非当前快照。
    let seed_channel_id = channel_id_by_name(&gw, "test-channel").await;
    let resp = admin_put(
        &gw,
        &format!("/prices/{seed_channel_id}/{TEST_MODEL}"),
        json!({
            "channel_id": seed_channel_id,
            "model": TEST_MODEL,
            "input_micros": 9_000_000,
            "output_micros": 10_000_000,
            "cache_read_micros": 1_250_000,
            "cache_write_micros": 10_000_000
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let mut rest = first_chunk.to_vec();
    while let Some(chunk) = stream.next().await {
        rest.extend_from_slice(&chunk.expect("在途流应可读"));
    }
    let body = String::from_utf8_lossy(&rest);
    assert!(body.contains("Hel"), "在途流应保留准入后的上游字节");
    assert!(body.contains("finish_reason"), "在途流不应被快照替换腰斩");

    let mut inflight_cost = None;
    for _ in 0..100 {
        inflight_cost = sqlx::query_as::<_, (i64,)>(
            "SELECT cost_usd_micros FROM request_log ORDER BY id DESC LIMIT 1",
        )
        .fetch_optional(&gw.pool)
        .await
        .expect("应能查询请求日志");
        if inflight_cost.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        inflight_cost.expect("在途请求应落结算").0,
        2_500_000,
        "在途请求应按准入时刻单价结算"
    );

    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-new", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1000000, "completion_tokens": 0, "total_tokens": 1000000}
    })));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    common::wait_for_request_persistence(&gw.pool).await;
    let new_cost: (i64,) =
        sqlx::query_as("SELECT cost_usd_micros FROM request_log ORDER BY id DESC LIMIT 1")
            .fetch_one(&gw.pool)
            .await
            .expect("新请求应落结算");
    assert_eq!(new_cost.0, 9_000_000, "新请求应按替换后单价结算");
}

/// 进程重启后从数据库加载全部运行时资源：令牌/渠道/价格/设置都不丢。
#[tokio::test]
async fn runtime_resources_survive_process_restart() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    // 经管理 API 写入：新令牌 + 充值 + 收紧 body 上限。原实例保持存活以持有临时库文件。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "restart", "balance_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建令牌");
    let restart_key = created["plaintext_key"]
        .as_str()
        .expect("应返回生成的 key")
        .to_string();
    let resp = reqwest::Client::new()
        .post(format!("{}/users/1/balance-adjustments", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({ "operation_id": "admin-balance-1", "delta_usd_micros": 5_000_000, "reason": "manual_adjustment" }))
        .send()
        .await
        .expect("应可充值");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = admin_put(
        &gw,
        "/settings",
        json!({
            "full_body": false,
            "max_request_bytes": 2048,
            "allow_private_networks": true
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 单独写入的渠道+价格：重启后必须仍能路由该模型，证明渠道/价格也从库加载。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body(
            "restart-channel",
            gw.upstream.base_url(),
            json!(["restart-only"]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let restart_channel: Value = resp.json().await.expect("应返回新建渠道");
    let restart_channel_id = restart_channel["id"]
        .as_i64()
        .expect("创建应回传库生成的 id");
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "channel_id": restart_channel_id,
            "model": "restart-only",
            "input_micros": 111_000,
            "output_micros": 222_000,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    let base2 = gw.spawn_reloaded_protocol().await;

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = reqwest::Client::new()
        .post(format!("{base2}/v1/chat/completions"))
        .bearer_auth(&restart_key)
        .json(&json!({
            "model": "restart-only",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("重启后应能请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "重启后应从库加载新令牌、渠道与价格并可用"
    );

    // 设置也从库加载：body 上限 2048 仍生效。
    let oversized_body = "x".repeat(3000);
    let resp = reqwest::Client::new()
        .post(format!("{base2}/v1/chat/completions"))
        .bearer_auth(&restart_key)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": oversized_body }]
        }))
        .send()
        .await
        .expect("重启后应能请求");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        "重启后设置上限应从库恢复"
    );
}

/// 首次部署空库：经管理 API 初始化令牌、渠道、价格后请求路径自洽可用。
#[tokio::test]
async fn empty_db_bootstraps_via_admin_api() {
    let mut gw = TestGateway::start_with_admin(common::empty_seed).await;

    let tokens: Value = admin_get(&gw, "/tokens")
        .await
        .json()
        .await
        .expect("空库令牌列表应可解析");
    assert_eq!(tokens.as_array().map(Vec::len), Some(0), "空库应无令牌");

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "boot", "balance_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建令牌");
    let boot_key = created["plaintext_key"]
        .as_str()
        .expect("应返回生成的 key")
        .to_string();

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body("boot-channel", gw.upstream.base_url(), json!([TEST_MODEL])),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let boot_channel: Value = resp.json().await.expect("应返回新建渠道");
    let boot_channel_id = boot_channel["id"].as_i64().expect("创建应回传库生成的 id");

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "channel_id": boot_channel_id,
            "model": TEST_MODEL,
            "input_micros": 2_500_000,
            "output_micros": 10_000_000,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    let resp = reqwest::Client::new()
        .post(format!("{}/users/1/balance-adjustments", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({ "operation_id": "admin-balance-2", "delta_usd_micros": 5_000_000, "reason": "manual_adjustment" }))
        .send()
        .await
        .expect("应可充值");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, &boot_key, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "空库经管理 API 初始化后请求路径应可用"
    );
}
