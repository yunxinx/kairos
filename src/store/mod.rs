//! SQLite 存储层：版本化迁移 + 请求日志落库 + 用户钱包结算。
//!
//! 本模块承载请求日志（`request_log`）、系统日志（`system_log`）、冒烟记录
//! （`smoke_probe`）、管理用户钱包（`user_balance`）与令牌累计结算
//! （`token_balance`，只保存令牌累计结算）。金额一律整数 micro-USD。管理面 `/stats` 与
//! `/stats/lifetime` 聚合也在此查询（时间窗夹取与日志分页同一惯例）。

pub mod balance_operations;
pub mod catalog;
pub mod channel_keys;
mod ids;
pub mod plans;
pub mod resources;
mod system_log;
pub mod users;

pub use system_log::{
    Actor, SystemLog, SystemLogEvent, SystemLogList, SystemLogQuery, SystemLogSortBy,
    insert_system_log, purge_system_logs_before, query_system_log_page, record_audit,
    record_audit_detached, record_system_error, record_system_warn,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::{
    AssertSqlSafe, Row, SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
};
use thiserror::Error;

use crate::core::billing::{self, PriceSnapshot};

/// 存储层错误，向上抛给应用边界。
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("连接 SQLite 失败: {0}")]
    Connect(sqlx::Error),
    #[error("执行迁移失败: {0}")]
    Migrate(sqlx::migrate::MigrateError),
    #[error("数据库操作失败: {0}")]
    Query(sqlx::Error),
    #[error("请求日志持久化超过请求截止时间")]
    PersistenceTimeout,
    #[error("读取数据库文件元数据 {path} 失败: {source}")]
    FileMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "WAL checkpoint 被活动读事务阻塞（WAL 帧 {log_frames}，已 checkpoint {checkpointed_frames}）"
    )]
    WalCheckpointBusy {
        log_frames: i64,
        checkpointed_frames: i64,
    },
    #[error("找不到令牌 {0} 所属用户的余额")]
    MissingToken(String),
    #[error("资源数据非法: {0}")]
    InvalidResource(String),
    #[error("管理主体无权操作该记录")]
    PermissionDenied,
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
    #[error("系统时钟早于资源 id 纪元")]
    EntityIdClockBeforeEpoch,
    #[error("资源 id 空间已耗尽")]
    EntityIdExhausted,
    #[error("用户余额不足以预留本次请求费用")]
    InsufficientFunds,
    #[error("令牌累计结算上限不足以预留本次请求费用")]
    TokenLimitExceeded,
    #[error("请求费用预留与已有请求身份不一致")]
    ReservationConflict,
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

    ids::initialize(&pool).await?;

    Ok(pool)
}

/// 写一条冒烟记录，返回时间有序 id。
pub async fn insert_smoke(pool: &SqlitePool, note: &str) -> Result<i64, StoreError> {
    let id = ids::next_id()?;
    sqlx::query("INSERT INTO smoke_probe (id, note) VALUES (?, ?)")
        .bind(id)
        .bind(note)
        .execute(pool)
        .await
        .map_err(StoreError::Query)?;

    Ok(id)
}

/// 一条请求日志的可持久化字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestLog {
    /// 时间有序主键：新增时由存储层分配，插入构造时填 0。
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
    /// 本次出站使用的密钥身份（名称或 id），绝不保存密钥明文。
    pub channel_key: Option<String>,
    pub status_code: i64,
    pub latency_ms: i64,
    /// usage 四分量与 1h 写入明细（明细为写入总数的子集）。
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_write_1h_tokens: u64,
    /// 上游结果是否明确携带 usage 字段；显式的全零 usage 仍为已报告。
    #[serde(default)]
    pub usage_reported: bool,
    /// 计费时的价格快照（micro-USD / 1M tokens）。
    pub price: PriceSnapshot,
    /// 渠道原价（micro-USD），不套用折扣。
    pub base_cost_usd_micros: i64,
    /// 本次使用的万分比折扣率（10000 = 原价）。
    pub discount_bp: i64,
    /// 本次实收（micro-USD，折后）。
    ///
    /// 补扣/豁免按此列入账；对账时由 `base_cost_usd_micros` 与 `discount_bp` 复核。
    pub cost_usd_micros: i64,
    /// 费用是否已完成所属用户钱包结算；结算失败时为 `false`，供对账补扣。
    pub settled: bool,
    /// 一次下游入站请求的身份；同一请求的多次出站尝试共用。存量行可能为 `None`。
    pub request_id: Option<String>,
    /// 一次实际出站尝试的计费身份。
    ///
    /// 同一个 `request_id` 可以产生多条不同的 attempt；该字段把最终日志与唯一的
    /// 预留、上游结果和钱包扣款对应起来。未进入出站阶段的请求日志为 `None`。
    pub billing_attempt_id: Option<String>,
    /// 可选的入站请求原始字节（仅 `logging.full_body` 开启时保存）。
    pub request_body: Option<Vec<u8>>,
    /// 可选的入站响应原始字节（仅 `logging.full_body` 开启时保存）。
    ///
    /// 非流式为返回下游的 JSON 字节；流式为实际下发的 SSE 帧 wire 文本拼接。
    pub response_body: Option<Vec<u8>>,
}

/// 已持久化、等待后台完成结算与写入最终日志的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PendingRequestLog {
    pub(crate) log: RequestLog,
    /// 费用计算阶段已失败时保留原因；此类记录不得执行扣费。
    pub(crate) settlement_error: Option<String>,
}

/// 落一条请求日志，返回时间有序 id。
pub async fn insert_request_log(pool: &SqlitePool, log: &RequestLog) -> Result<i64, StoreError> {
    let mut conn = pool.acquire().await.map_err(StoreError::Query)?;
    insert_request_log_on(&mut conn, log).await
}

/// 在已有连接/事务上插入请求日志，供结算与日志同事务提交。
pub async fn insert_request_log_on(
    conn: &mut SqliteConnection,
    log: &RequestLog,
) -> Result<i64, StoreError> {
    let id = ids::next_id()?;
    insert_request_log_with_id_on(conn, log, id).await?;
    Ok(id)
}

/// 使用预先分配的 id 插入请求日志，供持久化队列原子完成“入日志并出队”。
///
/// 仅对预先分配的主键做幂等处理；若同一计费尝试已由其它 id 写入，唯一约束
/// 错误必须显式暴露，不能把不同结果静默折叠成一条日志。
pub(crate) async fn insert_request_log_with_id_on(
    conn: &mut SqliteConnection,
    log: &RequestLog,
    id: i64,
) -> Result<(), StoreError> {
    let input_tokens = persisted_token_count("input_tokens", log.input_tokens)?;
    let output_tokens = persisted_token_count("output_tokens", log.output_tokens)?;
    let cache_read_tokens = persisted_token_count("cache_read_tokens", log.cache_read_tokens)?;
    let cache_write_tokens = persisted_token_count("cache_write_tokens", log.cache_write_tokens)?;
    let cache_write_1h_tokens =
        persisted_token_count("cache_write_1h_tokens", log.cache_write_1h_tokens)?;
    sqlx::query(
        "INSERT INTO request_log \
         (id, created_at, token_name, token_key, user_id, inbound_protocol, model, outbound_model, \
          channel, channel_key, status_code, latency_ms, input_tokens, output_tokens, cache_read_tokens, \
          cache_write_tokens, cache_write_1h_tokens, input_price_usd_micros, output_price_usd_micros, \
          cache_read_price_usd_micros, cache_write_price_usd_micros, cache_write_1h_price_usd_micros, \
          base_cost_usd_micros, discount_bp, cost_usd_micros, \
          settled, usage_reported, request_id, billing_attempt_id, request_body, response_body) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO NOTHING",
    )
    .bind(id)
    .bind(log.created_at)
    .bind(&log.token_name)
    .bind(&log.token_key)
    .bind(log.user_id)
    .bind(&log.inbound_protocol)
    .bind(&log.model)
    .bind(&log.outbound_model)
    .bind(&log.channel)
    .bind(&log.channel_key)
    .bind(log.status_code)
    .bind(log.latency_ms)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(cache_read_tokens)
    .bind(cache_write_tokens)
    .bind(cache_write_1h_tokens)
    .bind(log.price.input_micros)
    .bind(log.price.output_micros)
    .bind(log.price.cache_read_micros)
    .bind(log.price.cache_write_micros)
    .bind(log.price.cache_write_1h_micros)
    .bind(log.base_cost_usd_micros)
    .bind(log.discount_bp)
    .bind(log.cost_usd_micros)
    .bind(log.settled as i64)
    .bind(log.usage_reported as i64)
    .bind(&log.request_id)
    .bind(&log.billing_attempt_id)
    .bind(&log.request_body)
    .bind(&log.response_body)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;

    Ok(())
}

fn persisted_token_count(field: &str, count: u64) -> Result<i64, StoreError> {
    i64::try_from(count).map_err(|_| {
        StoreError::InvalidResource(format!("请求日志 {field} 超出 SQLite INTEGER 范围"))
    })
}

fn validate_request_log(log: &RequestLog) -> Result<(), StoreError> {
    persisted_token_count("input_tokens", log.input_tokens)?;
    persisted_token_count("output_tokens", log.output_tokens)?;
    persisted_token_count("cache_read_tokens", log.cache_read_tokens)?;
    persisted_token_count("cache_write_tokens", log.cache_write_tokens)?;
    persisted_token_count("cache_write_1h_tokens", log.cache_write_1h_tokens)?;
    Ok(())
}

