//! SQLite 存储层：版本化迁移 + 请求日志落库 + 用户钱包结算。
//!
//! 本模块承载请求日志（`request_log`）、系统日志（`system_log`）、冒烟记录
//! （`smoke_probe`）、管理用户钱包（`user_balance`）与令牌累计结算
//! （`token_balance`）。金额一律整数 micro-USD（ADR-0002）。管理面 `/stats` 与
//! `/stats/lifetime` 聚合也在此查询（时间窗夹取与日志分页同一惯例）。

pub mod catalog;
pub mod resources;
mod system_log;
pub mod users;

pub use system_log::{
    SystemLog, SystemLogList, SystemLogQuery, SystemLogSortBy, insert_system_log,
    query_system_log_page, record_system_error, record_system_warn,
};

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::{
    AssertSqlSafe, Row, SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
};
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
    #[error("找不到令牌 {0} 所属用户的余额")]
    MissingToken(String),
    #[error("资源数据非法: {0}")]
    InvalidResource(String),
    #[error("不能删除或降级最后一个 root")]
    LastRootProtected,
    #[error("用户 {0} 不存在")]
    UserNotFound(i64),
    #[error("用户 {0} 缺少钱包")]
    MissingWallet(i64),
    #[error("邮箱已被使用")]
    EmailTaken,
    #[error("密码处理失败")]
    PasswordHash,
}

/// 写锁等待上限：与 sqlx-sqlite 缺省一致，此处显式声明意图——SQLite 单写者下
/// 请求路径结算/日志与管理面写并发时排队等待，而不是立即失败。
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// 打开 SQLite 连接池并在事务内按序应用编号迁移。
///
/// 缺库文件时自动创建（`create_if_missing`），迁移脚本内建在 `migrations/`。
/// 连接选项统一治理 SQLite 的坏默认值：外键强制、写锁排队、WAL 日志模式。
pub async fn open(path: &Path) -> Result<SqlitePool, StoreError> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        // WAL：读写互不阻塞；提交只追加写 WAL，免去 DELETE 模式每次提交把被
        // 修改页原像复制进回滚日志的开销。WAL 会持久记录在库文件头，后续
        // 打开自动沿用。
        .journal_mode(SqliteJournalMode::Wal)
        // WAL 下 NORMAL 只在检查点前同步，崩溃不损坏库文件（掉电可能丢失上
        // 次检查点以来已提交的事务），官方推荐档位；FULL 每次提交都同步，
        // 无必要。
        .synchronous(SqliteSynchronous::Normal);

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
    /// 自增主键：新增时由库分配，读回后才有效（插入构造时填 0）。
    pub id: i64,
    /// unix 毫秒时间戳。
    pub created_at: i64,
    pub token_name: String,
    pub token_key: String,
    /// 归属管理用户，写入时定格。
    ///
    /// 冗余存储而非 JOIN `tokens`：令牌删除后归属仍在，日志过滤与用量统计不缩水。
    /// `0` 为存量行或归属未知，不匹配任何真实用户。
    pub user_id: i64,
    pub inbound_protocol: String,
    /// 入站模型名（下游请求的 `model`，别名或统一模型 ID 原样保留）。
    pub model: String,
    /// 实际出站模型名（别名改写后或统一模型落到的已登记模型）。
    ///
    /// 存量行或尚未出站的失败请求为 `None`。
    pub outbound_model: Option<String>,
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
    /// 费用是否已写入 `token_balance`；结算失败时为 `false`，供对账补扣。
    pub settled: bool,
    /// 一次下游入站请求的身份；同一请求的多次出站尝试共用。存量行可能为 `None`。
    pub request_id: Option<String>,
    /// 可选的入站请求原始字节（仅 `logging.full_body` 开启时保存）。
    pub request_body: Option<Vec<u8>>,
    /// 可选的入站响应原始字节（仅 `logging.full_body` 开启时保存）。
    ///
    /// 非流式为返回下游的 JSON 字节；流式为实际下发的 SSE 帧 wire 文本拼接。
    pub response_body: Option<Vec<u8>>,
}

/// 落一条请求日志，返回插入的自增 id。
pub async fn insert_request_log(pool: &SqlitePool, log: &RequestLog) -> Result<i64, StoreError> {
    let mut conn = pool.acquire().await.map_err(StoreError::Query)?;
    insert_request_log_on(&mut conn, log).await
}

/// 在已有连接/事务上插入请求日志，供结算与日志同事务提交。
pub async fn insert_request_log_on(
    conn: &mut SqliteConnection,
    log: &RequestLog,
) -> Result<i64, StoreError> {
    let result = sqlx::query(
        "INSERT INTO request_log \
         (created_at, token_name, token_key, user_id, inbound_protocol, model, outbound_model, \
          channel, status_code, latency_ms, input_tokens, output_tokens, cache_read_tokens, \
          cache_write_tokens, input_price_usd_micros, output_price_usd_micros, \
          cache_read_price_usd_micros, cache_write_price_usd_micros, cost_usd_micros, \
          settled, request_id, request_body, response_body) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(log.created_at)
    .bind(&log.token_name)
    .bind(&log.token_key)
    .bind(log.user_id)
    .bind(&log.inbound_protocol)
    .bind(&log.model)
    .bind(&log.outbound_model)
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
    .bind(log.settled as i64)
    .bind(&log.request_id)
    .bind(&log.request_body)
    .bind(&log.response_body)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;

    Ok(result.last_insert_rowid())
}

/// 令牌视角的计费快照：剩余来自所属用户钱包，累计结算来自该令牌。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBalance {
    /// 所属用户当前剩余（micro-USD），可为负（在途透支）。
    pub balance_usd_micros: i64,
    /// 该令牌累计结算总额（micro-USD），用于 `limit_usd` 上限检查。
    pub settled_usd_micros: i64,
}

/// 令牌首次出现时建累计结算行，并把初始余额记入所属用户钱包；已存在则原样返回。
///
/// 入参余额 `balance_usd` 为 USD，换算为整数 micro-USD。仅在新建结算行时入账，
/// 避免重启或重复调用把同一令牌的初始额再加一遍。
pub async fn ensure_token_balance(
    conn: &mut SqliteConnection,
    token_key: &str,
    balance_usd: f64,
    now: i64,
) -> Result<TokenBalance, StoreError> {
    let balance_micros = (balance_usd * 1_000_000.0).round() as i64;
    let inserted = sqlx::query(
        "INSERT INTO token_balance (token_key, settled_usd_micros, created_at) \
         VALUES (?, 0, ?) \
         ON CONFLICT(token_key) DO NOTHING",
    )
    .bind(token_key)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;

    if inserted.rows_affected() == 1 && balance_micros != 0 {
        let credited = sqlx::query(
            "UPDATE user_balance SET balance_usd_micros = balance_usd_micros + ? \
             WHERE user_id = (SELECT user_id FROM tokens WHERE token_key = ?)",
        )
        .bind(balance_micros)
        .bind(token_key)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
        if credited.rows_affected() == 0 {
            return Err(StoreError::MissingToken(token_key.to_string()));
        }
    }

    get_token_balance(conn, token_key)
        .await?
        .ok_or(StoreError::MissingToken(token_key.to_string()))
}

