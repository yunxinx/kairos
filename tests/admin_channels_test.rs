//! 渠道管理面：定义/价格/密钥/别名冲突、级联与健康冷却。

mod common;

use common::admin::{
    admin_get, admin_json, admin_put, channel_body, channel_id_by_name, chat_request,
    completion_body,
};
use common::{TEST_MODEL, TEST_TOKEN_KEY, TestGateway, UpstreamBehavior};
use serde_json::{Value, json};

/// 渠道与价格写后即时生效：渠道可路由、价格增减即时反映在计费准入。
#[tokio::test]
async fn channel_and_price_immediate_effect() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();

    // 新建一个指向 mock 上游、服务新模型的渠道。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body(
            "mini-channel",
            gw.upstream.base_url(),
            json!(["gpt-4o-mini"]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let mini_id = created["id"].as_i64().expect("创建应回传库生成的 id");

    // 渠道已建但无价格：请求被计费准入拒绝（503）。
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o-mini").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "有渠道但无价格应 503"
    );

    // 补上价格：请求立即可用。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "channel_id": mini_id,
            "model": "gpt-4o-mini",
            "input_micros": 150_000,
            "output_micros": 600_000,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o-mini").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "有价格后请求应立即可用"
    );

    // 禁用渠道：模型失去可用候选，请求 503（与无渠道同等处理）。
    let mut disabled = channel_body(
        "mini-channel",
        gw.upstream.base_url(),
        json!(["gpt-4o-mini"]),
    );
    disabled["enabled"] = json!(false);
    let resp = admin_put(&gw, &format!("/channels/{mini_id}"), disabled).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let updated: Value = resp.json().await.expect("应返回变更后渠道");
    assert_eq!(updated["enabled"], false, "PUT 回显应反映禁用");
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o-mini").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "禁用的渠道不应参与路由"
    );

    // 重新启用后立即可用。
    let resp = admin_put(
        &gw,
        &format!("/channels/{mini_id}"),
        channel_body(
            "mini-channel",
            gw.upstream.base_url(),
            json!(["gpt-4o-mini"]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o-mini").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "重新启用的渠道应立即可用"
    );

    // 删除价格：请求再次 503（无价格）。
    let resp = client
        .delete(format!(
            "{}/prices/{mini_id}/gpt-4o-mini",
            gw.admin_base_url()
        ))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可删除价格");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o-mini").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "删除价格后应恢复无价格拒绝"
    );

    // 删除渠道：模型失去路由，请求 503（无渠道）。
    let resp = client
        .delete(format!("{}/channels/{mini_id}", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可删除渠道");
    let status = resp.status();
    let body = resp.text().await.expect("删除渠道响应应可读取");
    assert_eq!(status, reqwest::StatusCode::OK, "删除渠道失败: {body}");
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "gpt-4o-mini").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "删除渠道后模型应无路由"
    );
}

