//! 管理面的历史计费对账。
//!
//! 结算与豁免共享一个事务闭环：先以日志冻结的用户归属做授权，再在 store 中原子
//! 更新钱包和日志状态，最后写同一事务的审计行。资源 CRUD 不需要了解这些细节。

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use crate::store;
use crate::store::users::{self, ManagementRole};

use super::auth::{ManagementCapability, ManagementIdentity};
use super::logs::{LogEntry, parse_log_id};
use super::{
    AdminDeps, AdminError, begin_write, db_err, format_usd_micros, reject_user_management,
};

pub(super) fn routes() -> Router<AdminDeps> {
    Router::new()
        .route("/logs/{id}/settle", post(settle_log))
        .route("/logs/{id}/waive", post(waive_log))
        .route("/logs/isolated", get(query_isolated))
        .route("/logs/isolated/{attempt_id}/replay", post(replay_isolated))
        .route(
            "/logs/isolated/outbox/{id}/replay",
            post(replay_isolated_by_id),
        )
}

#[derive(Debug, Deserialize)]
struct IsolatedQuery {
    limit: Option<u64>,
}

#[derive(Debug, Serialize)]
struct IsolatedEntry {
    id: i64,
    request_id: Option<String>,
    billing_attempt_id: Option<String>,
    token_key: String,
    user_id: i64,
    attempt_count: i64,
    next_retry_at: Option<i64>,
    last_error: Option<String>,
    usage_reported: Option<bool>,
    request_body_present: bool,
    response_body_present: bool,
    log: Option<LogEntry>,
}

#[derive(Debug, Serialize)]
struct IsolatedPage {
    items: Vec<IsolatedEntry>,
}

