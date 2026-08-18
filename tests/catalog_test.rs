//! 价格目录管理 API：整表替换、读回、设置间隔。
//!
//! 主接缝：独立管理监听上的 `/catalog` 与 `/settings.catalog_sync_interval_days`。
//! 目录不进运行时快照，失败不影响协议面。

mod common;

use common::{TEST_ADMIN_KEY, TestGateway};
use serde_json::{Value, json};

async fn admin_get(gw: &TestGateway, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}{path}", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .send()
        .await
        .expect("管理请求应可达")
}

async fn admin_json(
    gw: &TestGateway,
    method: reqwest::Method,
    path: &str,
    body: Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .request(method, format!("{}{path}", gw.admin_base_url()))
        .bearer_auth(TEST_ADMIN_KEY)
        .json(&body)
        .send()
        .await
        .expect("管理请求应可达")
}

fn sample_catalog_model() -> Value {
    json!({
        "provider_id": "openai",
        "provider_name": "OpenAI",
        "model_id": "gpt-4o",
        "input_micros": 2_500_000,
        "output_micros": 10_000_000,
        "cache_read_micros": 1_250_000,
        "cache_write_micros": null
    })
}

fn catalog_model(
    provider_id: &str,
    provider_name: &str,
    model_id: &str,
    input_micros: i64,
) -> Value {
    json!({
        "provider_id": provider_id,
        "provider_name": provider_name,
        "model_id": model_id,
        "input_micros": input_micros,
        "output_micros": input_micros * 4,
        "cache_read_micros": null,
        "cache_write_micros": null
    })
}

async fn put_sample_catalog(gw: &TestGateway, models: Vec<Value>) -> Value {
    let put = admin_json(
        gw,
        reqwest::Method::PUT,
        "/catalog",
        json!({ "models": models }),
    )
    .await;
    assert_eq!(put.status(), reqwest::StatusCode::OK);
    put.json().await.expect("写入响应应可解析")
}

/// `PUT /catalog` 整表替换后 `GET` 读回同一批行，并记下同步时刻。
#[tokio::test]
async fn catalog_put_then_get_roundtrip() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;

    let empty: Value = admin_get(&gw, "/catalog")
        .await
        .json()
        .await
        .expect("空目录应可解析");
    assert_eq!(empty["models"], json!([]));
    assert!(empty["synced_at"].is_null(), "从未同步应为 null");

    let put = admin_json(
        &gw,
        reqwest::Method::PUT,
        "/catalog",
        json!({ "models": [sample_catalog_model()] }),
    )
    .await;
    assert_eq!(put.status(), reqwest::StatusCode::OK);
    let written: Value = put.json().await.expect("写入响应应可解析");
    assert_eq!(written["models"].as_array().map(Vec::len), Some(1));
    let synced_at = written["synced_at"].as_i64().expect("写入应记下同步时刻");
    assert!(synced_at > 0);

    let got: Value = admin_get(&gw, "/catalog")
        .await
        .json()
        .await
        .expect("读回应可解析");
    assert_eq!(got["models"][0]["provider_id"], "openai");
    assert_eq!(got["models"][0]["model_id"], "gpt-4o");
    assert_eq!(got["models"][0]["input_micros"], 2_500_000);
    assert_eq!(got["synced_at"], synced_at);
}

/// `GET /catalog/meta` 返回同步时刻与按 name 排序的提供方计数，不含模型行。
#[tokio::test]
async fn catalog_meta_shape_and_provider_sort() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;

    let empty: Value = admin_get(&gw, "/catalog/meta")
        .await
        .json()
        .await
        .expect("空元数据应可解析");
    assert!(empty["synced_at"].is_null(), "从未同步应为 null");
    assert_eq!(empty["providers"], json!([]));
    assert!(empty.get("models").is_none(), "meta 不应含模型行");

    let written = put_sample_catalog(
        &gw,
        vec![
            catalog_model("openai", "OpenAI", "gpt-4o", 2_500_000),
            catalog_model("openai", "OpenAI", "gpt-4o-mini", 150_000),
            catalog_model("anthropic", "Anthropic", "claude-3", 3_000_000),
        ],
    )
    .await;
    let synced_at = written["synced_at"].as_i64().expect("写入应记下同步时刻");

    let meta: Value = admin_get(&gw, "/catalog/meta")
        .await
        .json()
        .await
        .expect("元数据应可解析");
    assert_eq!(meta["synced_at"], synced_at);
    assert_eq!(
        meta["providers"],
        json!([
            { "id": "anthropic", "name": "Anthropic", "count": 1 },
            { "id": "openai", "name": "OpenAI", "count": 2 }
        ])
    );
}