/// 把待结算请求持久化到短事务队列；正文独立保存为 BLOB。
pub(crate) async fn enqueue_pending_request_log(
    pool: &SqlitePool,
    mut pending: PendingRequestLog,
) -> Result<i64, StoreError> {
    validate_request_log(&pending.log)?;
    let id = ids::next_id()?;
    pending.log.id = 0;
    let request_body = pending.log.request_body.take();
    let response_body = pending.log.response_body.take();
    let metadata = serde_json::to_vec(&pending)
        .map_err(|err| StoreError::InvalidResource(format!("待结算请求无法编码: {err}")))?;
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(StoreError::Query)?;
    let inserted = sqlx::query(
        "INSERT INTO request_log_outbox \
         (id, token_key, user_id, cost_usd_micros, metadata, request_body, response_body, \
          request_id, billing_attempt_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(&pending.log.token_key)
    .bind(pending.log.user_id)
    .bind(pending.log.cost_usd_micros)
    .bind(&metadata)
    .bind(&request_body)
    .bind(&response_body)
    .bind(&pending.log.request_id)
    .bind(&pending.log.billing_attempt_id)
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?;
    let persisted_id = if inserted.rows_affected() == 0 {
        let attempt_id = pending.log.billing_attempt_id.as_deref().ok_or_else(|| {
            StoreError::InvalidResource("无计费尝试标识的日志发生唯一键冲突".to_string())
        })?;
        let existing = sqlx::query_as::<_, (i64, Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>)>(
            "SELECT id, metadata, request_body, response_body \
             FROM request_log_outbox WHERE billing_attempt_id = ?",
        )
        .bind(attempt_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
        if existing.1 != metadata || existing.2 != request_body || existing.3 != response_body {
            return Err(StoreError::ReservationConflict);
        }
        existing.0
    } else {
        id
    };
    if let Some(attempt_id) = pending.log.billing_attempt_id.as_deref() {
        mark_billing_attempt_result_persisted(&mut tx, attempt_id).await?;
    }
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(persisted_id)
}

/// 标记结果已进入 outbox，并清理预留行中的完整结果副本。
///
/// 预留元数据只需在 outbox 尚未形成时支持崩溃恢复；结果进入 outbox 后，继续保留
/// 请求体、响应体和 usage 会使同一份敏感数据在账务表中长期重复存储。元数据损坏时
/// 不覆盖原始 BLOB，只更新状态位并交由隔离记录保留原文。
async fn mark_billing_attempt_result_persisted(
    conn: &mut SqliteConnection,
    attempt_id: &str,
) -> Result<(), StoreError> {
    let metadata = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT recovery_metadata FROM billing_reservations \
         WHERE attempt_id = ? AND status = 'reserved'",
    )
    .bind(attempt_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?
    .ok_or_else(|| StoreError::InvalidResource(format!("费用预留 {attempt_id} 不存在或已终止")))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    clear_billing_attempt_recovery_payload(conn, attempt_id, &metadata).await?;
    let updated = sqlx::query(
        "UPDATE billing_reservations SET result_persisted = 1, updated_at = ? \
         WHERE attempt_id = ? AND status = 'reserved'",
    )
    .bind(now)
    .bind(attempt_id)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    if updated.rows_affected() != 1 {
        return Err(StoreError::InvalidResource(format!(
            "费用预留 {attempt_id} 不存在或已终止"
        )));
    }
    Ok(())
}

/// 清理预留元数据中的大对象结果；无法解析时保留原始字节供隔离记录复核。
async fn clear_billing_attempt_recovery_payload(
    conn: &mut SqliteConnection,
    attempt_id: &str,
    metadata: &[u8],
) -> Result<(), StoreError> {
    let Ok(mut recovery) = serde_json::from_slice::<BillingAttemptRecovery>(metadata) else {
        return Ok(());
    };
    recovery.request_body = None;
    recovery.result = None;
    recovery.result_settlement_error = None;
    let cleaned = serde_json::to_vec(&recovery)
        .map_err(|err| StoreError::InvalidResource(format!("费用元数据无法编码: {err}")))?;
    sqlx::query(
        "UPDATE billing_reservations SET recovery_metadata = ? \
         WHERE attempt_id = ?",
    )
    .bind(cleaned)
    .bind(attempt_id)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

async fn clear_billing_attempt_recovery_payload_by_id(
    conn: &mut SqliteConnection,
    attempt_id: &str,
) -> Result<(), StoreError> {
    let metadata = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT recovery_metadata FROM billing_reservations WHERE attempt_id = ?",
    )
    .bind(attempt_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    if let Some(metadata) = metadata {
        clear_billing_attempt_recovery_payload(conn, attempt_id, &metadata).await?;
    }
    Ok(())
}

/// 按 id 读取一批待结算请求；后台按该顺序处理，避免旧记录长期滞留。
pub(crate) async fn load_pending_request_logs(
    pool: &SqlitePool,
    limit: i64,
) -> Result<Vec<PendingRequestLog>, StoreError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let rows = sqlx::query(
        "SELECT id, metadata, request_body, response_body \
         FROM request_log_outbox \
         WHERE (state = 'queued' OR (state = 'isolated' AND next_retry_at IS NOT NULL AND next_retry_at <= ?)) \
         ORDER BY id LIMIT ?",
    )
    .bind(now)
    .bind(limit.max(1))
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;
    let mut pending = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get("id").map_err(StoreError::Query)?;
        let metadata: Vec<u8> = row.try_get("metadata").map_err(StoreError::Query)?;
        let mut item: PendingRequestLog = match serde_json::from_slice(&metadata) {
            Ok(item) => item,
            Err(err) => {
                // 元数据损坏只影响当前记录。把原始 BLOB 留在同一行并标记为
                // isolated，主队列仍可继续处理后续记录；数据库写入失败时才
                // 向上返回，让下一轮重试这次隔离动作。
                let reason = format!("待结算请求无法解码: {err}");
                isolate_pending_request_log(pool, id, &reason, None).await?;
                record_system_error(
                    pool,
                    "billing",
                    &SystemLogEvent::new(
                        "request_log.metadata_corrupt",
                        serde_json::json!({ "outbox_id": id }),
                        reason,
                    ),
                )
                .await;
                continue;
            }
        };
        item.log.id = id;
        // 新记录把正文放在独立 BLOB 列，恢复记录还可能把正文保存在 metadata
        // 内。仅在 BLOB 列确实有值时覆盖，避免旧版本崩溃恢复把已保存正文
        // 用 NULL 覆盖掉。
        let request_body: Option<Vec<u8>> =
            row.try_get("request_body").map_err(StoreError::Query)?;
        if request_body.is_some() {
            item.log.request_body = request_body;
        }
        let response_body: Option<Vec<u8>> =
            row.try_get("response_body").map_err(StoreError::Query)?;
        if response_body.is_some() {
            item.log.response_body = response_body;
        }
        pending.push(item);
    }
    Ok(pending)
}

/// 将结算失败永久保留在 outbox 中，并记录下次重放时间。
///
/// `isolated` 只表示该条记录不再阻塞主队列；原始 metadata、请求/响应 body
/// 仍在同一行，后台可按 request id 精确重放，运维也能据此定位失败原因。
pub(crate) async fn isolate_pending_request_log(
    pool: &SqlitePool,
    id: i64,
    reason: &str,
    retry_after: Option<Duration>,
) -> Result<(), StoreError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(StoreError::Query)?;
    let (attempt_count, billing_attempt_id): (i64, Option<String>) = sqlx::query_as(
        "SELECT attempt_count, billing_attempt_id FROM request_log_outbox WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(StoreError::Query)?
    .unwrap_or((0, None));
    let next_retry_at = retry_after.and_then(|delay| {
        let exponent = u32::try_from(attempt_count.min(16)).unwrap_or(0);
        let multiplier = 1_i64.checked_shl(exponent).unwrap_or(i64::MAX);
        let millis = i64::try_from(delay.as_millis())
            .unwrap_or(i64::MAX)
            .saturating_mul(multiplier)
            .min(10 * 60 * 1000);
        now.checked_add(millis)
    });
    sqlx::query(
        "UPDATE request_log_outbox \
         SET state = 'isolated', attempt_count = attempt_count + 1, \
             next_retry_at = ?, last_error = ? WHERE id = ?",
    )
    .bind(next_retry_at)
    .bind(reason)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?;
    if retry_after.is_none() {
        // 未派发尝试没有上游费用，可释放预留让后续请求继续使用余额；已派发
        // 尝试必须保留预留并占用准入额度，直到人工修正后以同一 attempt_id
        // 重放结算。
        if let Some(attempt_id) = billing_attempt_id {
            sqlx::query(
                "UPDATE billing_reservations SET status = 'released', updated_at = ? \
                 WHERE attempt_id = ? AND status = 'reserved' AND dispatched = 0",
            )
            .bind(now)
            .bind(attempt_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Query)?;
        }
    }
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(())
}

/// 永久隔离或等待重试的 outbox 记录，供管理面定位和发起重放。
///
/// `request_body` 和 `response_body` 均保留原始字节。元数据损坏时 `log` 为
/// `None`，但数据库中的原文不会被占位对象覆盖；调用方可以据此决定人工修复
/// 后再重放，确保“无法解析”不会退化成静默丢弃。
#[derive(Debug, Clone)]
pub(crate) struct IsolatedRequestLog {
    pub(crate) id: i64,
    pub(crate) request_id: Option<String>,
    pub(crate) billing_attempt_id: Option<String>,
    pub(crate) token_key: String,
    pub(crate) user_id: i64,
    pub(crate) attempt_count: i64,
    pub(crate) next_retry_at: Option<i64>,
    pub(crate) last_error: Option<String>,
    pub(crate) request_body: Option<Vec<u8>>,
    pub(crate) response_body: Option<Vec<u8>>,
    pub(crate) log: Option<RequestLog>,
}

/// 查询隔离 outbox 记录。
///
/// 结果按 outbox id 升序返回；无论元数据是否损坏，原始字段都会返回，因而
/// 管理面可以展示失败原因并按精确 attempt id 发起重放。`limit` 至少取 1。
/// 按管理主体范围读取隔离记录；非 root 仅能看到普通用户归属的记录。
pub(crate) async fn query_isolated_request_logs_scoped(
    pool: &SqlitePool,
    limit: i64,
    include_management_records: bool,
) -> Result<Vec<IsolatedRequestLog>, StoreError> {
    let sql = if include_management_records {
        "SELECT outbox.id, outbox.request_id, outbox.billing_attempt_id, outbox.token_key, outbox.user_id, outbox.attempt_count, \
                outbox.next_retry_at, outbox.last_error, outbox.metadata, outbox.request_body, outbox.response_body \
         FROM request_log_outbox outbox WHERE outbox.state = 'isolated' ORDER BY outbox.id LIMIT ?"
    } else {
        "SELECT outbox.id, outbox.request_id, outbox.billing_attempt_id, outbox.token_key, outbox.user_id, outbox.attempt_count, \
                outbox.next_retry_at, outbox.last_error, outbox.metadata, outbox.request_body, outbox.response_body \
         FROM request_log_outbox outbox \
         INNER JOIN users owner ON owner.id = outbox.user_id AND owner.role = 'user' \
         WHERE outbox.state = 'isolated' ORDER BY outbox.id LIMIT ?"
    };
    let rows = sqlx::query(sql)
        .bind(limit.max(1))
        .fetch_all(pool)
        .await
        .map_err(StoreError::Query)?;

    rows.into_iter()
        .map(|row| {
            let metadata: Vec<u8> = row.try_get("metadata").map_err(StoreError::Query)?;
            let request_body: Option<Vec<u8>> =
                row.try_get("request_body").map_err(StoreError::Query)?;
            let response_body: Option<Vec<u8>> =
                row.try_get("response_body").map_err(StoreError::Query)?;
            let log = match serde_json::from_slice::<PendingRequestLog>(&metadata) {
                Ok(mut pending) => {
                    if request_body.is_some() {
                        pending.log.request_body.clone_from(&request_body);
                    }
                    if response_body.is_some() {
                        pending.log.response_body.clone_from(&response_body);
                    }
                    Some(pending.log)
                }
                Err(_) => None,
            };
            Ok(IsolatedRequestLog {
                id: row.try_get("id").map_err(StoreError::Query)?,
                request_id: row.try_get("request_id").map_err(StoreError::Query)?,
                billing_attempt_id: row
                    .try_get("billing_attempt_id")
                    .map_err(StoreError::Query)?,
                token_key: row.try_get("token_key").map_err(StoreError::Query)?,
                user_id: row.try_get("user_id").map_err(StoreError::Query)?,
                attempt_count: row.try_get("attempt_count").map_err(StoreError::Query)?,
                next_retry_at: row.try_get("next_retry_at").map_err(StoreError::Query)?,
                last_error: row.try_get("last_error").map_err(StoreError::Query)?,
                request_body,
                response_body,
                log,
            })
        })
        .collect()
}

/// 隔离记录的重放结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IsolatedReplayAction {
    /// 已从隔离状态重新放入主队列。
    Requeued,
    /// 记录本来就在主队列中，重复操作不产生副作用。
    AlreadyQueued,
    /// 该尝试已经写入最终日志，重复操作不产生副作用。
    AlreadySettled,
    /// 没有找到指定的 attempt。
    NotFound,
}

/// 按计费尝试身份重放隔离记录。
///
/// 只清除调度字段，不改原始 metadata、正文、失败次数或计费尝试身份。
/// 后台再次消费时仍以 `billing_attempt_id` 唯一键结算，因而重复点击不会重复
/// 扣款。调用方应在管理层记录操作者审计信息。
pub(crate) async fn requeue_isolated_request_log(
    pool: &SqlitePool,
    billing_attempt_id: &str,
    include_management_records: bool,
) -> Result<IsolatedReplayAction, StoreError> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(StoreError::Query)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    if !include_management_records {
        let owner_role = sqlx::query_scalar::<_, String>(
            "SELECT owner.role FROM request_log_outbox outbox \
             LEFT JOIN users owner ON owner.id = outbox.user_id \
             WHERE outbox.billing_attempt_id = ? AND outbox.state = 'isolated'",
        )
        .bind(billing_attempt_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
        if owner_role.as_deref() != Some("user") {
            tx.rollback().await.map_err(StoreError::Query)?;
            return Err(StoreError::PermissionDenied);
        }
    }
    let updated = sqlx::query(
        "UPDATE request_log_outbox SET state = 'queued', next_retry_at = NULL, last_error = NULL \
         WHERE billing_attempt_id = ? AND state = 'isolated'",
    )
    .bind(billing_attempt_id)
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?;
    let action = if updated.rows_affected() == 1 {
        if let Err(err) =
            restore_released_billing_reservation(&mut tx, billing_attempt_id, now).await
        {
            tx.rollback().await.map_err(StoreError::Query)?;
            return Err(err);
        }
        IsolatedReplayAction::Requeued
    } else if sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM request_log_outbox WHERE billing_attempt_id = ? AND state = 'queued'",
    )
    .bind(billing_attempt_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(StoreError::Query)?
        > 0
    {
        IsolatedReplayAction::AlreadyQueued
    } else if sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM request_log WHERE billing_attempt_id = ?",
    )
    .bind(billing_attempt_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(StoreError::Query)?
        > 0
    {
        IsolatedReplayAction::AlreadySettled
    } else {
        IsolatedReplayAction::NotFound
    };
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(action)
}

