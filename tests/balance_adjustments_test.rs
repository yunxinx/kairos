//! 钱包与令牌余额命令：幂等、契约隔离与有限/无限模式。

mod common;

use common::TestGateway;
use reqwest::StatusCode;
use serde_json::{Value, json};

fn admin_url(gw: &TestGateway, path: &str) -> String {
    format!("{}{path}", gw.admin_base_url())
}

async fn post(gw: &TestGateway, path: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(admin_url(gw, path))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&body)
        .send()
        .await
        .expect("管理请求应可达")
}

#[tokio::test]
async fn user_balance_adjustment_replay_returns_original_result_without_double_credit() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let path = "/users/1/balance-adjustments";
    let body = json!({
        "operation_id": "user-adjustment-1",
        "delta_usd_micros": 1_000_000,
        "reason": "manual_adjustment"
    });

    let first = post(&gw, path, body.clone()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first: Value = first.json().await.expect("结果应可解析");
    assert_eq!(first["before_balance_usd_micros"], 5_000_000);
    assert_eq!(first["after_balance_usd_micros"], 6_000_000);

    let replay = post(&gw, path, body).await;
    assert_eq!(replay.status(), StatusCode::OK);
    let replay: Value = replay.json().await.expect("重放结果应可解析");
    assert_eq!(replay, first, "重放必须返回第一次提交的原始结果");

    let reused = post(
        &gw,
        path,
        json!({
            "operation_id": "user-adjustment-1",
            "delta_usd_micros": 2_000_000,
            "reason": "manual_adjustment"
        }),
    )
    .await;
    assert_eq!(reused.status(), StatusCode::CONFLICT);

    let token = post(
        &gw,
        "/tokens",
        json!({
            "name": "operation-scope",
            "balance_usd_micros": 1_000_000,
            "enabled": true
        }),
    )
    .await;
    assert_eq!(token.status(), StatusCode::CREATED);
    let token: Value = token.json().await.expect("令牌应可解析");
    let token_id = token["id"].as_i64().expect("应有令牌 id");
    let other_target = post(
        &gw,
        &format!("/tokens/{token_id}/balance-adjustments"),
        json!({
            "action": "adjust",
            "operation_id": "user-adjustment-1",
            "delta_usd_micros": 1_000_000
        }),
    )
    .await;
    assert_eq!(
        other_target.status(),
        StatusCode::OK,
        "不同目标资源的幂等键必须相互隔离"
    );

    let (balance,): (i64,) =
        sqlx::query_as("SELECT balance_usd_micros FROM user_balance WHERE user_id = 1")
            .fetch_one(&gw.pool)
            .await
            .expect("钱包应存在");
    assert_eq!(balance, 6_000_000, "同一操作只能入账一次");
}

#[tokio::test]
async fn token_attributes_and_balance_commands_have_disjoint_write_surfaces() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let created = post(
        &gw,
        "/tokens",
        json!({
            "name": "bounded",
            "balance_usd_micros": 10_000_000,
            "enabled": true
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await.expect("令牌应可解析");
    let id = created["id"].as_i64().expect("应有令牌 id");
    assert_eq!(created["balance_usd_micros"], 10_000_000);

    let bad_update = reqwest::Client::new()
        .put(admin_url(&gw, &format!("/tokens/{id}")))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({
            "name": "must-not-land",
            "enabled": true,
            "limit_usd_micros": 99_000_000
        }))
        .send()
        .await
        .expect("属性更新应可达");
    assert_eq!(bad_update.status(), StatusCode::BAD_REQUEST);

    let updated = reqwest::Client::new()
        .put(admin_url(&gw, &format!("/tokens/{id}")))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({ "name": "renamed", "enabled": true }))
        .send()
        .await
        .expect("属性更新应可达");
    assert_eq!(updated.status(), StatusCode::OK);
    let updated: Value = updated.json().await.expect("令牌应可解析");
    assert_eq!(updated["name"], "renamed");
    assert_eq!(updated["balance_usd_micros"], 10_000_000);

    let adjustment = json!({
        "action": "adjust",
        "operation_id": "token-adjustment-1",
        "delta_usd_micros": 5_000_000
    });
    let first = post(
        &gw,
        &format!("/tokens/{id}/balance-adjustments"),
        adjustment.clone(),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let first: Value = first.json().await.expect("余额结果应可解析");
    assert_eq!(first["before_balance_usd_micros"], 10_000_000);
    assert_eq!(first["after_balance_usd_micros"], 15_000_000);

    let replay = post(
        &gw,
        &format!("/tokens/{id}/balance-adjustments"),
        adjustment,
    )
    .await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.json::<Value>().await.expect("重放应可解析"), first);

    let (limit,): (Option<i64>,) =
        sqlx::query_as("SELECT limit_usd_micros FROM tokens WHERE id = ?")
            .bind(id)
            .fetch_one(&gw.pool)
            .await
            .expect("令牌应存在");
    assert_eq!(limit, Some(15_000_000));
}

