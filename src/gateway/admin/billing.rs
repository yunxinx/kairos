//! 管理面的历史计费对账。
//!
//! 结算与豁免共享一个事务闭环：先以日志冻结的用户归属做授权，再在 store 中原子
//! 更新钱包和日志状态，最后写同一事务的审计行。资源 CRUD 不需要了解这些细节。

use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    routing::post,
};

use crate::store;
use crate::store::users::{self, ManagementRole};

use super::auth::ManagementIdentity;
use super::logs::{LogEntry, parse_log_id};
use super::{
    AdminDeps, AdminError, begin_write, db_err, format_usd_micros, reject_user_management,
};

pub(super) fn routes() -> Router<AdminDeps> {
    Router::new()
        .route("/logs/{id}/settle", post(settle_log))
        .route("/logs/{id}/waive", post(waive_log))
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
        &format!(
            "{}未结算日志 {}（用户 {}，{} USD）",
            if charge { "补扣" } else { "豁免" },
            id,
            log.user_id,
            format_usd_micros(log.cost_usd_micros)
        ),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    let log = store::get_request_log(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("日志 {id} 不存在")))?;
    Ok(Json(LogEntry::from_store_log(log)))
}