/// 按 outbox 行身份重放隔离记录。
///
/// 没有计费尝试身份的历史日志只能通过 outbox 主键定位；这类记录不涉及
/// 钱包预留，重放仅恢复队列状态。若该行同时带有计费尝试，则复用同一预留
/// 恢复校验，保持与按尝试身份重放一致的结算语义。
pub(crate) async fn requeue_isolated_request_log_by_id(
    pool: &SqlitePool,
    id: i64,
    include_management_records: bool,
) -> Result<IsolatedReplayAction, StoreError> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(StoreError::Query)?;
    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT outbox.state, owner.role FROM request_log_outbox outbox \
         LEFT JOIN users owner ON owner.id = outbox.user_id \
         WHERE outbox.id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(StoreError::Query)?;
    let Some((state, owner_role)) = row else {
        tx.rollback().await.map_err(StoreError::Query)?;
        return Ok(IsolatedReplayAction::NotFound);
    };
    if state == "queued" {
        tx.rollback().await.map_err(StoreError::Query)?;
        return Ok(IsolatedReplayAction::AlreadyQueued);
    }
    if state != "isolated" {
        tx.rollback().await.map_err(StoreError::Query)?;
        return Ok(IsolatedReplayAction::NotFound);
    }
    if !include_management_records && owner_role.as_deref() != Some("user") {
        tx.rollback().await.map_err(StoreError::Query)?;
        return Err(StoreError::PermissionDenied);
    }
    let attempt_id = sqlx::query_scalar::<_, Option<String>>(
        "SELECT billing_attempt_id FROM request_log_outbox \
         WHERE id = ? AND state = 'isolated'",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(StoreError::Query)?
    .flatten();
    let updated = sqlx::query(
        "UPDATE request_log_outbox SET state = 'queued', next_retry_at = NULL, last_error = NULL \
         WHERE id = ? AND state = 'isolated'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?;
    let action = if updated.rows_affected() == 1 {
        if let Some(attempt_id) = attempt_id.as_deref() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis() as i64)
                .unwrap_or(0);
            if let Err(err) = restore_released_billing_reservation(&mut tx, attempt_id, now).await {
                tx.rollback().await.map_err(StoreError::Query)?;
                return Err(err);
            }
        }
        IsolatedReplayAction::Requeued
    } else {
        IsolatedReplayAction::NotFound
    };
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(action)
}

/// 恢复确定性隔离记录释放的预留，并再次执行原子准入检查。
///
/// 隔离时释放了未扣除的冻结金额；重放不能无条件把状态改回 reserved，
/// 否则余额或令牌累计上限在此期间下降时会绕过准入。检查通过后保留原
/// `attempt_id` 和原始金额，后台结算仍保持 exactly-once。
async fn restore_released_billing_reservation(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    attempt_id: &str,
    now: i64,
) -> Result<(), StoreError> {
    let row = sqlx::query_as::<_, (String, i64, i64, Option<i64>, String)>(
        "SELECT token_key, user_id, reserved_cost_usd_micros, token_limit_usd_micros, status \
         FROM billing_reservations WHERE attempt_id = ?",
    )
    .bind(attempt_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(StoreError::Query)?;
    let Some((token_key, user_id, reserved_cost, _token_limit, status)) = row else {
        return Err(StoreError::InvalidResource(format!(
            "费用预留 {attempt_id} 不存在"
        )));
    };
    if status != "reserved" && status != "released" {
        return Err(StoreError::ReservationConflict);
    }
    let current_limit: Option<Option<i64>> =
        sqlx::query_scalar("SELECT limit_usd_micros FROM tokens WHERE token_key = ?")
            .bind(&token_key)
            .fetch_optional(&mut **tx)
            .await
            .map_err(StoreError::Query)?;
    let Some(current_limit) = current_limit else {
        return Err(StoreError::InvalidResource(format!(
            "令牌 {token_key} 不存在，无法重放费用预留"
        )));
    };
    if reserved_cost > 0 {
        let pending_user: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(reserved_cost_usd_micros), 0) \
             FROM billing_reservations reserved \
             WHERE reserved.user_id = ? AND reserved.status = 'reserved' \
               AND reserved.attempt_id <> ?",
        )
        .bind(user_id)
        .bind(attempt_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(StoreError::Query)?;
        let balance: i64 =
            sqlx::query_scalar("SELECT balance_usd_micros FROM user_balance WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(StoreError::Query)?;
        if balance.saturating_sub(pending_user) < reserved_cost {
            return Err(StoreError::InsufficientFunds);
        }
        if let Some(limit) = current_limit {
            let settled: i64 = sqlx::query_scalar(
                "SELECT COALESCE(settled_usd_micros, 0) FROM token_balance WHERE token_key = ?",
            )
            .bind(&token_key)
            .fetch_optional(&mut **tx)
            .await
            .map_err(StoreError::Query)?
            .unwrap_or(0);
            let pending_token: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(reserved_cost_usd_micros), 0) \
                 FROM billing_reservations reserved \
                 WHERE reserved.token_key = ? AND reserved.status = 'reserved' \
                   AND reserved.attempt_id <> ?",
            )
            .bind(&token_key)
            .bind(attempt_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(StoreError::Query)?;
            if settled
                .saturating_add(pending_token)
                .saturating_add(reserved_cost)
                > limit
            {
                return Err(StoreError::TokenLimitExceeded);
            }
        }
    }
    if status == "released" {
        sqlx::query(
            "UPDATE billing_reservations SET status = 'reserved', updated_at = ? \
             WHERE attempt_id = ? AND status = 'released'",
        )
        .bind(now)
        .bind(attempt_id)
        .execute(&mut **tx)
        .await
        .map_err(StoreError::Query)?;
    }
    Ok(())
}

/// 在结算事务内删除已写入最终日志的队列项。
pub(crate) async fn delete_pending_request_log_on(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM request_log_outbox WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 所属用户的钱包余额。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserWallet {
    /// 用户当前剩余（micro-USD），可为负（在途透支）。
    pub balance_usd_micros: i64,
    /// 用户累计结算总额（micro-USD）。
    pub settled_usd_micros: i64,
}

/// 单个令牌的累计结算。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenSettlement {
    /// 该令牌累计结算总额（micro-USD），用于 `limit_usd` 上限检查。
    pub settled_usd_micros: i64,
}

/// 网关准入所需的组合快照：用户钱包与令牌累计结算来自同一读取边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionSnapshot {
    pub wallet: UserWallet,
    pub token: TokenSettlement,
}

/// 一次实际出站尝试在准入事务中冻结的账务信息。
pub struct BillingAttemptReservation<'a> {
    /// 每次实际出站尝试唯一；同一入站请求的重试不得复用。
    pub attempt_id: &'a str,
    /// 聚合同一次入站请求产生的所有出站尝试。
    pub request_id: &'a str,
    pub token_key: &'a str,
    pub user_id: i64,
    pub cost_usd_micros: i64,
    /// 令牌是累计用量边界而非钱包；`None` 表示不限制累计金额。
    pub token_limit_usd_micros: Option<i64>,
    /// 供进程崩溃恢复构造持久化结果的最小请求元数据。
    pub recovery_metadata: &'a [u8],
}

/// 已发出但尚未把结果写入 outbox 时，恢复任务用于重建日志的元数据。
///
/// 出站前先保存结算与审计所需的标识和价格快照；结果生成后再原位补入完整日志，
/// 因而恢复任务既能处理未知结果，也能在 outbox 写入失败时保留已生成的正文与 usage。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BillingAttemptRecovery {
    pub token_name: String,
    pub model: String,
    pub outbound_model: Option<String>,
    pub channel: String,
    pub channel_key: Option<String>,
    pub inbound_protocol: String,
    pub started: i64,
    pub price: PriceSnapshot,
    pub discount_bp: i64,
    /// 请求体快照仅在启用完整日志且结果尚未进入 outbox 时保留。
    #[serde(default)]
    pub request_body: Option<Vec<u8>>,
    /// 已生成但尚未进入 outbox 的完整结果；崩溃恢复优先使用它重建原始记录。
    #[serde(default)]
    pub result: Option<Box<RequestLog>>,
    /// 费用计算错误的稳定文本；存在时恢复记录必须保持未结算。
    #[serde(default)]
    pub result_settlement_error: Option<String>,
}

/// 在计费预留行中保存已生成的结果，供 outbox 写入失败或进程崩溃后的恢复任务
/// 使用。该更新不改变预留状态，重复写入同一结果保持幂等。
pub(crate) async fn persist_billing_attempt_result(
    pool: &SqlitePool,
    attempt_id: &str,
    pending: &PendingRequestLog,
) -> Result<(), StoreError> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(StoreError::Query)?;
    let metadata = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT recovery_metadata FROM billing_reservations \
         WHERE attempt_id = ? AND status = 'reserved'",
    )
    .bind(attempt_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(StoreError::Query)?
    .ok_or_else(|| StoreError::InvalidResource(format!("费用预留 {attempt_id} 不存在或已终止")))?;
    let mut recovery: BillingAttemptRecovery = serde_json::from_slice(&metadata)
        .map_err(|err| StoreError::InvalidResource(format!("费用恢复元数据无法解码: {err}")))?;
    if let Some(existing) = recovery.result.as_deref() {
        if existing == &pending.log && recovery.result_settlement_error == pending.settlement_error
        {
            tx.commit().await.map_err(StoreError::Query)?;
            return Ok(());
        }
        return Err(StoreError::ReservationConflict);
    }
    recovery.result = Some(Box::new(pending.log.clone()));
    recovery.result_settlement_error = pending.settlement_error.clone();
    let encoded = serde_json::to_vec(&recovery)
        .map_err(|err| StoreError::InvalidResource(format!("费用结果无法编码: {err}")))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let updated = sqlx::query(
        "UPDATE billing_reservations SET recovery_metadata = ?, updated_at = ? \
         WHERE attempt_id = ? AND status = 'reserved'",
    )
    .bind(encoded)
    .bind(now)
    .bind(attempt_id)
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?;
    if updated.rows_affected() != 1 {
        return Err(StoreError::InvalidResource(format!(
            "费用预留 {attempt_id} 不存在或已终止"
        )));
    }
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(())
}

/// 在实际出站调用前为一次物理尝试原子预留费用。
///
/// 用户钱包是唯一资金来源；令牌金额仅限制该令牌的累计结算。钱包余额、令牌
/// 累计上限和预留行在同一个 `BEGIN IMMEDIATE` 事务中检查并写入，因而并发尝试
/// 不能共同消费同一份可用额度。幂等性只作用于 `attempt_id`；`request_id` 相同的
/// 重试、渠道切换和统一模型跳转仍是互相独立的账务动作。
pub async fn reserve_billing_attempt(
    pool: &SqlitePool,
    reservation: BillingAttemptReservation<'_>,
) -> Result<bool, StoreError> {
    if reservation.cost_usd_micros < 0 {
        return Err(StoreError::InvalidResource(
            "预留费用不能为负数".to_string(),
        ));
    }
    if reservation.recovery_metadata.is_empty() {
        return Err(StoreError::InvalidResource(
            "计费预留缺少恢复元数据".to_string(),
        ));
    }
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(StoreError::Query)?;
    if let Some((existing_token, existing_user, existing_cost, status)) =
        sqlx::query_as::<_, (String, i64, i64, String)>(
            "SELECT token_key, user_id, reserved_cost_usd_micros, status \
             FROM billing_reservations WHERE attempt_id = ?",
        )
        .bind(reservation.attempt_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Query)?
    {
        if existing_token != reservation.token_key
            || existing_user != reservation.user_id
            || existing_cost != reservation.cost_usd_micros
        {
            return Err(StoreError::ReservationConflict);
        }
        tx.commit().await.map_err(StoreError::Query)?;
        return Ok(status == "reserved" || status == "settled");
    }

    let pending_user: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(reserved_cost_usd_micros), 0) \
         FROM billing_reservations reserved \
         WHERE reserved.user_id = ? AND reserved.status = 'reserved'",
    )
    .bind(reservation.user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(StoreError::Query)?;
    if reservation.cost_usd_micros > 0 {
        let balance: Option<i64> =
            sqlx::query_scalar("SELECT balance_usd_micros FROM user_balance WHERE user_id = ?")
                .bind(reservation.user_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(StoreError::Query)?;
        let balance = balance.ok_or(StoreError::MissingWallet(reservation.user_id))?;
        if balance.saturating_sub(pending_user) < reservation.cost_usd_micros {
            tx.rollback().await.map_err(StoreError::Query)?;
            return Ok(false);
        }
    }

    if let Some(limit) = reservation.token_limit_usd_micros {
        let settled: i64 = sqlx::query_scalar(
            "SELECT COALESCE(settled_usd_micros, 0) FROM token_balance WHERE token_key = ?",
        )
        .bind(reservation.token_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Query)?
        .unwrap_or(0);
        let pending_token: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(reserved_cost_usd_micros), 0) \
             FROM billing_reservations reserved \
             WHERE reserved.token_key = ? AND reserved.status = 'reserved'",
        )
        .bind(reservation.token_key)
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
        if settled
            .saturating_add(pending_token)
            .saturating_add(reservation.cost_usd_micros)
            > limit
        {
            tx.rollback().await.map_err(StoreError::Query)?;
            return Ok(false);
        }
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    sqlx::query(
        "INSERT INTO billing_reservations \
         (attempt_id, request_id, token_key, user_id, reserved_cost_usd_micros, token_limit_usd_micros, \
          recovery_metadata, status, dispatched, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'reserved', 0, ?, ?)",
    )
    .bind(reservation.attempt_id)
    .bind(reservation.request_id)
    .bind(reservation.token_key)
    .bind(reservation.user_id)
    .bind(reservation.cost_usd_micros)
    .bind(reservation.token_limit_usd_micros)
    .bind(reservation.recovery_metadata)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?;
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(true)
}

