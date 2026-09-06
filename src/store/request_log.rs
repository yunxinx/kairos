//! 请求日志域：落库、待结算队列、隔离重放、查询与统计。
//!
//! `RequestLog` 持久化与行校验；未即时结算的日志经
//! `request_log_outbox` 队列化（含隔离与人工重放）；管理面
//! 分页/过滤查询、`/stats` 与 `/stats/lifetime` 聚合、已结算
//! 日志批量清理都在此实现。时间窗夹取与分页沿用 [`super`] 的共享辅助。

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, Row, SqliteConnection, SqlitePool};

use super::settlement::{BillingAttemptRecovery, apply_charge, release_billing_attempt_on};
use super::{
    SortDir, StoreError, SystemLogEvent, as_count, clamp_page, ids, like_substring_pattern,
    push_column_in, push_created_at_range, push_limit_offset, push_where_cond, record_system_error,
};
use crate::core::billing::PriceSnapshot;

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
    /// 该行是否对应已实际派发上游的尝试。
    ///
    /// `false` 表示「未出站即终局」：请求在建立任何上游连接前就被网关终止
    ///（准入前置失败、全部渠道冷却 / 无可用密钥、本地计费拒绝、出站安全
    /// 策略拒绝等）。这类行零费用、无渠道归属，统计侧单列计数，不并入
    /// 出站请求口径。存量行与旧 outbox 元数据缺该字段时按已派发处理。
    #[serde(default = "default_dispatched")]
    pub dispatched: bool,
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
    /// 出站尝试是否确认到达上游；usage 缺失告警据此区分「上游未达」与
    /// 「可能已产生费用」两类。存量队列行缺该字段时按未知处理，归入
    /// 可能已产生费用一侧。
    #[serde(default = "default_upstream_reached")]
    pub(crate) upstream_reached: bool,
}

/// `upstream_reached` 缺省值：未知可达性按「可能已产生费用」告警。
pub(crate) fn default_upstream_reached() -> bool {
    true
}

/// `dispatched` 缺省值：存量行与旧元数据按已派发处理，统计口径不回溯改变。
fn default_dispatched() -> bool {
    true
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
          settled, usage_reported, request_id, billing_attempt_id, request_body, response_body, \
          dispatched) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
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
    .bind(log.dispatched as i64)
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
/// 把一次出站尝试的结果持久化到结算队列；带计费身份的结果与预留状态变更
/// （结果标记、缺 usage 的释放）在同一 `BEGIN IMMEDIATE` 事务内原子完成。
///
/// 原子性是防漏账的关键：事务未提交时预留仍是「无结果的 reserved」，由恢复
/// 任务按孤儿释放；事务提交后结果、结果标记与预留处置同时生效——不存在
/// 「结果已生成但未入队」的中间态，预留行也无需保留结果载荷副本。
/// 幂等性由 outbox 对 `billing_attempt_id` 的唯一键承担：重复投递同一结果
/// 逐字段比对放行，携带不同结果则报冲突，绝不静默覆盖。
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
    // 带计费身份的结果只能落在仍待结果的预留上：预留已终结说明结果曾被
    // 消费（恢复重建或重复投递），继续入队结算会造成重复计费——显式报错，
    // 由调用方保留原始结果交人工复核。
    if let Some(attempt_id) = pending.log.billing_attempt_id.as_deref() {
        let live: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM billing_reservations \
             WHERE attempt_id = ? AND status = 'reserved'",
        )
        .bind(attempt_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
        if live.is_none() {
            return Err(StoreError::InvalidResource(format!(
                "费用预留 {attempt_id} 不存在或已终止"
            )));
        }
    }
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
        // 结果已随本事务进入 outbox：置位标记。载荷副本从未写入预留行，
        // 无需清理。
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);
        let updated = sqlx::query(
            "UPDATE billing_reservations SET result_persisted = 1, updated_at = ? \
             WHERE attempt_id = ? AND status = 'reserved'",
        )
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
        // 入队时已结算且带计费身份的日志表示该尝试以「释放预留」结束：
        // 结果缺失上游 usage 时不产生费用，预留归还与对账日志入队必须在
        // 同一事务内成立，否则崩溃后要么额度滞留、要么日志缺失。回报
        // usage 的尝试保持预留，由后台按实际用量结算。
        if pending.log.settled && pending.settlement_error.is_none() {
            release_billing_attempt_on(&mut tx, attempt_id, now).await?;
        }
    }
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(persisted_id)
}

