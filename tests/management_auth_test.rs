//! 管理会话与 RBAC 端到端黑盒：登录、越权 403、最后 root 保护。
//!
//! 登录口令不能当作管理 API 的 Bearer；管理面只认 `POST /login` 签发的 `ksess_…`。

mod common;

use common::{TEST_MODEL, TEST_ROOT_PASSWORD, TEST_TOKEN_KEY, TestGateway};
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

/// 播种后的 root 可用邮箱密码换会话；错密码失败；登出后会话失效。
#[tokio::test]
async fn seeded_root_can_login_and_logout() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let denied = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({ "email": "root@localhost", "password": "password1" }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({
            "email": "root@localhost",
            "password": TEST_ROOT_PASSWORD
        }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(login.status(), StatusCode::OK);
    let body: Value = login.json().await.expect("登录响应应可解析");
    let token = body["token"].as_str().expect("应有会话令牌");
    assert!(token.starts_with("ksess_"));
    assert_eq!(body["user"]["role"], "root");

    let me = bearer_get(&gw, token, "/me").await;
    assert_eq!(me.status(), StatusCode::OK);
    let me_body: Value = me.json().await.expect("me 应可解析");
    assert_eq!(me_body["email"], "root@localhost");

    let tokens = bearer_get(&gw, token, "/tokens").await;
    assert_eq!(tokens.status(), StatusCode::OK);

    let logout = reqwest::Client::new()
        .post(admin_url(&gw, "/logout"))
        .bearer_auth(token)
        .send()
        .await
        .expect("登出应可达");
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    let after = bearer_get(&gw, token, "/tokens").await;
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
}

/// 登录口令不能当管理 Bearer，也不能当模型令牌；会话同样不能调模型路由。
#[tokio::test]
async fn login_password_is_not_a_bearer_and_cannot_call_models() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let as_admin = bearer_get(&gw, TEST_ROOT_PASSWORD, "/tokens").await;
    assert_eq!(
        as_admin.status(),
        StatusCode::UNAUTHORIZED,
        "登录口令不能作为管理 API Bearer"
    );

    for key in [gw.session.as_str(), TEST_ROOT_PASSWORD] {
        let resp = reqwest::Client::new()
            .post(format!("{}/v1/chat/completions", gw.base_url()))
            .bearer_auth(key)
            .json(&json!({
                "model": TEST_MODEL,
                "messages": [{ "role": "user", "content": "hi" }]
            }))
            .send()
            .await
            .expect("模型请求应可达");
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{key} 不应能调模型路由"
        );
    }

    let ok = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(TEST_TOKEN_KEY)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("令牌请求应可达");
    assert_ne!(ok.status(), StatusCode::UNAUTHORIZED);
}