/// 同一可调用名在两条渠道上各自定价；改其一不影响另一条，结算按实际打到的渠道。
#[tokio::test]
async fn same_model_prices_are_per_channel() {
    let mut gw = TestGateway::start_with_admin(|base| {
        let mut seed = common::test_seed(base);
        seed.tokens[0].balance_usd = 50.0;
        seed
    })
    .await;
    let left_id = channel_id_by_name(&gw, "test-channel").await;

    let right = channel_body("other-channel", gw.upstream.base_url(), json!([TEST_MODEL]));
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", right).await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let right_id = created["id"].as_i64().expect("创建应回传库生成的 id");

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "channel_id": right_id,
            "model": TEST_MODEL,
            "input_micros": 9_000_000,
            "output_micros": 0,
            "cache_read_micros": null,
            "cache_write_micros": null,
            "cache_write_1h_micros": 20_000_000
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    let resp = admin_put(
        &gw,
        &format!("/prices/{left_id}/{TEST_MODEL}"),
        json!({
            "channel_id": left_id,
            "model": TEST_MODEL,
            "input_micros": 1_000_000,
            "output_micros": 0,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let listed: Value = admin_get(&gw, "/prices")
        .await
        .json()
        .await
        .expect("价格列表应可解析");
    let prices = listed.as_array().expect("价格列表应为数组");
    let left_price = prices
        .iter()
        .find(|price| price["channel_id"] == left_id && price["model"] == TEST_MODEL)
        .expect("左渠道价格应在");
    let right_price = prices
        .iter()
        .find(|price| price["channel_id"] == right_id && price["model"] == TEST_MODEL)
        .expect("右渠道价格应在");
    assert_eq!(left_price["input_micros"], 1_000_000);
    assert_eq!(right_price["input_micros"], 9_000_000);
    // 1h 档按行独立：配置了即回读，未配置保持 null（整行单一费率）。
    assert_eq!(right_price["cache_write_1h_micros"], 20_000_000);
    assert_eq!(left_price["cache_write_1h_micros"], serde_json::Value::Null);

    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-per-ch", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1000000, "completion_tokens": 0, "total_tokens": 1000000}
    })));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let left_cost: (i64,) =
        sqlx::query_as("SELECT cost_usd_micros FROM request_log ORDER BY id DESC LIMIT 1")
            .fetch_one(&gw.pool)
            .await
            .expect("应落结算");
    assert_eq!(left_cost.0, 1_000_000, "优先渠道成功应按其单价结算");

    let mut disabled = channel_body("test-channel", gw.upstream.base_url(), json!([TEST_MODEL]));
    disabled["enabled"] = json!(false);
    let resp = admin_put(&gw, &format!("/channels/{left_id}"), disabled).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-per-ch-b", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1000000, "completion_tokens": 0, "total_tokens": 1000000}
    })));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let right_cost: (i64,) =
        sqlx::query_as("SELECT cost_usd_micros FROM request_log ORDER BY id DESC LIMIT 1")
            .fetch_one(&gw.pool)
            .await
            .expect("应落结算");
    assert_eq!(right_cost.0, 9_000_000, "切到右渠道后应按其单价结算");
}

/// 读当前价格列表。
async fn listed_prices(gw: &TestGateway) -> Value {
    admin_get(gw, "/prices")
        .await
        .json()
        .await
        .expect("价格列表应可解析")
}

/// 价格列表是否含该 (渠道, 模型) 行。
fn prices_contain(listed: &Value, channel_id: i64, model: &str) -> bool {
    listed
        .as_array()
        .expect("价格列表应为数组")
        .iter()
        .any(|price| price["channel_id"] == channel_id && price["model"] == model)
}

/// 未在该渠道清单或别名中登记的名字不能定价：POST/PUT 均为 400，列表不变。
#[tokio::test]
async fn price_for_unlisted_callable_is_400() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = channel_id_by_name(&gw, "test-channel").await;
    let before = listed_prices(&gw).await;
    let body = json!({
        "channel_id": channel_id,
        "model": "not-on-channel",
        "input_micros": 1_000_000,
        "output_micros": 0,
        "cache_read_micros": null,
        "cache_write_micros": null
    });

    let resp = admin_json(&gw, reqwest::Method::POST, "/prices", body.clone()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let error: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(error["error"]["code"], "invalid_body");
    let message = error["error"]["message"].as_str().expect("消息应为字符串");
    assert!(
        message.contains("清单") || message.contains("别名"),
        "应说明未在该渠道登记，实际 {message}"
    );
    assert!(
        message.contains("not-on-channel"),
        "应点名未登记的模型，实际 {message}"
    );
    assert_eq!(listed_prices(&gw).await, before, "拒绝写入后价格列表应不变");
    assert!(
        !prices_contain(&before, channel_id, "not-on-channel"),
        "播种不应含未登记名的价格"
    );

    let resp = admin_put(&gw, &format!("/prices/{channel_id}/not-on-channel"), body).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let error: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(error["error"]["code"], "invalid_body");
    assert_eq!(listed_prices(&gw).await, before, "PUT 拒绝后价格列表应不变");
}

/// 同一 (渠道, 模型) 再 POST 冲突；播种已有 test-channel / gpt-4o。
#[tokio::test]
async fn posting_existing_channel_model_price_is_409() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = channel_id_by_name(&gw, "test-channel").await;
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "channel_id": channel_id,
            "model": TEST_MODEL,
            "input_micros": 1_000_000,
            "output_micros": 0,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let error: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(error["error"]["code"], "conflict");
}