/// 标记结果已进入 outbox，并清理预留行中的完整结果副本。
///
/// 常规路径的结果随入队事务原子生效，预留行从不持有载荷副本；本函数仅供
/// 崩溃恢复的重建路径使用——存量库中「旧两步写入在崩溃窗口留下的载荷副本」
/// 重建入队后，由此清理解放。元数据损坏时不覆盖原始 BLOB，只更新状态位并
/// 交由隔离记录保留原文。
pub(crate) async fn mark_billing_attempt_result_persisted(
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

pub(crate) async fn clear_billing_attempt_recovery_payload_by_id(
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
         settled, usage_reported, request_id, billing_attempt_id, dispatched FROM request_log",
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
         settled, usage_reported, request_id, billing_attempt_id, dispatched, request_body, response_body \
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
pub(crate) const LOG_PURGE_BATCH_ROWS: u64 = 5_000;

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
///
/// 除 `not_dispatched` 外的全部指标只统计已派发出站的行（`dispatched = 1`），
/// 维持既有出站请求口径；`not_dispatched` 单列统计「未出站即终局」的请求数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsSummary {
    pub request_count: u64,
    pub success_count: u64,
    /// 未出站即终局的请求数：全部渠道冷却 / 无可用密钥、本地计费拒绝、出站
    /// 安全策略拒绝与准入前置失败等，从未建立上游连接。
    pub not_dispatched: u64,
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
/// 口径：`request_count` 只统计已派发出站的行，按 `request_id` 去重（存量无
/// id 的行回退到主键），表示实际打到上游的请求数；未出站即终局的请求不并入
///（在 `/stats` 的 `not_dispatched` 单列可见）。`total_tokens` 含全部已派发
/// 请求日志行（含未结算），`cost_usd_micros` 统计所有已结算出站尝试（包括
/// 失败尝试）。并列展示时不要把 token 合计当成已入账费用的用量。
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
/// 出站口径的指标只统计 `dispatched = 1` 的行；`not_dispatched` 单列统计
/// 未出站即终局的请求数，二者互不并入。
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
         FROM request_log WHERE created_at >= ? AND dispatched = 1{}",
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

    // 未出站即终局的请求数与出站口径分列统计：同一时间窗、同一归属范围。
    let not_dispatched_sql = format!(
        "SELECT COUNT(DISTINCT COALESCE(request_id, CAST(id AS TEXT))) AS not_dispatched \
         FROM request_log WHERE created_at >= ? AND dispatched = 0{}",
        user_scope_clause(user_id)
    );
    let mut not_dispatched_query =
        sqlx::query(AssertSqlSafe(not_dispatched_sql)).bind(from_created_at);
    if let Some(user_id) = user_id {
        not_dispatched_query = not_dispatched_query.bind(user_id);
    }
    let not_dispatched_row = not_dispatched_query
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
        not_dispatched: as_count(
            not_dispatched_row
                .try_get("not_dispatched")
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
/// 与 `/stats` 同口径：只统计 `dispatched = 1` 的行；未出站即终局的请求
/// 不进入本聚合。
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
         FROM request_log WHERE dispatched = 1{}",
        user_scope_clause(user_id)
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
            FROM request_log WHERE created_at >= ? AND dispatched = 1{} \
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
            FROM request_log WHERE created_at >= ? AND dispatched = 1{} \
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
         FROM request_log WHERE created_at >= ? AND dispatched = 1{} \
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
        dispatched: row
            .try_get::<i64, _>("dispatched")
            .map_err(StoreError::Query)?
            != 0,
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
    use crate::store::resources;
    use crate::store::test_support::{sample_log, test_pool};

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

    /// 用户统计聚合命中覆盖索引：index-only 扫描且无 GROUP BY 临时排序，
    /// 用户页统计不再退化为整表扫描。
    #[tokio::test]
    async fn user_stats_aggregate_uses_covering_index() {
        let (_dir, pool) = test_pool().await;
        let plan: Vec<String> = sqlx::query(
            "EXPLAIN QUERY PLAN SELECT user_id, \
                    COUNT(DISTINCT COALESCE(request_id, CAST(id AS TEXT))) AS request_count, \
                    COALESCE(SUM(input_tokens), 0) AS input_tokens, \
                    COALESCE(SUM(output_tokens), 0) AS output_tokens \
             FROM request_log GROUP BY user_id",
        )
        .fetch_all(&pool)
        .await
        .expect("应能读取查询计划")
        .into_iter()
        .map(|row| row.try_get::<String, _>("detail").expect("应有 detail 列"))
        .collect();
        assert!(
            plan.iter().any(|detail| detail.contains("COVERING INDEX")),
            "统计聚合应走覆盖索引，实际计划: {plan:?}"
        );
        // count(DISTINCT) 的逐组去重缓冲是聚合固有的；禁止的是 GROUP BY 的
        // 临时排序（应由索引前缀序消解）。
        assert!(
            !plan
                .iter()
                .any(|detail| detail.contains("TEMP B-TREE FOR GROUP BY")),
            "GROUP BY 不应引入临时排序，实际计划: {plan:?}"
        );
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
                    dispatched: true,
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
                    dispatched: true,
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
                    dispatched: true,
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
                dispatched: true,
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
                dispatched: true,
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
                dispatched: true,
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
                dispatched: true,
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
                dispatched: true,
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
                dispatched: true,
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
}
