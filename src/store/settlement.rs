//! 结算域：出站计费预留生命周期、令牌累计结算与用户钱包。
//!
//! 请求出站前按预估费用预留（`billing_reservations`，携带恢复元数据），
//! 结果落库后原子结算进用户钱包与令牌累计；崩溃后的孤儿预留由恢复
//! 任务重建或释放。金额一律整数 micro-USD，钱包可为负（在途透支）。

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection, SqlitePool};

use super::request_log::{
    LOG_PURGE_BATCH_ROWS, PendingRequestLog, RequestLog,
    clear_billing_attempt_recovery_payload_by_id, default_upstream_reached,
    mark_billing_attempt_result_persisted,
};
use super::{StoreError, SystemLogEvent, ids, record_system_error};
use crate::core::billing::{self, PriceSnapshot};

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
    /// 新写入路径的结果随入队事务原子生效，本字段只承载旧两步写入在崩溃
    /// 窗口留下的存量中间态，终态行清理后自然消失。
    #[serde(default)]
    pub result: Option<Box<RequestLog>>,
    /// 费用计算错误的稳定文本；存在时恢复记录必须保持未结算。
    #[serde(default)]
    pub result_settlement_error: Option<String>,
    /// 出站尝试是否确认到达上游；恢复重建的日志据此区分 usage 缺失告警
    /// 的两类事件码。存量元数据缺该字段时按未知（可能已产生费用）处理。
    #[serde(default = "default_upstream_reached")]
    pub upstream_reached: bool,
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

/// 标记预留已经进入实际出站调用阶段。
///
/// 进入出站后预留不再被无条件释放：合法出口只有两条——按上游明确回报的
/// usage 结算，或结果缺失 usage（含连接未建立）时随结果入队释放。恢复
/// 任务据此把 dispatched 且无结果的预留视为未知费用并按释放处理。
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

/// 释放预留，归还准入时冻结的额度；幂等（已释放、已结算或不存在时无操作）。
///
/// 未派发的尝试与已派发但确认无上游费用的尝试（连接未建立、结果缺失
/// usage）都以释放结束：`status='reserved'` 即可释放，`dispatched` 不再
/// 限制出口。已派发尝试的释放由结果入队路径触发，保证「日志照落」与
/// 「额度归还」一致。
pub async fn release_billing_attempt(
    pool: &SqlitePool,
    attempt_id: &str,
) -> Result<(), StoreError> {
    let mut conn = pool.acquire().await.map_err(StoreError::Query)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    release_billing_attempt_on(&mut conn, attempt_id, now).await
}

/// 在已有连接/事务上释放预留，供结果入队与崩溃恢复在同一事务内完成。
pub(crate) async fn release_billing_attempt_on(
    conn: &mut SqliteConnection,
    attempt_id: &str,
    now: i64,
) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE billing_reservations SET status = 'released', updated_at = ? \
         WHERE attempt_id = ? AND status = 'reserved'",
    )
    .bind(now)
    .bind(attempt_id)
    .execute(conn)
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// 长流存活心跳：把仍在等待结果的预留行 `updated_at` 推进到当前时刻。
///
/// 流式任务的上游消费不受请求总时限约束，可能远超恢复任务的孤儿阈值；心跳
/// 让恢复扫描持续视为新鲜，避免把仍在消费中的长流误判为孤儿而释放预留。
/// 幂等：行已离开「reserved 且无持久化结果」状态（已结算/已释放/结果已入队）
/// 时不改动并返回 `false`，调用方可据此停止心跳。
pub async fn touch_billing_attempt_heartbeat(
    pool: &SqlitePool,
    attempt_id: &str,
) -> Result<bool, StoreError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let updated = sqlx::query(
        "UPDATE billing_reservations SET updated_at = ? \
         WHERE attempt_id = ? AND status = 'reserved' AND result_persisted = 0",
    )
    .bind(now)
    .bind(attempt_id)
    .execute(pool)
    .await
    .map_err(StoreError::Query)?;
    Ok(updated.rows_affected() > 0)
}

