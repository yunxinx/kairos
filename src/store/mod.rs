//! SQLite 存储层：版本化迁移 + 请求日志落库 + 令牌余额结算。
//!
//! 本模块承载请求日志（`request_log`）、冒烟记录（`smoke_probe`）与令牌计费
//! 余额（`token_balance`）。金额一律整数 micro-USD（ADR-0002）。管理面 `/stats`
//! 与 `/stats/lifetime` 聚合也在此查询（时间窗夹取与日志分页同一惯例）。

pub mod resources;

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::{
    Row, SqliteConnection, SqlitePool,
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
    #[error("令牌 {0} 的余额记录在写入后仍不存在")]
    MissingToken(String),
    #[error("资源数据非法: {0}")]
    InvalidResource(String),
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
    ///
    /// 非流式为返回下游的 JSON 字节；流式为实际下发的 SSE 帧 wire 文本拼接。
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

/// 删除令牌余额行；不存在视为成功（幂等）。
///
/// 供删除令牌时同事务清理：余额行若残留，同 key 重建令牌会经
/// `ensure_token_balance` 的冲突跳过复活旧余额。
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

/// 相对调整令牌余额：充值传正数、扣减传负数，原子完成。返回调整后余额。
///
/// 与 `settle_charge` 不同，本原语只动余额、不动累计结算额，供运营调账使用。
pub async fn adjust_balance(
    conn: &mut SqliteConnection,
    token_key: &str,
    delta_usd_micros: i64,
) -> Result<TokenBalance, StoreError> {
    sqlx::query(
        "UPDATE token_balance \
         SET balance_usd_micros = balance_usd_micros + ? \
         WHERE token_key = ?",
    )
    .bind(delta_usd_micros)
    .bind(token_key)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;

    get_token_balance(conn, token_key)
        .await?
        .ok_or(StoreError::MissingToken(token_key.to_string()))
}

/// 请求日志查询过滤条件与分页。全部过滤维度可选，缺省即不限。
#[derive(Debug, Clone, Default)]
pub struct RequestLogQuery {
    /// 按令牌精确过滤。
    pub token_key: Option<String>,
    /// 按模型精确过滤。
    pub model: Option<String>,
    /// 综合关键字：对 `token_key`/`token_name`/`model`/`channel` 做 LIKE 子串匹配（OR）。
    pub keyword: Option<String>,
    /// 只返回 `created_at >= from_created_at`。
    pub from_created_at: Option<i64>,
    /// 只返回 `created_at <= to_created_at`。
    pub to_created_at: Option<i64>,
    /// 页码，从 1 起。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl RequestLogQuery {
    /// 用必填的分页参数构造查询，过滤维度缺省为空。
    pub fn new(page: u64, page_size: u64) -> Self {
        Self {
            page: page.max(1),
            page_size: page_size.clamp(1, 200),
            ..Self::default()
        }
    }
}

/// 按 `filter` 分页查询请求日志（时间倒序），返回本页条目。
pub async fn query_request_logs(
    pool: &SqlitePool,
    filter: &RequestLogQuery,
) -> Result<Vec<RequestLog>, StoreError> {
    let mut qb = sqlx::QueryBuilder::new(
        "SELECT id, created_at, token_name, token_key, inbound_protocol, model, channel, \
         status_code, latency_ms, input_tokens, output_tokens, cache_read_tokens, \
         cache_write_tokens, input_price_usd_micros, output_price_usd_micros, \
         cache_read_price_usd_micros, cache_write_price_usd_micros, cost_usd_micros, \
         request_body, response_body FROM request_log",
    );
    push_request_log_filters(&mut qb, filter);
    qb.push(" ORDER BY id DESC");
    // 分页参数在查询边界防御：`page`/`page_size` 可能为 0（`Default` 派生或
    // 结构体字面量绕过构造器夹取），用 saturating 运算避免下溢。offset 再夹到
    // `i64::MAX` 上限再转 i64，防止超大页码（如外部传入 u64::MAX）经 `as i64`
    // 回绕成负偏移（SQLite 拒绝负 OFFSET 报错）；超大偏移只返回空页，优雅降级。
    let page_size = filter.page_size.max(1);
    let offset = filter
        .page
        .saturating_sub(1)
        .saturating_mul(page_size)
        .min(i64::MAX as u64);
    qb.push(" LIMIT ").push_bind(page_size as i64);
    qb.push(" OFFSET ").push_bind(offset as i64);

    let rows = qb
        .build()
        .fetch_all(pool)
        .await
        .map_err(StoreError::Query)?;

    let mut logs = Vec::with_capacity(rows.len());
    for row in rows {
        logs.push(map_request_log_row(&row)?);
    }
    Ok(logs)
}

/// 按 `filter` 统计满足条件的日志总数（用于分页总页数）。
pub async fn count_request_logs(
    pool: &SqlitePool,
    filter: &RequestLogQuery,
) -> Result<u64, StoreError> {
    let mut qb = sqlx::QueryBuilder::new("SELECT COUNT(*) AS cnt FROM request_log");
    push_request_log_filters(&mut qb, filter);

    let row = qb
        .build()
        .fetch_one(pool)
        .await
        .map_err(StoreError::Query)?;
    let count: i64 = row.try_get("cnt").map_err(StoreError::Query)?;
    Ok(count.max(0) as u64)
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
    pub token_count: u64,
    pub channel_count: u64,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifetimeStats {
    pub request_count: u64,
    pub cost_usd_micros: i64,
    pub total_tokens: u64,
}

/// 聚合 `days` 天（已夹取）内的 stats。费用只计 HTTP 2xx（与计费「仅成功结算」一致）。
pub async fn query_stats(pool: &SqlitePool, days: u64) -> Result<Stats, StoreError> {
    let days = clamp_stats_days(Some(days));
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let today = now_millis.div_euclid(MS_PER_DAY);
    let start_day = today.saturating_sub(days as i64 - 1);
    let from_created_at = start_day.saturating_mul(MS_PER_DAY);

    let summary_row = sqlx::query(
        "SELECT COUNT(*) AS request_count, \
         COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 THEN 1 ELSE 0 END), 0) AS success_count, \
         COALESCE(SUM(input_tokens), 0) AS input_tokens, \
         COALESCE(SUM(output_tokens), 0) AS output_tokens, \
         COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 THEN cost_usd_micros ELSE 0 END), 0) \
           AS cost_usd_micros \
         FROM request_log WHERE created_at >= ?",
    )
    .bind(from_created_at)
    .fetch_one(pool)
    .await
    .map_err(StoreError::Query)?;

    let token_count = count_rows(pool, "SELECT COUNT(*) AS cnt FROM tokens").await?;
    let channel_count = count_rows(pool, "SELECT COUNT(*) AS cnt FROM channels").await?;

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
        query_hourly_buckets(pool, from_created_at).await?
    } else {
        query_daily_buckets(pool, from_created_at, days).await?
    };
    let by_model = query_cost_share(pool, from_created_at, CostDimension::Model).await?;
    let by_channel = query_cost_share(pool, from_created_at, CostDimension::Channel).await?;

    Ok(Stats {
        summary,
        daily,
        by_model,
        by_channel,
    })
}

/// 全量累计：请求数、成功结算费用、四分量 token 合计。
pub async fn query_lifetime_stats(pool: &SqlitePool) -> Result<LifetimeStats, StoreError> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS request_count, \
         COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 THEN cost_usd_micros ELSE 0 END), 0) \
           AS cost_usd_micros, \
         COALESCE(SUM(input_tokens + output_tokens + cache_read_tokens + cache_write_tokens), 0) \
           AS total_tokens \
         FROM request_log",
    )
    .fetch_one(pool)
    .await
    .map_err(StoreError::Query)?;

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
) -> Result<Vec<DailyBucket>, StoreError> {
    let rows = sqlx::query(
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
                   COUNT(*) AS request_count, \
                   COALESCE(SUM(input_tokens), 0) AS input_tokens, \
                   COALESCE(SUM(output_tokens), 0) AS output_tokens, \
                   COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 \
                        THEN cost_usd_micros ELSE 0 END), 0) AS cost_usd_micros \
            FROM request_log WHERE created_at >= ? \
            GROUP BY hour \
         ) agg ON agg.hour = strftime('%Y-%m-%dT%H:00:00Z', calendar.ts) \
         ORDER BY calendar.ts",
    )
    .bind(from_created_at)
    .bind(HOURS_PER_DAY)
    .bind(from_created_at)
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;

    rows.iter().map(trend_bucket).collect()
}