/// 标记预留已经进入实际出站调用阶段；恢复任务据此区分可释放和未知费用。
pub async fn mark_billing_attempt_dispatched(
    pool: &SqlitePool,
    attempt_id: &str,
) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE billing_reservations SET dispatched = 1, updated_at = ? \
         WHERE attempt_id = ? AND status = 'reserved' AND dispatched = 0",
    )
    .bind(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0),
    )
    .bind(attempt_id)
    .execute(pool)
    .await
    .map_err(StoreError::Query)?;
    if updated.rows_affected() != 1 {
        return Err(StoreError::InvalidResource(format!(
            "费用预留 {attempt_id} 不存在、已发出或已终止"
        )));
    }
    Ok(())
}

/// 结算一个预留。实际费用不足预留时退回差额；超过预留时只允许在同一事务中
/// 通过用户余额和令牌累计上限的再次检查后补差，绝不静默形成未记录欠款。
pub async fn settle_billing_attempt(
    conn: &mut SqliteConnection,
    attempt_id: &str,
    actual_cost_usd_micros: i64,
) -> Result<(), StoreError> {
    if actual_cost_usd_micros < 0 {
        return Err(StoreError::InvalidResource(
            "实际费用不能为负数".to_string(),
        ));
    }
    let row = sqlx::query_as::<_, (String, i64, i64, Option<i64>, Option<i64>, String)>(
        "SELECT token_key, user_id, reserved_cost_usd_micros, token_limit_usd_micros, \
                actual_cost_usd_micros, status \
         FROM billing_reservations WHERE attempt_id = ?",
    )
    .bind(attempt_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    let Some(row) = row else {
        if actual_cost_usd_micros == 0 {
            return Ok(());
        }
        return Err(StoreError::InvalidResource(format!(
            "找不到费用预留 {attempt_id}"
        )));
    };
    let (token_key, user_id, reserved, token_limit, recorded_actual, status) = row;
    if status == "settled" {
        if recorded_actual == Some(actual_cost_usd_micros) {
            return Ok(());
        }
        return Err(StoreError::ReservationConflict);
    }
    if status != "reserved" {
        return Err(StoreError::InvalidResource(format!(
            "费用预留 {attempt_id} 已终止"
        )));
    }
    if actual_cost_usd_micros > reserved {
        let pending_user: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(reserved_cost_usd_micros), 0) \
             FROM billing_reservations reserved \
             WHERE reserved.user_id = ? AND reserved.status = 'reserved' \
               AND reserved.attempt_id <> ?",
        )
        .bind(user_id)
        .bind(attempt_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
        let balance: i64 =
            sqlx::query_scalar("SELECT balance_usd_micros FROM user_balance WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(&mut *conn)
                .await
                .map_err(StoreError::Query)?;
        if balance
            .saturating_sub(pending_user)
            .saturating_sub(actual_cost_usd_micros)
            < 0
        {
            return Err(StoreError::InsufficientFunds);
        }
        if let Some(limit) = token_limit {
            let settled: i64 = sqlx::query_scalar(
                "SELECT COALESCE(settled_usd_micros, 0) FROM token_balance WHERE token_key = ?",
            )
            .bind(&token_key)
            .fetch_optional(&mut *conn)
            .await
            .map_err(StoreError::Query)?
            .unwrap_or(0);
            let pending_token: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(reserved_cost_usd_micros), 0) \
                 FROM billing_reservations reserved \
                 WHERE reserved.token_key = ? AND reserved.status = 'reserved' \
                   AND reserved.attempt_id <> ?",
            )
            .bind(&token_key)
            .bind(attempt_id)
            .fetch_one(&mut *conn)
            .await
            .map_err(StoreError::Query)?;
            if settled
                .saturating_add(pending_token)
                .saturating_add(actual_cost_usd_micros)
                > limit
            {
                return Err(StoreError::TokenLimitExceeded);
            }
        }
    }
    // 预留冻结了费用归属。令牌在请求完成前被删除时，结算仍扣所属用户钱包；
    // 令牌累计行是附属投影，不得反过来决定已发生费用能否入账。
    apply_charge(conn, user_id, &token_key, actual_cost_usd_micros, false).await?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    sqlx::query(
        "UPDATE billing_reservations SET actual_cost_usd_micros = ?, status = 'settled', updated_at = ? \
         WHERE attempt_id = ?",
    )
    .bind(actual_cost_usd_micros)
    .bind(now)
    .bind(attempt_id)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    clear_billing_attempt_recovery_payload_by_id(conn, attempt_id).await?;
    Ok(())
}

/// provider 尚未调用时释放预留；已 dispatch 的 attempt 不允许走退款路径。
pub async fn release_billing_attempt(
    pool: &SqlitePool,
    attempt_id: &str,
) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE billing_reservations SET status = 'released', updated_at = ? \
         WHERE attempt_id = ? AND status = 'reserved' AND dispatched = 0",
    )
    .bind(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0),
    )
    .bind(attempt_id)
    .execute(pool)
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// 扫描进程崩溃留下的预留：尚未进入 provider 阶段的释放，已进入阶段的生成
/// 保守结算记录。所有动作都在各自的短写事务中完成，和正常入队共享同一唯一键。
pub(crate) async fn recover_orphan_billing_attempts(
    pool: &SqlitePool,
    max_age: Duration,
    limit: i64,
) -> Result<usize, StoreError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let age_millis = i64::try_from(max_age.as_millis()).unwrap_or(i64::MAX);
    let cutoff = now.saturating_sub(age_millis);
    let candidates = sqlx::query(
        "SELECT attempt_id FROM billing_reservations \
         WHERE status = 'reserved' AND result_persisted = 0 \
           AND updated_at <= ? ORDER BY updated_at, attempt_id LIMIT ?",
    )
    .bind(cutoff)
    .bind(limit.max(1))
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;

    let mut recovered = 0usize;
    for candidate in candidates {
        let attempt_id: String = candidate.try_get("attempt_id").map_err(StoreError::Query)?;
        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(StoreError::Query)?;
        let row = sqlx::query(
            "SELECT request_id, token_key, user_id, reserved_cost_usd_micros, dispatched, \
                    recovery_metadata \
             FROM billing_reservations \
             WHERE attempt_id = ? AND status = 'reserved' AND result_persisted = 0 \
               AND updated_at <= ?",
        )
        .bind(&attempt_id)
        .bind(cutoff)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
        let Some(row) = row else {
            tx.rollback().await.map_err(StoreError::Query)?;
            continue;
        };
        let dispatched: i64 = row.try_get("dispatched").map_err(StoreError::Query)?;
        if dispatched == 0 {
            sqlx::query(
                "UPDATE billing_reservations SET status = 'released', updated_at = ? \
                 WHERE attempt_id = ? AND status = 'reserved' AND result_persisted = 0",
            )
            .bind(now)
            .bind(&attempt_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Query)?;
            tx.commit().await.map_err(StoreError::Query)?;
            recovered = recovered.saturating_add(1);
            continue;
        }

        let request_id: String = row.try_get("request_id").map_err(StoreError::Query)?;
        let token_key: String = row.try_get("token_key").map_err(StoreError::Query)?;
        let user_id: i64 = row.try_get("user_id").map_err(StoreError::Query)?;
        let reserved_cost: i64 = row
            .try_get("reserved_cost_usd_micros")
            .map_err(StoreError::Query)?;
        let metadata: Vec<u8> = row
            .try_get("recovery_metadata")
            .map_err(StoreError::Query)?;
        let (recovery, metadata_error) =
            match serde_json::from_slice::<BillingAttemptRecovery>(&metadata) {
                Ok(recovery) => (recovery, None),
                Err(err) => {
                    // 保留损坏的原始 BLOB，同时生成一条不可自动结算的隔离记录。
                    // 不能用占位元数据静默替换原始事实，否则后续人工复核无法判断
                    // 这条请求实际使用的模型、价格和响应是否可信。
                    let reason = format!("费用恢复元数据损坏: {err}");
                    (
                        BillingAttemptRecovery {
                            token_name: token_key.clone(),
                            model: "<recovered-attempt>".to_string(),
                            outbound_model: None,
                            channel: "<unknown-channel>".to_string(),
                            channel_key: None,
                            inbound_protocol: "unknown".to_string(),
                            started: now,
                            price: PriceSnapshot::default(),
                            discount_bp: billing::DEFAULT_DISCOUNT_BP,
                            request_body: None,
                            result: None,
                            result_settlement_error: Some(reason.clone()),
                        },
                        Some(reason),
                    )
                }
            };
        let BillingAttemptRecovery {
            token_name,
            model,
            outbound_model,
            channel,
            channel_key,
            inbound_protocol,
            started,
            price,
            discount_bp,
            request_body,
            result,
            result_settlement_error,
        } = recovery;
        let (mut log, settlement_error) = if let Some(result) = result {
            (*result, result_settlement_error.or(metadata_error.clone()))
        } else {
            let log = RequestLog {
                id: 0,
                created_at: now,
                token_name,
                token_key,
                user_id,
                inbound_protocol,
                model,
                outbound_model,
                channel,
                channel_key,
                status_code: 502,
                latency_ms: now.saturating_sub(started),
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_write_1h_tokens: 0,
                usage_reported: false,
                price,
                base_cost_usd_micros: conservative_base_from_charge(reserved_cost, discount_bp),
                discount_bp,
                cost_usd_micros: reserved_cost,
                settled: false,
                request_id: Some(request_id),
                billing_attempt_id: Some(attempt_id.clone()),
                request_body,
                response_body: None,
            };
            (log, result_settlement_error)
        };
        if !log.usage_reported && settlement_error.is_none() {
            // 结果可能来自旧版本：当时缺失 usage 的日志尚未把预留金额写进
            // result。恢复时按当前预留补齐费用与基础成本，避免崩溃窗口把
            // 请求错误地当成零费用。
            log.cost_usd_micros = reserved_cost;
            if log.base_cost_usd_micros == 0 {
                log.base_cost_usd_micros =
                    conservative_base_from_charge(reserved_cost, discount_bp);
            }
        }
        let mut pending = PendingRequestLog {
            log,
            settlement_error,
        };
        let outbox_id = ids::next_id()?;
        let request_body = pending.log.request_body.take();
        let response_body = pending.log.response_body.take();
        let encoded = serde_json::to_vec(&pending)
            .map_err(|err| StoreError::InvalidResource(format!("恢复记录无法编码: {err}")))?;
        let inserted = sqlx::query(
            "INSERT INTO request_log_outbox \
             (id, token_key, user_id, cost_usd_micros, metadata, request_body, response_body, \
              request_id, billing_attempt_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(outbox_id)
        .bind(&pending.log.token_key)
        .bind(pending.log.user_id)
        .bind(pending.log.cost_usd_micros)
        .bind(&encoded)
        .bind(&request_body)
        .bind(&response_body)
        .bind(&pending.log.request_id)
        .bind(&pending.log.billing_attempt_id)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
        if inserted.rows_affected() == 0 {
            let existing = sqlx::query_as::<
                _,
                (
                    String,
                    i64,
                    i64,
                    Vec<u8>,
                    Option<Vec<u8>>,
                    Option<Vec<u8>>,
                    Option<String>,
                    Option<String>,
                ),
            >(
                "SELECT token_key, user_id, cost_usd_micros, metadata, request_body, \
                        response_body, request_id, billing_attempt_id \
                 FROM request_log_outbox WHERE billing_attempt_id = ?",
            )
            .bind(&attempt_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(StoreError::Query)?;
            if existing.0 != pending.log.token_key
                || existing.1 != pending.log.user_id
                || existing.2 != pending.log.cost_usd_micros
                || existing.3 != encoded
                || existing.4 != request_body
                || existing.5 != response_body
                || existing.6 != pending.log.request_id
                || existing.7 != pending.log.billing_attempt_id
            {
                return Err(StoreError::ReservationConflict);
            }
        }
        mark_billing_attempt_result_persisted(&mut tx, &attempt_id).await?;
        tx.commit().await.map_err(StoreError::Query)?;
        if let Some(reason) = metadata_error {
            record_system_error(
                pool,
                "billing",
                &SystemLogEvent::new(
                    "billing.recovery_metadata_corrupt",
                    serde_json::json!({ "attempt_id": attempt_id }),
                    reason,
                ),
            )
            .await;
        }
        recovered = recovered.saturating_add(1);
    }
    Ok(recovered)
}

/// 由折后金额反推在整数截断下可能对应的原价上界。
///
/// 结算预留只保存折后金额，恢复任务无法重新获得原始 usage 时使用此上界
/// 记录基础成本，避免把整笔预留错误显示为毛利。免费折扣不可逆，返回 0。
fn conservative_base_from_charge(charge: i64, discount_bp: i64) -> i64 {
    if charge <= 0 || discount_bp <= 0 {
        return 0;
    }
    let numerator = (charge as i128 + 1)
        .saturating_mul(billing::DEFAULT_DISCOUNT_BP as i128)
        .saturating_sub(1);
    let estimate = numerator / discount_bp as i128;
    i64::try_from(estimate).unwrap_or(i64::MAX)
}

/// 令牌首次出现时建立累计结算行，并把初始余额记入所属用户钱包；已存在则原样返回。
///
/// 初始余额已经是整数 micro-USD。仅在新建结算行时入账，避免重启或重复调用
/// 把同一令牌的初始额再加一遍。
pub async fn initialize_token_settlement(
    conn: &mut SqliteConnection,
    token_key: &str,
    initial_balance_usd_micros: i64,
    now: i64,
) -> Result<TokenSettlement, StoreError> {
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

    if inserted.rows_affected() == 1 && initial_balance_usd_micros != 0 {
        let credited = sqlx::query(
            "UPDATE user_balance SET balance_usd_micros = balance_usd_micros + ? \
             WHERE user_id = (SELECT user_id FROM tokens WHERE token_key = ?)",
        )
        .bind(initial_balance_usd_micros)
        .bind(token_key)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
        if credited.rows_affected() == 0 {
            return Err(StoreError::MissingToken(token_key.to_string()));
        }
    }

    get_token_settlement(conn, token_key)
        .await?
        .ok_or(StoreError::MissingToken(token_key.to_string()))
}

/// 读取令牌累计结算；令牌不存在返回 `None`。
pub async fn get_token_settlement(
    conn: &mut SqliteConnection,
    token_key: &str,
) -> Result<Option<TokenSettlement>, StoreError> {
    let row = sqlx::query_scalar::<_, i64>(
        "SELECT settled_usd_micros FROM token_balance WHERE token_key = ?",
    )
    .bind(token_key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?;

    Ok(row.map(|settled_usd_micros| TokenSettlement { settled_usd_micros }))
}

/// 读取令牌所属用户的钱包与该令牌累计结算。
pub async fn get_admission_snapshot(
    conn: &mut SqliteConnection,
    token_key: &str,
) -> Result<Option<AdmissionSnapshot>, StoreError> {
    let row = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT balance_usd_micros - pending_user_cost, \
                user_settled_usd_micros + pending_user_cost, \
                token_settled_usd_micros + pending_token_cost \
         FROM ( \
             SELECT ub.balance_usd_micros, \
                    ub.settled_usd_micros AS user_settled_usd_micros, \
                    COALESCE(tb.settled_usd_micros, 0) AS token_settled_usd_micros, \
                    COALESCE(( \
                        SELECT SUM(reserved.reserved_cost_usd_micros) \
                        FROM billing_reservations reserved \
                        WHERE reserved.user_id = t.user_id AND reserved.status = 'reserved' \
                    ), 0) + COALESCE(( \
                        SELECT SUM(pending.cost_usd_micros) \
                        FROM request_log_outbox pending \
                        WHERE pending.user_id = t.user_id AND pending.request_id IS NULL \
                    ), 0) AS pending_user_cost, \
                    COALESCE(( \
                        SELECT SUM(reserved.reserved_cost_usd_micros) \
                        FROM billing_reservations reserved \
                        WHERE reserved.token_key = t.token_key AND reserved.status = 'reserved' \
                    ), 0) + COALESCE(( \
                        SELECT SUM(pending.cost_usd_micros) \
                        FROM request_log_outbox pending \
                        WHERE pending.token_key = t.token_key AND pending.request_id IS NULL \
                    ), 0) AS pending_token_cost \
             FROM tokens t \
             INNER JOIN user_balance ub ON ub.user_id = t.user_id \
             LEFT JOIN token_balance tb ON tb.token_key = t.token_key \
             WHERE t.token_key = ? \
         )",
    )
    .bind(token_key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    Ok(
        row.map(|(balance, user_settled, token_settled)| AdmissionSnapshot {
            wallet: UserWallet {
                balance_usd_micros: balance,
                settled_usd_micros: user_settled,
            },
            token: TokenSettlement {
                settled_usd_micros: token_settled,
            },
        }),
    )
}

/// 删除令牌累计结算行；不存在视为成功（幂等）。
///
/// 供删除令牌时同事务清理：结算行若残留，同 key 重建令牌会经
/// `initialize_token_settlement` 的冲突跳过、不再把初始额写入用户钱包。
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
) -> Result<TokenSettlement, StoreError> {
    let user_id: Option<i64> = sqlx::query_scalar("SELECT user_id FROM tokens WHERE token_key = ?")
        .bind(token_key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    let user_id = user_id.ok_or_else(|| StoreError::MissingToken(token_key.to_string()))?;
    apply_charge(conn, user_id, token_key, cost_usd_micros, true).await?;

    get_token_settlement(conn, token_key)
        .await?
        .ok_or(StoreError::MissingToken(token_key.to_string()))
}

/// 向指定用户钱包结算，并在令牌仍存在时累计令牌结算额。
///
/// `require_token` 只供在线请求结算使用；历史日志已经冻结 `user_id`，令牌删除后仍须
/// 能补扣钱包，因此历史路径把令牌累计视为可选的附属更新。
async fn apply_charge(
    conn: &mut SqliteConnection,
    user_id: i64,
    token_key: &str,
    cost_usd_micros: i64,
    require_token: bool,
) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE user_balance \
         SET balance_usd_micros = balance_usd_micros - ?, \
             settled_usd_micros = settled_usd_micros + ? \
         WHERE user_id = ?",
    )
    .bind(cost_usd_micros)
    .bind(cost_usd_micros)
    .bind(user_id)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    if updated.rows_affected() == 0 {
        return Err(StoreError::MissingWallet(user_id));
    }

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let token_updated = sqlx::query(
        "INSERT INTO token_balance (token_key, settled_usd_micros, created_at) \
         SELECT token_key, ?, ? FROM tokens WHERE token_key = ? AND user_id = ? \
         ON CONFLICT(token_key) DO UPDATE SET \
           settled_usd_micros = settled_usd_micros + excluded.settled_usd_micros",
    )
    .bind(cost_usd_micros)
    .bind(created_at)
    .bind(token_key)
    .bind(user_id)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    if require_token && token_updated.rows_affected() == 0 {
        return Err(StoreError::MissingToken(token_key.to_string()));
    }
    Ok(())
}

