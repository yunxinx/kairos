//! 令牌额度命令。
//!
//! 额度的计算、幂等与审计逻辑集中在此；独立余额端点和令牌复合更新共用同一个
//! 事务内执行入口。

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
use crate::store::resources::TokenRecord;

use super::auth::ManagementIdentity;
use super::tokens::{available_balance, parse_token_id, reject_cross_owner_mutation};
use super::{
    AdminDeps, AdminError, BalanceAdjustmentResult, begin_write, db_err, reload_and_swap,
    validate_operation_id,
};

/// 令牌余额写命令；模式切换与相对调整互斥，非法字段组合无法进入处理逻辑。
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum TokenBalanceCommand {
    Adjust {
        operation_id: String,
        delta_usd_micros: i64,
    },
    SetFinite {
        operation_id: String,
        balance_usd_micros: i64,
    },
    SetUnlimited {
        operation_id: String,
    },
}

impl TokenBalanceCommand {
    fn operation_id(&self) -> &str {
        match self {
            Self::Adjust { operation_id, .. }
            | Self::SetFinite { operation_id, .. }
            | Self::SetUnlimited { operation_id } => operation_id,
        }
    }

    fn operation_kind(&self) -> BalanceOperationKind {
        match self {
            Self::Adjust { .. } => BalanceOperationKind::Adjust,
            Self::SetFinite { .. } => BalanceOperationKind::SetFinite,
            Self::SetUnlimited { .. } => BalanceOperationKind::SetUnlimited,
        }
    }

    fn amount_usd_micros(&self) -> Option<i64> {
        match self {
            Self::Adjust {
                delta_usd_micros, ..
            } => Some(*delta_usd_micros),
            Self::SetFinite {
                balance_usd_micros, ..
            } => Some(*balance_usd_micros),
            Self::SetUnlimited { .. } => None,
        }
    }

    fn validate(&self) -> Result<(), AdminError> {
        validate_operation_id(self.operation_id())?;
        match self {
            Self::Adjust {
                delta_usd_micros: 0,
                ..
            } => Err(AdminError::InvalidBody(
                "delta_usd_micros 不能为 0".to_string(),
            )),
            Self::SetFinite {
                balance_usd_micros, ..
            } if *balance_usd_micros < 0 => Err(AdminError::InvalidBody(
                "balance_usd_micros 不能为负".to_string(),
            )),
            _ => Ok(()),
        }
    }
}

