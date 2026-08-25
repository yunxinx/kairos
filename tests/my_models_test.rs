//! 用户自己的模型页数据源 `GET /me/models`：管理 API 黑盒。
//!
//! 主接缝：名单与下游 `GET /v1/models` 同源、单价折后且跨渠道给区间、响应不含
//! 任何渠道拓扑（这是本端点存在的理由，不能把 `/model-groups` 放开给普通用户）。

mod common;

use common::{TEST_MODEL, TestGateway};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn admin_url(gw: &TestGateway, path: &str) -> String {
    format!("{}{path}", gw.admin_base_url())
}

async fn bearer_get(gw: &TestGateway, token: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(admin_url(gw, path))
        .bearer_auth(token)
        .send()
        .await
        .expect("管理请求应可达")
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

fn channel_body_with_keys(name: &str, base_url: &str, models: Value, keys: Value) -> Value {
    json!({
        "name": name,
        "protocol": "openai_chat",
        "base_url": base_url,
        "keys": keys,
        "models": models,
        "model_aliases": {},
        "timeout_ms": 1000,
        "max_retries": 0,
        "enabled": true
    })
}

async fn first_channel_id(gw: &TestGateway) -> i64 {
    let channels: Value = bearer_get(gw, &gw.session, "/channels")
        .await
        .json()
        .await
        .expect("渠道列表应可解析");
    channels.as_array().expect("应为数组")[0]["id"]
        .as_i64()
        .expect("应有渠道 id")
}

async fn create_channel(gw: &TestGateway, name: &str, models: Value) -> i64 {
    create_channel_with_keys(
        gw,
        name,
        models,
        json!([{"name": "default", "api_key": "sk-upstream", "weight": 1, "enabled": true, "models": null, "blocked_models": null}]),
    )
    .await
}

async fn create_channel_with_keys(gw: &TestGateway, name: &str, models: Value, keys: Value) -> i64 {
    let response = bearer_json(
        gw,
        &gw.session,
        reqwest::Method::POST,
        "/channels",
        channel_body_with_keys(name, &gw.upstream.base_url(), models, keys),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json::<Value>().await.expect("渠道应可解析")["id"]
        .as_i64()
        .expect("应有渠道 id")
}

async fn set_price(gw: &TestGateway, channel_id: i64, model: &str, input: i64, output: i64) {
    let response = bearer_json(
        gw,
        &gw.session,
        reqwest::Method::POST,
        "/prices",
        json!({
            "channel_id": channel_id,
            "model": model,
            "input_micros": input,
            "output_micros": output,
            "cache_read_micros": null,
            "cache_write_micros": null
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED, "定价应成功");
}

/// 建一个模型组，名单为若干「钉渠道的已登记名」。
async fn create_group(gw: &TestGateway, name: &str, pins: Vec<(i64, &str)>) {
    let models: Vec<Value> = pins
        .into_iter()
        .map(|(channel_id, model)| json!({ "kind": "source", "channel_id": channel_id, "model": model }))
        .collect();
    let response = bearer_json(
        gw,
        &gw.session,
        reqwest::Method::POST,
        "/model-groups",
        json!({ "name": name, "models": models }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED, "建组应成功");
}

/// 建一档带指定组名单的套餐，返回 id。
async fn create_plan(gw: &TestGateway, name: &str, groups: &[&str], discount_bp: i64) -> i64 {
    let response = bearer_json(
        gw,
        &gw.session,
        reqwest::Method::POST,
        "/plans",
        json!({
            "internal_name": name,
            "display_name": name,
            "groups": groups,
            "discount_bp": discount_bp
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED, "建档应成功");
    response.json::<Value>().await.expect("套餐应可解析")["id"]
        .as_i64()
        .expect("应有套餐 id")
}

/// 整体替换套餐的组名单（用于验证撤组语义）。
async fn set_plan_groups(gw: &TestGateway, plan_id: i64, name: &str, groups: &[&str]) {
    let response = bearer_json(
        gw,
        &gw.session,
        reqwest::Method::PUT,
        &format!("/plans/{plan_id}"),
        json!({ "internal_name": name, "display_name": name, "groups": groups }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "改档应成功");
}

/// 建一个普通用户并挂到指定套餐，返回其 id。
async fn create_user(gw: &TestGateway, email: &str, plan_id: i64) -> i64 {
    let response = bearer_json(
        gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": email,
            "display_name": email,
            "password": "password1",
            "role": "user",
            "plan_id": plan_id
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED, "建用户应成功");
    response.json::<Value>().await.expect("用户应可解析")["id"]
        .as_i64()
        .expect("应有用户 id")
}

/// 建一个带管理员套餐的管理员；模型页的查看范围应与 root 一致。
async fn create_admin(gw: &TestGateway, email: &str, plan_id: i64) -> i64 {
    let response = bearer_json(
        gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": email,
            "display_name": email,
            "password": "password1",
            "role": "admin",
            "plan_id": plan_id
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED, "管理员应可创建");
    response.json::<Value>().await.expect("管理员应可解析")["id"]
        .as_i64()
        .expect("应有管理员 id")
}

async fn login(gw: &TestGateway, email: &str) -> String {
    let response = reqwest::Client::new()
        .post(admin_url(gw, "/login"))
        .json(&json!({ "email": email, "password": "password1" }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().await.expect("登录应可解析")["token"]
        .as_str()
        .expect("应有会话")
        .to_string()
}

/// 以某个会话拉 `/me/models`。
async fn my_models(gw: &TestGateway, session: &str) -> Value {
    let response = bearer_get(gw, session, "/me/models").await;
    assert_eq!(response.status(), StatusCode::OK, "自己的模型页应可读");
    response.json().await.expect("响应应可解析")
}

/// 取某一段（模型组）的模型数组。
fn group_models<'a>(view: &'a Value, group: &str) -> &'a Vec<Value> {
    view["groups"]
        .as_array()
        .expect("groups 应为数组")
        .iter()
        .find(|section| section["name"] == group)
        .unwrap_or_else(|| panic!("应有 {group} 段"))["models"]
        .as_array()
        .expect("models 应为数组")
}

/// 该段里的可调用名。
fn model_ids(view: &Value, group: &str) -> Vec<String> {
    group_models(view, group)
        .iter()
        .map(|model| model["id"].as_str().expect("应有 id").to_string())
        .collect()
}

/// 段名列表（按响应顺序）。
fn group_names(view: &Value) -> Vec<String> {
    view["groups"]
        .as_array()
        .expect("groups 应为数组")
        .iter()
        .map(|section| section["name"].as_str().expect("应有 name").to_string())
        .collect()
}

/// 取某个名字在某段里的整行。
fn model_row<'a>(view: &'a Value, group: &str, id: &str) -> &'a Value {
    group_models(view, group)
        .iter()
        .find(|model| model["id"] == id)
        .unwrap_or_else(|| panic!("{group} 段应有 {id}"))
}

/// 只列出自己套餐名单里的组，按组分段；`default` 置顶。
#[tokio::test]
async fn lists_only_own_plan_groups_sectioned() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = first_channel_id(&gw).await;
    create_group(&gw, "coding", vec![(channel_id, TEST_MODEL)]).await;

    // 只给 coding：default 段不该出现。
    let narrow = create_plan(&gw, "coder", &["coding"], 10_000).await;
    create_user(&gw, "coder@example.com", narrow).await;
    let view = my_models(&gw, &login(&gw, "coder@example.com").await).await;
    assert_eq!(group_names(&view), vec!["coding"], "只列自己名单里的组");
    assert_eq!(model_ids(&view, "coding"), vec![TEST_MODEL]);

    // 两个组：default 置顶，且 `gpt-4o` 已归 coding，故 default 段只剩别名短名。
    let wide = create_plan(&gw, "both", &["default", "coding"], 10_000).await;
    create_user(&gw, "both@example.com", wide).await;
    let view = my_models(&gw, &login(&gw, "both@example.com").await).await;
    assert_eq!(group_names(&view), vec!["default", "coding"]);
    assert_eq!(model_ids(&view, "default"), vec!["fast"]);
    assert_eq!(model_ids(&view, "coding"), vec![TEST_MODEL]);
}

/// 管理员查看模型时不受套餐组名单裁剪，但创建令牌仍只能绑定套餐内的组。
#[tokio::test]
async fn admin_model_view_is_unrestricted_but_token_binding_remains_restricted() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = first_channel_id(&gw).await;
    create_group(&gw, "coding", vec![(channel_id, TEST_MODEL)]).await;

    let plan = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/plans",
        json!({
            "internal_name": "narrow-admin",
            "display_name": "narrow-admin",
            "audience": "admin",
            "groups": ["default"],
            "discount_bp": 9000
        }),
    )
    .await;
    assert_eq!(plan.status(), StatusCode::CREATED);
    let plan_id = plan.json::<Value>().await.expect("套餐应可解析")["id"]
        .as_i64()
        .expect("套餐应有 id");
    create_admin(&gw, "narrow-admin@example.com", plan_id).await;

    let admin_session = login(&gw, "narrow-admin@example.com").await;
    let view = my_models(&gw, &admin_session).await;
    assert!(group_names(&view).contains(&"coding".to_string()));
    assert_eq!(
        view["discount_bp"], 9000,
        "查看范围放开但价格仍按管理员套餐折扣"
    );

    let token = bearer_json(
        &gw,
        &admin_session,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "outside", "enabled": true, "model_group": "coding" }),
    )
    .await;
    assert_eq!(
        token.status(),
        StatusCode::BAD_REQUEST,
        "令牌绑定仍受套餐组限制"
    );
}

/// 单价是折后的；同名挂在两条渠道且单价不同时给出区间。
#[tokio::test]
async fn prices_are_discounted_and_span_channels() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    // 第二条渠道登记同一个名字但报价更高：区间的两端应分别落在两条渠道上。
    let second = create_channel(&gw, "pricey", json!([TEST_MODEL])).await;
    set_price(&gw, second, TEST_MODEL, 3_500_000, 12_000_000).await;

    // 八折档：报出的每个数都应已折过，界面不再自己乘。
    let plan_id = create_plan(&gw, "discounted", &["default"], 8_000).await;
    create_user(&gw, "discounted@example.com", plan_id).await;
    let view = my_models(&gw, &login(&gw, "discounted@example.com").await).await;

    assert_eq!(view["discount_bp"], 8_000, "折扣率随响应给出，供界面标注");
    let row = model_row(&view, "default", TEST_MODEL);
    assert_eq!(row["callable"], true);
    assert_eq!(
        row["input"],
        json!({ "min_micros": 2_000_000, "max_micros": 2_800_000 }),
        "2.5 与 3.5 美元的八折两端"
    );
    assert_eq!(
        row["output"],
        json!({ "min_micros": 8_000_000, "max_micros": 9_600_000 })
    );

    // 单渠道的名字两端相等，不因为「区间」而虚构出第二个数。
    let alias = model_row(&view, "default", "fast");
    assert_eq!(
        alias["input"],
        json!({ "min_micros": 120_000, "max_micros": 120_000 })
    );
    assert_eq!(
        alias["cache_read"],
        Value::Null,
        "该档不计价时不出现，而不是报 0"
    );
}

