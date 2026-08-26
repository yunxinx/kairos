//! 用户钱包的人工相对调整。
//!
//! 钱包命令独立于用户资料与套餐管理：同一操作 id 在同一用户上幂等，业务写、
//! 幂等事实与审计行共用一个写事务。

use axum::{
    Extension, Json,
    extract::{Path, State},
};
use serde::Deserialize;

use crate::gateway::logging;
use crate::store;
use crate::store::balance_operations::{
    BalanceOperationKind, BalanceOperationRecord, BalanceTargetKind,
};
use crate::store::users::{self, ManagementRole};

use super::auth::ManagementIdentity;
use super::{
    AdminDeps, AdminError, BalanceAdjustmentResult, begin_write, db_err, format_usd_micros,
    map_user_store_err, reject_user_management, validate_operation_id,
};

/// 人工钱包调整的业务原因；它也是幂等载荷的一部分。
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BalanceAdjustmentReason {
    ManualAdjustment,
}

impl BalanceAdjustmentReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ManualAdjustment => "manual_adjustment",
        }
    }
}

/// 余额调整请求体：相对量为正时充值、为负时扣减。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BalanceAdjustment {
    operation_id: String,
    delta_usd_micros: i64,
    reason: BalanceAdjustmentReason,
}

pub(super) async fn adjust_user_balance(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
    body: Result<Json<BalanceAdjustment>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<BalanceAdjustmentResult>, AdminError> {
    identity.require_capability(super::auth::ManagementCapability::ManageUsers)?;
    let adjustment = body.map_err(AdminError::bad_body)?.0;
    validate_operation_id(&adjustment.operation_id)?;
    if adjustment.delta_usd_micros == 0 {
        return Err(AdminError::InvalidBody(
            "delta_usd_micros 不能为 0".to_string(),
        ));
    }
    let delta = adjustment.delta_usd_micros;
    let reason = adjustment.reason.as_str();
    let mut tx = begin_write(&deps).await?;
    // 归档用户的钱包仍须可对账：补扣路径（结算/豁免）已允许 root 触碰归档账户，
    // 充值走同一语义。非 root 视角归档与不存在同响应（404）。
    let (target, archived) = match users::get_user_on_conn(&mut tx, id)
        .await
        .map_err(AdminError::Store)?
    {
        Some(target) => (target, false),
        None => match users::get_user_including_archived_on_conn(&mut tx, id)
            .await
            .map_err(AdminError::Store)?
        {
            Some(target) if identity.role() == ManagementRole::Root => (target, true),
            _ => return Err(AdminError::NotFound(format!("用户 {id} 不存在"))),
        },
    };
    reject_user_management(&identity, &target, None)?;

    if let Some(record) = store::balance_operations::get_balance_operation(
        &mut tx,
        BalanceTargetKind::UserWallet,
        id,
        identity.user_id(),
        &adjustment.operation_id,
    )
    .await
    .map_err(AdminError::Store)?
    {
        if !record.matches_command(
            BalanceTargetKind::UserWallet,
            id,
            BalanceOperationKind::Adjust,
            Some(delta),
            Some(reason),
        ) {
            return Err(AdminError::Conflict(
                "operation_id 已用于不同的余额操作".to_string(),
            ));
        }
        tx.commit().await.map_err(db_err)?;
        return Ok(Json(BalanceAdjustmentResult {
            operation_id: record.operation_id,
            before_balance_usd_micros: record.before_usd_micros,
            after_balance_usd_micros: record.after_usd_micros,
        }));
    }

    let change = store::adjust_user_balance(&mut tx, id, delta)
        .await
        .map_err(map_user_store_err)?;
    let record = BalanceOperationRecord {
        operation_id: adjustment.operation_id,
        target_kind: BalanceTargetKind::UserWallet,
        target_id: id,
        actor_user_id: identity.user_id(),
        operation_kind: BalanceOperationKind::Adjust,
        amount_usd_micros: Some(delta),
        reason: Some(reason.to_string()),
        before_usd_micros: Some(change.before_usd_micros),
        after_usd_micros: Some(change.after_usd_micros),
        created_at: logging::unix_millis(),
    };
    store::balance_operations::insert_balance_operation(&mut tx, &record)
        .await
        .map_err(AdminError::Store)?;
    store::record_audit(
        &mut tx,
        identity.actor(),
        "billing",
        &store::SystemLogEvent::new(
            "billing.user_balance_adjusted",
            serde_json::json!({
                "user_id": id,
                "email": target.email,
                "delta_usd_micros": delta,
                "before_usd_micros": change.before_usd_micros,
                "after_usd_micros": change.after_usd_micros,
                "archived": archived,
            }),
            format!(
                "用户 {} ({}) 余额 {}{} USD（{} → {}）{}",
                id,
                target.email,
                if delta > 0 { "+" } else { "" },
                format_usd_micros(delta),
                format_usd_micros(change.before_usd_micros),
                format_usd_micros(change.after_usd_micros),
                if archived { "（已归档）" } else { "" }
            ),
        ),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    Ok(Json(BalanceAdjustmentResult {
        operation_id: record.operation_id,
        before_balance_usd_micros: record.before_usd_micros,
        after_balance_usd_micros: record.after_usd_micros,
    }))
}