/// 逐日序列：用 SQLite 日历补齐无流量日，日期为 UTC `YYYY-MM-DD`。
async fn query_daily_buckets(
    pool: &SqlitePool,
    from_created_at: i64,
    days: u64,
) -> Result<Vec<DailyBucket>, StoreError> {
    let rows = sqlx::query(
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
                   COUNT(*) AS request_count, \
                   COALESCE(SUM(input_tokens), 0) AS input_tokens, \
                   COALESCE(SUM(output_tokens), 0) AS output_tokens, \
                   COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 \
                        THEN cost_usd_micros ELSE 0 END), 0) AS cost_usd_micros \
            FROM request_log WHERE created_at >= ? \
            GROUP BY day \
         ) agg ON agg.day = calendar.day \
         ORDER BY calendar.day",
    )
    .bind(from_created_at)
    .bind(days as i64)
    .bind(from_created_at)
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;

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
) -> Result<Vec<CostShare>, StoreError> {
    let sql = match dimension {
        CostDimension::Model => {
            "SELECT model AS name, COUNT(*) AS request_count, \
             COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 THEN cost_usd_micros ELSE 0 END), 0) \
               AS cost_usd_micros \
             FROM request_log WHERE created_at >= ? \
             GROUP BY model \
             ORDER BY cost_usd_micros DESC, name ASC"
        }
        CostDimension::Channel => {
            "SELECT channel AS name, COUNT(*) AS request_count, \
             COALESCE(SUM(CASE WHEN status_code BETWEEN 200 AND 299 THEN cost_usd_micros ELSE 0 END), 0) \
               AS cost_usd_micros \
             FROM request_log WHERE created_at >= ? \
             GROUP BY channel \
             ORDER BY cost_usd_micros DESC, name ASC"
        }
    };
    let rows = sqlx::query(sql)
        .bind(from_created_at)
        .fetch_all(pool)
        .await
        .map_err(StoreError::Query)?;

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
fn as_count(value: i64) -> u64 {
    value.max(0) as u64
}

/// 把 `filter` 中非空条件以 AND 拼入 WHERE 子句。
fn push_request_log_filters(qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>, filter: &RequestLogQuery) {
    let mut first = true;
    if let Some(token_key) = &filter.token_key {
        push_where_cond(qb, &mut first, "token_key = ");
        qb.push_bind(token_key);
    }
    if let Some(model) = &filter.model {
        push_where_cond(qb, &mut first, "model = ");
        qb.push_bind(model);
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
    if let Some(from) = filter.from_created_at {
        push_where_cond(qb, &mut first, "created_at >= ");
        qb.push_bind(from);
    }
    if let Some(to) = filter.to_created_at {
        push_where_cond(qb, &mut first, "created_at <= ");
        qb.push_bind(to);
    }
}

/// 关键字 → LIKE 子串模式：转义 `\`/`%`/`_`（配合 `ESCAPE '\'`），两端补 `%`。
fn like_substring_pattern(keyword: &str) -> String {
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

/// 向查询拼接一个条件：首个条件以 `WHERE` 开头，其余以 `AND` 连接。
fn push_where_cond(qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>, first: &mut bool, condition: &str) {
    qb.push(if *first { " WHERE " } else { " AND " });
    *first = false;
    qb.push(condition);
}

/// 把请求日志行映射为 `RequestLog`。
fn map_request_log_row(row: &sqlx::sqlite::SqliteRow) -> Result<RequestLog, StoreError> {
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
        inbound_protocol: row.try_get("inbound_protocol").map_err(StoreError::Query)?,
        model: row.try_get("model").map_err(StoreError::Query)?,
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
        request_body: row.try_get("request_body").map_err(StoreError::Query)?,
        response_body: row.try_get("response_body").map_err(StoreError::Query)?,
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

    /// 余额相对调整：充值/扣减同一原语，原子生效。
    #[tokio::test]
    async fn adjust_balance_recharges_and_deducts() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        seed_token(&mut conn, "sk-a").await;
        ensure_token_balance(&mut conn, "sk-a", 10.0, 1)
            .await
            .expect("应能初始化余额");

        let after_recharge = adjust_balance(&mut conn, "sk-a", 5_000_000)
            .await
            .expect("应能充值");
        assert_eq!(after_recharge.balance_usd_micros, 15_000_000);
        // 充值/扣减不动累计结算额。
        assert_eq!(after_recharge.settled_usd_micros, 0);

        let after_deduct = adjust_balance(&mut conn, "sk-a", -3_000_000)
            .await
            .expect("应能扣减");
        assert_eq!(after_deduct.balance_usd_micros, 12_000_000);
        assert_eq!(after_deduct.settled_usd_micros, 0);
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
                "INSERT INTO token_balance (token_key, balance_usd_micros, settled_usd_micros, created_at) \
                 VALUES ('strict-probe', 'not-a-number', 0, 0)",
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
                "prices",
                "INSERT INTO prices (model, input_micros, output_micros) \
                 VALUES ('m', 'not-a-number', 0)",
            ),
            (
                "settings",
                "INSERT INTO settings (setting_key, setting_value) VALUES ('k2', x'00')",
            ),
        ];
        for (table, sql) in probes {
            let result = sqlx::query(sql).execute(&pool).await;
            assert!(
                result.is_err(),
                "{table} 应仍是 STRICT 表，错类型写入须被拒"
            );
        }

        let result = sqlx::query(
            "INSERT INTO prices (model, input_micros, output_micros) VALUES ('m2', 1.5, 0)",
        )
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
            "INSERT INTO token_balance (token_key, balance_usd_micros, settled_usd_micros, created_at) \
             VALUES ('sk-ghost', 0, 0, 0)",
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
            .expect("应能查余额");
        assert!(balance.is_none(), "合法令牌的余额行本就不存在");

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
                    inbound_protocol: "openai_chat".to_string(),
                    model: model.to_string(),
                    channel: "c1".to_string(),
                    status_code: 200,
                    latency_ms: 10,
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    price,
                    cost_usd_micros: 12,
                    request_body: None,
                    response_body: None,
                },
            )
            .await
            .expect("应能写请求日志");
        }

        // 分页：每页 2 条，第一页取最新两条（id 倒序）。
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
                    inbound_protocol: "openai_chat".to_string(),
                    model: (*model).to_string(),
                    channel: (*channel).to_string(),
                    status_code: 200,
                    latency_ms: 10,
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    price,
                    cost_usd_micros: 0,
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
                inbound_protocol: "openai_chat".to_string(),
                model: "gpt-4o".to_string(),
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                price,
                cost_usd_micros: 12,
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
}