/// 读取令牌所属用户的剩余，以及该令牌累计结算；令牌不存在返回 `None`。
pub async fn get_token_balance(
    conn: &mut SqliteConnection,
    token_key: &str,
) -> Result<Option<TokenBalance>, StoreError> {
    let row = sqlx::query_as::<_, (i64, i64)>(
        "SELECT ub.balance_usd_micros, COALESCE(tb.settled_usd_micros, 0) \
         FROM tokens t \
         INNER JOIN user_balance ub ON ub.user_id = t.user_id \
         LEFT JOIN token_balance tb ON tb.token_key = t.token_key \
         WHERE t.token_key = ?",
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

/// 删除令牌累计结算行；不存在视为成功（幂等）。
///
/// 供删除令牌时同事务清理：结算行若残留，同 key 重建令牌会经
/// `ensure_token_balance` 的冲突跳过、不再把初始额写入用户钱包。
pub async fn delete_token_balance(
    conn: &mut SqliteConnection,
    token_key: &str,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM token_balance WHERE token_key = ?")
        .bind(token_key)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 结算一次费用：从所属用户钱包扣减（可为负），并增加用户与该令牌的累计结算。
///
/// 用户钱包以 `UPDATE` 原子完成；SQLite 单写者串行化保证单调。
pub async fn settle_charge(
    conn: &mut SqliteConnection,
    token_key: &str,
    cost_usd_micros: i64,
) -> Result<TokenBalance, StoreError> {
    let updated = sqlx::query(
        "UPDATE user_balance \
         SET balance_usd_micros = balance_usd_micros - ?, \
             settled_usd_micros = settled_usd_micros + ? \
         WHERE user_id = (SELECT user_id FROM tokens WHERE token_key = ?)",
    )
    .bind(cost_usd_micros)
    .bind(cost_usd_micros)
    .bind(token_key)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    if updated.rows_affected() == 0 {
        return Err(StoreError::MissingToken(token_key.to_string()));
    }

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    sqlx::query(
        "INSERT INTO token_balance (token_key, settled_usd_micros, created_at) \
         VALUES (?, ?, ?) \
         ON CONFLICT(token_key) DO UPDATE SET \
           settled_usd_micros = settled_usd_micros + excluded.settled_usd_micros",
    )
    .bind(token_key)
    .bind(cost_usd_micros)
    .bind(created_at)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;

    get_token_balance(conn, token_key)
        .await?
        .ok_or(StoreError::MissingToken(token_key.to_string()))
}

/// 读用户钱包。插入用户时同步建行；缺失视为数据损坏。
pub async fn get_user_wallet(pool: &SqlitePool, user_id: i64) -> Result<(i64, i64), StoreError> {
    sqlx::query_as(
        "SELECT balance_usd_micros, settled_usd_micros FROM user_balance WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(StoreError::Query)?
    .ok_or(StoreError::MissingWallet(user_id))
}

/// 相对调整用户钱包：充值传正数、扣减传负数。返回 `(剩余, 用户累计结算)`。
pub async fn adjust_user_balance(
    conn: &mut SqliteConnection,
    user_id: i64,
    delta_usd_micros: i64,
) -> Result<(i64, i64), StoreError> {
    let updated = sqlx::query(
        "UPDATE user_balance SET balance_usd_micros = balance_usd_micros + ? WHERE user_id = ?",
    )
    .bind(delta_usd_micros)
    .bind(user_id)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    if updated.rows_affected() == 0 {
        return Err(StoreError::MissingWallet(user_id));
    }
    sqlx::query_as(
        "SELECT balance_usd_micros, settled_usd_micros FROM user_balance WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?
    .ok_or(StoreError::MissingWallet(user_id))
}

/// 全部令牌的累计结算额，供管理列表拼进读视图。
pub async fn list_token_settled(pool: &SqlitePool) -> Result<HashMap<String, i64>, StoreError> {
    let rows = sqlx::query("SELECT token_key, settled_usd_micros FROM token_balance")
        .fetch_all(pool)
        .await
        .map_err(StoreError::Query)?;
    let mut settled = HashMap::with_capacity(rows.len());
    for row in rows {
        let token_key: String = row.try_get("token_key").map_err(StoreError::Query)?;
        let amount: i64 = row
            .try_get("settled_usd_micros")
            .map_err(StoreError::Query)?;
        settled.insert(token_key, amount);
    }
    Ok(settled)
}

/// 单令牌累计结算额；无结算行视为 0。
pub async fn get_token_settled(pool: &SqlitePool, token_key: &str) -> Result<i64, StoreError> {
    sqlx::query_scalar("SELECT settled_usd_micros FROM token_balance WHERE token_key = ?")
        .bind(token_key)
        .fetch_optional(pool)
        .await
        .map_err(StoreError::Query)
        .map(|amount: Option<i64>| amount.unwrap_or(0))
}

/// 列表排序方向；缺省新→旧。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    Asc,
    #[default]
    Desc,
}

impl SortDir {
    /// SQL `ASC` / `DESC` 片段（含前导空格）。
    pub(crate) fn sql(self) -> &'static str {
        match self {
            Self::Asc => " ASC",
            Self::Desc => " DESC",
        }
    }
}

/// 请求日志可排序列：时间与计量，不含类别/身份列。
///
/// 只接受白名单，拼进 `ORDER BY` 的是静态片段，避免把查询参数当标识符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestLogSortBy {
    #[default]
    Created,
    Tokens,
    Latency,
    Cache,
    Cost,
}

/// 请求日志查询过滤条件与分页。全部过滤维度可选，缺省即不限。
#[derive(Debug, Clone, Default)]
pub struct RequestLogQuery {
    /// 按归属管理用户精确过滤。
    ///
    /// 普通用户查询时由管理面强制注入自己的 id；`None` 表示不限（admin/root 看全量）。
    pub user_id: Option<i64>,
    /// 按令牌 key 精确过滤。
    pub token_key: Option<String>,
    /// 按令牌展示名精确过滤。列表接口脱敏 `token_key`，行内筛选只能按名匹配。
    pub token_name: Option<String>,
    /// 按模型精确过滤。
    pub model: Option<String>,
    /// 按渠道名精确过滤。
    pub channel: Option<String>,
    /// 综合关键字：对 `token_key`/`token_name`/`model`/`channel` 做 LIKE 子串匹配（OR）。
    pub keyword: Option<String>,
    /// 只返回 `created_at >= from_created_at`。
    pub from_created_at: Option<i64>,
    /// 只返回 `created_at <= to_created_at`。
    pub to_created_at: Option<i64>,
    /// 按是否已写入 `token_balance` 过滤；`None` 表示不限。
    pub settled: Option<bool>,
    /// 精确匹配的入站协议；空表示不限。
    pub inbound_protocols: Vec<String>,
    /// 排序列；缺省时间。
    pub sort_by: RequestLogSortBy,
    /// 排序方向；缺省倒序。
    pub sort_dir: SortDir,
    /// 页码，从 1 起。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl RequestLogQuery {
    /// 用必填的分页参数构造查询，过滤维度缺省为空。
    pub fn new(page: u64, page_size: u64) -> Self {
        let (page, page_size) = clamp_page(page, page_size);
        Self {
            page,
            page_size,
            ..Self::default()
        }
    }
}

/// 按 `filter` 分页查询请求日志（缺省时间倒序），返回本页条目（不含 body）。
async fn query_request_logs_on(
    conn: &mut SqliteConnection,
    filter: &RequestLogQuery,
) -> Result<Vec<RequestLog>, StoreError> {
    let mut qb = sqlx::QueryBuilder::new(
        "SELECT id, created_at, token_name, token_key, user_id, inbound_protocol, model, outbound_model, \
         channel, status_code, latency_ms, input_tokens, output_tokens, cache_read_tokens, \
         cache_write_tokens, input_price_usd_micros, output_price_usd_micros, \
         cache_read_price_usd_micros, cache_write_price_usd_micros, cost_usd_micros, \
         settled FROM request_log",
    );
    push_request_log_filters(&mut qb, filter);
    push_request_log_order(&mut qb, filter);
    push_limit_offset(&mut qb, filter.page, filter.page_size);

    let rows = qb
        .build()
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::Query)?;

    let mut logs = Vec::with_capacity(rows.len());
    for row in rows {
        logs.push(map_request_log_row(&row, false)?);
    }
    Ok(logs)
}

/// 按主键读一条请求日志（含 body）；不存在返回 `None`。
pub async fn get_request_log(pool: &SqlitePool, id: i64) -> Result<Option<RequestLog>, StoreError> {
    let row = sqlx::query(
        "SELECT id, created_at, token_name, token_key, user_id, inbound_protocol, model, outbound_model, \
         channel, status_code, latency_ms, input_tokens, output_tokens, cache_read_tokens, \
         cache_write_tokens, input_price_usd_micros, output_price_usd_micros, \
         cache_read_price_usd_micros, cache_write_price_usd_micros, cost_usd_micros, \
         settled, request_body, response_body FROM request_log WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(StoreError::Query)?;
    row.map(|row| map_request_log_row(&row, true)).transpose()
}

/// 未结算请求日志的运营闭环结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsettledLogAction {
    /// 已补扣或豁免，行现为已结算。
    Closed,
    /// 该行已经是已结算。
    AlreadySettled,
    /// 没有这条日志。
    NotFound,
}

/// 对未结算日志补扣：按行上费用写入所属用户钱包（允许透支），并标为已结算。
///
/// 费用为 0 时只翻 `settled`。已结算或缺失不改余额。
pub async fn settle_unsettled_log(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<UnsettledLogAction, StoreError> {
    let Some((token_key, cost, settled)) = load_log_settlement(conn, id).await? else {
        return Ok(UnsettledLogAction::NotFound);
    };
    if settled {
        return Ok(UnsettledLogAction::AlreadySettled);
    }
    if cost > 0 {
        settle_charge(conn, &token_key, cost).await?;
    }
    mark_request_log_settled(conn, id).await?;
    Ok(UnsettledLogAction::Closed)
}

/// 豁免未结算日志：只翻 `settled`，不动余额。
pub async fn waive_unsettled_log(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<UnsettledLogAction, StoreError> {
    let Some((_, _, settled)) = load_log_settlement(conn, id).await? else {
        return Ok(UnsettledLogAction::NotFound);
    };
    if settled {
        return Ok(UnsettledLogAction::AlreadySettled);
    }
    mark_request_log_settled(conn, id).await?;
    Ok(UnsettledLogAction::Closed)
}

/// 读一条日志的结算所需字段；不存在返回 `None`。
async fn load_log_settlement(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<Option<(String, i64, bool)>, StoreError> {
    let row =
        sqlx::query("SELECT token_key, cost_usd_micros, settled FROM request_log WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(StoreError::Query)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let token_key: String = row.try_get("token_key").map_err(StoreError::Query)?;
    let cost: i64 = row.try_get("cost_usd_micros").map_err(StoreError::Query)?;
    let settled = row
        .try_get::<i64, _>("settled")
        .map_err(StoreError::Query)?
        != 0;
    Ok(Some((token_key, cost, settled)))
}

async fn mark_request_log_settled(conn: &mut SqliteConnection, id: i64) -> Result<(), StoreError> {
    sqlx::query("UPDATE request_log SET settled = 1 WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 按 `filter` 分页查询请求日志（时间倒序），返回本页条目。
pub async fn query_request_logs(
    pool: &SqlitePool,
    filter: &RequestLogQuery,
) -> Result<Vec<RequestLog>, StoreError> {
    let mut conn = pool.acquire().await.map_err(StoreError::Query)?;
    query_request_logs_on(&mut conn, filter).await
}

/// 在同一事务内读本页条目、过滤总数与未结算条数。
///
/// 未结算计数套用同一套令牌/模型/关键字/时间过滤，但忽略 `settled` 维，
/// 便于列表在「看全部」时仍提示有多少条待对账。
pub async fn query_request_log_page(
    pool: &SqlitePool,
    filter: &RequestLogQuery,
) -> Result<(Vec<RequestLog>, u64, u64), StoreError> {
    let mut tx = pool.begin().await.map_err(StoreError::Query)?;
    let logs = query_request_logs_on(&mut tx, filter).await?;
    let total = count_request_logs_on(&mut tx, filter).await?;
    let mut unsettled_filter = filter.clone();
    unsettled_filter.settled = Some(false);
    let unsettled_total = count_request_logs_on(&mut tx, &unsettled_filter).await?;
    tx.commit().await.map_err(StoreError::Query)?;
    Ok((logs, total, unsettled_total))
}

/// 按 `filter` 统计满足条件的日志总数（用于分页总页数）。
async fn count_request_logs_on(
    conn: &mut SqliteConnection,
    filter: &RequestLogQuery,
) -> Result<u64, StoreError> {
    let mut qb = sqlx::QueryBuilder::new("SELECT COUNT(*) AS cnt FROM request_log");
    push_request_log_filters(&mut qb, filter);

    let row = qb
        .build()
        .fetch_one(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    let count: i64 = row.try_get("cnt").map_err(StoreError::Query)?;
    Ok(as_count(count))
}

/// 按 `filter` 统计满足条件的日志总数（用于分页总页数）。
pub async fn count_request_logs(
    pool: &SqlitePool,
    filter: &RequestLogQuery,
) -> Result<u64, StoreError> {
    let mut conn = pool.acquire().await.map_err(StoreError::Query)?;
    count_request_logs_on(&mut conn, filter).await
}

/// `/stats` 缺省时间窗（天）。
const DEFAULT_STATS_DAYS: u64 = 7;
/// `/stats` 时间窗上限（天）；外部传入的 `days` 夹取到 `[1, MAX]`。
const MAX_STATS_DAYS: u64 = 90;

const MS_PER_DAY: i64 = 86_400_000;
/// `days=1` 时趋势按 UTC 小时补齐，长度为 24。
const HOURS_PER_DAY: i64 = 24;

/// 把外部传入的 `days` 夹取到合法时间窗：缺省 7，下限 1，上限 90。
pub fn clamp_stats_days(days: Option<u64>) -> u64 {
    days.unwrap_or(DEFAULT_STATS_DAYS).clamp(1, MAX_STATS_DAYS)
}

/// `/stats` 只读聚合：时间窗内请求量、token、费用与分布。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    pub summary: StatsSummary,
    pub daily: Vec<DailyBucket>,
    pub by_model: Vec<CostShare>,
    pub by_channel: Vec<CostShare>,
}

/// 时间窗汇总。令牌数/渠道数来自资源表（当前存量），其余来自 `request_log`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsSummary {
    pub request_count: u64,
    pub success_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd_micros: i64,
    /// 令牌数：全局视图为全部令牌，归属视图只数该用户自己的。
    pub token_count: u64,
    /// 出站渠道数。归属视图为 `None`：渠道是运营视角的数字，普通用户不该看到。
    pub channel_count: Option<u64>,
}

/// 趋势桶：`days=1` 为 UTC 小时（24 点），否则为日历日；无流量的桶补零。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyBucket {
    pub date: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd_micros: i64,
}

/// 按模型或按渠道的费用/请求分布。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostShare {
    pub name: String,
    pub request_count: u64,
    pub cost_usd_micros: i64,
}

/// 全量累计：不受 `/stats` 时间窗影响。
///
/// 口径：`request_count` 按 `request_id` 去重（存量无 id 的行回退到主键），
/// 表示下游入站次数；`total_tokens` 含全部请求日志行（含未结算），
/// `cost_usd_micros` 只计 HTTP 2xx 且已结算的费用。并列展示时
/// 不要把 token 合计当成已入账费用的用量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifetimeStats {
    pub request_count: u64,
    pub cost_usd_micros: i64,
    pub total_tokens: u64,
}

/// 聚合 `days` 天（已夹取）内的 stats。费用只计 HTTP 2xx（与计费「仅成功结算」一致）。
///
/// `user_id` 为 `Some` 时只统计该用户名下的流量（普通用户视图），并省略渠道数。
pub async fn query_stats(
    pool: &SqlitePool,
    days: u64,
    user_id: Option<i64>,
) -> Result<Stats, StoreError> {
    let days = clamp_stats_days(Some(days));
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let today = now_millis.div_euclid(MS_PER_DAY);
    let start_day = today.saturating_sub(days as i64 - 1);
    let from_created_at = start_day.saturating_mul(MS_PER_DAY);

    let summary_sql = format!(
        "SELECT COUNT(DISTINCT COALESCE(request_id, CAST(id AS TEXT))) AS request_count, \
         COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 THEN 1 ELSE 0 END), 0) AS success_count, \
         COALESCE(SUM(input_tokens), 0) AS input_tokens, \
         COALESCE(SUM(output_tokens), 0) AS output_tokens, \
         COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 AND settled = 1 THEN cost_usd_micros ELSE 0 END), 0) \
           AS cost_usd_micros \
         FROM request_log WHERE created_at >= ?{}",
        user_scope_clause(user_id)
    );
    let mut summary_query = sqlx::query(AssertSqlSafe(summary_sql)).bind(from_created_at);
    if let Some(user_id) = user_id {
        summary_query = summary_query.bind(user_id);
    }
    let summary_row = summary_query
        .fetch_one(pool)
        .await
        .map_err(StoreError::Query)?;

    let token_count = match user_id {
        Some(user_id) => {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tokens WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(pool)
                .await
                .map_err(StoreError::Query)?;
            as_count(count)
        }
        None => count_rows(pool, "SELECT COUNT(*) AS cnt FROM tokens").await?,
    };
    // 渠道数只在全局视图给出：普通用户看不到渠道，也不需要知道有多少条。
    let channel_count = match user_id {
        Some(_) => None,
        None => Some(count_rows(pool, "SELECT COUNT(*) AS cnt FROM channels").await?),
    };

    let summary = StatsSummary {
        request_count: as_count(
            summary_row
                .try_get("request_count")
                .map_err(StoreError::Query)?,
        ),
        success_count: as_count(
            summary_row
                .try_get("success_count")
                .map_err(StoreError::Query)?,
        ),
        input_tokens: as_count(
            summary_row
                .try_get("input_tokens")
                .map_err(StoreError::Query)?,
        ),
        output_tokens: as_count(
            summary_row
                .try_get("output_tokens")
                .map_err(StoreError::Query)?,
        ),
        cost_usd_micros: summary_row
            .try_get("cost_usd_micros")
            .map_err(StoreError::Query)?,
        token_count,
        channel_count,
    };

    let daily = if days == 1 {
        query_hourly_buckets(pool, from_created_at, user_id).await?
    } else {
        query_daily_buckets(pool, from_created_at, days, user_id).await?
    };
    let by_model = query_cost_share(pool, from_created_at, CostDimension::Model, user_id).await?;
    let by_channel =
        query_cost_share(pool, from_created_at, CostDimension::Channel, user_id).await?;

    Ok(Stats {
        summary,
        daily,
        by_model,
        by_channel,
    })
}

/// 全量累计：请求数、成功结算费用、四分量 token 合计。
///
/// `user_id` 为 `Some` 时只累计该用户名下的流量。
pub async fn query_lifetime_stats(
    pool: &SqlitePool,
    user_id: Option<i64>,
) -> Result<LifetimeStats, StoreError> {
    let sql = format!(
        "SELECT COUNT(DISTINCT COALESCE(request_id, CAST(id AS TEXT))) AS request_count, \
         COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 AND settled = 1 THEN cost_usd_micros ELSE 0 END), 0) \
           AS cost_usd_micros, \
         COALESCE(SUM(input_tokens + output_tokens + cache_read_tokens + cache_write_tokens), 0) \
           AS total_tokens \
         FROM request_log{}",
        lifetime_user_scope_clause(user_id)
    );
    let mut query = sqlx::query(AssertSqlSafe(sql));
    if let Some(user_id) = user_id {
        query = query.bind(user_id);
    }
    let row = query.fetch_one(pool).await.map_err(StoreError::Query)?;

    Ok(LifetimeStats {
        request_count: as_count(row.try_get("request_count").map_err(StoreError::Query)?),
        cost_usd_micros: row.try_get("cost_usd_micros").map_err(StoreError::Query)?,
        total_tokens: as_count(row.try_get("total_tokens").map_err(StoreError::Query)?),
    })
}

/// 把趋势查询行映射为桶；`date` 列已是展示用标签。
fn trend_bucket(row: &sqlx::sqlite::SqliteRow) -> Result<DailyBucket, StoreError> {
    Ok(DailyBucket {
        date: row.try_get("date").map_err(StoreError::Query)?,
        request_count: as_count(row.try_get("request_count").map_err(StoreError::Query)?),
        input_tokens: as_count(row.try_get("input_tokens").map_err(StoreError::Query)?),
        output_tokens: as_count(row.try_get("output_tokens").map_err(StoreError::Query)?),
        cost_usd_micros: row.try_get("cost_usd_micros").map_err(StoreError::Query)?,
    })
}

/// `days=1`：当日 UTC 0–23 时补齐，标签为 `YYYY-MM-DDTHH:00:00Z`。
async fn query_hourly_buckets(
    pool: &SqlitePool,
    from_created_at: i64,
    user_id: Option<i64>,
) -> Result<Vec<DailyBucket>, StoreError> {
    let sql = format!(
        "WITH RECURSIVE calendar(ts, n) AS ( \
            SELECT datetime(? / 1000, 'unixepoch') AS ts, 1 AS n \
            UNION ALL \
            SELECT datetime(ts, '+1 hour'), n + 1 FROM calendar WHERE n < ? \
         ) \
         SELECT strftime('%Y-%m-%dT%H:00:00Z', calendar.ts) AS date, \
                COALESCE(agg.request_count, 0) AS request_count, \
                COALESCE(agg.input_tokens, 0) AS input_tokens, \
                COALESCE(agg.output_tokens, 0) AS output_tokens, \
                COALESCE(agg.cost_usd_micros, 0) AS cost_usd_micros \
         FROM calendar \
         LEFT JOIN ( \
            SELECT strftime('%Y-%m-%dT%H:00:00Z', created_at / 1000, 'unixepoch') AS hour, \
                   COUNT(DISTINCT COALESCE(request_id, CAST(id AS TEXT))) AS request_count, \
                   COALESCE(SUM(input_tokens), 0) AS input_tokens, \
                   COALESCE(SUM(output_tokens), 0) AS output_tokens, \
                   COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 AND settled = 1 \
                        THEN cost_usd_micros ELSE 0 END), 0) AS cost_usd_micros \
            FROM request_log WHERE created_at >= ?{} \
            GROUP BY hour \
         ) agg ON agg.hour = strftime('%Y-%m-%dT%H:00:00Z', calendar.ts) \
         ORDER BY calendar.ts",
        user_scope_clause(user_id)
    );
    let mut query = sqlx::query(AssertSqlSafe(sql))
        .bind(from_created_at)
        .bind(HOURS_PER_DAY)
        .bind(from_created_at);
    if let Some(user_id) = user_id {
        query = query.bind(user_id);
    }
    let rows = query.fetch_all(pool).await.map_err(StoreError::Query)?;

    rows.iter().map(trend_bucket).collect()
}

/// 逐日序列：用 SQLite 日历补齐无流量日，日期为 UTC `YYYY-MM-DD`。
async fn query_daily_buckets(
    pool: &SqlitePool,
    from_created_at: i64,
    days: u64,
    user_id: Option<i64>,
) -> Result<Vec<DailyBucket>, StoreError> {
    let sql = format!(
        "WITH RECURSIVE calendar(day, n) AS ( \
            SELECT date(? / 1000, 'unixepoch') AS day, 1 AS n \
            UNION ALL \
            SELECT date(day, '+1 day'), n + 1 FROM calendar WHERE n < ? \
         ) \
         SELECT calendar.day AS date, \
                COALESCE(agg.request_count, 0) AS request_count, \
                COALESCE(agg.input_tokens, 0) AS input_tokens, \
                COALESCE(agg.output_tokens, 0) AS output_tokens, \
                COALESCE(agg.cost_usd_micros, 0) AS cost_usd_micros \
         FROM calendar \
         LEFT JOIN ( \
            SELECT date(created_at / 1000, 'unixepoch') AS day, \
                   COUNT(DISTINCT COALESCE(request_id, CAST(id AS TEXT))) AS request_count, \
                   COALESCE(SUM(input_tokens), 0) AS input_tokens, \
                   COALESCE(SUM(output_tokens), 0) AS output_tokens, \
                   COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 AND settled = 1 \
                        THEN cost_usd_micros ELSE 0 END), 0) AS cost_usd_micros \
            FROM request_log WHERE created_at >= ?{} \
            GROUP BY day \
         ) agg ON agg.day = calendar.day \
         ORDER BY calendar.day",
        user_scope_clause(user_id)
    );
    let mut query = sqlx::query(AssertSqlSafe(sql))
        .bind(from_created_at)
        .bind(days as i64)
        .bind(from_created_at);
    if let Some(user_id) = user_id {
        query = query.bind(user_id);
    }
    let rows = query.fetch_all(pool).await.map_err(StoreError::Query)?;

    rows.iter().map(trend_bucket).collect()
}

/// 分布聚合的分组列。
enum CostDimension {
    Model,
    Channel,
}

/// 按模型或按渠道聚合费用/请求；费用仅计 2xx。
async fn query_cost_share(
    pool: &SqlitePool,
    from_created_at: i64,
    dimension: CostDimension,
    user_id: Option<i64>,
) -> Result<Vec<CostShare>, StoreError> {
    let column = match dimension {
        CostDimension::Model => "model",
        CostDimension::Channel => "channel",
    };
    let sql = format!(
        "SELECT {column} AS name, COUNT(DISTINCT COALESCE(request_id, CAST(id AS TEXT))) AS request_count, \
         COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 AND settled = 1 THEN cost_usd_micros ELSE 0 END), 0) \
           AS cost_usd_micros \
         FROM request_log WHERE created_at >= ?{} \
         GROUP BY {column} \
         ORDER BY cost_usd_micros DESC, name ASC",
        user_scope_clause(user_id)
    );
    let mut query = sqlx::query(AssertSqlSafe(sql)).bind(from_created_at);
    if let Some(user_id) = user_id {
        query = query.bind(user_id);
    }
    let rows = query.fetch_all(pool).await.map_err(StoreError::Query)?;

    let mut shares = Vec::with_capacity(rows.len());
    for row in rows {
        shares.push(CostShare {
            name: row.try_get("name").map_err(StoreError::Query)?,
            request_count: as_count(row.try_get("request_count").map_err(StoreError::Query)?),
            cost_usd_micros: row.try_get("cost_usd_micros").map_err(StoreError::Query)?,
        });
    }
    Ok(shares)
}

/// 执行 `SELECT COUNT(*) AS cnt ...`，把结果夹到非负 u64。
async fn count_rows(pool: &SqlitePool, sql: &'static str) -> Result<u64, StoreError> {
    let row = sqlx::query(sql)
        .fetch_one(pool)
        .await
        .map_err(StoreError::Query)?;
    let count: i64 = row.try_get("cnt").map_err(StoreError::Query)?;
    Ok(as_count(count))
}

/// SQLite 聚合整数转计数；负值视为 0。
pub(crate) fn as_count(value: i64) -> u64 {
    value.max(0) as u64
}

/// 归属过滤片段，拼在已有 `WHERE` 之后；`Some` 时调用方须紧接着 bind 该 id。
///
/// 用拼接而非 `(? IS NULL OR user_id = ?)`：后者会让 SQLite 放弃
/// `idx_request_log_user_id`，而归属视图正是最常走的那条路径。
fn user_scope_clause(user_id: Option<i64>) -> &'static str {
    if user_id.is_some() {
        " AND user_id = ?"
    } else {
        ""
    }
}

/// 同 [`user_scope_clause`]，但用于本身没有 `WHERE` 的查询。
fn lifetime_user_scope_clause(user_id: Option<i64>) -> &'static str {
    if user_id.is_some() {
        " WHERE user_id = ?"
    } else {
        ""
    }
}

/// 页码从 1 起，每页条数夹到 `[1, 200]`。请求日志与系统日志共用。
pub(crate) fn clamp_page(page: u64, page_size: u64) -> (u64, u64) {
    (page.max(1), page_size.clamp(1, 200))
}

/// 向查询拼接一个条件：首个条件以 `WHERE` 开头，其余以 `AND` 连接。
pub(crate) fn push_where_cond(
    qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>,
    first: &mut bool,
    condition: &str,
) {
    qb.push(if *first { " WHERE " } else { " AND " });
    *first = false;
    qb.push(condition);
}

/// 可选时间窗：`created_at >= from` 与 `created_at <= to`。
pub(crate) fn push_created_at_range(
    qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>,
    first: &mut bool,
    from: Option<i64>,
    to: Option<i64>,
) {
    if let Some(from) = from {
        push_where_cond(qb, first, "created_at >= ");
        qb.push_bind(from);
    }
    if let Some(to) = to {
        push_where_cond(qb, first, "created_at <= ");
        qb.push_bind(to);
    }
}

/// 非空时拼接 `column IN (...)`。`column` 仅允许调用方硬编码的标识符。
pub(crate) fn push_column_in(
    qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>,
    first: &mut bool,
    column: &'static str,
    values: &[String],
) {
    if values.is_empty() {
        return;
    }
    push_where_cond(qb, first, column);
    qb.push(" IN (");
    let mut separated = qb.separated(", ");
    for value in values {
        separated.push_bind(value);
    }
    separated.push_unseparated(")");
}

/// 分页 LIMIT/OFFSET：页码与每页条数在边界防御，超大偏移只返回空页。
pub(crate) fn push_limit_offset(
    qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>,
    page: u64,
    page_size: u64,
) {
    // `page`/`page_size` 可能为 0（`Default` 派生或结构体字面量绕过构造器夹取），
    // saturating 避免下溢。offset 夹到 `i64::MAX` 再转 i64，防止超大页码经
    // `as i64` 回绕成负偏移（SQLite 拒绝负 OFFSET）。
    let page_size = page_size.max(1);
    let offset = page
        .saturating_sub(1)
        .saturating_mul(page_size)
        .min(i64::MAX as u64);
    qb.push(" LIMIT ").push_bind(page_size as i64);
    qb.push(" OFFSET ").push_bind(offset as i64);
}

/// 把 `filter` 中非空条件以 AND 拼入 WHERE 子句。
fn push_request_log_filters(qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>, filter: &RequestLogQuery) {
    let mut first = true;
    if let Some(user_id) = filter.user_id {
        push_where_cond(qb, &mut first, "user_id = ");
        qb.push_bind(user_id);
    }
    if let Some(token_key) = &filter.token_key {
        push_where_cond(qb, &mut first, "token_key = ");
        qb.push_bind(token_key);
    }
    if let Some(token_name) = &filter.token_name {
        push_where_cond(qb, &mut first, "token_name = ");
        qb.push_bind(token_name);
    }
    if let Some(model) = &filter.model {
        push_where_cond(qb, &mut first, "model = ");
        qb.push_bind(model);
    }
    if let Some(channel) = &filter.channel {
        push_where_cond(qb, &mut first, "channel = ");
        qb.push_bind(channel);
    }
    if let Some(keyword) = filter
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|kw| !kw.is_empty())
    {
        let pattern = like_substring_pattern(keyword);
        push_where_cond(qb, &mut first, "(token_key LIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" ESCAPE '\\' OR token_name LIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" ESCAPE '\\' OR model LIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" ESCAPE '\\' OR channel LIKE ");
        qb.push_bind(pattern);
        qb.push(" ESCAPE '\\')");
    }
    push_created_at_range(qb, &mut first, filter.from_created_at, filter.to_created_at);
    if let Some(settled) = filter.settled {
        push_where_cond(qb, &mut first, "settled = ");
        qb.push_bind(settled as i64);
    }
    push_column_in(
        qb,
        &mut first,
        "inbound_protocol",
        &filter.inbound_protocols,
    );
}

/// 把白名单排序列拼进 `ORDER BY`；同向 `id` 保证分页稳定。
fn push_request_log_order(qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>, filter: &RequestLogQuery) {
    qb.push(" ORDER BY ");
    match filter.sort_by {
        RequestLogSortBy::Created => {
            qb.push("created_at");
        }
        RequestLogSortBy::Tokens => {
            // 与 Token 列一致：只计 input/output，缓存档有单独列。
            qb.push("(input_tokens + output_tokens)");
        }
        RequestLogSortBy::Latency => {
            qb.push("latency_ms");
        }
        RequestLogSortBy::Cache => {
            qb.push("(cache_read_tokens + cache_write_tokens)");
        }
        RequestLogSortBy::Cost => {
            qb.push("cost_usd_micros");
        }
    }
    qb.push(filter.sort_dir.sql());
    qb.push(", id");
    qb.push(filter.sort_dir.sql());
}

/// 关键字 → LIKE 子串模式：转义 `\`/`%`/`_`（配合 `ESCAPE '\'`），两端补 `%`。
pub(crate) fn like_substring_pattern(keyword: &str) -> String {
    let mut pattern = String::with_capacity(keyword.len() + 2);
    pattern.push('%');
    for ch in keyword.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

/// 把请求日志行映射为 `RequestLog`。列表查询不选 BLOB 列，`include_body` 为 false。
fn map_request_log_row(
    row: &sqlx::sqlite::SqliteRow,
    include_body: bool,
) -> Result<RequestLog, StoreError> {
    let price = PriceSnapshot {
        input_micros: row
            .try_get("input_price_usd_micros")
            .map_err(StoreError::Query)?,
        output_micros: row
            .try_get("output_price_usd_micros")
            .map_err(StoreError::Query)?,
        cache_read_micros: row
            .try_get("cache_read_price_usd_micros")
            .map_err(StoreError::Query)?,
        cache_write_micros: row
            .try_get("cache_write_price_usd_micros")
            .map_err(StoreError::Query)?,
    };
    Ok(RequestLog {
        id: row.try_get("id").map_err(StoreError::Query)?,
        created_at: row.try_get("created_at").map_err(StoreError::Query)?,
        token_name: row.try_get("token_name").map_err(StoreError::Query)?,
        token_key: row.try_get("token_key").map_err(StoreError::Query)?,
        user_id: row.try_get("user_id").map_err(StoreError::Query)?,
        inbound_protocol: row.try_get("inbound_protocol").map_err(StoreError::Query)?,
        model: row.try_get("model").map_err(StoreError::Query)?,
        outbound_model: row.try_get("outbound_model").map_err(StoreError::Query)?,
        channel: row.try_get("channel").map_err(StoreError::Query)?,
        status_code: row.try_get("status_code").map_err(StoreError::Query)?,
        latency_ms: row.try_get("latency_ms").map_err(StoreError::Query)?,
        input_tokens: row.try_get("input_tokens").map_err(StoreError::Query)?,
        output_tokens: row.try_get("output_tokens").map_err(StoreError::Query)?,
        cache_read_tokens: row
            .try_get("cache_read_tokens")
            .map_err(StoreError::Query)?,
        cache_write_tokens: row
            .try_get("cache_write_tokens")
            .map_err(StoreError::Query)?,
        price,
        cost_usd_micros: row.try_get("cost_usd_micros").map_err(StoreError::Query)?,
        settled: row
            .try_get::<i64, _>("settled")
            .map_err(StoreError::Query)?
            != 0,
        request_id: None,
        request_body: if include_body {
            row.try_get("request_body").map_err(StoreError::Query)?
        } else {
            None
        },
        response_body: if include_body {
            row.try_get("response_body").map_err(StoreError::Query)?
        } else {
            None
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::billing::PriceSnapshot;
    use sqlx::Connection;

    /// 建一个临时 SQLite 连接池并跑完全部迁移。
    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let pool = open(&dir.path().join("test.db"))
            .await
            .expect("应能打开临时库");
        (dir, pool)
    }

    /// 直写一条令牌定义行：`token_balance` 外键指向 `tokens`，余额相关测试
    /// 需先有归属令牌。
    async fn seed_token(conn: &mut SqliteConnection, token_key: &str) {
        sqlx::query(
            "INSERT INTO tokens (token_key, name, enabled, created_at) VALUES (?, ?, 1, 0)",
        )
        .bind(token_key)
        .bind(token_key)
        .execute(&mut *conn)
        .await
        .expect("应能写令牌行");
    }

    /// 空库迁移后即有内置 root（id=1）与零额钱包；尚未设密码。
    #[tokio::test]
    async fn open_seeds_root_user_and_empty_wallet() {
        let (_dir, pool) = test_pool().await;
        let row: (i64, String, Option<String>, String, i64) = sqlx::query_as(
            "SELECT id, email, password_hash, role, enabled FROM users WHERE id = 1",
        )
        .fetch_one(&pool)
        .await
        .expect("应有内置 root");
        assert_eq!(row.0, 1);
        assert_eq!(row.1, "root@localhost");
        assert!(row.2.is_none(), "尚未设密码");
        assert_eq!(row.3, "root");
        assert_eq!(row.4, 1);

        let wallet: (i64, i64) = sqlx::query_as(
            "SELECT balance_usd_micros, settled_usd_micros FROM user_balance WHERE user_id = 1",
        )
        .fetch_one(&pool)
        .await
        .expect("应有用户钱包");
        assert_eq!(wallet, (0, 0));

        let assigned: String =
            sqlx::query_scalar("SELECT group_name FROM user_model_groups WHERE user_id = 1")
                .fetch_one(&pool)
                .await
                .expect("root 应有默认可用组");
        assert_eq!(assigned, "default");

        let remaining_col: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('token_balance') \
             WHERE name = 'balance_usd_micros'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能查列");
        assert_eq!(remaining_col, 0, "token_balance 不应再存剩余余额");
    }

    /// 同一用户的多把令牌共用钱包：扣第一把，第二把读到同一剩余；settled 仍按令牌分开。
    #[tokio::test]
    async fn tokens_of_same_user_share_wallet() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        seed_token(&mut conn, "sk-a").await;
        seed_token(&mut conn, "sk-b").await;
        ensure_token_balance(&mut conn, "sk-a", 5.0, 1)
            .await
            .expect("应能初始化 a");
        ensure_token_balance(&mut conn, "sk-b", 0.0, 1)
            .await
            .expect("应能初始化 b");

        settle_charge(&mut conn, "sk-a", 1_000_000)
            .await
            .expect("应能结算");

        let a = get_token_balance(&mut conn, "sk-a")
            .await
            .expect("应能读")
            .expect("a 应有视图");
        let b = get_token_balance(&mut conn, "sk-b")
            .await
            .expect("应能读")
            .expect("b 应有视图");
        assert_eq!(a.balance_usd_micros, 4_000_000);
        assert_eq!(b.balance_usd_micros, 4_000_000);
        assert_eq!(a.settled_usd_micros, 1_000_000);
        assert_eq!(b.settled_usd_micros, 0);
    }

    /// 钱包相对调整：充值/扣减同一原语，只动剩余、不动累计结算额。
    #[tokio::test]
    async fn adjust_user_balance_recharges_and_deducts() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        seed_token(&mut conn, "sk-a").await;
        ensure_token_balance(&mut conn, "sk-a", 10.0, 1)
            .await
            .expect("应能初始化余额");

        let (balance, settled) = adjust_user_balance(&mut conn, resources::ROOT_USER_ID, 5_000_000)
            .await
            .expect("应能充值");
        assert_eq!(balance, 15_000_000);
        assert_eq!(settled, 0, "调账不动累计结算额");

        let (balance, settled) =
            adjust_user_balance(&mut conn, resources::ROOT_USER_ID, -3_000_000)
                .await
                .expect("应能扣减");
        assert_eq!(balance, 12_000_000);
        assert_eq!(settled, 0);

        // 令牌视图读到的剩余就是所属用户的钱包。
        let view = get_token_balance(&mut conn, "sk-a")
            .await
            .expect("应能读")
            .expect("应有视图");
        assert_eq!(view.balance_usd_micros, 12_000_000);
    }

    /// 连接选项治理 SQLite 坏默认值：WAL 日志模式、NORMAL 同步、外键强制、
    /// 写锁排队 5 秒（缺省分别是 DELETE、FULL、关闭、立即 BUSY）。
    #[tokio::test]
    async fn open_applies_hardened_pragmas() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");

        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&mut *conn)
            .await
            .expect("应能查日志模式");
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");

        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&mut *conn)
            .await
            .expect("应能查同步档位");
        assert_eq!(synchronous, 1, "1 = NORMAL");

        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut *conn)
            .await
            .expect("应能查外键开关");
        assert_eq!(foreign_keys, 1);

        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&mut *conn)
            .await
            .expect("应能查写锁等待");
        assert_eq!(busy_timeout, SQLITE_BUSY_TIMEOUT.as_millis() as i64);
    }

    /// 业务表一律 STRICT：错类型写入直接报错，而非按亲和性静默收下。逐表探测，
    /// 任一表回退成非 STRICT 都会被此测试捕获。探测方向：INTEGER 列写 TEXT/REAL；
    /// `settings` 无 INTEGER 列，用 BLOB 写 TEXT 列（STRICT 拒绝，非 STRICT 的
    /// TEXT 亲和性会原样收下）。
    #[tokio::test]
    async fn strict_tables_reject_wrong_types() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        // token_balance 有外键，先播种归属令牌，让探测只命中 STRICT 而非外键。
        seed_token(&mut conn, "strict-probe").await;
        let channel_id = crate::store::resources::insert_channel(
            &mut conn,
            &crate::store::resources::Channel {
                name: "strict-price".to_string(),
                protocol: crate::config::Protocol::OpenAiChat,
                base_url: "http://127.0.0.1:9".to_string(),
                api_key: "sk".to_string(),
                models: vec![],
                model_aliases: std::collections::HashMap::new(),
                priority: 0,
                weight: 1,
                timeout_ms: 1000,
                max_retries: 0,
                enabled: true,
                model_group: crate::store::resources::DEFAULT_MODEL_GROUP.to_string(),
            },
        )
        .await
        .expect("应能写渠道");

        let probes = [
            (
                "smoke_probe",
                "INSERT INTO smoke_probe (note, id) VALUES ('x', 'not-a-number')",
            ),
            (
                "tokens",
                "INSERT INTO tokens (token_key, name, enabled, created_at) \
                 VALUES ('k1', 'n', 1, 'not-a-number')",
            ),
            (
                "token_balance",
                "INSERT INTO token_balance (token_key, settled_usd_micros, created_at) \
                 VALUES ('strict-probe', 'not-a-number', 0)",
            ),
            (
                "users",
                "INSERT INTO users (id, email, display_name, role, enabled, created_at) \
                 VALUES ('not-a-number', 'a@b.c', 'n', 'user', 1, 0)",
            ),
            (
                "user_balance",
                "INSERT INTO user_balance (user_id, balance_usd_micros, settled_usd_micros, created_at) \
                 VALUES ('not-a-number', 0, 0, 0)",
            ),
            (
                "user_model_groups",
                "INSERT INTO user_model_groups (user_id, group_name) VALUES ('not-a-number', 'default')",
            ),
            (
                "management_sessions",
                "INSERT INTO management_sessions (token_hash, user_id, created_at, expires_at, revoked) \
                 VALUES ('h', 'not-a-number', 0, 0, 0)",
            ),
            (
                "request_log",
                "INSERT INTO request_log (token_name, inbound_protocol, model, channel, \
                     status_code, latency_ms, created_at) \
                 VALUES ('t', 'openai_chat', 'm', 'c', 200, 10, 'not-a-number')",
            ),
            (
                "channels",
                "INSERT INTO channels (name, protocol, base_url, api_key, models_json, \
                     model_aliases_json, priority, weight, timeout_ms, max_retries) \
                 VALUES ('c', 'openai_chat', 'u', 'k', '[]', '{}', 'not-a-number', 1, 1000, 1)",
            ),
            (
                "settings",
                "INSERT INTO settings (setting_key, setting_value) VALUES ('k2', x'00')",
            ),
            (
                "model_groups",
                "INSERT INTO model_groups (name, models_json) VALUES ('k3', x'00')",
            ),
            (
                "unified_models",
                "INSERT INTO unified_models (id, models_json, hide) VALUES ('k4', x'00', 0)",
            ),
            (
                "catalog_models",
                "INSERT INTO catalog_models (provider_id, provider_name, model_id, input_micros) \
                 VALUES ('p', 'P', 'm', 'not-a-number')",
            ),
        ];
        for (table, sql) in probes {
            let result = sqlx::query(sql).execute(&pool).await;
            assert!(
                result.is_err(),
                "{table} 应仍是 STRICT 表，错类型写入须被拒"
            );
        }

        assert!(
            sqlx::query(
                "INSERT INTO prices (channel_id, model, input_micros, output_micros) \
                 VALUES (?, 'm', 'not-a-number', 0)"
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .is_err(),
            "prices 应仍是 STRICT 表，错类型写入须被拒"
        );

        let result = sqlx::query(
            "INSERT INTO prices (channel_id, model, input_micros, output_micros) \
             VALUES (?, 'm2', 1.5, 0)",
        )
        .bind(channel_id)
        .execute(&pool)
        .await;
        assert!(result.is_err(), "INTEGER 列写 REAL 应被 STRICT 拒绝");
    }

    /// token_balance 外键：无归属令牌的余额行被拒绝；删除令牌级联清理余额行，
    /// 同 key 重建不再复活旧余额。
    #[tokio::test]
    async fn token_balance_fk_enforced_and_cascades() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");

        let orphan = sqlx::query(
            "INSERT INTO token_balance (token_key, settled_usd_micros, created_at) \
             VALUES ('sk-ghost', 0, 0)",
        )
        .execute(&mut *conn)
        .await;
        assert!(orphan.is_err(), "外键应拒绝无归属令牌的余额行");

        seed_token(&mut conn, "sk-a").await;
        ensure_token_balance(&mut conn, "sk-a", 5.0, 1)
            .await
            .expect("应能初始化余额");
        sqlx::query("DELETE FROM tokens WHERE token_key = ?")
            .bind("sk-a")
            .execute(&mut *conn)
            .await
            .expect("应能删令牌");
        let balance = get_token_balance(&mut conn, "sk-a")
            .await
            .expect("应能查余额");
        assert!(balance.is_none(), "级联删除应带走余额行");
    }

    /// 迁移 0001–0006 全部应用后的表终态（非 STRICT、无外键）。配合播种
    /// `_sqlx_migrations` 记账行（版本 1–6 标记已应用），`open()` 只会应用
    /// 迁移 0007，精确模拟存量库升级。
    const LEGACY_SCHEMA: &str = "
        CREATE TABLE smoke_probe (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            note TEXT NOT NULL
        );
        CREATE TABLE request_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at INTEGER NOT NULL,
            token_name TEXT NOT NULL,
            inbound_protocol TEXT NOT NULL,
            model TEXT NOT NULL,
            channel TEXT NOT NULL,
            status_code INTEGER NOT NULL,
            latency_ms INTEGER NOT NULL,
            token_key TEXT NOT NULL DEFAULT '',
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0,
            cache_write_tokens INTEGER NOT NULL DEFAULT 0,
            input_price_usd_micros INTEGER NOT NULL DEFAULT 0,
            output_price_usd_micros INTEGER NOT NULL DEFAULT 0,
            cache_read_price_usd_micros INTEGER NOT NULL DEFAULT 0,
            cache_write_price_usd_micros INTEGER NOT NULL DEFAULT 0,
            cost_usd_micros INTEGER NOT NULL DEFAULT 0,
            request_body BLOB,
            response_body BLOB
        );
        CREATE TABLE token_balance (
            token_key TEXT PRIMARY KEY,
            balance_usd_micros INTEGER NOT NULL,
            settled_usd_micros INTEGER NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE channels (
            name TEXT PRIMARY KEY,
            protocol TEXT NOT NULL,
            base_url TEXT NOT NULL,
            api_key TEXT NOT NULL,
            models_json TEXT NOT NULL,
            model_aliases_json TEXT NOT NULL,
            priority INTEGER NOT NULL,
            weight INTEGER NOT NULL,
            timeout_ms INTEGER NOT NULL,
            max_retries INTEGER NOT NULL
        );
        CREATE TABLE tokens (
            token_key TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            limit_usd_micros INTEGER,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT 0,
            last_used_at INTEGER
        );
        CREATE TABLE prices (
            model TEXT PRIMARY KEY,
            input_micros INTEGER NOT NULL,
            output_micros INTEGER NOT NULL,
            cache_read_micros INTEGER,
            cache_write_micros INTEGER
        );
        CREATE TABLE settings (
            setting_key TEXT PRIMARY KEY,
            setting_value TEXT NOT NULL
        );";

    /// sqlx 迁移记账表（结构与 sqlx-sqlite 建表语句一致）：手工播种版本 1–6
    /// 的已应用记录，校验和取自 `migrate!()` 嵌入内容，与真实应用无异。
    async fn seed_migrations_bookkeeping(raw: &mut SqliteConnection) {
        sqlx::raw_sql(
            "CREATE TABLE _sqlx_migrations (
                version BIGINT PRIMARY KEY,
                description TEXT NOT NULL,
                installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                success BOOLEAN NOT NULL,
                checksum BLOB NOT NULL,
                execution_time BIGINT NOT NULL
            );",
        )
        .execute(&mut *raw)
        .await
        .expect("应能建迁移记账表");
        for migration in sqlx::migrate!().iter().filter(|m| m.version < 7) {
            sqlx::query(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
                 VALUES (?, ?, TRUE, ?, -1)",
            )
            .bind(migration.version)
            .bind(&*migration.description)
            .bind(&*migration.checksum)
            .execute(&mut *raw)
            .await
            .expect("应能记账已应用迁移");
        }
    }

    /// 存量库升级路径：真实部署的旧库带脏数据（BLOB 列被写入 TEXT、孤儿余额行），
    /// `open()` 应用迁移 0007 后须完成清理、字节无损转 BLOB、全表 STRICT 化，
    /// 且 AUTOINCREMENT 计数延续。
    #[tokio::test]
    async fn legacy_db_upgrades_through_migration_0007() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("legacy.db");
        std::fs::File::create(&path).expect("应能创建空库文件");
        let mut raw = SqliteConnection::connect(path.to_str().expect("路径应可转字符串"))
            .await
            .expect("应能建旧库");
        seed_migrations_bookkeeping(&mut raw).await;
        sqlx::raw_sql(LEGACY_SCHEMA)
            .execute(&mut raw)
            .await
            .expect("应能建旧 schema");
        sqlx::query("INSERT INTO tokens (token_key, name) VALUES ('sk-live', '生产')")
            .execute(&mut raw)
            .await
            .expect("应能写旧令牌");
        sqlx::query(
            "INSERT INTO token_balance (token_key, balance_usd_micros, settled_usd_micros, created_at) \
             VALUES ('sk-live', 1500000, 200, 0)",
        )
        .execute(&mut raw)
        .await
        .expect("应能写旧令牌余额");
        // 脏数据一：无归属令牌的孤儿余额行。
        sqlx::query(
            "INSERT INTO token_balance (token_key, balance_usd_micros, settled_usd_micros, created_at) \
             VALUES ('sk-orphan', 999, 0, 0)",
        )
        .execute(&mut raw)
        .await
        .expect("旧库无外键，孤儿余额应能写入");
        // 脏数据二：BLOB 列被按 TEXT 亲和性收下字符串。
        sqlx::query(
            "INSERT INTO request_log (created_at, token_name, inbound_protocol, model, channel, \
                 status_code, latency_ms, token_key, request_body) \
             VALUES (1, '生产', 'openai_chat', 'gpt-4o', 'c1', 200, 10, 'sk-live', 'legacy text body')",
        )
        .execute(&mut raw)
        .await
        .expect("旧库非 STRICT，TEXT 写 BLOB 列应能写入");
        raw.close().await.expect("应能关闭旧库连接");

        let pool = open(&path).await.expect("迁移应能吃下带脏数据的旧库");
        let mut conn = pool.acquire().await.expect("应能获取连接");

        let strict: i64 =
            sqlx::query_scalar("SELECT strict FROM pragma_table_list WHERE name = 'request_log'")
                .fetch_one(&mut *conn)
                .await
                .expect("应能查表属性");
        assert_eq!(strict, 1, "重建后应为 STRICT 表");

        let body: Vec<u8> = sqlx::query_scalar("SELECT request_body FROM request_log")
            .fetch_one(&mut *conn)
            .await
            .expect("日志应被保留");
        assert_eq!(body, b"legacy text body", "TEXT 应字节无损转为 BLOB");

        let balance = get_token_balance(&mut conn, "sk-orphan")
            .await
            .expect("应能查余额");
        assert!(balance.is_none(), "孤儿余额行应被迁移清理");
        let balance = get_token_balance(&mut conn, "sk-live")
            .await
            .expect("应能查余额")
            .expect("存量令牌应能读到用户钱包");
        assert_eq!(
            balance.balance_usd_micros, 1_500_000,
            "root 钱包应为各令牌剩余之和"
        );
        assert_eq!(balance.settled_usd_micros, 200, "令牌 settled 应保留");
        let wallet: (i64, i64) = sqlx::query_as(
            "SELECT balance_usd_micros, settled_usd_micros FROM user_balance WHERE user_id = 1",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("应有 root 钱包");
        assert_eq!(wallet, (1_500_000, 200));
        let owner: i64 =
            sqlx::query_scalar("SELECT user_id FROM tokens WHERE token_key = 'sk-live'")
                .fetch_one(&mut *conn)
                .await
                .expect("令牌应有归属");
        assert_eq!(owner, 1);

        let id = insert_smoke(&pool, "after-upgrade")
            .await
            .expect("升级后应能写入");
        assert!(id >= 1, "AUTOINCREMENT 计数应延续");
    }

    /// 请求日志分页查询：时间倒序、LIMIT/OFFSET 生效、过滤维度生效。
    #[tokio::test]
    async fn request_log_query_paginates_and_filters() {
        let (_dir, pool) = test_pool().await;
        let price = PriceSnapshot {
            input_micros: 2_500_000,
            output_micros: 10_000_000,
            cache_read_micros: 1_250_000,
            cache_write_micros: 10_000_000,
        };
        for (i, model) in ["gpt-4o", "gpt-4o-mini", "gpt-4o", "gpt-4o-mini"]
            .iter()
            .enumerate()
        {
            insert_request_log(
                &pool,
                &RequestLog {
                    id: 0,
                    created_at: 1000 + i as i64,
                    token_name: format!("t{i}"),
                    token_key: "sk-a".to_string(),
                    user_id: resources::ROOT_USER_ID,
                    inbound_protocol: "openai_chat".to_string(),
                    model: model.to_string(),
                    outbound_model: None,
                    channel: "c1".to_string(),
                    status_code: 200,
                    latency_ms: 10,
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    price,
                    cost_usd_micros: i as i64,
                    settled: true,
                    request_id: None,
                    request_body: None,
                    response_body: None,
                },
            )
            .await
            .expect("应能写请求日志");
        }

        // 分页：每页 2 条，第一页取最新两条（时间倒序）。
        let page1 = RequestLogQuery::new(1, 2);
        let rows = query_request_logs(&pool, &page1).await.expect("应能查询");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].created_at, 1003, "倒序：最新在前");
        assert_eq!(rows[1].created_at, 1002);

        // 页码 2：取剩余两条。
        let page2 = RequestLogQuery::new(2, 2);
        let rows = query_request_logs(&pool, &page2).await.expect("应能查询");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].created_at, 1001);
        assert_eq!(rows[1].created_at, 1000);

        // 按模型过滤 + 统计总数。
        let mut filter = RequestLogQuery::new(1, 10);
        filter.model = Some("gpt-4o".to_string());
        let rows = query_request_logs(&pool, &filter).await.expect("应能过滤");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.model == "gpt-4o"));
        assert_eq!(
            count_request_logs(&pool, &filter).await.expect("应能统计"),
            2
        );

        // 时间范围过滤。
        let mut filter = RequestLogQuery::new(1, 10);
        filter.from_created_at = Some(1002);
        let rows = query_request_logs(&pool, &filter).await.expect("应能过滤");
        assert_eq!(
            count_request_logs(&pool, &filter).await.expect("应能统计"),
            2
        );
        assert!(rows.iter().all(|r| r.created_at >= 1002));

        let mut filter = RequestLogQuery::new(1, 10);
        filter.sort_dir = SortDir::Asc;
        let rows = query_request_logs(&pool, &filter).await.expect("应能正序");
        assert_eq!(rows[0].created_at, 1000);
        assert_eq!(rows[3].created_at, 1003);

        filter.sort_by = RequestLogSortBy::Cost;
        let rows = query_request_logs(&pool, &filter)
            .await
            .expect("应能按费用排");
        assert_eq!(
            rows.iter()
                .map(|row| row.cost_usd_micros)
                .collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );

        let mut proto = RequestLogQuery::new(1, 10);
        proto.inbound_protocols = vec!["openai_chat".to_string()];
        assert_eq!(
            count_request_logs(&pool, &proto)
                .await
                .expect("应能按协议过滤"),
            4
        );
        proto.inbound_protocols = vec!["anthropic_messages".to_string()];
        assert_eq!(
            count_request_logs(&pool, &proto)
                .await
                .expect("应能按协议过滤"),
            0
        );
    }

    /// `keyword` 综合搜索：对 token_key/token_name/model/channel 做 LIKE OR 子串匹配，
    /// 与其余条件 AND 组合；`%`/`_` 等通配符按字面量转义。
    #[tokio::test]
    async fn request_log_query_filters_by_keyword() {
        let (_dir, pool) = test_pool().await;
        let price = PriceSnapshot {
            input_micros: 0,
            output_micros: 0,
            cache_read_micros: 0,
            cache_write_micros: 0,
        };
        let rows = [
            ("sk-alpha", "生产令牌", "gpt-4o", "azure-east"),
            ("sk-beta", "测试", "claude-3", "openai-direct"),
            ("sk-gamma", "试用", "gpt-4o-mini", "azure-west"),
        ];
        for (i, (token_key, token_name, model, channel)) in rows.iter().enumerate() {
            insert_request_log(
                &pool,
                &RequestLog {
                    id: 0,
                    created_at: 2000 + i as i64,
                    token_name: (*token_name).to_string(),
                    token_key: (*token_key).to_string(),
                    user_id: resources::ROOT_USER_ID,
                    inbound_protocol: "openai_chat".to_string(),
                    model: (*model).to_string(),
                    outbound_model: None,
                    channel: (*channel).to_string(),
                    status_code: 200,
                    latency_ms: 10,
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    price,
                    cost_usd_micros: 0,
                    settled: true,
                    request_id: None,
                    request_body: None,
                    response_body: None,
                },
            )
            .await
            .expect("应能写请求日志");
        }

        // 命中 token_key 子串。
        let mut filter = RequestLogQuery::new(1, 10);
        filter.keyword = Some("alpha".to_string());
        let rows = query_request_logs(&pool, &filter).await.expect("应能查询");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].token_key, "sk-alpha");

        // 命中 channel 子串（OR 语义：azure 命中两条）。
        let mut filter = RequestLogQuery::new(1, 10);
        filter.keyword = Some("azure".to_string());
        assert_eq!(
            count_request_logs(&pool, &filter).await.expect("应能统计"),
            2
        );

        // 命中 token_name（中文）并与模型条件 AND 组合。
        let mut filter = RequestLogQuery::new(1, 10);
        filter.keyword = Some("令牌".to_string());
        filter.model = Some("gpt-4o".to_string());
        let rows = query_request_logs(&pool, &filter).await.expect("应能查询");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].token_name, "生产令牌");

        // 通配符按字面量处理：`_` 不应匹配任意字符。
        let mut filter = RequestLogQuery::new(1, 10);
        filter.keyword = Some("sk_".to_string());
        let rows = query_request_logs(&pool, &filter).await.expect("应能查询");
        assert!(rows.is_empty(), "转义后 `_` 不是通配符");

        // 空白关键字视作不过滤。
        let mut filter = RequestLogQuery::new(1, 10);
        filter.keyword = Some("   ".to_string());
        assert_eq!(
            count_request_logs(&pool, &filter).await.expect("应能统计"),
            3
        );
    }

    /// 行内筛选按列精确匹配：渠道/令牌名不是关键字 OR，子串不误伤。
    #[tokio::test]
    async fn request_log_query_filters_exact_channel_and_token_name() {
        let (_dir, pool) = test_pool().await;
        let price = PriceSnapshot {
            input_micros: 0,
            output_micros: 0,
            cache_read_micros: 0,
            cache_write_micros: 0,
        };
        let rows = [
            ("生产", "sk-a", "gpt-4o", "azure"),
            ("生产备用", "sk-b", "gpt-4o", "azure-east"),
        ];
        for (i, (token_name, token_key, model, channel)) in rows.iter().enumerate() {
            insert_request_log(
                &pool,
                &RequestLog {
                    id: 0,
                    created_at: 3000 + i as i64,
                    token_name: (*token_name).to_string(),
                    token_key: (*token_key).to_string(),
                    user_id: resources::ROOT_USER_ID,
                    inbound_protocol: "openai_chat".to_string(),
                    model: (*model).to_string(),
                    outbound_model: None,
                    channel: (*channel).to_string(),
                    status_code: 200,
                    latency_ms: 10,
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    price,
                    cost_usd_micros: 0,
                    settled: true,
                    request_id: None,
                    request_body: None,
                    response_body: None,
                },
            )
            .await
            .expect("应能写请求日志");
        }

        let mut by_channel = RequestLogQuery::new(1, 10);
        by_channel.channel = Some("azure".to_string());
        let rows = query_request_logs(&pool, &by_channel)
            .await
            .expect("应能按渠道精确过滤");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].channel, "azure");

        let mut by_name = RequestLogQuery::new(1, 10);
        by_name.token_name = Some("生产".to_string());
        let rows = query_request_logs(&pool, &by_name)
            .await
            .expect("应能按令牌名精确过滤");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].token_name, "生产");
    }

    /// Token 列排序只计 input+output：缓存量大的行不应排到展示量更小的行前面。
    #[tokio::test]
    async fn request_log_tokens_sort_excludes_cache() {
        let (_dir, pool) = test_pool().await;
        let price = PriceSnapshot {
            input_micros: 0,
            output_micros: 0,
            cache_read_micros: 0,
            cache_write_micros: 0,
        };
        insert_request_log(
            &pool,
            &RequestLog {
                id: 0,
                created_at: 1,
                token_name: "t".to_string(),
                token_key: "sk-a".to_string(),
                user_id: resources::ROOT_USER_ID,
                inbound_protocol: "openai_chat".to_string(),
                model: "cached".to_string(),
                outbound_model: None,
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 10,
                output_tokens: 10,
                cache_read_tokens: 1_000,
                cache_write_tokens: 0,
                price,
                cost_usd_micros: 0,
                settled: true,
                request_id: None,
                request_body: None,
                response_body: None,
            },
        )
        .await
        .expect("应能写请求日志");
        insert_request_log(
            &pool,
            &RequestLog {
                id: 0,
                created_at: 2,
                token_name: "t".to_string(),
                token_key: "sk-a".to_string(),
                user_id: resources::ROOT_USER_ID,
                inbound_protocol: "openai_chat".to_string(),
                model: "heavy".to_string(),
                outbound_model: None,
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 20,
                output_tokens: 20,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                price,
                cost_usd_micros: 0,
                settled: true,
                request_id: None,
                request_body: None,
                response_body: None,
            },
        )
        .await
        .expect("应能写请求日志");

        let mut filter = RequestLogQuery::new(1, 10);
        filter.sort_by = RequestLogSortBy::Tokens;
        filter.sort_dir = SortDir::Desc;
        let rows = query_request_logs(&pool, &filter)
            .await
            .expect("应能按 Token 列排序");
        assert_eq!(
            rows.iter()
                .map(|row| row.model.as_str())
                .collect::<Vec<_>>(),
            ["heavy", "cached"]
        );
    }

    /// 分页参数在查询边界防御：`Default` 派生的 page/page_size 为 0 时不 panic、
    /// 不下溢，且行为等同于第一页。
    #[tokio::test]
    async fn request_log_query_defends_zero_pagination() {
        let (_dir, pool) = test_pool().await;
        let price = PriceSnapshot {
            input_micros: 2_500_000,
            output_micros: 10_000_000,
            cache_read_micros: 1_250_000,
            cache_write_micros: 10_000_000,
        };
        insert_request_log(
            &pool,
            &RequestLog {
                id: 0,
                created_at: 1000,
                token_name: "t".to_string(),
                token_key: "sk-a".to_string(),
                user_id: resources::ROOT_USER_ID,
                inbound_protocol: "openai_chat".to_string(),
                model: "gpt-4o".to_string(),
                outbound_model: None,
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                price,
                cost_usd_micros: 12,
                settled: true,
                request_id: None,
                request_body: None,
                response_body: None,
            },
        )
        .await
        .expect("应能写请求日志");

        // `RequestLogQuery::default()` 的 page/page_size 均为 0，不应引发下溢。
        let rows = query_request_logs(&pool, &RequestLogQuery::default())
            .await
            .expect("page=0 不应 panic");
        assert_eq!(rows.len(), 1, "page=0 视作第一页且 page_size 至少为 1");

        // 超大页码：offset 经 i64::MAX 夹取不回绕成负偏移，SQLite 不报错，返回空页。
        let huge = RequestLogQuery::new(u64::MAX, 200);
        let rows = query_request_logs(&pool, &huge)
            .await
            .expect("超大页码不应触发负 OFFSET 报错");
        assert!(rows.is_empty(), "超大页码应返回空页而非报错");
    }

    /// 出站模型列可空：存量行不写出站名；新行写入后原样读回。
    #[tokio::test]
    async fn request_log_outbound_model_nullable_and_roundtrips() {
        let (_dir, pool) = test_pool().await;
        sqlx::query(
            "INSERT INTO request_log (created_at, token_name, inbound_protocol, model, channel, \
                 status_code, latency_ms) \
             VALUES (1, 't', 'openai_chat', 'fast', 'c1', 200, 10)",
        )
        .execute(&pool)
        .await
        .expect("缺 outbound_model 的存量行应能写入");

        let rows = query_request_logs(&pool, &RequestLogQuery::new(1, 10))
            .await
            .expect("应能查询");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "fast");
        assert_eq!(rows[0].outbound_model, None, "旧行出站名可空");

        insert_request_log(
            &pool,
            &RequestLog {
                id: 0,
                created_at: 2,
                token_name: "t".to_string(),
                token_key: "sk-a".to_string(),
                user_id: resources::ROOT_USER_ID,
                inbound_protocol: "openai_chat".to_string(),
                model: "fast".to_string(),
                outbound_model: Some("gpt-4o-mini".to_string()),
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                price: PriceSnapshot::default(),
                cost_usd_micros: 0,
                settled: true,
                request_id: None,
                request_body: None,
                response_body: None,
            },
        )
        .await
        .expect("应能写出站模型");

        let rows = query_request_logs(&pool, &RequestLogQuery::new(1, 10))
            .await
            .expect("应能查询");
        assert_eq!(rows[0].outbound_model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(rows[1].outbound_model, None);
        assert!(rows[1].settled, "缺 settled 列的存量行默认已结算");
    }

    /// 迁移 0016：热表过滤列有索引；未结算费用不进入 stats 聚合。
    #[tokio::test]
    async fn request_log_indexes_exist_and_unsettled_cost_is_excluded() {
        let (_dir, pool) = test_pool().await;
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'request_log'",
        )
        .fetch_all(&pool)
        .await
        .expect("应能查索引");
        for expected in [
            "idx_request_log_created_at",
            "idx_request_log_token_key",
            "idx_request_log_model",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "应有索引 {expected}，实际 {names:?}"
            );
        }

        let price = PriceSnapshot::default();
        insert_request_log(
            &pool,
            &RequestLog {
                id: 0,
                created_at: 1,
                token_name: "t".to_string(),
                token_key: "sk-a".to_string(),
                user_id: resources::ROOT_USER_ID,
                inbound_protocol: "openai_chat".to_string(),
                model: "gpt-4o".to_string(),
                outbound_model: None,
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                price,
                cost_usd_micros: 9_999,
                settled: false,
                request_id: None,
                request_body: None,
                response_body: None,
            },
        )
        .await
        .expect("应能写未结算日志");
        insert_request_log(
            &pool,
            &RequestLog {
                id: 0,
                created_at: 2,
                token_name: "t".to_string(),
                token_key: "sk-a".to_string(),
                user_id: resources::ROOT_USER_ID,
                inbound_protocol: "openai_chat".to_string(),
                model: "gpt-4o".to_string(),
                outbound_model: None,
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                price,
                cost_usd_micros: 100,
                settled: true,
                request_id: None,
                request_body: None,
                response_body: None,
            },
        )
        .await
        .expect("应能写已结算日志");

        let lifetime = query_lifetime_stats(&pool, None).await.expect("应能聚合");
        assert_eq!(lifetime.cost_usd_micros, 100, "未结算费用不应计入");
    }

    fn sample_log(created_at: i64, settled: bool) -> RequestLog {
        RequestLog {
            id: 0,
            created_at,
            token_name: "t".to_string(),
            token_key: "sk-a".to_string(),
            user_id: resources::ROOT_USER_ID,
            inbound_protocol: "openai_chat".to_string(),
            model: "m".to_string(),
            outbound_model: None,
            channel: "c".to_string(),
            status_code: 200,
            latency_ms: 1,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            price: PriceSnapshot::default(),
            cost_usd_micros: 1,
            settled,
            request_id: None,
            request_body: None,
            response_body: None,
        }
    }

    /// 同一下游请求的多跳对账行按 `request_id` 计一次；无 id 的存量行仍按主键计。
    #[tokio::test]
    async fn lifetime_stats_counts_unique_request_id() {
        let (_dir, pool) = test_pool().await;
        let mut hop1 = sample_log(1, true);
        hop1.request_id = Some("req-shared".to_string());
        hop1.status_code = 429;
        let mut hop2 = sample_log(2, true);
        hop2.request_id = Some("req-shared".to_string());
        insert_request_log(&pool, &hop1)
            .await
            .expect("应能写失败跳");
        insert_request_log(&pool, &hop2)
            .await
            .expect("应能写成功跳");
        insert_request_log(&pool, &sample_log(3, true))
            .await
            .expect("应能写无 id 存量行");

        let lifetime = query_lifetime_stats(&pool, None).await.expect("应能聚合");
        assert_eq!(
            lifetime.request_count, 2,
            "共享 request_id 的两跳计 1，加上一条存量"
        );
    }

    /// 请求日志分页的 settled 过滤；未结算计数忽略 settled 维。
    #[tokio::test]
    async fn request_log_page_filters_settled_and_counts_unsettled() {
        let (_dir, pool) = test_pool().await;
        insert_request_log(&pool, &sample_log(1, false))
            .await
            .expect("应能写未结算日志");
        insert_request_log(&pool, &sample_log(2, true))
            .await
            .expect("应能写已结算日志");

        let mut filter = RequestLogQuery::new(1, 10);
        filter.settled = Some(false);
        let (rows, total, unsettled_total) = query_request_log_page(&pool, &filter)
            .await
            .expect("应能分页");
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].settled);
        assert_eq!(total, 1);
        assert_eq!(unsettled_total, 1);
    }

    /// 列表查询不读 body；按 id 详情才返回 BLOB。
    #[tokio::test]
    async fn request_log_list_omits_bodies_and_detail_returns_them() {
        let (_dir, pool) = test_pool().await;
        let mut log = sample_log(1, true);
        log.request_body = Some(b"{\"model\":\"gpt-4o\"}".to_vec());
        log.response_body = Some(b"{\"ok\":true}".to_vec());
        let id = insert_request_log(&pool, &log)
            .await
            .expect("应能写带 body 的日志");

        let (rows, _, _) = query_request_log_page(&pool, &RequestLogQuery::new(1, 10))
            .await
            .expect("应能分页");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].request_body.is_none(), "列表不应读 request_body");
        assert!(rows[0].response_body.is_none(), "列表不应读 response_body");

        let detail = get_request_log(&pool, id)
            .await
            .expect("应能按 id 读取")
            .expect("详情应存在");
        assert_eq!(
            detail.request_body.as_deref(),
            Some(b"{\"model\":\"gpt-4o\"}".as_slice())
        );
        assert_eq!(
            detail.response_body.as_deref(),
            Some(b"{\"ok\":true}".as_slice())
        );
        assert!(
            get_request_log(&pool, id + 1)
                .await
                .expect("不存在也应成功")
                .is_none()
        );
    }

    /// 系统日志分页与关键字过滤。
    #[tokio::test]
    async fn system_log_page_filters_by_keyword() {
        let (_dir, pool) = test_pool().await;
        insert_system_log(&pool, "error", "billing", "结算失败")
            .await
            .expect("应能写系统日志");
        insert_system_log(&pool, "error", "catalog", "目录同步失败")
            .await
            .expect("应能写系统日志");

        let mut filter = SystemLogQuery::new(1, 10);
        filter.keyword = Some("billing".to_string());
        let page = query_system_log_page(&pool, &filter)
            .await
            .expect("应能查询系统日志");
        assert_eq!(page.total, 1);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].target, "billing");
        assert_eq!(page.items[0].message, "结算失败");
        assert_eq!(page.targets, vec!["billing".to_string()]);

        let mut by_level = SystemLogQuery::new(1, 10);
        by_level.levels = vec!["warn".to_string()];
        let empty = query_system_log_page(&pool, &by_level)
            .await
            .expect("应能按级别过滤");
        assert_eq!(empty.total, 0);

        let mut by_target = SystemLogQuery::new(1, 10);
        by_target.targets = vec!["catalog".to_string()];
        let catalog = query_system_log_page(&pool, &by_target)
            .await
            .expect("应能按目标过滤");
        assert_eq!(catalog.total, 1);
        assert_eq!(catalog.items[0].target, "catalog");
    }
}