/// 账户页：改邮箱与改密码都必须带对的当前密码——邮箱是唯一登录标识，
/// 被盗会话若能静默改邮箱即等于永久劫持账户。
#[tokio::test]
async fn update_me_email_and_password_with_current() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let missing_current = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({ "email": "root-renamed@example.com" }),
    )
    .await;
    assert_eq!(
        missing_current.status(),
        StatusCode::BAD_REQUEST,
        "改邮箱必须提供当前密码"
    );

    let wrong_current = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({
            "email": "root-renamed@example.com",
            "current_password": "not-the-password"
        }),
    )
    .await;
    assert_eq!(wrong_current.status(), StatusCode::BAD_REQUEST);

    let email = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({
            "email": "root-renamed@example.com",
            "current_password": TEST_ROOT_PASSWORD
        }),
    )
    .await;
    assert_eq!(email.status(), StatusCode::OK);
    let body: Value = email.json().await.expect("应可解析");
    assert_eq!(body["email"], "root-renamed@example.com");

    let missing_current = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({ "password": "password1" }),
    )
    .await;
    assert_eq!(missing_current.status(), StatusCode::BAD_REQUEST);

    let wrong_current = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({
            "password": "password1",
            "current_password": "not-the-password"
        }),
    )
    .await;
    assert_eq!(wrong_current.status(), StatusCode::BAD_REQUEST);

    let changed = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({
            "password": "password1",
            "current_password": TEST_ROOT_PASSWORD
        }),
    )
    .await;
    assert_eq!(changed.status(), StatusCode::OK);

    // 位图 data URL 可以存。SVG 不在允许名单：它能内联脚本与外链资源，一旦哪天
    // 被 v-html/object 渲染就成了 XSS，头像不值得冒这个险。
    const AVATAR_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let avatar_update = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({ "avatar": AVATAR_PNG }),
    )
    .await;
    assert_eq!(avatar_update.status(), StatusCode::OK);
    let me: Value = bearer_get(&gw, &gw.session, "/me")
        .await
        .json()
        .await
        .expect("json");
    assert_eq!(me["avatar"], AVATAR_PNG);

    let svg_rejected = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({ "avatar": "data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=" }),
    )
    .await;
    assert_eq!(
        svg_rejected.status(),
        StatusCode::BAD_REQUEST,
        "SVG 头像应被拒绝"
    );

    let oversized = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/me",
        json!({ "avatar": format!("data:image/png;base64,{}", "A".repeat(300 * 1024)) }),
    )
    .await;
    assert_eq!(
        oversized.status(),
        StatusCode::BAD_REQUEST,
        "超限头像应被拒绝，而不是撑大 users 表"
    );

    let old_login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({
            "email": "root-renamed@example.com",
            "password": TEST_ROOT_PASSWORD
        }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(old_login.status(), StatusCode::UNAUTHORIZED);

    let new_login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({
            "email": "root-renamed@example.com",
            "password": "password1"
        }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(new_login.status(), StatusCode::OK);
}

/// user 不能写渠道/模型组；admin 不能写渠道；最后 root 不能降级。
#[tokio::test]
async fn rbac_forbids_cross_role_writes_and_protects_last_root() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let created = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "user@example.com",
            "display_name": "普通",
            "password": "password1",
            "role": "user"
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let user_id = created.json::<Value>().await.expect("用户应可解析")["id"]
        .as_i64()
        .expect("应有用户 id");

    let admin_created = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "admin@example.com",
            "display_name": "管理",
            "password": "password1",
            "role": "admin"
        }),
    )
    .await;
    assert_eq!(admin_created.status(), StatusCode::CREATED);

    let user_login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({ "email": "user@example.com", "password": "password1" }))
        .send()
        .await
        .expect("登录应可达");
    let user_body: Value = user_login.json().await.expect("登录应可解析");
    let user_token = user_body["token"].as_str().expect("应有会话");

    let admin_login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({ "email": "admin@example.com", "password": "password1" }))
        .send()
        .await
        .expect("登录应可达");
    let admin_body: Value = admin_login.json().await.expect("登录应可解析");
    let admin_token = admin_body["token"].as_str().expect("应有会话");

    let user_channels = bearer_get(&gw, user_token, "/channels").await;
    assert_eq!(user_channels.status(), StatusCode::FORBIDDEN);
    let user_groups = bearer_get(&gw, user_token, "/model-groups").await;
    assert_eq!(user_groups.status(), StatusCode::FORBIDDEN);

    let admin_channels = bearer_get(&gw, admin_token, "/channels").await;
    assert_eq!(admin_channels.status(), StatusCode::FORBIDDEN);
    let admin_groups = bearer_get(&gw, admin_token, "/model-groups").await;
    assert_eq!(admin_groups.status(), StatusCode::OK);
    let admin_settings = bearer_get(&gw, admin_token, "/settings").await;
    assert_eq!(admin_settings.status(), StatusCode::FORBIDDEN);

    let user_creates = bearer_json(
        &gw,
        user_token,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "another@example.com",
            "display_name": "越权",
            "password": "password1",
            "role": "user"
        }),
    )
    .await;
    assert_eq!(user_creates.status(), StatusCode::FORBIDDEN);

    let user_escalates_self = bearer_json(
        &gw,
        user_token,
        reqwest::Method::PUT,
        &format!("/users/{user_id}"),
        json!({ "role": "root" }),
    )
    .await;
    assert_eq!(user_escalates_self.status(), StatusCode::FORBIDDEN);

    let user_resets_self_without_current = bearer_json(
        &gw,
        user_token,
        reqwest::Method::PUT,
        &format!("/users/{user_id}"),
        json!({ "password": "stolen-session-reset" }),
    )
    .await;
    assert_eq!(
        user_resets_self_without_current.status(),
        StatusCode::FORBIDDEN
    );

    let admin_creates_admin = bearer_json(
        &gw,
        admin_token,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "other-admin@example.com",
            "display_name": "越权管理",
            "password": "password1",
            "role": "admin"
        }),
    )
    .await;
    assert_eq!(admin_creates_admin.status(), StatusCode::FORBIDDEN);

    let demote = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        "/users/1",
        json!({ "role": "admin" }),
    )
    .await;
    assert_eq!(demote.status(), StatusCode::CONFLICT);
    let body: Value = demote.json().await.expect("应可解析");
    assert_eq!(body["error"]["code"], "last_root_protected");

    let delete_last = reqwest::Client::new()
        .delete(admin_url(&gw, "/users/1"))
        .bearer_auth(&gw.session)
        .send()
        .await
        .expect("删除应可达");
    assert_eq!(delete_last.status(), StatusCode::CONFLICT);
    let delete_body: Value = delete_last.json().await.expect("应可解析");
    assert_eq!(delete_body["error"]["code"], "last_root_protected");
}