/// 响应不含任何渠道拓扑。这是本端点存在的理由：组的原始形状带 `channel_id`，
/// 所以不能把 `/model-groups` 放开给普通用户。
#[tokio::test]
async fn response_carries_no_channel_topology() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = first_channel_id(&gw).await;
    create_group(&gw, "coding", vec![(channel_id, TEST_MODEL)]).await;
    let plan_id = create_plan(&gw, "coder", &["default", "coding"], 10_000).await;
    create_user(&gw, "opaque@example.com", plan_id).await;

    let response = bearer_get(&gw, &login(&gw, "opaque@example.com").await, "/me/models").await;
    assert_eq!(response.status(), StatusCode::OK);
    let raw = response.text().await.expect("响应应可读");

    // 在原始 JSON 文本上断言：换成结构化遍历会漏掉未来新加的嵌套字段。
    for leaked in [
        "channel_id",
        "channel_ids",
        "base_url",
        "api_key",
        "keys",
        "test-channel",
        "sk-upstream",
    ] {
        assert!(
            !raw.contains(leaked),
            "响应不应泄漏渠道拓扑，但出现了 {leaked}：{raw}"
        );
    }
    // 反向确认这次响应确实有内容，避免上面的断言因为空响应而空过。
    assert!(raw.contains(TEST_MODEL), "应确实列出了可调用名：{raw}");
}