/// 渠道 PUT 拿掉已登记名时保留该渠道价格；另一渠道同名价格仍在。
#[tokio::test]
async fn dropping_listed_name_retains_channel_prices() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let left_id = channel_id_by_name(&gw, "test-channel").await;

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body("other-channel", gw.upstream.base_url(), json!([TEST_MODEL])),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let right_id = created["id"].as_i64().expect("创建应回传库生成的 id");

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "channel_id": right_id,
            "model": TEST_MODEL,
            "input_micros": 9_000_000,
            "output_micros": 0,
            "cache_read_micros": null,
            "cache_write_micros": null,
            "cache_write_1h_micros": 20_000_000
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    let resp = admin_put(
        &gw,
        &format!("/channels/{left_id}"),
        channel_body(
            "test-channel",
            gw.upstream.base_url(),
            json!(["kept-unrelated"]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let listed = listed_prices(&gw).await;
    assert!(
        prices_contain(&listed, left_id, TEST_MODEL),
        "拿掉清单名后该渠道价格仍应保留"
    );
    assert!(
        prices_contain(&listed, right_id, TEST_MODEL),
        "另一渠道同名价格应仍在"
    );
}

/// 删除渠道级联删掉其价格行。
#[tokio::test]
async fn deleting_channel_cascades_its_prices() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body(
            "cascade-channel",
            gw.upstream.base_url(),
            json!(["cascade-model"]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let channel_id = created["id"].as_i64().expect("创建应回传库生成的 id");

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "channel_id": channel_id,
            "model": "cascade-model",
            "input_micros": 1_000_000,
            "output_micros": 0,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    assert!(prices_contain(
        &listed_prices(&gw).await,
        channel_id,
        "cascade-model"
    ));

    let resp = reqwest::Client::new()
        .delete(format!("{}/channels/{channel_id}", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可删除渠道");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let listed = listed_prices(&gw).await;
    let leftover = listed
        .as_array()
        .expect("价格列表应为数组")
        .iter()
        .any(|price| price["channel_id"] == channel_id);
    assert!(!leftover, "删除渠道后不应残留该渠道的价格行");
}

/// 同名两渠道仅低优先级有价：跳过未定价渠道，按有价渠道结算，不 503。
#[tokio::test]
async fn unpriced_sibling_is_skipped_not_503() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let seed_id = channel_id_by_name(&gw, "test-channel").await;
    let resp = reqwest::Client::new()
        .delete(format!(
            "{}/prices/{seed_id}/{TEST_MODEL}",
            gw.admin_base_url()
        ))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可删除播种价格");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let sibling = channel_body(
        "priced-sibling",
        gw.upstream.base_url(),
        json!([TEST_MODEL]),
    );
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", sibling).await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let sibling_id = created["id"].as_i64().expect("创建应回传库生成的 id");

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "channel_id": sibling_id,
            "model": TEST_MODEL,
            "input_micros": 3_000_000,
            "output_micros": 0,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    gw.upstream.set_behavior(UpstreamBehavior::Json(json!({
        "id": "chatcmpl-skip-unpriced", "object": "chat.completion", "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "logprobs": null, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1_000_000, "completion_tokens": 0, "total_tokens": 1_000_000}
    })));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "有价渠道在后也应跳过未定价 hop，不得 503"
    );

    common::wait_for_request_persistence(&gw.pool).await;

    let log: (i64, String) =
        sqlx::query_as("SELECT cost_usd_micros, channel FROM request_log ORDER BY id DESC LIMIT 1")
            .fetch_one(&gw.pool)
            .await
            .expect("应落结算");
    assert_eq!(
        log.0, 3_000_000,
        "1M prompt 费用应等于有价渠道 input_micros"
    );
    assert_eq!(log.1, "priced-sibling");
}

/// 渠道 PUT 追加模型 ID（对应编辑器手动添加并保存）：保存前该 ID 不可路由；
/// 保存后未定价仍 503；补价后可调。
#[tokio::test]
async fn channel_appended_model_unpriced_is_503_then_callable() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body("manual-add", gw.upstream.base_url(), json!(["gpt-4o-mini"])),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let channel_id = created["id"].as_i64().expect("创建应回传库生成的 id");

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "manual-only").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "未写入渠道的模型应 503"
    );
    let body: Value = resp.json().await.expect("503 响应应可解析");
    let msg = body["error"]["message"].as_str().expect("消息应为字符串");
    assert!(msg.contains("渠道"), "保存前应按无渠道拒绝，实际 {msg}");

    let resp = admin_put(
        &gw,
        &format!("/channels/{channel_id}"),
        channel_body(
            "manual-add",
            gw.upstream.base_url(),
            json!(["gpt-4o-mini", "manual-only"]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let resp = chat_request(&gw, TEST_TOKEN_KEY, "manual-only").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "有渠道但无价格应 503"
    );
    let body: Value = resp.json().await.expect("503 响应应可解析");
    let msg = body["error"]["message"].as_str().expect("消息应为字符串");
    assert!(
        msg.contains("价格"),
        "保存后未定价应按无价格拒绝，实际 {msg}"
    );

    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "channel_id": channel_id,
            "model": "manual-only",
            "input_micros": 150_000,
            "output_micros": 600_000,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, "manual-only").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "定价后请求应立即可用"
    );
}