/// 缺失或形态错误的凭证不是有效会话猜测，不应耗尽登录失败预算。
#[tokio::test]
async fn malformed_management_credentials_do_not_throttle_login() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    for _ in 0..10 {
        let missing = reqwest::Client::new()
            .get(admin_url(&gw, "/me"))
            .send()
            .await
            .expect("请求应可达");
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let malformed = bearer_get(&gw, "not-a-session", "/me").await;
        assert_eq!(malformed.status(), StatusCode::UNAUTHORIZED);
    }

    let login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({
            "email": common::TEST_ROOT_EMAIL,
            "password": common::TEST_ROOT_PASSWORD
        }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(login.status(), StatusCode::OK);
}

#[tokio::test]
async fn user_rate_limit_and_stats_roundtrip() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let user_res = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "rpm-user@example.com",
            "display_name": "限速用户",
            "password": "password1",
            "role": "user",
            "rate_limit_rpm": 2
        }),
    )
    .await;
    assert_eq!(user_res.status(), StatusCode::CREATED);
    let user_view: Value = user_res.json().await.expect("json");
    let user_id = user_view["id"].as_i64().expect("user_id");

    // 给用户钱包充值
    let recharge = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        &format!("/users/{user_id}/balance"),
        json!({ "delta_usd_micros": 10_000_000 }),
    )
    .await;
    assert_eq!(recharge.status(), StatusCode::OK);

    // 用户登录并创建令牌
    let login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({
            "email": "rpm-user@example.com",
            "password": "password1"
        }))
        .send()
        .await
        .expect("登录");
    assert_eq!(login.status(), StatusCode::OK);
    let session_token = login.json::<Value>().await.expect("json")["token"]
        .as_str()
        .expect("token")
        .to_string();

    let token_res = bearer_json(
        &gw,
        &session_token,
        reqwest::Method::POST,
        "/tokens",
        json!({
            "name": "User RPM Token",
            "model_group": "default",
            "rate_limit_rpm": 100, // 令牌自己配了 100，但全局被用户 2 RPM 压制
            "enabled": true
        }),
    )
    .await;
    assert_eq!(token_res.status(), StatusCode::CREATED);
    let token_view: Value = token_res.json().await.expect("json");
    let token_key = token_view["token_key"]
        .as_str()
        .expect("token_key")
        .to_string();

    // 发起调用
    let call1 = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(&token_key)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .expect("call 1");
    assert_eq!(call1.status(), StatusCode::OK);

    let call2 = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(&token_key)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .expect("call 2");
    assert_eq!(call2.status(), StatusCode::OK);

    // 第 3 次超限 429
    let call3 = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base_url()))
        .bearer_auth(&token_key)
        .json(&json!({
            "model": TEST_MODEL,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .expect("call 3");
    assert_eq!(call3.status(), StatusCode::TOO_MANY_REQUESTS);

    // Root 查询 /users 列表，验证统计数据已聚合
    let users_list: Vec<Value> = bearer_get(&gw, &gw.session, "/users")
        .await
        .json()
        .await
        .expect("json");
    let user_stats = users_list
        .iter()
        .find(|u| u["id"] == user_id)
        .expect("user exists");
    assert_eq!(user_stats["rate_limit_rpm"], 2);
    assert_eq!(user_stats["request_count"], 2);
    assert!(user_stats["last_used_at"].as_i64().is_some());
}

/// 登录入口形状封顶：超长/控制字符输入按 400 拒绝，不进 Argon2、不写审计。
#[tokio::test]
async fn login_rejects_oversized_and_control_char_input() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let auth_rows = || async {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM system_log WHERE target = 'auth'")
                .fetch_one(&gw.pool)
                .await
                .expect("应能数审计行");
        count
    };
    let before = auth_rows().await;

    let oversized = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({
            "email": format!("{}@x.com", "a".repeat(400)),
            "password": "password1"
        }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(oversized.status(), StatusCode::BAD_REQUEST);

    let control_chars = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({ "email": "a\nb@c.com", "password": "password1" }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(
        control_chars.status(),
        StatusCode::BAD_REQUEST,
        "控制字符可伪造多行审计日志，应在入口拒绝"
    );

    let oversized_password = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({
            "email": "root@localhost",
            "password": "x".repeat(200)
        }))
        .send()
        .await
        .expect("登录应可达");
    assert_eq!(oversized_password.status(), StatusCode::BAD_REQUEST);

    let after = auth_rows().await;
    assert_eq!(
        before, after,
        "形状违规不应消耗 Argon2，也不应留下 auth 审计行"
    );
}

/// root 全局唯一：创建与晋升第二 root 都被 API 拒绝。
#[tokio::test]
async fn second_root_cannot_be_created_or_promoted() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let created = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "second-root@example.com",
            "display_name": "第二个 root",
            "password": "password1",
            "role": "root"
        }),
    )
    .await;
    assert_eq!(
        created.status(),
        StatusCode::FORBIDDEN,
        "创建接口不接受 root 角色"
    );

    // 晋升同样被拒：先建一个 admin，再试图提成 root。
    let admin = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "promote-me@example.com",
            "display_name": "待晋升",
            "password": "password1",
            "role": "admin"
        }),
    )
    .await;
    assert_eq!(admin.status(), StatusCode::CREATED);
    let admin_id = admin.json::<Value>().await.expect("json")["id"]
        .as_i64()
        .expect("id");

    let promoted = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::PUT,
        &format!("/users/{admin_id}"),
        json!({ "role": "root" }),
    )
    .await;
    assert_eq!(
        promoted.status(),
        StatusCode::FORBIDDEN,
        "root 是内置唯一账号，不能经 API 晋升"
    );
}

