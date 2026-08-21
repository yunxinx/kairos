//! 系统日志：结算失败、落库失败、目录同步失败等运维事件，与请求日志分表。

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection, SqlitePool};

use super::{
    SortDir, StoreError, as_count, clamp_page, like_substring_pattern, push_column_in,
    push_created_at_range, push_limit_offset, push_where_cond,
};

/// 系统日志行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemLog {
    pub id: i64,
    pub created_at: i64,
    pub level: String,
    pub target: String,
    pub message: String,
    /// 操作者 id；系统自身产生的运维事件为 `None`。
    pub actor_user_id: Option<i64>,
    /// 操作者邮箱（写入时冗余定格）。
    ///
    /// 不只存 id：用户可被归档改名，审计行要能独立还原「当时是谁」。
    pub actor_email: Option<String>,
}

/// 审计事件的操作者。
#[derive(Debug, Clone, Copy)]
pub struct Actor<'a> {
    pub user_id: i64,
    pub email: &'a str,
}

/// 系统日志可排序列：只有时间有顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemLogSortBy {
    #[default]
    Created,
}

/// 系统日志查询：分页 + 可选关键字/时间窗/级别/目标。
#[derive(Debug, Clone, Default)]
pub struct SystemLogQuery {
    pub keyword: Option<String>,
    pub from_created_at: Option<i64>,
    pub to_created_at: Option<i64>,
    /// 精确匹配的级别；空表示不限。
    pub levels: Vec<String>,
    /// 精确匹配的目标；空表示不限。
    pub targets: Vec<String>,
    /// 按操作者过滤；`None` 表示不限。
    pub actor_user_id: Option<i64>,
    /// 排序列；缺省时间。
    pub sort_by: SystemLogSortBy,
    /// 排序方向；缺省倒序。
    pub sort_dir: SortDir,
    pub page: u64,
    pub page_size: u64,
}

impl SystemLogQuery {
    /// 用必填分页构造查询，过滤维度缺省为空。
    pub fn new(page: u64, page_size: u64) -> Self {
        let (page, page_size) = clamp_page(page, page_size);
        Self {
            page,
            page_size,
            ..Self::default()
        }
    }
}

/// 分页结果：本页行 + 总数 + 可供分面筛选的 target（忽略 target 维）。
pub struct SystemLogList {
    pub items: Vec<SystemLog>,
    pub total: u64,
    pub targets: Vec<String>,
}

/// 写入一条系统日志。
pub async fn insert_system_log(
    pool: &SqlitePool,
    level: &str,
    target: &str,
    message: &str,
) -> Result<i64, StoreError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let result = sqlx::query(
        "INSERT INTO system_log (created_at, level, target, message) VALUES (?, ?, ?, ?)",
    )
    .bind(now)
    .bind(level)
    .bind(target)
    .bind(message)
    .execute(pool)
    .await
    .map_err(StoreError::Query)?;
    Ok(result.last_insert_rowid())
}

/// 记一条带操作者的审计事件（info 级），与业务写在同一事务内。
///
/// 接 `&mut SqliteConnection` 而非 `&SqlitePool`：审计行必须与它描述的那次改动
/// 同生共死——业务回滚了却留下一条「已充值」的审计行，比没有审计更糟。
///
/// 只记写入与认证事件，不记读取：`GET /users` 被导航 hover 预取反复触发，逐次
/// 落库会把这张表淹掉。
pub async fn record_audit(
    conn: &mut SqliteConnection,
    actor: Actor<'_>,
    target: &str,
    message: &str,
) -> Result<(), StoreError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    sqlx::query(
        "INSERT INTO system_log (created_at, level, target, message, actor_user_id, actor_email) \
         VALUES (?, 'info', ?, ?, ?, ?)",
    )
    .bind(now)
    .bind(target)
    .bind(message)
    .bind(actor.user_id)
    .bind(actor.email)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// 同 [`record_audit`]，但不参与调用方事务、失败只打 tracing。
///
/// 供登录这类「没有业务事务可挂」的路径使用。
pub async fn record_audit_detached(
    pool: &SqlitePool,
    actor: Option<Actor<'_>>,
    level: &str,
    target: &str,
    message: &str,
) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let result = sqlx::query(
        "INSERT INTO system_log (created_at, level, target, message, actor_user_id, actor_email) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(now)
    .bind(level)
    .bind(target)
    .bind(message)
    .bind(actor.map(|a| a.user_id))
    .bind(actor.map(|a| a.email.to_string()))
    .execute(pool)
    .await;
    if let Err(err) = result {
        tracing::error!(target: "system_log", "审计日志落库失败: {err}");
    }
}