/// 渠道改名：按 id 定位的 PUT 携带新 name 即改名，id 保持稳定、即时可路由；
/// 新名已被占用返回 409，id 不存在返回 404。
#[tokio::test]
async fn channel_rename_moves_definition() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    // 先按名查出 seed 渠道的 id。
    let list: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let seed_channel = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "test-channel")
        .expect("seed 渠道应在列表")
        .clone();
    let channel_id = seed_channel["id"].as_i64().expect("列表应回传库生成的 id");

    // 改名 test-channel → renamed-channel：回显为新名，id 保持不变。
    let resp = admin_put(
        &gw,
        &format!("/channels/{channel_id}"),
        channel_body(
            "renamed-channel",
            gw.upstream.base_url(),
            json!([TEST_MODEL]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let updated: Value = resp.json().await.expect("应返回变更后渠道");
    assert_eq!(updated["name"], "renamed-channel", "PUT 回显应为新名");
    assert_eq!(updated["id"], channel_id, "改名不应改变 id");

    // 列表中新名在、旧名消失；改名后立即可路由。
    let list: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"renamed-channel"), "新名应在列表");
    assert!(!names.contains(&"test-channel"), "旧名应被移除");
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "改名后模型应立即可路由"
    );

    // 改成已存在的名字 → 409，且不产生副作用。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        channel_body(
            "second-channel",
            gw.upstream.base_url(),
            json!(["other-model"]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let resp = admin_put(
        &gw,
        &format!("/channels/{channel_id}"),
        channel_body(
            "second-channel",
            gw.upstream.base_url(),
            json!([TEST_MODEL]),
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "冲突改名不应影响原渠道"
    );

    // id 不存在 → 404。
    let resp = admin_put(
        &gw,
        "/channels/999999",
        channel_body("whatever", gw.upstream.base_url(), json!([TEST_MODEL])),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

/// 非法输入返回结构化错误；失败与冲突的写不污染库与快照。
#[tokio::test]
async fn invalid_input_returns_structured_error_and_leaves_state() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let client = reqwest::Client::new();
    let admin = gw.admin_base_url();

    // 畸形 JSON body → 400 结构化错误。
    let resp = client
        .post(format!("{admin}/tokens"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .header("content-type", "application/json")
        .body("{ not json")
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");

    // 缺必填字段 → 400（serde 拒绝）。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "x" }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 指定 token_key → 400：key 只由系统生成，创建契约不接受该字段。
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/tokens",
        json!({ "token_key": "ks-custom", "name": "x", "balance_usd_micros": null, "enabled": true }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 负单价 → 400（语义校验）。
    let seed_channel_id = channel_id_by_name(&gw, "test-channel").await;
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/prices",
        json!({
            "channel_id": seed_channel_id,
            "model": "m-neg",
            "input_micros": -1,
            "output_micros": 0,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 超时低于下限 → 400：1000ms 以下的超时连常规握手都容不下，只会把每次
    // 调用变成确定性失败。
    let mut tiny_timeouts =
        channel_body("tiny-timeouts", gw.upstream.base_url(), json!([TEST_MODEL]));
    tiny_timeouts["timeout_ms"] = json!(999);
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", tiny_timeouts).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    let mut tiny_request_timeout = channel_body(
        "tiny-request-timeout",
        gw.upstream.base_url(),
        json!([TEST_MODEL]),
    );
    tiny_request_timeout["request_timeout_ms"] = json!(999);
    let resp = admin_json(
        &gw,
        reqwest::Method::POST,
        "/channels",
        tiny_request_timeout,
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 旧的优先级/权重字段不再属于写契约，deny_unknown_fields 必须直接拒绝。
    let mut legacy_routing = channel_body(
        "legacy-routing",
        gw.upstream.base_url(),
        json!([TEST_MODEL]),
    );
    legacy_routing["priority"] = json!(1);
    legacy_routing["weight"] = json!(1);
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", legacy_routing).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // 重复新建 → 409，且不覆盖原资源（令牌 key 系统生成、创建不会冲突，以渠道为例）。
    let before: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let mut conflict = channel_body("test-channel", gw.upstream.base_url(), json!([TEST_MODEL]));
    conflict["keys"][0]["api_key"] = json!("sk-other");
    conflict["timeout_ms"] = json!(1000);
    conflict["max_retries"] = json!(4);
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", conflict).await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let after: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    assert_eq!(before, after, "冲突写不应改变库与快照");

    // 同一渠道内密钥名（按保存时 trim 后）必须唯一，避免日志无法辨识实际凭据。
    let mut duplicate_keys = channel_body(
        "duplicate-key-names",
        gw.upstream.base_url(),
        json!([TEST_MODEL]),
    );
    duplicate_keys["keys"] = json!([
        { "name": "primary", "api_key": "sk-a", "weight": 1, "enabled": true },
        { "name": " primary ", "api_key": "sk-b", "weight": 1, "enabled": true }
    ]);
    let duplicate_response =
        admin_json(&gw, reqwest::Method::POST, "/channels", duplicate_keys).await;
    assert_eq!(duplicate_response.status(), reqwest::StatusCode::CONFLICT);

    // 删除不存在的资源 → 404。
    let resp = client
        .delete(format!("{admin}/tokens/does-not-exist"))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("应可请求管理面");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

/// 未知字段直接拒绝（deny_unknown_fields）：字段拼写错误不静默忽略。
#[tokio::test]
async fn unknown_field_is_rejected() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let mut typo = channel_body("typo-channel", gw.upstream.base_url(), json!([]));
    typo["protcol"] = json!("openai_chat");
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", typo).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "invalid_body");
}

/// 读取面密钥掩码：渠道列表与创建/更新/删除响应一律不回显明文密钥。
#[tokio::test]
async fn channel_key_read_responses_are_masked() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let list: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let seeded = &list[0]["keys"][0];
    assert_eq!(seeded["api_key"], json!("******"), "短密钥读取面应完全掩码");
    let body_text = serde_json::to_string(&list).expect("响应应可序列化");
    assert!(
        !body_text.contains("sk-upstream"),
        "渠道读取面响应不得包含明文密钥: {body_text}"
    );

    // 创建响应同样掩码：长密钥保留前后 8 字符，中段不出现。
    let mut created_body = channel_body("masked-read", gw.upstream.base_url(), json!([TEST_MODEL]));
    created_body["keys"][0]["api_key"] = json!("sk-ant-api03-1234567890abcdef-secret-tail");
    let created = admin_json(&gw, reqwest::Method::POST, "/channels", created_body).await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let view: Value = created.json().await.expect("创建响应应可解析");
    assert_eq!(
        view["keys"][0]["api_key"],
        json!("sk-ant-a******ret-tail"),
        "长密钥应保留前后 8 字符并以 * 作掩码"
    );

    // 更新与删除响应同样掩码。
    let id = channel_id_by_name(&gw, "masked-read").await;
    let mut update = channel_body("masked-read", gw.upstream.base_url(), json!([TEST_MODEL]));
    update["keys"][0]["api_key"] = json!("sk-replaced-plaintext-key-xyz");
    let updated = admin_put(&gw, &format!("/channels/{id}"), update).await;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    let view: Value = updated.json().await.expect("更新响应应可解析");
    assert!(
        !serde_json::to_string(&view)
            .expect("响应应可序列化")
            .contains("sk-replaced-plaintext-key-xyz"),
        "更新响应不得回显刚写入的明文"
    );
}

/// 更新渠道时空串或掩码串按 name 保留库中原值；新明文照常替换。
#[tokio::test]
async fn channel_update_preserves_key_when_masked_or_empty() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;
    let id = channel_id_by_name(&gw, "test-channel").await;

    async fn stored_api_key(pool: &sqlx::SqlitePool, channel_id: i64, name: &str) -> String {
        sqlx::query_scalar("SELECT api_key FROM channel_keys WHERE channel_id = ? AND name = ?")
            .bind(channel_id)
            .bind(name)
            .fetch_one(pool)
            .await
            .expect("应能查询渠道密钥")
    }

    // 掩码串 + 同名条目：保留原值。
    let mut masked = channel_body("test-channel", gw.upstream.base_url(), json!([TEST_MODEL]));
    masked["keys"][0]["api_key"] = json!("******");
    let resp = admin_put(&gw, &format!("/channels/{id}"), masked).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        stored_api_key(&gw.pool, id, "default").await,
        "sk-upstream",
        "掩码串应保留库中原值"
    );

    // 空串 + 同名条目：保留原值。
    let mut empty = channel_body("test-channel", gw.upstream.base_url(), json!([TEST_MODEL]));
    empty["keys"][0]["api_key"] = json!("");
    let resp = admin_put(&gw, &format!("/channels/{id}"), empty).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        stored_api_key(&gw.pool, id, "default").await,
        "sk-upstream",
        "空串应保留库中原值"
    );

    // 新明文：照常替换。
    let mut replaced = channel_body("test-channel", gw.upstream.base_url(), json!([TEST_MODEL]));
    replaced["keys"][0]["api_key"] = json!("sk-brand-new");
    let resp = admin_put(&gw, &format!("/channels/{id}"), replaced).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        stored_api_key(&gw.pool, id, "default").await,
        "sk-brand-new",
        "新明文应替换原值"
    );

    // 同名多条目各自匹配：一条掩码保留、一条空串保留、一条明文替换。
    let mut multi = channel_body("test-channel", gw.upstream.base_url(), json!([TEST_MODEL]));
    multi["keys"] = json!([
        { "name": "default", "api_key": "******", "weight": 1, "enabled": true },
        { "name": "second", "api_key": "", "weight": 1, "enabled": true },
        { "name": "third", "api_key": "sk-third-plain", "weight": 1, "enabled": true }
    ]);
    let resp = admin_put(&gw, &format!("/channels/{id}"), multi).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "second 无原值可保留，应校验失败"
    );

    // 先补上 second 的明文，再验证同名多条目各自匹配。
    let mut multi = channel_body("test-channel", gw.upstream.base_url(), json!([TEST_MODEL]));
    multi["keys"] = json!([
        { "name": "default", "api_key": "sk-brand-new", "weight": 1, "enabled": true },
        { "name": "second", "api_key": "sk-second-plain", "weight": 1, "enabled": true }
    ]);
    let resp = admin_put(&gw, &format!("/channels/{id}"), multi).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let mut masked = channel_body("test-channel", gw.upstream.base_url(), json!([TEST_MODEL]));
    masked["keys"] = json!([
        { "name": "default", "api_key": "******", "weight": 1, "enabled": true },
        { "name": "second", "api_key": "", "weight": 1, "enabled": true }
    ]);
    let resp = admin_put(&gw, &format!("/channels/{id}"), masked).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        stored_api_key(&gw.pool, id, "default").await,
        "sk-brand-new"
    );
    assert_eq!(
        stored_api_key(&gw.pool, id, "second").await,
        "sk-second-plain"
    );

    // 保留原值后出站认证仍使用库中原值：mock 上游收到替换后的明文。
    // second 停用，出站密钥选择确定落在 default 上。
    let mut masked = channel_body("test-channel", gw.upstream.base_url(), json!([TEST_MODEL]));
    masked["keys"] = json!([
        { "name": "default", "api_key": "******", "weight": 1, "enabled": true },
        { "name": "second", "api_key": "", "weight": 1, "enabled": false }
    ]);
    let resp = admin_put(&gw, &format!("/channels/{id}"), masked).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        gw.upstream.received_api_keys().last().cloned().flatten(),
        Some("Bearer sk-brand-new".to_string()),
        "出站认证应使用库中保留的原值"
    );
}