/// 读用户钱包。插入用户时同步建行；缺失视为数据损坏。
pub async fn get_user_wallet(pool: &SqlitePool, user_id: i64) -> Result<UserWallet, StoreError> {
    let (balance_usd_micros, settled_usd_micros) = sqlx::query_as(
        "SELECT balance_usd_micros, settled_usd_micros FROM user_balance WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(StoreError::Query)?
    .ok_or(StoreError::MissingWallet(user_id))?;
    Ok(UserWallet {
        balance_usd_micros,
        settled_usd_micros,
    })
}

/// 全部用户钱包，供管理列表一次取回。
pub async fn list_user_wallets(pool: &SqlitePool) -> Result<HashMap<i64, UserWallet>, StoreError> {
    let rows =
        sqlx::query("SELECT user_id, balance_usd_micros, settled_usd_micros FROM user_balance")
            .fetch_all(pool)
            .await
            .map_err(StoreError::Query)?;
    let mut wallets = HashMap::with_capacity(rows.len());
    for row in rows {
        let user_id: i64 = row.try_get("user_id").map_err(StoreError::Query)?;
        let balance: i64 = row
            .try_get("balance_usd_micros")
            .map_err(StoreError::Query)?;
        let settled: i64 = row
            .try_get("settled_usd_micros")
            .map_err(StoreError::Query)?;
        wallets.insert(
            user_id,
            UserWallet {
                balance_usd_micros: balance,
                settled_usd_micros: settled,
            },
        );
    }
    Ok(wallets)
}

/// 一次用户钱包相对调整产生的事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalanceChange {
    pub before_usd_micros: i64,
    pub after_usd_micros: i64,
    pub settled_usd_micros: i64,
}

/// 相对调整用户钱包：充值传正数、扣减传负数。
///
/// 前后值来自同一条原子 `UPDATE ... RETURNING`，调用方可直接用于审计，不需要在
/// 事务外预读一个可能已过时的钱包快照。
pub async fn adjust_user_balance(
    conn: &mut SqliteConnection,
    user_id: i64,
    delta_usd_micros: i64,
) -> Result<BalanceChange, StoreError> {
    let row = sqlx::query(
        "UPDATE user_balance SET balance_usd_micros = balance_usd_micros + ? \
         WHERE user_id = ? RETURNING balance_usd_micros, settled_usd_micros",
    )
    .bind(delta_usd_micros)
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    let row = row.ok_or(StoreError::MissingWallet(user_id))?;
    let after_usd_micros: i64 = row
        .try_get("balance_usd_micros")
        .map_err(StoreError::Query)?;
    let settled_usd_micros: i64 = row
        .try_get("settled_usd_micros")
        .map_err(StoreError::Query)?;
    let before_usd_micros = after_usd_micros
        .checked_sub(delta_usd_micros)
        .ok_or_else(|| StoreError::InvalidResource("余额调整超出整数范围".to_string()))?;
    Ok(BalanceChange {
        before_usd_micros,
        after_usd_micros,
        settled_usd_micros,
    })
}

