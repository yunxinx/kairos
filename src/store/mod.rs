//! SQLite 存储层：版本化迁移 + 请求日志落库。
//!
//! 本模块当前承载请求日志（`request_log` 表）与冒烟记录（`smoke_probe`）；
//! 令牌余额、usage 与费用等计费相关列在对应票据接入。

use std::path::Path;

use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use thiserror::Error;

/// 存储层错误，向上抛给应用边界。
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("连接 SQLite 失败: {0}")]
    Connect(sqlx::Error),
    #[error("执行迁移失败: {0}")]
    Migrate(sqlx::migrate::MigrateError),
    #[error("数据库操作失败: {0}")]
    Query(sqlx::Error),
}

/// 打开 SQLite 连接池并在事务内按序应用编号迁移。
///
/// 缺库文件时自动创建（`create_if_missing`），迁移脚本内建在 `migrations/`。
pub async fn open(path: &Path) -> Result<SqlitePool, StoreError> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true);

    let pool = SqlitePool::connect_with(options)
        .await
        .map_err(StoreError::Connect)?;

    sqlx::migrate!()
        .run(&pool)
        .await
        .map_err(StoreError::Migrate)?;

    Ok(pool)
}

/// 写一条冒烟记录，返回插入的自增 id。
pub async fn insert_smoke(pool: &SqlitePool, note: &str) -> Result<i64, StoreError> {
    let result = sqlx::query("INSERT INTO smoke_probe (note) VALUES (?)")
        .bind(note)
        .execute(pool)
        .await
        .map_err(StoreError::Query)?;

    Ok(result.last_insert_rowid())
}

/// 一条请求日志的可持久化字段。
#[derive(Debug, Clone)]
pub struct RequestLog {
    /// unix 毫秒时间戳。
    pub created_at: i64,
    pub token_name: String,
    pub inbound_protocol: String,
    pub model: String,
    pub channel: String,
    pub status_code: i64,
    pub latency_ms: i64,
}

/// 落一条请求日志，返回插入的自增 id。
pub async fn insert_request_log(pool: &SqlitePool, log: &RequestLog) -> Result<i64, StoreError> {
    let result = sqlx::query(
        "INSERT INTO request_log \
         (created_at, token_name, inbound_protocol, model, channel, status_code, latency_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(log.created_at)
    .bind(&log.token_name)
    .bind(&log.inbound_protocol)
    .bind(&log.model)
    .bind(&log.channel)
    .bind(log.status_code)
    .bind(log.latency_ms)
    .execute(pool)
    .await
    .map_err(StoreError::Query)?;

    Ok(result.last_insert_rowid())
}