/// 查询不阻塞主队列的隔离记录，保留正文存在性但不直接暴露原始凭证。
async fn query_isolated(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    query: Result<Query<IsolatedQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<IsolatedPage>, AdminError> {
    identity.require_capability(ManagementCapability::SettleWaive)?;
    let limit = query
        .map_err(|rejection| AdminError::InvalidBody(format!("查询参数非法: {rejection}")))?
        .0
        .limit
        .unwrap_or(100)
        .clamp(1, 500);
    let rows = store::query_isolated_request_logs_scoped(
        &deps.pool,
        limit as i64,
        identity.role() == ManagementRole::Root,
    )
    .await
    .map_err(AdminError::Store)?;
    let reveal_topology = identity.role().at_least(ManagementRole::Admin);
    Ok(Json(IsolatedPage {
        items: rows
            .into_iter()
            .map(|row| IsolatedEntry {
                id: row.id,
                request_id: row.request_id,
                billing_attempt_id: row.billing_attempt_id,
                token_key: super::tokens::mask_token_key(&row.token_key),
                user_id: row.user_id,
                attempt_count: row.attempt_count,
                next_retry_at: row.next_retry_at,
                last_error: row.last_error,
                usage_reported: row.log.as_ref().map(|log| log.usage_reported),
                request_body_present: row.request_body.is_some(),
                response_body_present: row.response_body.is_some(),
                log: row
                    .log
                    .map(|log| LogEntry::from_store_log(log, reveal_topology)),
            })
            .collect(),
    }))
}

/// 将指定隔离记录重新放回结算队列；重复调用保持幂等并写入审计。
async fn replay_isolated(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(attempt_id): Path<String>,
) -> Result<Json<serde_json::Value>, AdminError> {
    identity.require_capability(ManagementCapability::SettleWaive)?;
    if attempt_id.trim().is_empty() || attempt_id.len() > 128 {
        return Err(AdminError::InvalidBody("计费尝试标识无效".to_string()));
    }
    let action = store::requeue_isolated_request_log(
        &deps.pool,
        &attempt_id,
        identity.role() == ManagementRole::Root,
    )
    .await
    .map_err(|err| match err {
        store::StoreError::PermissionDenied => AdminError::Forbidden,
        other => AdminError::Store(other),
    })?;
    let action_name = match action {
        store::IsolatedReplayAction::Requeued => "requeued",
        store::IsolatedReplayAction::AlreadyQueued => "already_queued",
        store::IsolatedReplayAction::AlreadySettled => "already_settled",
        store::IsolatedReplayAction::NotFound => {
            return Err(AdminError::NotFound(format!(
                "隔离计费尝试 {attempt_id} 不存在"
            )));
        }
    };
    store::record_audit_detached(
        &deps.pool,
        Some(identity.actor()),
        "info",
        "billing",
        &store::SystemLogEvent::new(
            "billing.isolated_replayed",
            serde_json::json!({ "billing_attempt_id": attempt_id, "action": action_name }),
            format!("重新入队隔离计费尝试 {attempt_id}: {action_name}"),
        ),
    )
    .await;
    deps.request_log_writer.wake();
    Ok(Json(serde_json::json!({
        "billing_attempt_id": attempt_id,
        "action": action_name,
    })))
}

/// 按 outbox 行 id 重放没有计费尝试身份的隔离记录。
async fn replay_isolated_by_id(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(raw): Path<String>,
) -> Result<Json<serde_json::Value>, AdminError> {
    identity.require_capability(ManagementCapability::SettleWaive)?;
    let id = raw
        .parse::<i64>()
        .map_err(|_| AdminError::NotFound(format!("隔离 outbox {raw} 不存在")))?;
    if id <= 0 {
        return Err(AdminError::NotFound(format!("隔离 outbox {raw} 不存在")));
    }
    let action = store::requeue_isolated_request_log_by_id(
        &deps.pool,
        id,
        identity.role() == ManagementRole::Root,
    )
    .await
    .map_err(|err| match err {
        store::StoreError::PermissionDenied => AdminError::Forbidden,
        other => AdminError::Store(other),
    })?;
    let action_name = match action {
        store::IsolatedReplayAction::Requeued => "requeued",
        store::IsolatedReplayAction::AlreadyQueued => "already_queued",
        store::IsolatedReplayAction::AlreadySettled => "already_settled",
        store::IsolatedReplayAction::NotFound => {
            return Err(AdminError::NotFound(format!("隔离 outbox {id} 不存在")));
        }
    };
    store::record_audit_detached(
        &deps.pool,
        Some(identity.actor()),
        "info",
        "billing",
        &store::SystemLogEvent::new(
            "billing.isolated_replayed",
            serde_json::json!({ "outbox_id": id, "action": action_name }),
            format!("重新入队隔离 outbox {id}: {action_name}"),
        ),
    )
    .await;
    deps.request_log_writer.wake();
    Ok(Json(serde_json::json!({
        "outbox_id": id,
        "action": action_name,
    })))
}

/// 对未结算日志补扣：按行上费用写入余额（允许透支），再标为已结算。
pub(super) async fn settle_log(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(raw): Path<String>,
) -> Result<Json<LogEntry>, AdminError> {
    close_unsettled_log(&deps, &identity, &raw, true).await
}

/// 豁免未结算日志：只翻 `settled`，不改余额。
pub(super) async fn waive_log(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(raw): Path<String>,
) -> Result<Json<LogEntry>, AdminError> {
    close_unsettled_log(&deps, &identity, &raw, false).await
}

/// 未结算闭环：`charge` 为 true 时补扣，否则豁免。
///
/// 路由层已要求 admin+；这里再按日志归属用户过一次 `reject_user_management`，使
/// admin 只能处理普通用户的行，不能动 root/其他 admin 的账。归档用户仍可被 root
/// 处理，历史钱包不会因为账户停用而失去可追溯性。
async fn close_unsettled_log(
    deps: &AdminDeps,
    identity: &ManagementIdentity,
    raw: &str,
    charge: bool,
) -> Result<Json<LogEntry>, AdminError> {
    identity.require_capability(ManagementCapability::SettleWaive)?;
    let id = parse_log_id(raw)?;
    let mut tx = begin_write(deps).await?;
    let log = store::get_request_log_on_conn(&mut tx, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("日志 {id} 不存在")))?;
    // 存量行的 user_id 为 0（迁移前无归属），此时无从判定越权，只允许 root 处理。
    if log.user_id == 0 {
        if identity.role() != ManagementRole::Root {
            return Err(AdminError::Forbidden);
        }
    } else {
        let owner = users::get_user_including_archived_on_conn(&mut tx, log.user_id)
            .await
            .map_err(AdminError::Store)?
            .ok_or_else(|| AdminError::NotFound(format!("用户 {} 不存在", log.user_id)))?;
        reject_user_management(identity, &owner, None)?;
    }
    let outcome = if charge {
        store::settle_unsettled_log(&mut tx, id).await
    } else {
        store::waive_unsettled_log(&mut tx, id).await
    }
    .map_err(AdminError::Store)?;
    match outcome {
        store::UnsettledLogAction::NotFound => {
            return Err(AdminError::NotFound(format!("日志 {id} 不存在")));
        }
        store::UnsettledLogAction::AlreadySettled => {
            return Err(AdminError::Conflict(format!("日志 {id} 已结算")));
        }
        store::UnsettledLogAction::Closed => {}
    }
    store::record_audit(
        &mut tx,
        identity.actor(),
        "billing",
        &store::SystemLogEvent::new(
            if charge {
                "billing.log_charged"
            } else {
                "billing.log_waived"
            },
            serde_json::json!({
                "log_id": id,
                "user_id": log.user_id,
                "cost_usd_micros": log.cost_usd_micros,
            }),
            format!(
                "{}未结算日志 {}（用户 {}，{} USD）",
                if charge { "补扣" } else { "豁免" },
                id,
                log.user_id,
                format_usd_micros(log.cost_usd_micros)
            ),
        ),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    let log = store::get_request_log(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("日志 {id} 不存在")))?;
    Ok(Json(LogEntry::from_store_log(log, true)))
}