/// 从套餐名单撤组后该段消失（延续 ADR-0010 的撤组语义）。
#[tokio::test]
async fn withdrawing_a_group_removes_its_section() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = first_channel_id(&gw).await;
    create_group(&gw, "coding", vec![(channel_id, TEST_MODEL)]).await;
    let plan_id = create_plan(&gw, "coder", &["default", "coding"], 10_000).await;
    create_user(&gw, "withdraw@example.com", plan_id).await;
    let session = login(&gw, "withdraw@example.com").await;

    assert_eq!(group_names(&my_models(&gw, &session).await).len(), 2);

    set_plan_groups(&gw, plan_id, "coder", &["default"]).await;
    let view = my_models(&gw, &session).await;
    assert_eq!(group_names(&view), vec!["default"], "撤掉的组应立即消失");
    // 撤组不需要重新登录：会话仍有效，页面读的是当前快照。
    assert!(
        !model_ids(&view, "default").contains(&TEST_MODEL.to_string()),
        "钉进 coding 的名字不隐式回到 default"
    );
}

/// 统一模型只占一行且标 `unified`；开隐藏时被收进的成员不再单独出现。
#[tokio::test]
async fn unified_model_occupies_one_row_and_hides_members() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = first_channel_id(&gw).await;
    let created = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/unified-models",
        json!({
            "id": "smart",
            "models": [
                { "channel_id": channel_id, "model": TEST_MODEL },
                { "channel_id": channel_id, "model": "fast" }
            ],
            "hide": true
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let plan_id = create_plan(&gw, "unified", &["default"], 10_000).await;
    create_user(&gw, "unified@example.com", plan_id).await;
    let view = my_models(&gw, &login(&gw, "unified@example.com").await).await;

    let ids = model_ids(&view, "default");
    assert!(
        ids.contains(&"smart".to_string()),
        "统一模型应出现：{ids:?}"
    );
    assert!(
        !ids.contains(&TEST_MODEL.to_string()) && !ids.contains(&"fast".to_string()),
        "开隐藏后成员不单独出现：{ids:?}"
    );

    let row = model_row(&view, "default", "smart");
    assert_eq!(row["unified"], true);
    assert_eq!(row["callable"], true);
    // 区间覆盖全部可用成员：2.5 与 0.15 美元。
    assert_eq!(
        row["input"],
        json!({ "min_micros": 150_000, "max_micros": 2_500_000 })
    );
}

/// 与下游 `GET /v1/models` 名单一致——这一页的正确性锚点。
#[tokio::test]
async fn matches_downstream_model_list_for_the_same_group() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let channel_id = first_channel_id(&gw).await;
    create_group(&gw, "coding", vec![(channel_id, TEST_MODEL)]).await;
    let plan_id = create_plan(&gw, "coder", &["coding"], 10_000).await;
    create_user(&gw, "parity@example.com", plan_id).await;
    let session = login(&gw, "parity@example.com").await;

    // 该用户自己的令牌绑 coding，下游列表应与页面里的 coding 段逐字相同。
    let created = bearer_json(
        &gw,
        &session,
        reqwest::Method::POST,
        "/tokens",
        json!({ "name": "mine", "model_group": "coding", "enabled": true }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let token_key = created.json::<Value>().await.expect("令牌应可解析")["token_key"]
        .as_str()
        .expect("应有明文 key")
        .to_string();

    let downstream: Value = reqwest::Client::new()
        .get(format!("{}/v1/models", gw.base_url()))
        .bearer_auth(&token_key)
        .send()
        .await
        .expect("下游列表应可达")
        .json()
        .await
        .expect("下游列表应可解析");
    let mut downstream_ids: Vec<String> = downstream["data"]
        .as_array()
        .expect("data 应为数组")
        .iter()
        .map(|item| item["id"].as_str().expect("应有 id").to_string())
        .collect();
    downstream_ids.sort();

    let mut page_ids = model_ids(&my_models(&gw, &session).await, "coding");
    page_ids.sort();
    assert_eq!(page_ids, downstream_ids, "页面与下游列表必须同源");
}

/// 没有启用且已定价渠道的名字标 `callable: false`，四档留空。
#[tokio::test]
async fn unpriced_or_disabled_name_is_not_callable() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    // 建渠道但不定价：请求会被计费准入拒绝，页面必须如实标出来。
    create_channel(&gw, "unpriced", json!(["gpt-4o-mini"])).await;

    let plan_id = create_plan(&gw, "plain", &["default"], 10_000).await;
    create_user(&gw, "unpriced@example.com", plan_id).await;
    let view = my_models(&gw, &login(&gw, "unpriced@example.com").await).await;

    let row = model_row(&view, "default", "gpt-4o-mini");
    assert_eq!(row["callable"], false, "无价格的名字不可调用");
    assert_eq!(row["input"], Value::Null);
    assert_eq!(row["output"], Value::Null);

    // 已定价的名字仍正常可调用，确认上面的断言不是整页都空。
    assert_eq!(model_row(&view, "default", TEST_MODEL)["callable"], true);
}

/// `callable` 与真实路由的密钥筛选一致，不能只看渠道和价格。
#[tokio::test]
async fn callable_requires_an_enabled_key_that_allows_the_model() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let cases = [
        (
            "disabled-key",
            "key-disabled-model",
            json!([{"name": "disabled", "api_key": "sk-disabled", "weight": 1, "enabled": false, "models": null, "blocked_models": null}]),
            false,
        ),
        (
            "allow-excludes",
            "allow-excluded-model",
            json!([{"name": "allow", "api_key": "sk-allow", "weight": 1, "enabled": true, "models": ["another-model"], "blocked_models": null}]),
            false,
        ),
        (
            "block-includes",
            "blocked-model",
            json!([{"name": "block", "api_key": "sk-block", "weight": 1, "enabled": true, "models": null, "blocked_models": ["blocked-model"]}]),
            false,
        ),
        (
            "eligible-key",
            "eligible-model",
            json!([
                {"name": "excluded", "api_key": "sk-excluded", "weight": 1, "enabled": true, "models": ["another-model"], "blocked_models": null},
                {"name": "eligible", "api_key": "sk-eligible", "weight": 1, "enabled": true, "models": ["eligible-model"], "blocked_models": null}
            ]),
            true,
        ),
    ];

    for (channel_name, model, keys, _) in &cases {
        let channel_id =
            create_channel_with_keys(&gw, channel_name, json!([model]), keys.clone()).await;
        set_price(&gw, channel_id, model, 1_000_000, 2_000_000).await;
    }

    let plan_id = create_plan(&gw, "key-rules", &["default"], 10_000).await;
    create_user(&gw, "key-rules@example.com", plan_id).await;
    let view = my_models(&gw, &login(&gw, "key-rules@example.com").await).await;

    for (_, model, _, expected) in cases {
        let row = model_row(&view, "default", model);
        assert_eq!(row["callable"], expected, "模型 {model} 的密钥资格判断错误");
        if !expected {
            assert_eq!(row["input"], Value::Null);
            assert_eq!(row["output"], Value::Null);
        }
    }
}