/// 更新时空串/掩码串的 name 无匹配（无原值可保留）→ 校验错误；
/// 创建渠道仍要求明文，掩码形态一律拒绝。
#[tokio::test]
async fn channel_placeholder_keys_without_match_are_rejected() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let id = channel_id_by_name(&gw, "test-channel").await;

    // 更新：掩码串的 name 在当前渠道上不存在 → 400「密钥 api_key 不能为空」。
    let mut unmatched = channel_body("test-channel", gw.upstream.base_url(), json!([TEST_MODEL]));
    unmatched["keys"][0]["name"] = json!("ghost");
    unmatched["keys"][0]["api_key"] = json!("******");
    let resp = admin_put(&gw, &format!("/channels/{id}"), unmatched).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["message"], json!("密钥 api_key 不能为空"));

    // 创建：掩码形态没有「原值」语义，一律要求明文。
    let mut masked_create =
        channel_body("masked-create", gw.upstream.base_url(), json!([TEST_MODEL]));
    masked_create["keys"][0]["api_key"] = json!("******");
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", masked_create).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    let mut empty_create =
        channel_body("masked-create", gw.upstream.base_url(), json!([TEST_MODEL]));
    empty_create["keys"][0]["api_key"] = json!("");
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", empty_create).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// 两条启用渠道将同一别名指到不同真名：创建/更新 409，文案提示用统一模型。
/// 指向同一真名允许；禁用渠道不参与冲突，启用时再拦。
#[tokio::test]
async fn enabled_channels_reject_divergent_alias_values() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    // seed 渠道已有 fast → gpt-4o-mini。

    let mut divergent = channel_body("divergent", gw.upstream.base_url(), json!([TEST_MODEL]));
    divergent["model_aliases"] = json!({ "fast": TEST_MODEL });
    let before: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", divergent.clone()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: Value = resp.json().await.expect("冲突体应可解析");
    assert_eq!(body["error"]["code"], "conflict");
    let message = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("统一模型"),
        "冲突文案应提示用统一模型，实际: {message}"
    );
    assert!(
        message.contains("fast"),
        "冲突文案应点名别名 key，实际: {message}"
    );
    let after: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    assert_eq!(before, after, "别名冲突写不应改变库与快照");

    let mut same_value = channel_body("same-alias", gw.upstream.base_url(), json!([TEST_MODEL]));
    same_value["model_aliases"] = json!({ "fast": "gpt-4o-mini" });
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", same_value.clone()).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "同一别名指向同一真名应允许（多渠道 failover）"
    );
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let same_id = created["id"].as_i64().expect("创建应回传整数 id");

    let mut disabled = channel_body(
        "disabled-divergent",
        gw.upstream.base_url(),
        json!([TEST_MODEL]),
    );
    disabled["model_aliases"] = json!({ "fast": TEST_MODEL });
    disabled["enabled"] = json!(false);
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", disabled.clone()).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "禁用渠道不参与别名冲突"
    );
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let disabled_id = created["id"].as_i64().expect("创建应回传整数 id");

    disabled["enabled"] = json!(true);
    let resp = admin_put(&gw, &format!("/channels/{disabled_id}"), disabled).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "启用后与已有别名冲突应拒绝"
    );
    let listed: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let still_disabled = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == disabled_id)
        .expect("禁用渠道应仍在列表");
    assert_eq!(still_disabled["enabled"], false, "冲突启用不应落地");

    same_value["model_aliases"] = json!({ "fast": TEST_MODEL });
    let resp = admin_put(&gw, &format!("/channels/{same_id}"), same_value).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "更新为不同真名应拒绝"
    );
}