/// `q` 对 model_id 做大小写不敏感子串匹配，并转义 LIKE 通配符。
#[tokio::test]
async fn catalog_get_filters_by_model_id_substring() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    put_sample_catalog(
        &gw,
        vec![
            catalog_model("openai", "OpenAI", "gpt-4o", 2_500_000),
            catalog_model("openai", "OpenAI", "gpt-4o-mini", 150_000),
            catalog_model("openai", "OpenAI", "gpt%special", 1),
            catalog_model("anthropic", "Anthropic", "claude-3", 3_000_000),
        ],
    )
    .await;

    let gpt: Value = admin_get(&gw, "/catalog?q=GPT-4O")
        .await
        .json()
        .await
        .expect("q 过滤应可解析");
    let ids: Vec<&str> = gpt["models"]
        .as_array()
        .expect("models 应为数组")
        .iter()
        .map(|row| row["model_id"].as_str().expect("model_id"))
        .collect();
    assert_eq!(ids, vec!["gpt-4o", "gpt-4o-mini"]);

    let escaped: Value = admin_get(&gw, "/catalog?q=gpt%25special")
        .await
        .json()
        .await
        .expect("百分号字面量应可解析");
    assert_eq!(escaped["models"].as_array().map(Vec::len), Some(1));
    assert_eq!(escaped["models"][0]["model_id"], "gpt%special");

    let none: Value = admin_get(&gw, "/catalog?q=no-such-model")
        .await
        .json()
        .await
        .expect("无命中应可解析");
    assert_eq!(none["models"], json!([]));
    assert!(none["synced_at"].as_i64().is_some());
}

/// `provider_id` 精确匹配；逗号分隔表示多个提供方。
#[tokio::test]
async fn catalog_get_filters_by_provider_id() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    put_sample_catalog(
        &gw,
        vec![
            catalog_model("openai", "OpenAI", "gpt-4o", 2_500_000),
            catalog_model("openai", "OpenAI", "gpt-4o-mini", 150_000),
            catalog_model("anthropic", "Anthropic", "claude-3", 3_000_000),
        ],
    )
    .await;

    let openai: Value = admin_get(&gw, "/catalog?provider_id=openai")
        .await
        .json()
        .await
        .expect("提供方过滤应可解析");
    assert_eq!(openai["models"].as_array().map(Vec::len), Some(2));
    assert!(
        openai["models"]
            .as_array()
            .expect("models")
            .iter()
            .all(|row| row["provider_id"] == "openai")
    );

    let both: Value = admin_get(&gw, "/catalog?provider_id=openai,anthropic")
        .await
        .json()
        .await
        .expect("多提供方应可解析");
    assert_eq!(both["models"].as_array().map(Vec::len), Some(3));

    let with_q: Value = admin_get(&gw, "/catalog?provider_id=openai&q=mini")
        .await
        .json()
        .await
        .expect("联合过滤应可解析");
    assert_eq!(with_q["models"].as_array().map(Vec::len), Some(1));
    assert_eq!(with_q["models"][0]["model_id"], "gpt-4o-mini");
}

/// 无查询参数的 `GET /catalog` 仍返回全表。
#[tokio::test]
async fn catalog_get_without_params_returns_full_table() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    put_sample_catalog(
        &gw,
        vec![
            catalog_model("openai", "OpenAI", "gpt-4o", 2_500_000),
            catalog_model("anthropic", "Anthropic", "claude-3", 3_000_000),
        ],
    )
    .await;

    let got: Value = admin_get(&gw, "/catalog")
        .await
        .json()
        .await
        .expect("全量读回应可解析");
    assert_eq!(got["models"].as_array().map(Vec::len), Some(2));
}

/// 设置间隔写入后读回；缺省为 0（只手动）。
#[tokio::test]
async fn settings_catalog_sync_interval_roundtrip() {
    let gw = TestGateway::start_with_admin(common::empty_seed).await;
    let settings: Value = admin_get(&gw, "/settings")
        .await
        .json()
        .await
        .expect("设置应可解析");
    assert_eq!(settings["catalog_sync_interval_days"], 0);

    let put = admin_json(
        &gw,
        reqwest::Method::PUT,
        "/settings",
        json!({
            "full_body": false,
            "max_request_bytes": 1_000_000,
            "catalog_sync_interval_days": 7
        }),
    )
    .await;
    assert_eq!(put.status(), reqwest::StatusCode::OK);
    let updated: Value = put.json().await.expect("设置响应应可解析");
    assert_eq!(updated["catalog_sync_interval_days"], 7);

    let got: Value = admin_get(&gw, "/settings")
        .await
        .json()
        .await
        .expect("设置应可解析");
    assert_eq!(got["catalog_sync_interval_days"], 7);
}