/// 用户桶跨令牌共享：用户 1 RPM 时换第二把令牌也压不过（令牌写 0 同样无效）。
#[tokio::test]
async fn user_rate_limit_bucket_is_shared_across_tokens() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;

    let user_res = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        "/users",
        json!({
            "email": "shared-rpm@example.com",
            "display_name": "共享限速用户",
            "password": "password1",
            "role": "user",
            "rate_limit_rpm": 1
        }),
    )
    .await;
    assert_eq!(user_res.status(), StatusCode::CREATED);
    let user_id = user_res.json::<Value>().await.expect("json")["id"]
        .as_i64()
        .expect("id");
    let _ = bearer_json(
        &gw,
        &gw.session,
        reqwest::Method::POST,
        &format!("/users/{user_id}/balance"),
        json!({ "delta_usd_micros": 10_000_000 }),
    )
    .await;

    let login = reqwest::Client::new()
        .post(admin_url(&gw, "/login"))
        .json(&json!({
            "email": "shared-rpm@example.com",
            "password": "password1"
        }))
        .send()
        .await
        .expect("登录");
    let session_token = login.json::<Value>().await.expect("json")["token"]
        .as_str()
        .expect("token")
        .to_string();

    // 两把令牌都显式不限速（rate_limit_rpm: 0），只能被用户桶约束。
    let mut keys = Vec::new();
    for name in ["key-a", "key-b"] {
        let token_res = bearer_json(
            &gw,
            &session_token,
            reqwest::Method::POST,
            "/tokens",
            json!({
                "name": name,
                "model_group": "default",
                "rate_limit_rpm": 0,
                "enabled": true
            }),
        )
        .await;
        assert_eq!(token_res.status(), StatusCode::CREATED);
        keys.push(
            token_res.json::<Value>().await.expect("json")["token_key"]
                .as_str()
                .expect("token_key")
                .to_string(),
        );
    }

    let call = |key: String| {
        let gw = &gw;
        async move {
            reqwest::Client::new()
                .get(format!("{}/v1/models", gw.base_url()))
                .bearer_auth(&key)
                .send()
                .await
                .expect("模型列表应可达")
        }
    };
    // 第一把令牌的首次请求占用用户桶；换第二把（令牌桶全新）仍被用户桶挡住。
    assert_eq!(call(keys[0].clone()).await.status(), StatusCode::OK);
    assert_eq!(
        call(keys[1].clone()).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "用户级 RPM 是名下所有令牌合计的硬性上限"
    );
}