/// 同一渠道上非恒等别名的 key 与 value 都在 `models` 里：创建/更新 409。
/// 昵称不在清单、或仅别名在清单（value 不在 `models`）则允许。
#[tokio::test]
async fn channel_rejects_intra_channel_alias_occupancy() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let owner = "occ-owner";
    let occupied = "occ-nick";

    let mut occupying = channel_body(
        "occ-reject",
        gw.upstream.base_url(),
        json!([owner, occupied]),
    );
    occupying["model_aliases"] = json!({ occupied: owner });
    let before: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", occupying).await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: Value = resp.json().await.expect("冲突体应可解析");
    assert_eq!(body["error"]["code"], "conflict");
    let message = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains(occupied),
        "冲突文案应点名占用别名，实际: {message}"
    );
    assert!(
        message.contains(owner),
        "冲突文案应点名被指向的主模型，实际: {message}"
    );
    let after: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    assert_eq!(before, after, "占用关系写不应改变库与快照");

    let mut nickname = channel_body("occ-nick-ok", gw.upstream.base_url(), json!([owner]));
    nickname["model_aliases"] = json!({ occupied: owner });
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", nickname).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "昵称不在 models 时应允许"
    );

    let mut alias_only = channel_body("occ-alias-only", gw.upstream.base_url(), json!([occupied]));
    alias_only["model_aliases"] = json!({ occupied: owner });
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", alias_only).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "仅别名在 models 时应允许"
    );

    let mut allowed = channel_body("occ-put-base", gw.upstream.base_url(), json!([owner]));
    allowed["model_aliases"] = json!({ occupied: owner });
    let resp = admin_json(&gw, reqwest::Method::POST, "/channels", allowed.clone()).await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: Value = resp.json().await.expect("应返回新建渠道");
    let channel_id = created["id"].as_i64().expect("创建应回传整数 id");

    let mut occupy_update = allowed;
    occupy_update["models"] = json!([owner, occupied]);
    let resp = admin_put(&gw, &format!("/channels/{channel_id}"), occupy_update).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "更新为占用形应拒绝"
    );
    let listed: Value = admin_get(&gw, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    let still = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == channel_id)
        .expect("渠道应仍在列表");
    assert_eq!(still["models"], json!([owner]), "占用更新不应改 models");
    assert_eq!(
        still["model_aliases"],
        json!({ occupied: owner }),
        "占用更新不应改别名"
    );
}