/// 统一模型的价格区间只覆盖真实可路由成员；所有成员无合格密钥时不可调用。
#[tokio::test]
async fn unified_price_range_ignores_members_without_eligible_keys() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let disabled = create_channel_with_keys(
        &gw,
        "unified-disabled",
        json!(["member-disabled"]),
        json!([{"name": "disabled", "api_key": "sk-disabled", "weight": 1, "enabled": false, "models": null, "blocked_models": null}]),
    )
    .await;
    let eligible = create_channel_with_keys(
        &gw,
        "unified-eligible",
        json!(["member-eligible"]),
        json!([{"name": "eligible", "api_key": "sk-eligible", "weight": 1, "enabled": true, "models": null, "blocked_models": null}]),
    )
    .await;
    set_price(&gw, disabled, "member-disabled", 9_000_000, 10_000_000).await;
    set_price(&gw, eligible, "member-eligible", 2_000_000, 3_000_000).await;

    for (id, members) in [
        (
            "unified-partial",
            json!([
                {"channel_id": disabled, "model": "member-disabled"},
                {"channel_id": eligible, "model": "member-eligible"}
            ]),
        ),
        (
            "unified-none",
            json!([{"channel_id": disabled, "model": "member-disabled"}]),
        ),
    ] {
        let response = bearer_json(
            &gw,
            &gw.session,
            reqwest::Method::POST,
            "/unified-models",
            json!({"id": id, "models": members, "hide": false}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    let plan_id = create_plan(&gw, "unified-keys", &["default"], 10_000).await;
    create_user(&gw, "unified-keys@example.com", plan_id).await;
    let view = my_models(&gw, &login(&gw, "unified-keys@example.com").await).await;

    let partial = model_row(&view, "default", "unified-partial");
    assert_eq!(partial["callable"], true);
    assert_eq!(
        partial["input"],
        json!({"min_micros": 2_000_000, "max_micros": 2_000_000})
    );
    let none = model_row(&view, "default", "unified-none");
    assert_eq!(none["callable"], false);
    assert_eq!(none["input"], Value::Null);
}
