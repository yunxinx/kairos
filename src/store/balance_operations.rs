//! 用户钱包与令牌余额命令的幂等事实。

use sqlx::{Row, SqliteConnection};

use super::StoreError;

/// 余额命令作用的领域对象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalanceTargetKind {
    UserWallet,
    TokenBalance,
}

impl BalanceTargetKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::UserWallet => "user_wallet",
            Self::TokenBalance => "token_balance",
        }
    }

    fn from_db(raw: &str) -> Result<Self, StoreError> {
        match raw {
            "user_wallet" => Ok(Self::UserWallet),
            "token_balance" => Ok(Self::TokenBalance),
            other => Err(StoreError::InvalidResource(format!(
                "余额操作目标类型非法: {other}"
            ))),
        }
    }
}

/// 余额命令的业务动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalanceOperationKind {
    Adjust,
    SetFinite,
    SetUnlimited,
}

impl BalanceOperationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Adjust => "adjust",
            Self::SetFinite => "set_finite",
            Self::SetUnlimited => "set_unlimited",
        }
    }

    fn from_db(raw: &str) -> Result<Self, StoreError> {
        match raw {
            "adjust" => Ok(Self::Adjust),
            "set_finite" => Ok(Self::SetFinite),
            "set_unlimited" => Ok(Self::SetUnlimited),
            other => Err(StoreError::InvalidResource(format!(
                "余额操作动作非法: {other}"
            ))),
        }
    }
}

/// 一次已提交余额命令的输入指纹与原始结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceOperationRecord {
    pub operation_id: String,
    pub target_kind: BalanceTargetKind,
    pub target_id: i64,
    pub actor_user_id: i64,
    pub operation_kind: BalanceOperationKind,
    pub amount_usd_micros: Option<i64>,
    pub reason: Option<String>,
    pub before_usd_micros: Option<i64>,
    pub after_usd_micros: Option<i64>,
    pub created_at: i64,
}

impl BalanceOperationRecord {
    /// 是否与一次重试携带的完整业务载荷相同。
    pub fn matches_command(
        &self,
        target_kind: BalanceTargetKind,
        target_id: i64,
        operation_kind: BalanceOperationKind,
        amount_usd_micros: Option<i64>,
        reason: Option<&str>,
    ) -> bool {
        self.target_kind == target_kind
            && self.target_id == target_id
            && self.operation_kind == operation_kind
            && self.amount_usd_micros == amount_usd_micros
            && self.reason.as_deref() == reason
    }
}

/// 按操作者、目标与客户端操作 id 读取已提交结果。
pub async fn get_balance_operation(
    conn: &mut SqliteConnection,
    target_kind: BalanceTargetKind,
    target_id: i64,
    actor_user_id: i64,
    operation_id: &str,
) -> Result<Option<BalanceOperationRecord>, StoreError> {
    let row = sqlx::query(
        "SELECT operation_id, target_kind, target_id, actor_user_id, operation_kind, amount_usd_micros, \
         reason, before_usd_micros, after_usd_micros, created_at \
         FROM balance_operations \
         WHERE target_kind = ? AND target_id = ? AND actor_user_id = ? AND operation_id = ?",
    )
    .bind(target_kind.as_str())
    .bind(target_id)
    .bind(actor_user_id)
    .bind(operation_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let target_kind: String = row.try_get("target_kind").map_err(StoreError::Query)?;
    let operation_kind: String = row.try_get("operation_kind").map_err(StoreError::Query)?;
    Ok(Some(BalanceOperationRecord {
        operation_id: row.try_get("operation_id").map_err(StoreError::Query)?,
        target_kind: BalanceTargetKind::from_db(&target_kind)?,
        target_id: row.try_get("target_id").map_err(StoreError::Query)?,
        actor_user_id: row.try_get("actor_user_id").map_err(StoreError::Query)?,
        operation_kind: BalanceOperationKind::from_db(&operation_kind)?,
        amount_usd_micros: row
            .try_get("amount_usd_micros")
            .map_err(StoreError::Query)?,
        reason: row.try_get("reason").map_err(StoreError::Query)?,
        before_usd_micros: row
            .try_get("before_usd_micros")
            .map_err(StoreError::Query)?,
        after_usd_micros: row.try_get("after_usd_micros").map_err(StoreError::Query)?,
        created_at: row.try_get("created_at").map_err(StoreError::Query)?,
    }))
}

/// 记录一次余额命令；调用方须与业务写和审计共用同一事务。
pub async fn insert_balance_operation(
    conn: &mut SqliteConnection,
    record: &BalanceOperationRecord,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO balance_operations (operation_id, target_kind, target_id, actor_user_id, operation_kind, \
         amount_usd_micros, reason, before_usd_micros, after_usd_micros, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&record.operation_id)
    .bind(record.target_kind.as_str())
    .bind(record.target_id)
    .bind(record.actor_user_id)
    .bind(record.operation_kind.as_str())
    .bind(record.amount_usd_micros)
    .bind(&record.reason)
    .bind(record.before_usd_micros)
    .bind(record.after_usd_micros)
    .bind(record.created_at)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}