/// 读取指定用户令牌的累计结算额，避免令牌列表为每个用户扫描整张结算表。
pub async fn list_token_settled_for_user(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<HashMap<String, i64>, StoreError> {
    let rows = sqlx::query(
        "SELECT tb.token_key, tb.settled_usd_micros \
         FROM token_balance tb JOIN tokens t ON t.token_key = tb.token_key \
         WHERE t.user_id = ?",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;
    let mut settled = HashMap::with_capacity(rows.len());
    for row in rows {
        settled.insert(
            row.try_get("token_key").map_err(StoreError::Query)?,
            row.try_get("settled_usd_micros")
                .map_err(StoreError::Query)?,
        );
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

/// 在调用方事务内读取单令牌累计结算额；无结算行视为 0。
pub async fn get_token_settled_on_conn(
    conn: &mut SqliteConnection,
    token_key: &str,
) -> Result<i64, StoreError> {
    sqlx::query_scalar("SELECT settled_usd_micros FROM token_balance WHERE token_key = ?")
        .bind(token_key)
        .fetch_optional(&mut *conn)
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
    /// 按是否已完成所属用户钱包结算过滤；`None` 表示不限。
    pub settled: Option<bool>,
    /// 按该次使用的万分比折扣率精确过滤；`None` 表示不限。
    pub discount_bp: Option<i64>,
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
         channel, channel_key, status_code, latency_ms, input_tokens, output_tokens, cache_read_tokens, \
         cache_write_tokens, cache_write_1h_tokens, input_price_usd_micros, output_price_usd_micros, \
         cache_read_price_usd_micros, cache_write_price_usd_micros, cache_write_1h_price_usd_micros, \
         base_cost_usd_micros, discount_bp, cost_usd_micros, \
         settled, usage_reported, request_id, billing_attempt_id FROM request_log",
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
    let mut conn = pool.acquire().await.map_err(StoreError::Query)?;
    get_request_log_on_conn(&mut conn, id).await
}

/// 在现有连接/事务上按主键读取请求日志（含 body）。
pub async fn get_request_log_on_conn(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<Option<RequestLog>, StoreError> {
    let row = sqlx::query(
        "SELECT id, created_at, token_name, token_key, user_id, inbound_protocol, model, outbound_model, \
         channel, channel_key, status_code, latency_ms, input_tokens, output_tokens, cache_read_tokens, \
         cache_write_tokens, cache_write_1h_tokens, input_price_usd_micros, output_price_usd_micros, \
         cache_read_price_usd_micros, cache_write_price_usd_micros, cache_write_1h_price_usd_micros, \
         base_cost_usd_micros, discount_bp, cost_usd_micros, \
         settled, usage_reported, request_id, billing_attempt_id, request_body, response_body \
         FROM request_log WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
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
    let Some((token_key, mut user_id, cost, settled)) = load_log_settlement(conn, id).await? else {
        return Ok(UnsettledLogAction::NotFound);
    };
    if settled {
        return Ok(UnsettledLogAction::AlreadySettled);
    }
    if cost > 0 {
        // 迁移前无法回填归属的存量行以 0 表示未知。仅这类行退回当前令牌关系；
        // 新行始终以日志冻结的 user_id 为准，令牌删除也不会改变债务归属。
        if user_id == 0 {
            user_id = sqlx::query_scalar("SELECT user_id FROM tokens WHERE token_key = ?")
                .bind(&token_key)
                .fetch_optional(&mut *conn)
                .await
                .map_err(StoreError::Query)?
                .ok_or_else(|| StoreError::MissingToken(token_key.clone()))?;
        }
        apply_charge(conn, user_id, &token_key, cost, false).await?;
    }
    mark_request_log_settled(conn, id).await?;
    Ok(UnsettledLogAction::Closed)
}

/// 豁免未结算日志：清除待收费用并翻 `settled`，不动余额。
///
/// `settled` 同时表示财务聚合可纳入的已完成状态，因此豁免行必须把费用列
/// 归零，否则会在不扣钱包的情况下被统计为收入。原始请求/响应正文和审计
/// 事件仍保留，便于追溯豁免前的记录。
pub async fn waive_unsettled_log(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<UnsettledLogAction, StoreError> {
    let Some((_, _, _, settled)) = load_log_settlement(conn, id).await? else {
        return Ok(UnsettledLogAction::NotFound);
    };
    if settled {
        return Ok(UnsettledLogAction::AlreadySettled);
    }
    sqlx::query(
        "UPDATE request_log SET settled = 1, cost_usd_micros = 0, base_cost_usd_micros = 0 \
         WHERE id = ?",
    )
    .bind(id)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    Ok(UnsettledLogAction::Closed)
}

/// 读一条日志的结算所需字段；不存在返回 `None`。
async fn load_log_settlement(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<Option<(String, i64, i64, bool)>, StoreError> {
    let row = sqlx::query(
        "SELECT token_key, user_id, cost_usd_micros, settled FROM request_log WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let token_key: String = row.try_get("token_key").map_err(StoreError::Query)?;
    let user_id: i64 = row.try_get("user_id").map_err(StoreError::Query)?;
    let cost: i64 = row.try_get("cost_usd_micros").map_err(StoreError::Query)?;
    let settled = row
        .try_get::<i64, _>("settled")
        .map_err(StoreError::Query)?
        != 0;
    Ok(Some((token_key, user_id, cost, settled)))
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

/// 单批删除的行数：批间提交让请求路径的结算写入得以插队，避免单事务长写锁。
const LOG_PURGE_BATCH_ROWS: u64 = 5_000;

/// 删除早于截止时刻的**已结算**请求日志，返回删除总行数。
///
/// 未结算行是对账队列（补扣/豁免的依据），删除即坏账，永不清理。分批提交：
/// SQLite 单写者下一次性删百万行会长时间占住写锁，把请求路径的结算写入
/// 挤到 `busy_timeout` 之外。
pub async fn purge_settled_request_logs_before(
    pool: &SqlitePool,
    cutoff_created_at: i64,
) -> Result<u64, StoreError> {
    let mut removed = 0u64;
    loop {
        let result = sqlx::query(
            "DELETE FROM request_log WHERE id IN ( \
                SELECT id FROM request_log WHERE created_at < ? AND settled != 0 \
                LIMIT ?)",
        )
        .bind(cutoff_created_at)
        .bind(LOG_PURGE_BATCH_ROWS as i64)
        .execute(pool)
        .await
        .map_err(StoreError::Query)?;
        let affected = result.rows_affected();
        removed += affected;
        if affected < LOG_PURGE_BATCH_ROWS {
            return Ok(removed);
        }
    }
}

/// 日志存储占用与行数快照，供 root 在设置页决定何时清理。
///
/// 体积走**文件系统**：主库文件 + WAL 边车的实际字节数。SQL 的
/// `page_count × page_size` 只覆盖主库文件，WAL（批量写入期间可能相当大，
/// 见 [`purge_settled_request_logs_before`] 的分批提交）拿不到——判断磁盘
/// 压力需要的是文件系统真相。两个 `COUNT(*)` 在清理后体量有界，按需拉取。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogStoreStats {
    /// 主库文件字节数（含空闲页：删除不回缩，后续写入逐步复用）。
    pub db_size_bytes: u64,
    /// `<db>-wal` 边车字节数；边车不存在（checkpoint 成功截断或尚未写入）为 0。
    pub wal_size_bytes: u64,
    pub request_log_rows: u64,
    pub system_log_rows: u64,
}

pub async fn log_store_stats(
    pool: &SqlitePool,
    db_path: &Path,
) -> Result<LogStoreStats, StoreError> {
    // 这是管理面的运维诊断：主库路径来自已经打开的配置，读取失败不能伪装成
    // 「0 字节」。WAL 尚未创建是正常状态，只有 NotFound 才折算为 0。
    let db_size_bytes = tokio::fs::metadata(db_path)
        .await
        .map_err(|source| StoreError::FileMetadata {
            path: db_path.to_path_buf(),
            source,
        })?
        .len();
    let mut wal_path = db_path.to_path_buf();
    // 在 OsString 层追加后缀，保留非 UTF-8 路径的原始字节；display() 再拼接会
    // 经过 lossy UTF-8 转换，导致合法的 Unix 路径找不到对应的 WAL 文件。
    wal_path.as_mut_os_string().push("-wal");
    let wal_size_bytes = match tokio::fs::metadata(&wal_path).await {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
        Err(source) => {
            return Err(StoreError::FileMetadata {
                path: wal_path,
                source,
            });
        }
    };
    let request_log_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_log")
        .fetch_one(pool)
        .await
        .map_err(StoreError::Query)?;
    let system_log_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM system_log")
        .fetch_one(pool)
        .await
        .map_err(StoreError::Query)?;
    Ok(LogStoreStats {
        db_size_bytes,
        wal_size_bytes,
        request_log_rows: as_count(request_log_rows),
        system_log_rows: as_count(system_log_rows),
    })
}

/// 清理后的收尾：尝试把 WAL 全量并入主库并将边车截断为零。
///
/// 批量删除的多批独立提交会让 WAL 持续增长，不收尾的话「删完日志磁盘占用
/// 反而更大」会成为常态观感。TRUNCATE 模式会等待在途读事务（受
/// busy_timeout 约束）。SQLite 会把读事务阻塞放在结果行的 `busy` 列中返回，
/// 而不是报 SQL 错；本函数显式检查该列，失败由调用方降级处理。主库文件本身
/// 不缩小（空闲页复用），由调用方的契约文案说明。
pub async fn checkpoint_wal_truncate(pool: &SqlitePool) -> Result<(), StoreError> {
    let row = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(pool)
        .await
        .map_err(StoreError::Query)?;
    let busy: i64 = row.try_get(0).map_err(StoreError::Query)?;
    let log_frames: i64 = row.try_get(1).map_err(StoreError::Query)?;
    let checkpointed_frames: i64 = row.try_get(2).map_err(StoreError::Query)?;
    if busy != 0 {
        return Err(StoreError::WalCheckpointBusy {
            log_frames,
            checkpointed_frames,
        });
    }
    Ok(())
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
    /// 实收（折后）合计。
    pub cost_usd_micros: i64,
    /// 渠道原价合计（成本）。
    pub base_cost_usd_micros: i64,
    /// 毛利：实收 - 渠道原价（折后合计减原价合计）。
    pub gross_profit_usd_micros: i64,
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
    pub base_cost_usd_micros: i64,
    pub gross_profit_usd_micros: i64,
}

/// 按模型或按渠道的费用/请求分布。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostShare {
    pub name: String,
    pub request_count: u64,
    pub cost_usd_micros: i64,
    pub base_cost_usd_micros: i64,
    pub gross_profit_usd_micros: i64,
}

/// 全量累计：不受 `/stats` 时间窗影响。
///
/// 口径：`request_count` 按 `request_id` 去重（存量无 id 的行回退到主键），
/// 表示下游入站次数；`total_tokens` 含全部请求日志行（含未结算），
/// `cost_usd_micros` 统计所有已结算出站尝试（包括失败尝试）。并列展示时
/// 不要把 token 合计当成已入账费用的用量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifetimeStats {
    pub request_count: u64,
    pub cost_usd_micros: i64,
    pub base_cost_usd_micros: i64,
    pub gross_profit_usd_micros: i64,
    pub total_tokens: u64,
}

/// 聚合 `days` 天（已夹取）内的 stats。费用统计所有已结算尝试，成功数仍只
/// 统计 HTTP 2xx，避免失败尝试扣费后在财务报表中消失。
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
         COALESCE(SUM(CASE WHEN settled = 1 THEN cost_usd_micros ELSE 0 END), 0) \
           AS cost_usd_micros, \
         COALESCE(SUM(CASE WHEN settled = 1 THEN base_cost_usd_micros ELSE 0 END), 0) \
           AS base_cost_usd_micros, \
         COALESCE(SUM(CASE WHEN settled = 1 \
             THEN cost_usd_micros - base_cost_usd_micros ELSE 0 END), 0) \
           AS gross_profit_usd_micros \
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
        base_cost_usd_micros: summary_row
            .try_get("base_cost_usd_micros")
            .map_err(StoreError::Query)?,
        gross_profit_usd_micros: summary_row
            .try_get("gross_profit_usd_micros")
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

/// 全量累计：请求数、已结算费用和四分量 token 合计。
///
/// `user_id` 为 `Some` 时只累计该用户名下的流量。
pub async fn query_lifetime_stats(
    pool: &SqlitePool,
    user_id: Option<i64>,
) -> Result<LifetimeStats, StoreError> {
    let sql = format!(
        "SELECT COUNT(DISTINCT COALESCE(request_id, CAST(id AS TEXT))) AS request_count, \
         COALESCE(SUM(CASE WHEN settled = 1 THEN cost_usd_micros ELSE 0 END), 0) \
           AS cost_usd_micros, \
         COALESCE(SUM(CASE WHEN settled = 1 THEN base_cost_usd_micros ELSE 0 END), 0) \
           AS base_cost_usd_micros, \
         COALESCE(SUM(CASE WHEN settled = 1 \
             THEN cost_usd_micros - base_cost_usd_micros ELSE 0 END), 0) \
           AS gross_profit_usd_micros, \
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
        base_cost_usd_micros: row
            .try_get("base_cost_usd_micros")
            .map_err(StoreError::Query)?,
        gross_profit_usd_micros: row
            .try_get("gross_profit_usd_micros")
            .map_err(StoreError::Query)?,
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
        base_cost_usd_micros: row
            .try_get("base_cost_usd_micros")
            .map_err(StoreError::Query)?,
        gross_profit_usd_micros: row
            .try_get("gross_profit_usd_micros")
            .map_err(StoreError::Query)?,
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
                COALESCE(agg.cost_usd_micros, 0) AS cost_usd_micros, \
                COALESCE(agg.base_cost_usd_micros, 0) AS base_cost_usd_micros, \
                COALESCE(agg.gross_profit_usd_micros, 0) AS gross_profit_usd_micros \
         FROM calendar \
         LEFT JOIN ( \
            SELECT strftime('%Y-%m-%dT%H:00:00Z', created_at / 1000, 'unixepoch') AS hour, \
                   COUNT(DISTINCT COALESCE(request_id, CAST(id AS TEXT))) AS request_count, \
                   COALESCE(SUM(input_tokens), 0) AS input_tokens, \
                   COALESCE(SUM(output_tokens), 0) AS output_tokens, \
                   COALESCE(SUM(CASE WHEN settled = 1 \
                        THEN cost_usd_micros ELSE 0 END), 0) AS cost_usd_micros, \
                   COALESCE(SUM(CASE WHEN settled = 1 \
                        THEN base_cost_usd_micros ELSE 0 END), 0) AS base_cost_usd_micros, \
                   COALESCE(SUM(CASE WHEN settled = 1 \
                        THEN cost_usd_micros - base_cost_usd_micros ELSE 0 END), 0) AS gross_profit_usd_micros \
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
                COALESCE(agg.cost_usd_micros, 0) AS cost_usd_micros, \
                COALESCE(agg.base_cost_usd_micros, 0) AS base_cost_usd_micros, \
                COALESCE(agg.gross_profit_usd_micros, 0) AS gross_profit_usd_micros \
         FROM calendar \
         LEFT JOIN ( \
            SELECT date(created_at / 1000, 'unixepoch') AS day, \
                   COUNT(DISTINCT COALESCE(request_id, CAST(id AS TEXT))) AS request_count, \
                   COALESCE(SUM(input_tokens), 0) AS input_tokens, \
                   COALESCE(SUM(output_tokens), 0) AS output_tokens, \
                   COALESCE(SUM(CASE WHEN settled = 1 \
                        THEN cost_usd_micros ELSE 0 END), 0) AS cost_usd_micros, \
                   COALESCE(SUM(CASE WHEN settled = 1 \
                        THEN base_cost_usd_micros ELSE 0 END), 0) AS base_cost_usd_micros, \
                   COALESCE(SUM(CASE WHEN settled = 1 \
                        THEN cost_usd_micros - base_cost_usd_micros ELSE 0 END), 0) AS gross_profit_usd_micros \
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

/// 按模型或按渠道聚合费用/请求；费用统计所有已结算尝试。
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
         COALESCE(SUM(CASE WHEN settled = 1 THEN cost_usd_micros ELSE 0 END), 0) \
           AS cost_usd_micros, \
         COALESCE(SUM(CASE WHEN settled = 1 THEN base_cost_usd_micros ELSE 0 END), 0) \
           AS base_cost_usd_micros, \
         COALESCE(SUM(CASE WHEN settled = 1 \
             THEN cost_usd_micros - base_cost_usd_micros ELSE 0 END), 0) \
           AS gross_profit_usd_micros \
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
            base_cost_usd_micros: row
                .try_get("base_cost_usd_micros")
                .map_err(StoreError::Query)?,
            gross_profit_usd_micros: row
                .try_get("gross_profit_usd_micros")
                .map_err(StoreError::Query)?,
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
    if let Some(discount_bp) = filter.discount_bp {
        push_where_cond(qb, &mut first, "discount_bp = ");
        qb.push_bind(discount_bp);
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
        cache_write_1h_micros: row
            .try_get("cache_write_1h_price_usd_micros")
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
        channel_key: row.try_get("channel_key").map_err(StoreError::Query)?,
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
        cache_write_1h_tokens: row
            .try_get("cache_write_1h_tokens")
            .map_err(StoreError::Query)?,
        price,
        base_cost_usd_micros: row
            .try_get("base_cost_usd_micros")
            .map_err(StoreError::Query)?,
        discount_bp: row.try_get("discount_bp").map_err(StoreError::Query)?,
        cost_usd_micros: row.try_get("cost_usd_micros").map_err(StoreError::Query)?,
        settled: row
            .try_get::<i64, _>("settled")
            .map_err(StoreError::Query)?
            != 0,
        usage_reported: row
            .try_get::<i64, _>("usage_reported")
            .map_err(StoreError::Query)?
            != 0,
        request_id: row.try_get("request_id").map_err(StoreError::Query)?,
        billing_attempt_id: row
            .try_get("billing_attempt_id")
            .map_err(StoreError::Query)?,
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
    use serde_json::json;
    use sqlx::Connection;

    #[test]
    fn token_count_conversion_rejects_sqlite_integer_overflow() {
        assert_eq!(
            persisted_token_count("input_tokens", i64::MAX as u64).expect("i64::MAX 应可持久化"),
            i64::MAX
        );
        assert!(
            matches!(
                persisted_token_count("input_tokens", i64::MAX as u64 + 1),
                Err(StoreError::InvalidResource(_))
            ),
            "超界 token 计数不能回绕成负数"
        );
    }

    #[tokio::test]
    async fn pending_charge_is_visible_to_admission_before_background_settlement() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        seed_token(&mut conn, "sk-a").await;
        initialize_token_settlement(&mut conn, "sk-a", 1_000, resources::ROOT_USER_ID)
            .await
            .expect("应能初始化余额");
        drop(conn);

        let mut log = sample_log(1, false);
        log.cost_usd_micros = 100;
        log.request_body = Some(b"request".to_vec());
        log.response_body = Some(b"response".to_vec());
        enqueue_pending_request_log(
            &pool,
            PendingRequestLog {
                log,
                settlement_error: None,
            },
        )
        .await
        .expect("应能持久化待结算请求");

        let mut conn = pool.acquire().await.expect("应能获取连接");
        let admission = get_admission_snapshot(&mut conn, "sk-a")
            .await
            .expect("应能读取准入快照")
            .expect("令牌应有准入快照");
        assert_eq!(admission.wallet.balance_usd_micros, 900);
        assert_eq!(admission.wallet.settled_usd_micros, 100);
        assert_eq!(admission.token.settled_usd_micros, 100);

        let pending = load_pending_request_logs(&pool, 16)
            .await
            .expect("应能读取待结算请求");
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].log.request_body.as_deref(),
            Some(&b"request"[..])
        );
        assert_eq!(
            pending[0].log.response_body.as_deref(),
            Some(&b"response"[..])
        );
    }

    #[tokio::test]
    async fn orphan_dispatched_attempt_is_rebuilt_into_outbox() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        seed_token(&mut conn, "sk-recovery").await;
        initialize_token_settlement(&mut conn, "sk-recovery", 10_000, 1)
            .await
            .expect("应能初始化余额");
        let mut result = sample_log(1, false);
        result.token_name = "recovery".to_string();
        result.token_key = "sk-recovery".to_string();
        result.channel = "channel".to_string();
        result.channel_key = Some("key-1".to_string());
        result.status_code = 200;
        result.cost_usd_micros = 123;
        result.usage_reported = true;
        result.request_id = Some("request-recovery".to_string());
        result.billing_attempt_id = Some("attempt-recovery".to_string());
        result.request_body = Some(b"original request".to_vec());
        result.response_body = Some(b"original response".to_vec());
        let metadata = serde_json::to_vec(&BillingAttemptRecovery {
            token_name: "recovery".to_string(),
            model: "model".to_string(),
            outbound_model: Some("provider-model".to_string()),
            channel: "channel".to_string(),
            channel_key: Some("key-1".to_string()),
            inbound_protocol: "openai_chat".to_string(),
            started: 1,
            price: PriceSnapshot::default(),
            discount_bp: billing::DEFAULT_DISCOUNT_BP,
            request_body: None,
            result: Some(Box::new(result)),
            result_settlement_error: None,
        })
        .expect("恢复元数据应可编码");
        sqlx::query(
            "INSERT INTO billing_reservations \
             (attempt_id, request_id, token_key, user_id, reserved_cost_usd_micros, \
              token_limit_usd_micros, recovery_metadata, status, dispatched, result_persisted, \
              created_at, updated_at) \
             VALUES ('attempt-recovery', 'request-recovery', 'sk-recovery', 1, 123, NULL, ?, \
                     'reserved', 1, 0, 0, 0)",
        )
        .bind(metadata)
        .execute(&mut *conn)
        .await
        .expect("应能写入模拟遗留预留");
        drop(conn);

        assert_eq!(
            recover_orphan_billing_attempts(&pool, Duration::ZERO, 16)
                .await
                .expect("恢复任务应成功"),
            1
        );
        let pending = load_pending_request_logs(&pool, 16)
            .await
            .expect("应能读取恢复记录");
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].log.billing_attempt_id.as_deref(),
            Some("attempt-recovery")
        );
        assert_eq!(pending[0].log.cost_usd_micros, 123);
        assert_eq!(pending[0].log.status_code, 200);
        assert_eq!(
            pending[0].log.request_body.as_deref(),
            Some(&b"original request"[..])
        );
        assert_eq!(
            pending[0].log.response_body.as_deref(),
            Some(&b"original response"[..])
        );
        assert!(pending[0].log.usage_reported);
        let recovery_metadata: Vec<u8> = sqlx::query_scalar(
            "SELECT recovery_metadata FROM billing_reservations WHERE attempt_id = 'attempt-recovery'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取恢复元数据");
        let recovery: BillingAttemptRecovery =
            serde_json::from_slice(&recovery_metadata).expect("恢复元数据应保持可解析");
        assert!(recovery.result.is_none(), "结果进入 outbox 后不应重复保留");
        assert!(
            recovery.request_body.is_none(),
            "请求体进入 outbox 后不应重复保留"
        );
        isolate_pending_request_log(&pool, pending[0].log.id, "需要人工复核", None)
            .await
            .expect("应能隔离记录");
        let isolated = query_isolated_request_logs_scoped(&pool, 16, true)
            .await
            .expect("应能查询隔离记录");
        assert_eq!(isolated.len(), 1);
        assert_eq!(
            isolated[0].billing_attempt_id.as_deref(),
            Some("attempt-recovery")
        );
        assert_eq!(
            isolated[0].request_body.as_deref(),
            Some(&b"original request"[..])
        );
        assert_eq!(
            isolated[0].response_body.as_deref(),
            Some(&b"original response"[..])
        );
        sqlx::query("UPDATE user_balance SET balance_usd_micros = 0 WHERE user_id = 1")
            .execute(&pool)
            .await
            .expect("应能暂时收紧用户余额");
        assert!(matches!(
            requeue_isolated_request_log(&pool, "attempt-recovery", true).await,
            Err(StoreError::InsufficientFunds)
        ));
        let state: String = sqlx::query_scalar(
            "SELECT state FROM request_log_outbox WHERE billing_attempt_id = 'attempt-recovery'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取回滚后的队列状态");
        assert_eq!(state, "isolated");
        sqlx::query("UPDATE user_balance SET balance_usd_micros = 5_000_000 WHERE user_id = 1")
            .execute(&pool)
            .await
            .expect("应能恢复用户余额");
        sqlx::query("UPDATE tokens SET limit_usd_micros = 100 WHERE token_key = 'sk-recovery'")
            .execute(&pool)
            .await
            .expect("应能收紧令牌累计上限");
        assert!(matches!(
            requeue_isolated_request_log(&pool, "attempt-recovery", true).await,
            Err(StoreError::TokenLimitExceeded)
        ));
        sqlx::query("UPDATE tokens SET limit_usd_micros = 1_000 WHERE token_key = 'sk-recovery'")
            .execute(&pool)
            .await
            .expect("应能放宽令牌累计上限");
        assert!(matches!(
            requeue_isolated_request_log(&pool, "attempt-recovery", true).await,
            Ok(IsolatedReplayAction::Requeued)
        ));
        let state: String = sqlx::query_scalar(
            "SELECT state FROM request_log_outbox WHERE billing_attempt_id = 'attempt-recovery'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取重放状态");
        assert_eq!(state, "queued");
        let reservation_status: String = sqlx::query_scalar(
            "SELECT status FROM billing_reservations WHERE attempt_id = 'attempt-recovery'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取重放预留状态");
        assert_eq!(reservation_status, "reserved");
        let persisted: i64 = sqlx::query_scalar(
            "SELECT result_persisted FROM billing_reservations WHERE attempt_id = 'attempt-recovery'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取恢复状态");
        assert_eq!(persisted, 1);
        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("应能开启结算事务");
        settle_billing_attempt(&mut tx, "attempt-recovery", 123)
            .await
            .expect("首次重放应能结算");
        tx.commit().await.expect("应能提交首次结算");
        let balance_after_first: i64 =
            sqlx::query_scalar("SELECT balance_usd_micros FROM user_balance WHERE user_id = 1")
                .fetch_one(&pool)
                .await
                .expect("应能读取首次结算余额");
        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("应能开启幂等结算事务");
        settle_billing_attempt(&mut tx, "attempt-recovery", 123)
            .await
            .expect("重复重放应保持幂等");
        tx.commit().await.expect("应能提交幂等结算");
        let balance_after_second: i64 =
            sqlx::query_scalar("SELECT balance_usd_micros FROM user_balance WHERE user_id = 1")
                .fetch_one(&pool)
                .await
                .expect("应能读取重复结算余额");
        assert_eq!(
            balance_after_second, balance_after_first,
            "同一计费尝试重复结算不得重复扣款"
        );
    }

    #[tokio::test]
    async fn orphan_undispatched_attempt_is_released() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        seed_token(&mut conn, "sk-release").await;
        initialize_token_settlement(&mut conn, "sk-release", 10_000, 1)
            .await
            .expect("应能初始化余额");
        let metadata = serde_json::to_vec(&BillingAttemptRecovery {
            token_name: "release".to_string(),
            model: "model".to_string(),
            outbound_model: None,
            channel: "channel".to_string(),
            channel_key: None,
            inbound_protocol: "openai_chat".to_string(),
            started: 1,
            price: PriceSnapshot::default(),
            discount_bp: billing::DEFAULT_DISCOUNT_BP,
            request_body: None,
            result: None,
            result_settlement_error: None,
        })
        .expect("恢复元数据应可编码");
        sqlx::query(
            "INSERT INTO billing_reservations \
             (attempt_id, request_id, token_key, user_id, reserved_cost_usd_micros, \
              token_limit_usd_micros, recovery_metadata, status, dispatched, result_persisted, \
              created_at, updated_at) \
             VALUES ('attempt-release', 'request-release', 'sk-release', 1, 123, NULL, ?, \
                     'reserved', 0, 0, 0, 0)",
        )
        .bind(metadata)
        .execute(&mut *conn)
        .await
        .expect("应能写入模拟遗留预留");
        drop(conn);

        assert_eq!(
            recover_orphan_billing_attempts(&pool, Duration::ZERO, 16)
                .await
                .expect("恢复任务应成功"),
            1
        );
        let status: String = sqlx::query_scalar(
            "SELECT status FROM billing_reservations WHERE attempt_id = 'attempt-release'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取释放状态");
        assert_eq!(status, "released");
        let pending = load_pending_request_logs(&pool, 16)
            .await
            .expect("应能读取待结算请求");
        assert!(pending.is_empty());
    }

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

        let root_plan: Option<i64> = sqlx::query_scalar("SELECT plan_id FROM users WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("应能读 root 套餐");
        assert_eq!(root_plan, None, "root 不挂套餐");

        let standard_group: String =
            sqlx::query_scalar("SELECT group_name FROM plan_model_groups WHERE plan_id = 1")
                .fetch_one(&pool)
                .await
                .expect("standard 应含 default 组");
        assert_eq!(standard_group, "default");

        let builtin_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plans WHERE builtin = 1")
            .fetch_one(&pool)
            .await
            .expect("应能数内置套餐");
        assert_eq!(builtin_count, 2, "内置两档应已播种");

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
        initialize_token_settlement(&mut conn, "sk-a", 5_000_000, 1)
            .await
            .expect("应能初始化 a");
        initialize_token_settlement(&mut conn, "sk-b", 0, 1)
            .await
            .expect("应能初始化 b");

        settle_charge(&mut conn, "sk-a", 1_000_000)
            .await
            .expect("应能结算");

        let a = get_admission_snapshot(&mut conn, "sk-a")
            .await
            .expect("应能读")
            .expect("a 应有视图");
        let b = get_admission_snapshot(&mut conn, "sk-b")
            .await
            .expect("应能读")
            .expect("b 应有视图");
        assert_eq!(a.wallet.balance_usd_micros, 4_000_000);
        assert_eq!(b.wallet.balance_usd_micros, 4_000_000);
        assert_eq!(a.token.settled_usd_micros, 1_000_000);
        assert_eq!(b.token.settled_usd_micros, 0);
    }

    /// 钱包相对调整：充值/扣减同一原语，只动剩余、不动累计结算额。
    #[tokio::test]
    async fn adjust_user_balance_recharges_and_deducts() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        seed_token(&mut conn, "sk-a").await;
        initialize_token_settlement(&mut conn, "sk-a", 10_000_000, 1)
            .await
            .expect("应能初始化余额");

        let change = adjust_user_balance(&mut conn, resources::ROOT_USER_ID, 5_000_000)
            .await
            .expect("应能充值");
        assert_eq!(change.before_usd_micros, 10_000_000);
        assert_eq!(change.after_usd_micros, 15_000_000);
        assert_eq!(change.settled_usd_micros, 0, "调账不动累计结算额");

        let change = adjust_user_balance(&mut conn, resources::ROOT_USER_ID, -3_000_000)
            .await
            .expect("应能扣减");
        assert_eq!(change.before_usd_micros, 15_000_000);
        assert_eq!(change.after_usd_micros, 12_000_000);
        assert_eq!(change.settled_usd_micros, 0);

        // 令牌视图读到的剩余就是所属用户的钱包。
        let view = get_admission_snapshot(&mut conn, "sk-a")
            .await
            .expect("应能读")
            .expect("应有视图");
        assert_eq!(view.wallet.balance_usd_micros, 12_000_000);
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
                keys: vec![resources::ChannelKey {
                    name: "default".to_string(),
                    api_key: "sk".to_string(),
                    weight: 1,
                    enabled: true,
                    models: None,
                    blocked_models: None,
                }],
                models: vec![],
                model_aliases: std::collections::HashMap::new(),
                timeout_ms: 1000,
                max_retries: 0,
                enabled: true,
                model_group: crate::store::resources::DEFAULT_MODEL_GROUP.to_string(),
                reasoning_output: Default::default(),
                session_cache_key: Default::default(),
                injects_cache_breakpoints: false,
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
                "plans",
                "INSERT INTO plans (id, internal_name, display_name, note, note_visible_to_admin, \
                     discount_bp, default_rpm, shared_rpm, initial_grant_usd_micros, \
                     capabilities_json, shared_with_admin, builtin, created_at) \
                 VALUES ('not-a-number', 'x', 'X', '', 0, 10000, NULL, NULL, 0, '{}', 0, 0, 0)",
            ),
            (
                "plan_model_groups",
                "INSERT INTO plan_model_groups (plan_id, group_name) VALUES ('not-a-number', 'default')",
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
                "request_log_outbox",
                "INSERT INTO request_log_outbox \
                     (id, token_key, user_id, cost_usd_micros, metadata) \
                 VALUES ('not-a-number', 'k', 1, 0, x'00')",
            ),
            (
                "channels",
                "INSERT INTO channels (name, protocol, base_url, models_json, \
                     model_aliases_json, timeout_ms, max_retries) \
                 VALUES ('c', 'openai_chat', 'u', '[]', '{}', 'not-a-number', 1)",
            ),
            (
                "channel_keys",
                "INSERT INTO channel_keys (channel_id, name, api_key, weight, enabled, created_at) \
                 VALUES (?, 'k', 'secret', 'not-a-number', 1, 0)",
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
            let result = if table == "channel_keys" {
                sqlx::query(sql).bind(channel_id).execute(&pool).await
            } else {
                sqlx::query(sql).execute(&pool).await
            };
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

        assert!(
            sqlx::query(
                "INSERT INTO channel_model_order (model, channel_id, position) \
                 VALUES ('m', ?, 'not-a-number')",
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .is_err(),
            "channel_model_order 应仍是 STRICT 表，错类型写入须被拒"
        );
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
        initialize_token_settlement(&mut conn, "sk-a", 5_000_000, 1)
            .await
            .expect("应能初始化余额");
        sqlx::query("DELETE FROM tokens WHERE token_key = ?")
            .bind("sk-a")
            .execute(&mut *conn)
            .await
            .expect("应能删令牌");
        let balance = get_admission_snapshot(&mut conn, "sk-a")
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

        let balance = get_admission_snapshot(&mut conn, "sk-orphan")
            .await
            .expect("应能查余额");
        assert!(balance.is_none(), "孤儿余额行应被迁移清理");
        let balance = get_admission_snapshot(&mut conn, "sk-live")
            .await
            .expect("应能查余额")
            .expect("存量令牌应能读到用户钱包");
        assert_eq!(
            balance.wallet.balance_usd_micros, 1_500_000,
            "root 钱包应为各令牌剩余之和"
        );
        assert_eq!(balance.token.settled_usd_micros, 200, "令牌 settled 应保留");
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
            cache_write_1h_micros: 0,
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
                    channel_key: None,
                    channel: "c1".to_string(),
                    status_code: 200,
                    latency_ms: 10,
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    cache_write_1h_tokens: 0,
                    usage_reported: false,
                    price,
                    cost_usd_micros: i as i64,
                    base_cost_usd_micros: 0,
                    discount_bp: 10_000,
                    settled: true,
                    request_id: None,
                    billing_attempt_id: None,
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
            cache_write_1h_micros: 0,
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
                    channel_key: None,
                    channel: (*channel).to_string(),
                    status_code: 200,
                    latency_ms: 10,
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    cache_write_1h_tokens: 0,
                    usage_reported: false,
                    price,
                    cost_usd_micros: 0,
                    base_cost_usd_micros: 0,
                    discount_bp: 10_000,
                    settled: true,
                    request_id: None,
                    billing_attempt_id: None,
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
            cache_write_1h_micros: 0,
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
                    channel_key: None,
                    channel: (*channel).to_string(),
                    status_code: 200,
                    latency_ms: 10,
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    cache_write_1h_tokens: 0,
                    usage_reported: false,
                    price,
                    cost_usd_micros: 0,
                    base_cost_usd_micros: 0,
                    discount_bp: 10_000,
                    settled: true,
                    request_id: None,
                    billing_attempt_id: None,
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
            cache_write_1h_micros: 0,
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
                channel_key: None,
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 10,
                output_tokens: 10,
                cache_read_tokens: 1_000,
                cache_write_tokens: 0,
                cache_write_1h_tokens: 0,
                usage_reported: false,
                price,
                cost_usd_micros: 0,
                base_cost_usd_micros: 0,
                discount_bp: 10_000,
                settled: true,
                request_id: None,
                billing_attempt_id: None,
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
                channel_key: None,
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 20,
                output_tokens: 20,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_write_1h_tokens: 0,
                usage_reported: false,
                price,
                cost_usd_micros: 0,
                base_cost_usd_micros: 0,
                discount_bp: 10_000,
                settled: true,
                request_id: None,
                billing_attempt_id: None,
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
            cache_write_1h_micros: 0,
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
                channel_key: None,
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_write_1h_tokens: 0,
                usage_reported: false,
                price,
                cost_usd_micros: 12,
                base_cost_usd_micros: 0,
                discount_bp: 10_000,
                settled: true,
                request_id: None,
                billing_attempt_id: None,
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
                channel_key: None,
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_write_1h_tokens: 0,
                usage_reported: false,
                price: PriceSnapshot::default(),
                cost_usd_micros: 0,
                base_cost_usd_micros: 0,
                discount_bp: 10_000,
                settled: true,
                request_id: None,
                billing_attempt_id: None,
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

    /// 请求日志过滤列有索引；未结算费用不进入聚合，已结算失败费用仍计入。
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
            "idx_request_log_user_created_at",
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
                channel_key: None,
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_write_1h_tokens: 0,
                usage_reported: false,
                price,
                cost_usd_micros: 9_999,
                base_cost_usd_micros: 0,
                discount_bp: 10_000,
                settled: false,
                request_id: None,
                billing_attempt_id: None,
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
                channel_key: None,
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 10,
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_write_1h_tokens: 0,
                usage_reported: false,
                price,
                cost_usd_micros: 100,
                base_cost_usd_micros: 0,
                discount_bp: 10_000,
                settled: true,
                request_id: None,
                billing_attempt_id: None,
                request_body: None,
                response_body: None,
            },
        )
        .await
        .expect("应能写已结算日志");
        let mut failed = sample_log(3, true);
        failed.status_code = 503;
        failed.cost_usd_micros = 50;
        insert_request_log(&pool, &failed)
            .await
            .expect("应能写已结算失败日志");

        let lifetime = query_lifetime_stats(&pool, None).await.expect("应能聚合");
        assert_eq!(lifetime.cost_usd_micros, 150, "已结算失败费用也应计入");
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
            channel_key: None,
            channel: "c".to_string(),
            status_code: 200,
            latency_ms: 1,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_write_1h_tokens: 0,
            usage_reported: false,
            price: PriceSnapshot::default(),
            cost_usd_micros: 1,
            base_cost_usd_micros: 0,
            discount_bp: 10_000,
            settled,
            request_id: None,
            billing_attempt_id: None,
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

    /// 主库文件读取失败必须显式报错，不能把路径错误伪装成零字节占用。
    #[tokio::test]
    async fn log_store_stats_reports_database_metadata_errors() {
        let (dir, pool) = test_pool().await;
        let missing_path = dir.path().join("missing.db");
        let err = log_store_stats(&pool, &missing_path)
            .await
            .expect_err("主库 metadata 失败应向上返回");
        assert!(matches!(
            err,
            StoreError::FileMetadata { path, source }
                if path == missing_path && source.kind() == std::io::ErrorKind::NotFound
        ));
    }

    /// 活动读事务会令 SQLite 返回 busy=1；checkpoint 辅助必须检查结果行，不能只
    /// 看 SQL 是否报错，否则调用方会误以为 WAL 已经截断。
    #[tokio::test]
    async fn checkpoint_wal_truncate_reports_busy_reader() {
        let (_dir, pool) = test_pool().await;
        let mut reader = pool.acquire().await.expect("应能取得读连接");
        let mut read_tx = reader.begin().await.expect("应能开启读事务");
        let _: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM smoke_probe")
            .fetch_one(&mut *read_tx)
            .await
            .expect("应能建立读快照");

        insert_smoke(&pool, "checkpoint-busy")
            .await
            .expect("应能追加 WAL");
        let err = checkpoint_wal_truncate(&pool)
            .await
            .expect_err("活动读事务应报告 busy");
        assert!(
            matches!(err, StoreError::WalCheckpointBusy { log_frames, checkpointed_frames }
            if log_frames > checkpointed_frames)
        );

        read_tx.rollback().await.expect("应能结束读事务");
        checkpoint_wal_truncate(&pool)
            .await
            .expect("读事务结束后应能完成 checkpoint");
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

    #[tokio::test]
    async fn structured_system_log_event_roundtrips_and_legacy_rows_fallback() {
        let (_dir, pool) = test_pool().await;
        let event = SystemLogEvent::new(
            "billing.user_balance_adjusted",
            json!({ "user_id": 42, "delta_usd_micros": 1_000_000 }),
            "用户 42 余额 +$1.00",
        );
        let mut tx = pool.begin().await.expect("应能开启事务");
        record_audit(
            &mut tx,
            Actor {
                user_id: 1,
                email: "root@example.com",
            },
            "billing",
            &event,
        )
        .await
        .expect("结构化事件应能写入");
        tx.commit().await.expect("应能提交事务");

        insert_system_log(&pool, "error", "catalog", "旧式日志")
            .await
            .expect("旧式日志应能写入");
        let page = query_system_log_page(&pool, &SystemLogQuery::new(1, 10))
            .await
            .expect("应能查询系统日志");
        let structured = page
            .items
            .iter()
            .find(|item| item.event_code.as_deref() == Some("billing.user_balance_adjusted"))
            .expect("应取回事件编码");
        assert_eq!(
            structured.event_params,
            Some(json!({
                "user_id": 42,
                "delta_usd_micros": 1_000_000
            }))
        );
        let legacy = page
            .items
            .iter()
            .find(|item| item.message == "旧式日志")
            .expect("应取回旧式日志");
        assert!(legacy.event_code.is_none());
        assert!(legacy.event_params.is_none());

        let malformed = sqlx::query(
            "INSERT INTO system_log \
             (created_at, level, target, message, event_code, event_params) \
             VALUES (0, 'info', 'test', 'fallback', 'test.invalid', 'not-json')",
        )
        .execute(&pool)
        .await;
        assert!(malformed.is_err(), "事件参数必须是合法 JSON");
    }
}