#[tokio::test]
async fn token_update_commits_attributes_and_balance_atomically() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let created = post(
        &gw,
        "/tokens",
        json!({
            "name": "atomic-before",
            "balance_usd_micros": 10_000_000,
            "rate_limit_rpm": 30,
            "enabled": true
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await.expect("令牌应可解析");
    let id = created["id"].as_i64().expect("应有令牌 id");
    let path = format!("/tokens/{id}");

    let updated = reqwest::Client::new()
        .put(admin_url(&gw, &path))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({
            "name": "atomic-after",
            "rate_limit_rpm": 60,
            "enabled": false,
            "balance_change": {
                "action": "adjust",
                "operation_id": "token-atomic-success",
                "delta_usd_micros": 2_000_000
            }
        }))
        .send()
        .await
        .expect("复合更新应可达");
    assert_eq!(updated.status(), StatusCode::OK);
    let updated: Value = updated.json().await.expect("令牌应可解析");
    assert_eq!(updated["name"], "atomic-after");
    assert_eq!(updated["rate_limit_rpm"], 60);
    assert_eq!(updated["enabled"], false);
    assert_eq!(updated["balance_usd_micros"], 12_000_000);

    let replay = reqwest::Client::new()
        .put(admin_url(&gw, &path))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({
            "name": "replayed-attributes",
            "rate_limit_rpm": 90,
            "enabled": true,
            "balance_change": {
                "action": "adjust",
                "operation_id": "token-atomic-success",
                "delta_usd_micros": 2_000_000
            }
        }))
        .send()
        .await
        .expect("幂等重试应可达");
    assert_eq!(replay.status(), StatusCode::OK);
    let replay: Value = replay.json().await.expect("令牌应可解析");
    assert_eq!(replay["name"], "replayed-attributes");
    assert_eq!(replay["rate_limit_rpm"], 90);
    assert_eq!(replay["balance_usd_micros"], 12_000_000);

    let failed = reqwest::Client::new()
        .put(admin_url(&gw, &path))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({
            "name": "must-roll-back",
            "rate_limit_rpm": 120,
            "enabled": false,
            "balance_change": {
                "action": "adjust",
                "operation_id": "token-atomic-failure",
                "delta_usd_micros": -20_000_000
            }
        }))
        .send()
        .await
        .expect("失败更新应可达");
    assert_eq!(failed.status(), StatusCode::BAD_REQUEST);

    let conflict = reqwest::Client::new()
        .put(admin_url(&gw, &path))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .json(&json!({
            "name": "must-also-roll-back",
            "rate_limit_rpm": 120,
            "enabled": false,
            "balance_change": {
                "action": "adjust",
                "operation_id": "token-atomic-success",
                "delta_usd_micros": 3_000_000
            }
        }))
        .send()
        .await
        .expect("幂等冲突应可达");
    assert_eq!(conflict.status(), StatusCode::CONFLICT);

    let (name, rate_limit_rpm, enabled, limit): (String, Option<i64>, bool, Option<i64>) =
        sqlx::query_as(
            "SELECT name, rate_limit_rpm, enabled, limit_usd_micros FROM tokens WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&gw.pool)
        .await
        .expect("令牌应存在");
    assert_eq!(name, "replayed-attributes");
    assert_eq!(rate_limit_rpm, Some(90));
    assert!(enabled);
    assert_eq!(limit, Some(12_000_000));
}