/// 在调用方已经开启的写事务中执行令牌余额命令。
///
/// 该函数只负责余额、幂等事实和审计，不提交事务也不重载运行时快照，因而可与
/// 令牌属性更新共享同一个原子提交点。
pub(super) async fn apply_token_balance_command(
    conn: &mut sqlx::SqliteConnection,
    identity: &ManagementIdentity,
    id: i64,
    existing: &TokenRecord,
    command: TokenBalanceCommand,
    audit_name: &str,
) -> Result<BalanceOperationRecord, AdminError> {
    command.validate()?;
    let operation_id = command.operation_id().to_string();
    let operation_kind = command.operation_kind();
    let amount_usd_micros = command.amount_usd_micros();

    if let Some(record) = store::balance_operations::get_balance_operation(
        conn,
        BalanceTargetKind::TokenBalance,
        id,
        identity.user_id(),
        &operation_id,
    )
    .await
    .map_err(AdminError::Store)?
    {
        if !record.matches_command(
            BalanceTargetKind::TokenBalance,
            id,
            operation_kind,
            amount_usd_micros,
            None,
        ) {
            return Err(AdminError::Conflict(
                "operation_id 已用于不同的余额操作".to_string(),
            ));
        }
        return Ok(record);
    }

    let settled_usd_micros = store::get_token_settlement(conn, &existing.token.token_key)
        .await
        .map_err(AdminError::Store)?
        .map(|settlement| settlement.settled_usd_micros)
        .unwrap_or(0);
    let before_usd_micros = available_balance(existing.token.limit_usd_micros, settled_usd_micros)?;

    let (next_limit_usd_micros, after_usd_micros, audit_message) = match command {
        TokenBalanceCommand::Adjust {
            delta_usd_micros, ..
        } => {
            let current_limit = existing.token.limit_usd_micros.ok_or_else(|| {
                AdminError::Conflict("无限额令牌不能直接调整余额，请先切换为有限额".to_string())
            })?;
            let next_limit = current_limit
                .checked_add(delta_usd_micros)
                .ok_or_else(|| AdminError::InvalidBody("余额调整超出整数范围".to_string()))?;
            if next_limit < 0 {
                return Err(AdminError::InvalidBody(
                    "余额扣减后累计消费上限不能为负".to_string(),
                ));
            }
            let after = available_balance(Some(next_limit), settled_usd_micros)?;
            let before = before_usd_micros.ok_or_else(|| {
                AdminError::Store(store::StoreError::InvalidResource(
                    "有限额令牌缺少可用余额".to_string(),
                ))
            })?;
            let after_amount = after.ok_or_else(|| {
                AdminError::Store(store::StoreError::InvalidResource(
                    "有限额令牌调整后缺少可用余额".to_string(),
                ))
            })?;
            (
                Some(next_limit),
                after,
                format!(
                    "令牌 {} ({}) 余额 {}{} USD（{} → {}）",
                    id,
                    audit_name,
                    if delta_usd_micros > 0 { "+" } else { "" },
                    super::format_usd_micros(delta_usd_micros),
                    super::format_usd_micros(before),
                    super::format_usd_micros(after_amount)
                ),
            )
        }
        TokenBalanceCommand::SetFinite {
            balance_usd_micros, ..
        } => {
            if existing.token.limit_usd_micros.is_some() {
                return Err(AdminError::Conflict(
                    "令牌已经是有限额模式，请使用相对余额调整".to_string(),
                ));
            }
            let next_limit = settled_usd_micros
                .checked_add(balance_usd_micros)
                .ok_or_else(|| AdminError::InvalidBody("初始余额超出整数范围".to_string()))?;
            (
                Some(next_limit),
                Some(balance_usd_micros),
                format!(
                    "令牌 {} ({}) 从无限额切换为有限额，初始余额 {} USD",
                    id,
                    audit_name,
                    super::format_usd_micros(balance_usd_micros)
                ),
            )
        }
        TokenBalanceCommand::SetUnlimited { .. } => (
            None,
            None,
            format!("令牌 {} ({}) 切换为无限额", id, audit_name),
        ),
    };

    let changed = next_limit_usd_micros != existing.token.limit_usd_micros;
    if changed {
        store::resources::set_token_limit(conn, id, next_limit_usd_micros)
            .await
            .map_err(AdminError::Store)?;
    }
    let record = BalanceOperationRecord {
        operation_id,
        target_kind: BalanceTargetKind::TokenBalance,
        target_id: id,
        actor_user_id: identity.user_id(),
        operation_kind,
        amount_usd_micros,
        reason: None,
        before_usd_micros,
        after_usd_micros,
        created_at: logging::unix_millis(),
    };
    store::balance_operations::insert_balance_operation(conn, &record)
        .await
        .map_err(AdminError::Store)?;
    if changed {
        let event_code = match operation_kind {
            BalanceOperationKind::Adjust => "billing.token_balance_adjusted",
            BalanceOperationKind::SetFinite => "billing.token_balance_set_finite",
            BalanceOperationKind::SetUnlimited => "billing.token_balance_set_unlimited",
        };
        store::record_audit(
            conn,
            identity.actor(),
            "billing",
            &store::SystemLogEvent::new(
                event_code,
                serde_json::json!({
                    "token_id": id,
                    "token_name": audit_name,
                    "delta_usd_micros": amount_usd_micros,
                    "before_usd_micros": before_usd_micros,
                    "after_usd_micros": after_usd_micros,
                }),
                audit_message,
            ),
        )
        .await
        .map_err(AdminError::Store)?;
    }
    Ok(record)
}

pub(super) async fn adjust_token_balance(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(raw_id): Path<String>,
    body: Result<Json<TokenBalanceCommand>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<BalanceAdjustmentResult>, AdminError> {
    let id = parse_token_id(&raw_id)?;
    let command = body.map_err(AdminError::bad_body)?.0;
    command.validate()?;

    let mut tx = begin_write(&deps).await?;
    let existing = store::resources::get_token_record_on_conn(&mut tx, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("令牌 {id} 不存在")))?;
    reject_cross_owner_mutation(&identity, &existing)?;
    let record = apply_token_balance_command(
        &mut tx,
        &identity,
        id,
        &existing,
        command,
        &existing.token.name,
    )
    .await?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(balance_operation_result(record)))
}

fn balance_operation_result(record: BalanceOperationRecord) -> BalanceAdjustmentResult {
    BalanceAdjustmentResult {
        operation_id: record.operation_id,
        before_balance_usd_micros: record.before_usd_micros,
        after_balance_usd_micros: record.after_usd_micros,
    }
}
