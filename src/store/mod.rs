//! SQLite 存储层：版本化迁移 + 请求日志落库 + 令牌余额结算。
//!
//! 本模块承载请求日志（`request_log`）、冒烟记录（`smoke_probe`）与令牌计费
//! 余额（`token_balance`）。金额一律整数 micro-USD（ADR-0002）。

use std::path::Path;

use sqlx::{SqliteConnection, SqlitePool, sqlite::SqliteConnectOptions};
use thiserror::Error;

use crate::core::billing::PriceSnapshot;

/// 存储层错误，向上抛给应用边界。
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("连接 SQLite 失败: {0}")]
    Connect(sqlx::Error),
    #[error("执行迁移失败: {0}")]
    Migrate(sqlx::migrate::MigrateError),
    #[error("数据库操作失败: {0}")]
    Query(sqlx::Error),
    #[error("令牌 {0} 的余额记录在写入后仍不存在")]
    MissingToken(String),
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
    pub token_key: String,
    pub inbound_protocol: String,
    pub model: String,
    pub channel: String,
    pub status_code: i64,
    pub latency_ms: i64,
    /// usage 四分量。
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// 计费时的四档价格快照（micro-USD / 1M tokens）。
    pub price: PriceSnapshot,
    /// 本次费用（micro-USD）。
    pub cost_usd_micros: i64,
    /// 可选的入站请求原始字节（仅 `logging.full_body` 开启时保存）。
    pub request_body: Option<Vec<u8>>,
    /// 可选的入站响应原始字节（仅 `logging.full_body` 开启时保存）。
    pub response_body: Option<Vec<u8>>,
}

/// 落一条请求日志，返回插入的自增 id。
pub async fn insert_request_log(pool: &SqlitePool, log: &RequestLog) -> Result<i64, StoreError> {
    let result = sqlx::query(
        "INSERT INTO request_log \
         (created_at, token_name, token_key, inbound_protocol, model, channel, status_code, \
          latency_ms, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, \
         input_price_usd_micros, output_price_usd_micros, cache_read_price_usd_micros, \
         cache_write_price_usd_micros, cost_usd_micros, request_body, response_body) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(log.created_at)
    .bind(&log.token_name)
    .bind(&log.token_key)
    .bind(&log.inbound_protocol)
    .bind(&log.model)
    .bind(&log.channel)
    .bind(log.status_code)
    .bind(log.latency_ms)
    .bind(log.input_tokens as i64)
    .bind(log.output_tokens as i64)
    .bind(log.cache_read_tokens as i64)
    .bind(log.cache_write_tokens as i64)
    .bind(log.price.input_micros)
    .bind(log.price.output_micros)
    .bind(log.price.cache_read_micros)
    .bind(log.price.cache_write_micros)
    .bind(log.cost_usd_micros)
    .bind(&log.request_body)
    .bind(&log.response_body)
    .execute(pool)
    .await
    .map_err(StoreError::Query)?;

    Ok(result.last_insert_rowid())
}

/// 令牌余额视图。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBalance {
    /// 当前余额（micro-USD），可为负（在途透支）。
    pub balance_usd_micros: i64,
    /// 累计结算总额（micro-USD），用于 limit_usd 上限检查。
    pub settled_usd_micros: i64,
}

/// 令牌首次出现时按配置初始余额落库；已存在则原样返回（重启不重置）。
///
/// 入参余额 `balance_usd` 为 USD，换算为整数 micro-USD 落库。
pub async fn ensure_token_balance(
    conn: &mut SqliteConnection,
    token_key: &str,
    balance_usd: f64,
    now: i64,
) -> Result<TokenBalance, StoreError> {
    let balance_micros = (balance_usd * 1_000_000.0).round() as i64;
    sqlx::query(
        "INSERT INTO token_balance (token_key, balance_usd_micros, settled_usd_micros, created_at) \
         VALUES (?, ?, 0, ?) \
         ON CONFLICT(token_key) DO NOTHING",
    )
    .bind(token_key)
    .bind(balance_micros)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;

    get_token_balance(conn, token_key)
        .await?
        .ok_or(StoreError::MissingToken(token_key.to_string()))
}

/// 读取令牌余额；不存在返回 `None`。
pub async fn get_token_balance(
    conn: &mut SqliteConnection,
    token_key: &str,
) -> Result<Option<TokenBalance>, StoreError> {
    let row = sqlx::query_as::<_, (i64, i64)>(
        "SELECT balance_usd_micros, settled_usd_micros FROM token_balance WHERE token_key = ?",
    )
    .bind(token_key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?;

    Ok(row.map(|(balance, settled)| TokenBalance {
        balance_usd_micros: balance,
        settled_usd_micros: settled,
    }))
}

/// 结算一次费用：余额扣减（可为负）、累计结算增加。返回结算后的余额。
///
/// 以 `UPDATE` 原子完成，避免并发读改写；SQLite 单写者串行化保证单调。
pub async fn settle_charge(
    conn: &mut SqliteConnection,
    token_key: &str,
    cost_usd_micros: i64,
) -> Result<TokenBalance, StoreError> {
    sqlx::query(
        "UPDATE token_balance \
         SET balance_usd_micros = balance_usd_micros - ?, \
             settled_usd_micros = settled_usd_micros + ? \
         WHERE token_key = ?",
    )
    .bind(cost_usd_micros)
    .bind(cost_usd_micros)
    .bind(token_key)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;

    get_token_balance(conn, token_key)
        .await?
        .ok_or(StoreError::MissingToken(token_key.to_string()))
}