/// 扫描进程崩溃留下的预留：尚未进入出站阶段的释放，已进入出站且无结果的
/// 按释放处理（费用未知交人工对账）。带明确 usage 的完整结果仍重建入队，
/// 由后台按实际用量结算。所有动作都在各自的短写事务中完成，和正常入队
/// 共享同一唯一键。
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
                            upstream_reached: true,
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
            upstream_reached,
        } = recovery;
        let (log, settlement_error) = if let Some(result) = result {
            (*result, result_settlement_error.or(metadata_error.clone()))
        } else {
            // 出站结果未知：无法确认上游是否产生费用，因此不结算。以零费用
            // 已结算的日志记录事实，预留释放、告警交人工对账。
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
                base_cost_usd_micros: 0,
                discount_bp,
                cost_usd_micros: 0,
                settled: true,
                request_id: Some(request_id),
                billing_attempt_id: Some(attempt_id.clone()),
                // 能进入恢复的预留都已标记 dispatched（未派发的在更早分支释放）。
                dispatched: true,
                request_body,
                response_body: None,
            };
            (log, result_settlement_error)
        };
        let mut pending = PendingRequestLog {
            log,
            settlement_error,
            upstream_reached,
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
        // 与结果入队同一事务内释放：结果缺失 usage 的已派发尝试不产生费用，
        // 交给人工对账；带结算错误或明确 usage 的记录保持预留原状。
        if pending.log.settled && pending.settlement_error.is_none() {
            release_billing_attempt_on(&mut tx, &attempt_id, now).await?;
        }
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
pub(crate) async fn apply_charge(
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

/// 终态预留行的保留期：对账以 request_log（`billing_attempt_id` 关联）为准，
/// 预留行在结算/释放后只余即时复核的冗余价值，保留 7 天。
pub const BILLING_RESERVATION_RETENTION_MILLIS: i64 = 7 * 24 * 3_600_000;

/// 分批删除早于截止时刻的终态预留行（已结算/已释放）；`reserved` 与带未消费
/// 结果的行永不触碰，崩溃恢复语义不变。单批上限与请求日志清理一致，避免一次
/// 长写事务挤占 `busy_timeout`。
pub async fn purge_terminal_billing_reservations_before(
    pool: &SqlitePool,
    cutoff_updated_at: i64,
) -> Result<u64, StoreError> {
    let mut removed = 0u64;
    loop {
        let result = sqlx::query(
            "DELETE FROM billing_reservations WHERE attempt_id IN ( \
                SELECT attempt_id FROM billing_reservations \
                WHERE status IN ('settled', 'released') AND updated_at < ? \
                LIMIT ?)",
        )
        .bind(cutoff_updated_at)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::request_log::{
        IsolatedReplayAction, enqueue_pending_request_log, isolate_pending_request_log,
        load_pending_request_logs, query_isolated_request_logs_scoped,
        requeue_isolated_request_log,
    };
    use crate::store::resources;
    use crate::store::test_support::{sample_log, seed_token, test_pool};

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
                upstream_reached: true,
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
            upstream_reached: true,
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
            upstream_reached: true,
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

    /// 已派发但崩溃时没有结果：恢复任务不结算，释放预留并入队零费用已结算
    /// 的日志行，钱包不动。
    #[tokio::test]
    async fn orphan_dispatched_attempt_without_result_is_released() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        seed_token(&mut conn, "sk-orphan-dispatched").await;
        initialize_token_settlement(&mut conn, "sk-orphan-dispatched", 10_000, 1)
            .await
            .expect("应能初始化余额");
        let metadata = serde_json::to_vec(&BillingAttemptRecovery {
            token_name: "orphan".to_string(),
            model: "model".to_string(),
            outbound_model: None,
            channel: "channel".to_string(),
            channel_key: Some("key-1".to_string()),
            inbound_protocol: "openai_chat".to_string(),
            started: 1,
            price: PriceSnapshot::default(),
            discount_bp: billing::DEFAULT_DISCOUNT_BP,
            request_body: None,
            result: None,
            result_settlement_error: None,
            upstream_reached: true,
        })
        .expect("恢复元数据应可编码");
        sqlx::query(
            "INSERT INTO billing_reservations \
             (attempt_id, request_id, token_key, user_id, reserved_cost_usd_micros, \
              token_limit_usd_micros, recovery_metadata, status, dispatched, result_persisted, \
              created_at, updated_at) \
             VALUES ('attempt-orphan-dispatched', 'request-orphan-dispatched', \
                     'sk-orphan-dispatched', 1, 123, NULL, ?, 'reserved', 1, 0, 0, 0)",
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
            "SELECT status FROM billing_reservations \
             WHERE attempt_id = 'attempt-orphan-dispatched'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取预留状态");
        assert_eq!(status, "released", "已派发且无结果的尝试以释放结束");

        let pending = load_pending_request_logs(&pool, 16)
            .await
            .expect("应能读取恢复记录");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].log.cost_usd_micros, 0, "无结果不产生费用");
        assert_eq!(pending[0].log.base_cost_usd_micros, 0);
        assert!(pending[0].log.settled, "释放即终态，后台无需再结算");
        assert!(!pending[0].log.usage_reported);
        assert!(
            pending[0].upstream_reached,
            "恢复的可达性未知，按可能已计费告警"
        );

        let balance: i64 =
            sqlx::query_scalar("SELECT balance_usd_micros FROM user_balance WHERE user_id = 1")
                .fetch_one(&pool)
                .await
                .expect("应能读取余额");
        assert_eq!(balance, 10_000, "恢复释放不得扣减钱包");
    }

    /// 长流心跳把超龄预留行的 `updated_at` 推进到当前时刻：恢复扫描不再把
    /// 仍在消费中的长流判为孤儿，预留保持 `reserved` 等待流结束后的结果。
    /// 同库对照行未做心跳，按既有孤儿语义回收。
    #[tokio::test]
    async fn heartbeat_keeps_stale_reservation_out_of_recovery() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        seed_token(&mut conn, "sk-heartbeat").await;
        initialize_token_settlement(&mut conn, "sk-heartbeat", 10_000, 1)
            .await
            .expect("应能初始化余额");
        let metadata = serde_json::to_vec(&BillingAttemptRecovery {
            token_name: "heartbeat".to_string(),
            model: "model".to_string(),
            outbound_model: None,
            channel: "channel".to_string(),
            channel_key: Some("key-1".to_string()),
            inbound_protocol: "openai_chat".to_string(),
            started: 1,
            price: PriceSnapshot::default(),
            discount_bp: billing::DEFAULT_DISCOUNT_BP,
            request_body: None,
            result: None,
            result_settlement_error: None,
            upstream_reached: true,
        })
        .expect("恢复元数据应可编码");
        // updated_at = 0：已超过任何恢复阈值的长流（派发后始终未出结果）。
        // 两行形状相同，仅一行做心跳。
        sqlx::query(
            "INSERT INTO billing_reservations \
             (attempt_id, request_id, token_key, user_id, reserved_cost_usd_micros, \
              token_limit_usd_micros, recovery_metadata, status, dispatched, result_persisted, \
              created_at, updated_at) \
             VALUES ('attempt-heartbeat', 'request-heartbeat', 'sk-heartbeat', 1, 123, NULL, ?, \
                     'reserved', 1, 0, 0, 0), \
                    ('attempt-stale', 'request-stale', 'sk-heartbeat', 1, 123, NULL, ?, \
                     'reserved', 1, 0, 0, 0)",
        )
        .bind(&metadata)
        .bind(&metadata)
        .execute(&mut *conn)
        .await
        .expect("应能写入超龄预留");
        drop(conn);

        let touched = touch_billing_attempt_heartbeat(&pool, "attempt-heartbeat")
            .await
            .expect("心跳应成功");
        assert!(touched, "待恢复状态的预留应被心跳刷新");
        let updated_at: i64 = sqlx::query_scalar(
            "SELECT updated_at FROM billing_reservations WHERE attempt_id = 'attempt-heartbeat'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取心跳时刻");
        assert!(updated_at > 0, "心跳应把 updated_at 推进到当前时刻");

        // 生产比例的等价断言：心跳间隔（60s）远小于恢复阈值时，被心跳刷新的
        // 行始终新鲜于阈值截点。此处以 60s 阈值断言「刷新后不被回收」，与
        // 生产的 60s 心跳 / 10min 阈值同构。
        assert_eq!(
            recover_orphan_billing_attempts(&pool, Duration::from_secs(60), 16)
                .await
                .expect("恢复任务应成功"),
            1,
            "未心跳的对照行按孤儿语义回收"
        );
        let status: String = sqlx::query_scalar(
            "SELECT status FROM billing_reservations WHERE attempt_id = 'attempt-heartbeat'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取预留状态");
        assert_eq!(status, "reserved", "长流预留应保持等待结果");
        let stale_status: String = sqlx::query_scalar(
            "SELECT status FROM billing_reservations WHERE attempt_id = 'attempt-stale'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取对照行状态");
        assert_eq!(stale_status, "released", "未心跳的超龄行照常回收");
    }

    /// 预留离开「reserved 且无持久化结果」状态后心跳是空操作：不改动行，
    /// 返回 `false` 让流任务停止心跳。
    #[tokio::test]
    async fn heartbeat_is_noop_for_terminal_reservations() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        seed_token(&mut conn, "sk-heartbeat-terminal").await;
        initialize_token_settlement(&mut conn, "sk-heartbeat-terminal", 10_000, 1)
            .await
            .expect("应能初始化余额");
        let metadata = serde_json::to_vec(&BillingAttemptRecovery {
            token_name: "terminal".to_string(),
            model: "model".to_string(),
            outbound_model: None,
            channel: "channel".to_string(),
            channel_key: Some("key-1".to_string()),
            inbound_protocol: "openai_chat".to_string(),
            started: 1,
            price: PriceSnapshot::default(),
            discount_bp: billing::DEFAULT_DISCOUNT_BP,
            request_body: None,
            result: None,
            result_settlement_error: None,
            upstream_reached: true,
        })
        .expect("恢复元数据应可编码");
        sqlx::query(
            "INSERT INTO billing_reservations \
             (attempt_id, request_id, token_key, user_id, reserved_cost_usd_micros, \
              token_limit_usd_micros, recovery_metadata, status, dispatched, result_persisted, \
              created_at, updated_at) \
             VALUES ('attempt-released', 'request-released', 'sk-heartbeat-terminal', 1, 123, \
                     NULL, ?, 'reserved', 1, 0, 0, 0), \
                    ('attempt-resulted', 'request-resulted', 'sk-heartbeat-terminal', 1, 123, \
                     NULL, ?, 'reserved', 1, 1, 0, 0)",
        )
        .bind(&metadata)
        .bind(&metadata)
        .execute(&mut *conn)
        .await
        .expect("应能写入终态预留");
        release_billing_attempt(&pool, "attempt-released")
            .await
            .expect("释放应成功");
        let released_at: i64 = sqlx::query_scalar(
            "SELECT updated_at FROM billing_reservations WHERE attempt_id = 'attempt-released'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取释放时刻");
        drop(conn);

        assert!(
            !touch_billing_attempt_heartbeat(&pool, "attempt-released")
                .await
                .expect("心跳对已释放行应成功"),
            "已释放预留不应被心跳刷新"
        );
        assert!(
            !touch_billing_attempt_heartbeat(&pool, "attempt-resulted")
                .await
                .expect("心跳对已入队结果行应成功"),
            "结果已持久化的预留不应被心跳刷新"
        );
        assert!(
            !touch_billing_attempt_heartbeat(&pool, "attempt-missing")
                .await
                .expect("心跳对不存在的行应成功"),
            "不存在的预留不应被心跳刷新"
        );
        let released_after: i64 = sqlx::query_scalar(
            "SELECT updated_at FROM billing_reservations WHERE attempt_id = 'attempt-released'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取释放行的时刻");
        assert_eq!(released_after, released_at, "空操作不得改动已释放行");
        let resulted_at: i64 = sqlx::query_scalar(
            "SELECT updated_at FROM billing_reservations WHERE attempt_id = 'attempt-resulted'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取已入队行的时刻");
        assert_eq!(resulted_at, 0, "空操作不得改动结果已入队的行");
    }

    /// 终态预留行按保留期清理：已结算/已释放且超龄的行被删除，`reserved`
    /// 与结果未消费的行不受影响。
    #[tokio::test]
    async fn terminal_reservations_are_purged_by_retention() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        seed_token(&mut conn, "sk-purge").await;
        let metadata = serde_json::to_vec(&BillingAttemptRecovery {
            token_name: "purge".to_string(),
            model: "model".to_string(),
            outbound_model: None,
            channel: "channel".to_string(),
            channel_key: Some("key-1".to_string()),
            inbound_protocol: "openai_chat".to_string(),
            started: 1,
            price: PriceSnapshot::default(),
            discount_bp: billing::DEFAULT_DISCOUNT_BP,
            request_body: None,
            result: None,
            result_settlement_error: None,
            upstream_reached: true,
        })
        .expect("恢复元数据应可编码");
        sqlx::query(
            "INSERT INTO billing_reservations \
             (attempt_id, request_id, token_key, user_id, reserved_cost_usd_micros, \
              token_limit_usd_micros, recovery_metadata, status, dispatched, result_persisted, \
              created_at, updated_at) \
             VALUES ('attempt-settled-old', 'r1', 'sk-purge', 1, 1, NULL, ?, 'settled', 1, 1, 0, 0), \
                    ('attempt-released-old', 'r2', 'sk-purge', 1, 1, NULL, ?, 'released', 1, 0, 0, 0), \
                    ('attempt-reserved-old', 'r3', 'sk-purge', 1, 1, NULL, ?, 'reserved', 1, 0, 0, 0), \
                    ('attempt-settled-new', 'r4', 'sk-purge', 1, 1, NULL, ?, 'settled', 1, 1, 0, ?)",
        )
        .bind(&metadata)
        .bind(&metadata)
        .bind(&metadata)
        .bind(&metadata)
        .bind(i64::MAX / 2)
        .execute(&mut *conn)
        .await
        .expect("应能写入各状态预留");
        drop(conn);

        let removed = purge_terminal_billing_reservations_before(&pool, i64::MAX / 2)
            .await
            .expect("清理应成功");
        assert_eq!(removed, 2, "仅删除超龄的终态行");

        let remaining: Vec<String> =
            sqlx::query("SELECT attempt_id FROM billing_reservations ORDER BY attempt_id")
                .fetch_all(&pool)
                .await
                .expect("应能读取剩余行")
                .into_iter()
                .map(|row| row.try_get::<String, _>("attempt_id").expect("应有 id"))
                .collect();
        assert_eq!(
            remaining,
            vec![
                "attempt-reserved-old".to_string(),
                "attempt-settled-new".to_string()
            ],
            "reserved 与未超龄的行保留"
        );
    }

    /// 恢复扫描候选查询命中部分索引：治理后的查询计划不再全表扫描。
    #[tokio::test]
    async fn recovery_scan_uses_partial_index() {
        let (_dir, pool) = test_pool().await;
        let plan: Vec<String> = sqlx::query(
            "EXPLAIN QUERY PLAN SELECT attempt_id FROM billing_reservations \
             WHERE status = 'reserved' AND result_persisted = 0 AND updated_at <= 0 \
             ORDER BY updated_at, attempt_id LIMIT 16",
        )
        .fetch_all(&pool)
        .await
        .expect("应能读取查询计划")
        .into_iter()
        .map(|row| row.try_get::<String, _>("detail").expect("应有 detail 列"))
        .collect();
        assert!(
            plan.iter()
                .any(|detail| detail.contains("idx_billing_reservations_recovery")),
            "恢复候选查询应命中部分索引，实际计划: {plan:?}"
        );
    }

    /// 释放是已派发尝试的合法终态：释放后重复入队的结果不再结算——对
    /// `released` 预留结算必须显式报错，重复结果也无法再写回预留。
    #[tokio::test]
    async fn released_attempt_rejects_late_settlement() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        seed_token(&mut conn, "sk-released").await;
        initialize_token_settlement(&mut conn, "sk-released", 10_000, 1)
            .await
            .expect("应能初始化余额");
        let reserved = reserve_billing_attempt(
            &pool,
            BillingAttemptReservation {
                attempt_id: "attempt-released",
                request_id: "request-released",
                token_key: "sk-released",
                user_id: 1,
                cost_usd_micros: 123,
                token_limit_usd_micros: None,
                recovery_metadata: br#"{"token_name":"t","model":"m","channel":"c","inbound_protocol":"openai_chat","started":0,"price":{"input_micros":0,"output_micros":0,"cache_read_micros":0,"cache_write_micros":0,"cache_write_1h_micros":0},"discount_bp":10000}"#,
            },
        )
        .await
        .expect("应能预留");
        assert!(reserved);
        release_billing_attempt(&pool, "attempt-released")
            .await
            .expect("已派发预留也应可释放");

        // 结算对已释放预留报错：费用与钱包都不能再变更。
        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("应能开启事务");
        assert!(matches!(
            settle_billing_attempt(&mut tx, "attempt-released", 123).await,
            Err(StoreError::InvalidResource(_))
        ));
        tx.commit().await.expect("应能提交事务");
        let balance: i64 =
            sqlx::query_scalar("SELECT balance_usd_micros FROM user_balance WHERE user_id = 1")
                .fetch_one(&pool)
                .await
                .expect("应能读取余额");
        assert_eq!(balance, 10_000, "释放后的结算不得扣款");

        // 重复结果无法写回已释放的预留：入队前置检查显式报错。
        let mut log = sample_log(1, true);
        log.billing_attempt_id = Some("attempt-released".to_string());
        assert!(matches!(
            enqueue_pending_request_log(
                &pool,
                PendingRequestLog {
                    log,
                    settlement_error: None,
                    upstream_reached: true,
                },
            )
            .await,
            Err(StoreError::InvalidResource(_))
        ));

        // 释放幂等：重复释放不报错，状态保持 released。
        release_billing_attempt(&pool, "attempt-released")
            .await
            .expect("重复释放应无操作成功");
        let status: String = sqlx::query_scalar(
            "SELECT status FROM billing_reservations WHERE attempt_id = 'attempt-released'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能读取预留状态");
        assert_eq!(status, "released");
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
}