/// 记录一条 error 级系统日志，同时打 tracing；落库失败只再记 tracing，避免递归。
pub async fn record_system_error(pool: &SqlitePool, target: &str, message: &str) {
    tracing::error!(target, "{message}");
    if let Err(err) = insert_system_log(pool, "error", target, message).await {
        tracing::error!(target: "system_log", "系统日志落库失败: {err}");
    }
}

/// 记录一条 warn 级系统日志，同时打 tracing；落库失败只再记 tracing，避免递归。
pub async fn record_system_warn(pool: &SqlitePool, target: &str, message: &str) {
    tracing::warn!(target, "{message}");
    if let Err(err) = insert_system_log(pool, "warn", target, message).await {
        tracing::error!(target: "system_log", "系统日志落库失败: {err}");
    }
}

/// 分页查询系统日志（缺省时间倒序）。
pub async fn query_system_log_page(
    pool: &SqlitePool,
    filter: &SystemLogQuery,
) -> Result<SystemLogList, StoreError> {
    let mut tx = pool.begin().await.map_err(StoreError::Query)?;
    let items = query_system_logs_on(&mut tx, filter).await?;
    let total = count_system_logs_on(&mut tx, filter).await?;
    let targets = distinct_system_log_targets_on(&mut tx, filter).await?;
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(SystemLogList {
        items,
        total,
        targets,
    })
}

fn push_system_log_filters(
    qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>,
    filter: &SystemLogQuery,
    include_target: bool,
) {
    let mut first = true;
    if let Some(keyword) = filter
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|kw| !kw.is_empty())
    {
        let pattern = like_substring_pattern(keyword);
        push_where_cond(qb, &mut first, "(target LIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" ESCAPE '\\' OR message LIKE ");
        qb.push_bind(pattern);
        qb.push(" ESCAPE '\\')");
    }
    push_created_at_range(qb, &mut first, filter.from_created_at, filter.to_created_at);
    push_column_in(qb, &mut first, "level", &filter.levels);
    if let Some(actor_user_id) = filter.actor_user_id {
        push_where_cond(qb, &mut first, "actor_user_id = ");
        qb.push_bind(actor_user_id);
    }
    if include_target {
        push_column_in(qb, &mut first, "target", &filter.targets);
    }
}

fn push_system_log_order(qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>, filter: &SystemLogQuery) {
    qb.push(" ORDER BY ");
    match filter.sort_by {
        SystemLogSortBy::Created => {
            qb.push("created_at");
        }
    }
    qb.push(filter.sort_dir.sql());
    qb.push(", id");
    qb.push(filter.sort_dir.sql());
}

async fn query_system_logs_on(
    conn: &mut SqliteConnection,
    filter: &SystemLogQuery,
) -> Result<Vec<SystemLog>, StoreError> {
    let mut qb = sqlx::QueryBuilder::new(
        "SELECT id, created_at, level, target, message, actor_user_id, actor_email \
             FROM system_log",
    );
    push_system_log_filters(&mut qb, filter, true);
    push_system_log_order(&mut qb, filter);
    push_limit_offset(&mut qb, filter.page, filter.page_size);
    let rows = qb
        .build()
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    let mut logs = Vec::with_capacity(rows.len());
    for row in rows {
        logs.push(SystemLog {
            id: row.try_get("id").map_err(StoreError::Query)?,
            created_at: row.try_get("created_at").map_err(StoreError::Query)?,
            level: row.try_get("level").map_err(StoreError::Query)?,
            target: row.try_get("target").map_err(StoreError::Query)?,
            message: row.try_get("message").map_err(StoreError::Query)?,
            actor_user_id: row.try_get("actor_user_id").map_err(StoreError::Query)?,
            actor_email: row.try_get("actor_email").map_err(StoreError::Query)?,
        });
    }
    Ok(logs)
}

async fn count_system_logs_on(
    conn: &mut SqliteConnection,
    filter: &SystemLogQuery,
) -> Result<u64, StoreError> {
    let mut qb = sqlx::QueryBuilder::new("SELECT COUNT(*) AS cnt FROM system_log");
    push_system_log_filters(&mut qb, filter, true);
    let row = qb
        .build()
        .fetch_one(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    let count: i64 = row.try_get("cnt").map_err(StoreError::Query)?;
    Ok(as_count(count))
}

/// 分面用的 target 列表：套用关键字/时间/级别，忽略 target 维。
async fn distinct_system_log_targets_on(
    conn: &mut SqliteConnection,
    filter: &SystemLogQuery,
) -> Result<Vec<String>, StoreError> {
    let mut qb = sqlx::QueryBuilder::new("SELECT DISTINCT target FROM system_log");
    push_system_log_filters(&mut qb, filter, false);
    qb.push(" ORDER BY target");
    let rows = qb
        .build()
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    let mut targets = Vec::with_capacity(rows.len());
    for row in rows {
        targets.push(row.try_get("target").map_err(StoreError::Query)?);
    }
    Ok(targets)
}