#[tokio::test]
async fn token_mode_changes_are_explicit_and_finite_balance_is_derived_from_settlement() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let created = post(
        &gw,
        "/tokens",
        json!({
            "name": "mode-switch",
            "balance_usd_micros": 4_000_000,
            "enabled": true
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await.expect("令牌应可解析");
    let id = created["id"].as_i64().expect("应有令牌 id");
    let key = created["token_key"].as_str().expect("应有 key");
    let path = format!("/tokens/{id}/balance-adjustments");

    let unlimited = post(
        &gw,
        &path,
        json!({ "action": "set_unlimited", "operation_id": "token-unlimited-1" }),
    )
    .await;
    assert_eq!(unlimited.status(), StatusCode::OK);
    let unlimited: Value = unlimited.json().await.expect("结果应可解析");
    assert_eq!(unlimited["before_balance_usd_micros"], 4_000_000);
    assert!(unlimited["after_balance_usd_micros"].is_null());

    let invalid_adjustment = post(
        &gw,
        &path,
        json!({
            "action": "adjust",
            "operation_id": "token-adjust-unlimited",
            "delta_usd_micros": 1_000_000
        }),
    )
    .await;
    assert_eq!(invalid_adjustment.status(), StatusCode::CONFLICT);

    sqlx::query("UPDATE token_balance SET settled_usd_micros = 3_000_000 WHERE token_key = ?")
        .bind(key)
        .execute(&gw.pool)
        .await
        .expect("应能模拟累计结算");
    let finite = post(
        &gw,
        &path,
        json!({
            "action": "set_finite",
            "operation_id": "token-finite-1",
            "balance_usd_micros": 7_000_000
        }),
    )
    .await;
    assert_eq!(finite.status(), StatusCode::OK);
    let finite: Value = finite.json().await.expect("结果应可解析");
    assert!(finite["before_balance_usd_micros"].is_null());
    assert_eq!(finite["after_balance_usd_micros"], 7_000_000);

    let (limit,): (Option<i64>,) =
        sqlx::query_as("SELECT limit_usd_micros FROM tokens WHERE id = ?")
            .bind(id)
            .fetch_one(&gw.pool)
            .await
            .expect("令牌应存在");
    assert_eq!(
        limit,
        Some(10_000_000),
        "有限额上限必须按 settled + 可用余额计算"
    );
}

#[tokio::test]
async fn delete_token_returns_the_balance_observed_before_settlement_cleanup() {
    let gw = TestGateway::start_with_admin(common::test_seed).await;
    let created = post(
        &gw,
        "/tokens",
        json!({
            "name": "delete-balance",
            "balance_usd_micros": 10_000_000,
            "enabled": true
        }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await.expect("令牌应可解析");
    let id = created["id"].as_i64().expect("应有令牌 id");
    let key = created["token_key"].as_str().expect("应有 key");
    sqlx::query("UPDATE token_balance SET settled_usd_micros = 3_000_000 WHERE token_key = ?")
        .bind(key)
        .execute(&gw.pool)
        .await
        .expect("应能模拟累计结算");

    let deleted = reqwest::Client::new()
        .delete(admin_url(&gw, &format!("/tokens/{id}")))
        .header(reqwest::header::COOKIE, &gw.session)
        .header(reqwest::header::ORIGIN, gw.admin_origin())
        .send()
        .await
        .expect("删除请求应可达");
    assert_eq!(deleted.status(), StatusCode::OK);
    let deleted: Value = deleted.json().await.expect("删除响应应可解析");
    assert_eq!(deleted["settled_usd_micros"], 3_000_000);
    assert_eq!(deleted["balance_usd_micros"], 7_000_000);
}