/// 以 root 会话新建一个 admin 角色账号并登录，返回其会话 Cookie。
async fn create_admin_session(gw: &TestGateway) -> String {
    let resp = admin_json(
        gw,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "health-admin@example.com",
            "display_name": "health-admin",
            "password": "password1",
            "role": "admin"
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let resp = reqwest::Client::new()
        .post(format!("{}/login", gw.admin_base_url()))
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({
            "email": "health-admin@example.com",
            "password": "password1"
        }))
        .send()
        .await
        .expect("admin 应能登录");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    common::session_cookie(&resp)
}

/// 渠道连续三次可重试失败后进入冷却：root 健康端点可见冷却行；
/// 冷却中的渠道被路由跳过——上游恢复健康也不出站；admin 角色 403。
#[tokio::test]
async fn channel_health_reports_cooldown_and_routing_skips_cooled_channel() {
    let mut gw = TestGateway::start_with_admin(common::test_seed).await;

    // seed 渠道 max_retries = 0：每次请求记一次可重试失败，跨请求累计。
    for _ in 0..3 {
        gw.upstream.set_behavior(UpstreamBehavior::Status5xx(500));
        let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "冷却前失败应原样返回上游 5xx"
        );
    }

    // root：健康端点列出冷却中渠道，含渠道 id/名、到期时刻与连续失败计数。
    let health: Value = admin_get(&gw, "/channels/health")
        .await
        .json()
        .await
        .expect("健康端点应可解析");
    let entries = health["channels"].as_array().expect("channels 应为数组");
    assert_eq!(entries.len(), 1, "唯一渠道应处于冷却中");
    assert_eq!(entries[0]["channel"], "test-channel");
    assert!(
        entries[0]["channel_id"].as_i64().is_some_and(|id| id > 0),
        "应回传库生成的渠道 id"
    );
    assert!(
        entries[0]["cooldown_until"]
            .as_i64()
            .is_some_and(|ms| ms > common::unix_millis()),
        "冷却到期时刻应在未来"
    );
    assert_eq!(
        entries[0]["consecutive_failures"], 3,
        "三次连续失败应被累计"
    );

    // 冷却中的渠道被跳过：上游恢复健康也不再出站，请求以无候选的 502 结束。
    let received_before = gw.upstream.received().len();
    gw.upstream
        .set_behavior(UpstreamBehavior::Json(completion_body()));
    let resp = chat_request(&gw, TEST_TOKEN_KEY, TEST_MODEL).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_GATEWAY,
        "唯一渠道冷却后应无候选可用"
    );
    assert_eq!(
        gw.upstream.received().len(),
        received_before,
        "冷却中的渠道不应产生出站请求"
    );

    // admin 角色会话：root 层端点返回 403 结构化错误。
    let admin_session = create_admin_session(&gw).await;
    let resp = reqwest::Client::new()
        .get(format!("{}/channels/health", gw.admin_base_url()))
        .header(reqwest::header::COOKIE, admin_session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("管理请求应可达");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "非 root 角色读取健康端点应被拒绝"
    );
    let body: Value = resp.json().await.expect("应返回结构化错误");
    assert_eq!(body["error"]["code"], "forbidden");
}
