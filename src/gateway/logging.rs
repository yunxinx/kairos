//! 请求日志与计费结果的持久化适配。

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;

use crate::{
    config::Protocol,
    core::billing::{self, Error as BillingError, PriceSnapshot},
    core::ir::Usage,
    store,
    store::resources::Token,
};

use super::http::Deps;

/// 同一令牌/模型/渠道组合的 usage 缺失告警冷却窗口。
const USAGE_WARNING_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Clone, PartialEq, Eq, Hash)]
struct UsageWarningKey {
    token_key: String,
    model: String,
    channel: String,
}

/// usage 缺失告警的进程内去重器，避免异常上游把 system_log 写满。
struct UsageWarningGate {
    seen: HashMap<UsageWarningKey, std::time::Instant>,
}

impl UsageWarningGate {
    pub(super) fn new() -> Self {
        Self {
            seen: HashMap::new(),
        }
    }

    /// 组合首次出现或冷却期已过时允许落一条告警。
    fn should_warn(&mut self, token_key: &str, model: &str, channel: &str) -> bool {
        self.seen
            .retain(|_, recorded| recorded.elapsed() < USAGE_WARNING_COOLDOWN);
        let key = UsageWarningKey {
            token_key: token_key.to_string(),
            model: model.to_string(),
            channel: channel.to_string(),
        };
        match self.seen.get_mut(&key) {
            Some(recorded) if recorded.elapsed() < USAGE_WARNING_COOLDOWN => false,
            Some(recorded) => {
                *recorded = std::time::Instant::now();
                true
            }
            None => {
                self.seen.insert(key, std::time::Instant::now());
                true
            }
        }
    }
}

/// 一次请求的计费结果，供日志落库。
///
/// 请求日志的 `settled` 由后台结算结果填写，调用方不必预置。
#[derive(Debug, Clone)]
pub(super) struct Billing {
    pub(super) usage: Usage,
    pub(super) price: PriceSnapshot,
    /// 渠道原价（折扣前），为零表示未产生计费/失败请求。
    pub(super) base_cost_usd_micros: i64,
    /// 本次使用的万分比折扣率（10000 = 原价）。
    pub(super) discount_bp: i64,
    /// 实收（折后），用于扣钱包、累计结算与日志。
    pub(super) cost_usd_micros: i64,
    /// 费用不可表示时保留错误；该请求只能写成未结算，禁止用零费用掩盖。
    pub(super) calculation_error: Option<BillingError>,
    /// 上游结果是否显式包含 usage。
    ///
    /// 该状态不能由 token 数值推断：显式回报的全零 usage 是可信结果，而没有
    /// usage 字段的结果必须由结算队列按本次出站尝试的保守预留处理。
    pub(super) usage_reported: bool,
    pub(super) request_body: Option<Bytes>,
    pub(super) response_body: Option<Vec<u8>>,
}

impl Default for Billing {
    fn default() -> Self {
        Self {
            usage: Usage::default(),
            price: PriceSnapshot::default(),
            base_cost_usd_micros: 0,
            discount_bp: billing::DEFAULT_DISCOUNT_BP,
            cost_usd_micros: 0,
            calculation_error: None,
            usage_reported: false,
            request_body: None,
            response_body: None,
        }
    }
}

impl Billing {
    /// 从 usage 与价格受检构造计费结果；失败时保留原始 usage/价格并标记未结算。
    pub(super) fn try_calculated(
        usage: Usage,
        usage_reported: bool,
        price: PriceSnapshot,
        discount_bp: i64,
        request_body: Option<Bytes>,
        response_body: Option<Vec<u8>>,
    ) -> Result<Self, BillingError> {
        let charge = billing::charge_micros(&usage, &price, discount_bp)?;
        Ok(Self {
            usage,
            price,
            base_cost_usd_micros: charge.base_cost_usd_micros,
            discount_bp,
            cost_usd_micros: charge.cost_usd_micros,
            calculation_error: None,
            usage_reported,
            request_body,
            response_body,
        })
    }

    /// 从 usage 与价格受检构造计费结果；失败时保留原始 usage/价格并标记未结算。
    pub(super) fn calculated(
        usage: Usage,
        usage_reported: bool,
        price: PriceSnapshot,
        discount_bp: i64,
        request_body: Option<Bytes>,
        response_body: Option<Vec<u8>>,
    ) -> Self {
        match Self::try_calculated(
            usage.clone(),
            usage_reported,
            price,
            discount_bp,
            request_body.clone(),
            response_body.clone(),
        ) {
            Ok(billing) => billing,
            Err(err) => Self {
                usage_reported,
                usage,
                price,
                base_cost_usd_micros: 0,
                discount_bp,
                cost_usd_micros: 0,
                calculation_error: Some(err),
                request_body,
                response_body,
            },
        }
    }
}

/// 按独立的日志 body 上限截断落库字节，避免 full_body 把库撑爆。
///
/// 流式封顶可能恰好停在字符中间且 `len == max`，因此无论是否超限都按 UTF-8
/// 边界下取整。非法 UTF-8（二进制 body）仍按字节截断。
fn clip_logged_body(body: Option<Vec<u8>>, max_bytes: u64) -> Option<Vec<u8>> {
    body.map(|mut bytes| {
        let max = max_bytes.min(usize::MAX as u64) as usize;
        bytes.truncate(utf8_prefix_len(&bytes, max));
        bytes
    })
}

/// 不超过 `max` 的前缀长度：完整 UTF-8 字符边界，或无法判定时的字节上限。
///
/// `max` 大于切片长度时仍检查整段：末尾不完整序列退到 `valid_up_to`。
fn utf8_prefix_len(bytes: &[u8], max: usize) -> usize {
    let end = max.min(bytes.len());
    if end == 0 {
        return 0;
    }
    match std::str::from_utf8(&bytes[..end]) {
        Ok(_) => end,
        Err(err) if err.error_len().is_none() => err.valid_up_to(),
        Err(_) => end,
    }
}

/// 一次请求日志的调用侧字段。
pub(super) struct RequestLogDraft<'a> {
    pub(super) token: &'a Token,
    pub(super) model: &'a str,
    pub(super) outbound_model: Option<&'a str>,
    pub(super) channel: &'a str,
    pub(super) channel_key: Option<&'a str>,
    pub(super) status: u16,
    pub(super) started: i64,
    pub(super) billing: Billing,
    pub(super) inbound_protocol: Protocol,
    pub(super) request_id: &'a str,
    /// 实际出站调用的唯一计费身份；未进入出站阶段的请求日志为 `None`。
    pub(super) billing_attempt_id: Option<&'a str>,
    /// 请求级绝对截止时刻；设置后队列持久化不得越过该时刻。
    pub(super) deadline: Option<tokio::time::Instant>,
}

/// 持久化队列的唤醒端；队列内容本身在 SQLite 中，通知丢失由周期扫描兜底。
#[derive(Clone)]
pub struct RequestLogWriter {
    wake_sender: tokio::sync::mpsc::Sender<()>,
}

impl RequestLogWriter {
    pub fn start(pool: sqlx::SqlitePool) -> Self {
        let (wake_sender, wake_receiver) = tokio::sync::mpsc::channel(1);
        tokio::spawn(run_request_log_writer(pool, wake_receiver));
        let writer = Self { wake_sender };
        writer.wake();
        writer
    }

    pub(super) fn wake(&self) {
        let _ = self.wake_sender.try_send(());
    }
}

/// 把请求结果追加到持久化队列；响应路径不执行余额更新与最终日志事务。
pub(super) async fn queue_request_log(
    deps: &Deps,
    draft: RequestLogDraft<'_>,
) -> Result<(), store::StoreError> {
    let deadline = draft.deadline;
    let operation = queue_request_log_inner(deps, draft);
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, operation)
            .await
            .map_err(|_| store::StoreError::PersistenceTimeout)?,
        None => operation.await,
    }
}

async fn queue_request_log_inner(
    deps: &Deps,
    draft: RequestLogDraft<'_>,
) -> Result<(), store::StoreError> {
    let now = unix_millis();
    let max_bytes = deps.snapshot.read().await.log_body_max_bytes;
    let settlement_error = draft
        .billing
        .calculation_error
        .map(|err| format!("费用计算失败，未执行结算: {err}"));
    let reserved_fallback = if !draft.billing.usage_reported
        && settlement_error.is_none()
        && draft.billing_attempt_id.is_some()
    {
        sqlx::query_scalar::<_, i64>(
            "SELECT reserved_cost_usd_micros FROM billing_reservations \
             WHERE attempt_id = ? AND status = 'reserved'",
        )
        .bind(draft.billing_attempt_id)
        .fetch_optional(&deps.pool)
        .await
        .map_err(store::StoreError::Query)?
    } else {
        None
    };
    let cost_usd_micros = reserved_fallback.unwrap_or(draft.billing.cost_usd_micros);
    // 折后预留额由整数截断得到，反推原价时取同一折扣下可能产生该实收额的
    // 最大原价，确保兜底记录不会把渠道成本低估。折扣为 0 时无法从实收额
    // 推导原价，只能保留现有原价字段并通过告警暴露该不确定性。
    let base_cost_usd_micros = reserved_fallback
        .map(|charge| conservative_base_from_charge(charge, draft.billing.discount_bp))
        .unwrap_or(draft.billing.base_cost_usd_micros);
    let log = store::RequestLog {
        id: 0,
        created_at: now,
        token_name: draft.token.name.clone(),
        token_key: draft.token.token_key.clone(),
        user_id: draft.token.user_id,
        inbound_protocol: protocol_name(draft.inbound_protocol).to_string(),
        model: draft.model.to_string(),
        outbound_model: draft.outbound_model.map(str::to_string),
        channel: draft.channel.to_string(),
        channel_key: draft.channel_key.map(str::to_string),
        status_code: i64::from(draft.status),
        latency_ms: now - draft.started,
        input_tokens: draft.billing.usage.input_tokens,
        output_tokens: draft.billing.usage.output_tokens,
        cache_read_tokens: draft.billing.usage.cache_read_tokens,
        cache_write_tokens: draft.billing.usage.cache_write_tokens,
        cache_write_1h_tokens: draft.billing.usage.cache_write_1h_tokens,
        usage_reported: draft.billing.usage_reported,
        price: draft.billing.price,
        base_cost_usd_micros,
        discount_bp: draft.billing.discount_bp,
        cost_usd_micros,
        // 出站尝试即使实际费用为零也必须由后台消费预留，只有从未出站的零费用
        // 日志可以在入队时直接视为已结算。
        settled: settlement_error.is_none()
            && draft.billing_attempt_id.is_none()
            && cost_usd_micros == 0,
        request_id: Some(draft.request_id.to_string()),
        billing_attempt_id: draft.billing_attempt_id.map(str::to_string),
        request_body: clip_logged_body(
            draft.billing.request_body.map(|bytes| bytes.to_vec()),
            max_bytes,
        ),
        response_body: clip_logged_body(draft.billing.response_body, max_bytes),
    };
    let pending = store::PendingRequestLog {
        log,
        settlement_error,
    };
    if let Some(attempt_id) = pending.log.billing_attempt_id.as_deref() {
        // 先把完整结果写入预留行，再写 outbox。两步任一步骤后进程崩溃时，
        // 恢复任务都能从同一条预留重建原始日志，而不是用空结果覆盖它。
        store::persist_billing_attempt_result(&deps.pool, attempt_id, &pending).await?;
    }
    store::enqueue_pending_request_log(&deps.pool, pending).await?;
    deps.request_log_writer.wake();
    Ok(())
}

/// 由折后金额反推可产生该金额的原价上界。
///
/// 折后计算使用向下取整：`charge = floor(base * discount / 10000)`。因此
/// `((charge + 1) * 10000 - 1) / discount` 是所有可能原价中的最大值；使用
/// 上界可以避免缺失 usage 时把基础成本记成低于实际的数值。免费折扣没有
/// 可逆信息，返回 0 并由调用方通过 usage 缺失告警暴露该事实。
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

const REQUEST_LOG_BATCH_SIZE: i64 = 16;
const REQUEST_LOG_RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// 只有超过正常请求总时限仍未写入结果的预留才进入恢复，给响应路径留下
/// 足够时间处理短暂的数据库写锁或调度延迟。
const BILLING_RECOVERY_MAX_AGE: Duration = Duration::from_secs(10 * 60);

async fn run_request_log_writer(
    pool: sqlx::SqlitePool,
    mut wake_receiver: tokio::sync::mpsc::Receiver<()>,
) {
    let mut retry = tokio::time::interval(REQUEST_LOG_RETRY_INTERVAL);
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    retry.tick().await;
    let mut usage_warning_gate = UsageWarningGate::new();
    loop {
        tokio::select! {
            wake = wake_receiver.recv() => {
                if wake.is_none() {
                    break;
                }
            }
            _ = retry.tick() => {}
        }
        if let Err(err) = store::recover_orphan_billing_attempts(
            &pool,
            BILLING_RECOVERY_MAX_AGE,
            REQUEST_LOG_BATCH_SIZE,
        )
        .await
        {
            tracing::error!(error = %err, "崩溃遗留的计费预留恢复失败，将稍后重试");
        }
        if let Err(err) = drain_pending_request_logs(&pool, &mut usage_warning_gate).await {
            tracing::error!(error = %err, "后台请求日志持久化失败，将稍后重试");
        }
    }
}

async fn drain_pending_request_logs(
    pool: &sqlx::SqlitePool,
    usage_warning_gate: &mut UsageWarningGate,
) -> Result<(), store::StoreError> {
    loop {
        let pending = store::load_pending_request_logs(pool, REQUEST_LOG_BATCH_SIZE).await?;
        if pending.is_empty() {
            return Ok(());
        }
        for item in pending {
            let log = item.log.clone();
            let usage_reported = item.log.usage_reported;
            if let Err(err) = process_pending_request_log(pool, usage_warning_gate, item).await {
                // 结算事务失败时只隔离当前记录并继续消费后续记录。隔离本身
                // 也失败才向上返回；此时保留队列状态，下一轮仍会重试该动作。
                let reason = format!("持久化请求日志失败: {err}");
                store::isolate_pending_request_log(
                    pool,
                    log.id,
                    &reason,
                    Some(REQUEST_LOG_RETRY_INTERVAL),
                )
                .await?;
                record_request_log_notes(
                    pool,
                    usage_warning_gate,
                    &log,
                    usage_reported,
                    Some(reason),
                    None,
                )
                .await;
            }
        }
    }
}

async fn process_pending_request_log(
    pool: &sqlx::SqlitePool,
    usage_warning_gate: &mut UsageWarningGate,
    mut pending: store::PendingRequestLog,
) -> Result<(), store::StoreError> {
    if let Some(reason) = pending.settlement_error.take() {
        pending.log.settled = false;
        // 费用计算失败是确定性故障：原始记录保留在隔离状态，主队列继续
        // 消费其它请求；人工修复价格或数据后可按 request_id 重放。
        store::isolate_pending_request_log(pool, pending.log.id, &reason, None).await?;
        record_request_log_notes(
            pool,
            usage_warning_gate,
            &pending.log,
            pending.log.usage_reported,
            Some(reason),
            None,
        )
        .await;
        return Ok(());
    }

    if pending.log.cost_usd_micros > 0 || pending.log.billing_attempt_id.is_some() {
        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(store::StoreError::Query)?;
        let settlement = match pending.log.billing_attempt_id.as_deref() {
            Some(attempt_id) => {
                store::settle_billing_attempt(&mut tx, attempt_id, pending.log.cost_usd_micros)
                    .await
            }
            None => {
                store::settle_charge(&mut tx, &pending.log.token_key, pending.log.cost_usd_micros)
                    .await
                    .map(|_| ())
            }
        };
        match settlement {
            Ok(()) => {
                pending.log.settled = true;
                let touch_error = match finish_pending_request_log(&mut tx, &pending.log).await {
                    Ok(touch_error) => touch_error,
                    Err(err) => return Err(rollback_request_log_transaction(tx, err).await),
                };
                tx.commit().await.map_err(store::StoreError::Query)?;
                record_request_log_notes(
                    pool,
                    usage_warning_gate,
                    &pending.log,
                    pending.log.usage_reported,
                    None,
                    touch_error,
                )
                .await;
                return Ok(());
            }
            Err(err) => {
                let reason = format!("结算失败: {err}");
                tx.rollback().await.map_err(store::StoreError::Query)?;
                // 隔离写入与失败详情是独立事务，避免一个坏请求再次阻塞后续
                // 记录。指数间隔由存储层按失败次数计算，持续保留原始结果。
                store::isolate_pending_request_log(
                    pool,
                    pending.log.id,
                    &reason,
                    Some(Duration::from_secs(1)),
                )
                .await?;
                record_request_log_notes(
                    pool,
                    usage_warning_gate,
                    &pending.log,
                    pending.log.usage_reported,
                    Some(reason),
                    None,
                )
                .await;
                return Ok(());
            }
        }
    }

    pending.log.settled = true;

    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(store::StoreError::Query)?;
    let touch_error = match finish_pending_request_log(&mut tx, &pending.log).await {
        Ok(touch_error) => touch_error,
        Err(err) => return Err(rollback_request_log_transaction(tx, err).await),
    };
    tx.commit().await.map_err(store::StoreError::Query)?;
    record_request_log_notes(
        pool,
        usage_warning_gate,
        &pending.log,
        pending.log.usage_reported,
        None,
        touch_error,
    )
    .await;
    Ok(())
}

/// 在一个事务内写最终日志、刷新最后使用时间并删除队列项。
async fn finish_pending_request_log(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    log: &store::RequestLog,
) -> Result<Option<String>, store::StoreError> {
    store::insert_request_log_with_id_on(tx, log, log.id).await?;
    let touch_error = if log.channel.is_empty() {
        None
    } else {
        store::resources::touch_token_used(tx, &log.token_key, log.created_at)
            .await
            .err()
            .map(|err| err.to_string())
    };
    store::delete_pending_request_log_on(tx, log.id).await?;
    Ok(touch_error)
}

async fn rollback_request_log_transaction(
    tx: sqlx::Transaction<'_, sqlx::Sqlite>,
    error: store::StoreError,
) -> store::StoreError {
    if let Err(rollback_error) = tx.rollback().await {
        tracing::error!(
            error = %error,
            rollback_error = %rollback_error,
            "请求日志事务回滚失败"
        );
    }
    error
}

async fn record_request_log_notes(
    pool: &sqlx::SqlitePool,
    usage_warning_gate: &mut UsageWarningGate,
    log: &store::RequestLog,
    usage_reported: bool,
    settlement_error: Option<String>,
    touch_error: Option<String>,
) {
    if !usage_reported
        && log.billing_attempt_id.is_some()
        && usage_warning_gate.should_warn(&log.token_key, &log.model, &log.channel)
    {
        store::record_system_warn(
            pool,
            "billing",
            &store::SystemLogEvent::new(
                "billing.usage_missing",
                serde_json::json!({
                    "request_id": log.request_id,
                    "token_name": log.token_name,
                    "model": log.model,
                    "channel": log.channel,
                    "inbound_protocol": log.inbound_protocol,
                    "usage_reported": false,
                    "amount_source": "reservation",
                    "base_cost_usd_micros": log.base_cost_usd_micros,
                    "cost_usd_micros": log.cost_usd_micros,
                }),
                format!(
                    "上游未回报 usage，本次按保守预留结算（request_id={} token={} model={} channel={} protocol={}）",
                    log.request_id.as_deref().unwrap_or(""),
                    log.token_name,
                    log.model,
                    log.channel,
                    log.inbound_protocol,
                ),
            ),
        )
        .await;
    }
    if let Some(reason) = settlement_error {
        store::record_system_error(
            pool,
            "billing",
            &store::SystemLogEvent::new(
                "request_log.unsettled",
                serde_json::json!({ "reason": reason, "request_id": log.request_id }),
                reason,
            ),
        )
        .await;
    }
    if let Some(error) = touch_error {
        store::record_system_warn(
            pool,
            "request_log",
            &store::SystemLogEvent::new(
                "request_log.token_last_used_update_failed",
                serde_json::json!({ "error": error, "request_id": log.request_id }),
                "刷新令牌最后使用时间失败".to_string(),
            ),
        )
        .await;
    }
}

/// 入站协议名（日志落库用）。
pub(super) fn protocol_name(inbound_protocol: Protocol) -> &'static str {
    match inbound_protocol {
        Protocol::OpenAiChat => "openai_chat",
        Protocol::OpenAiResponses => "openai_responses",
        Protocol::AnthropicMessages => "anthropic_messages",
        Protocol::Gemini => "gemini",
    }
}

/// 当前 unix 毫秒时间戳。
pub fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 一次下游入站请求的身份，供 hop 之间共用。
pub(super) fn new_request_id() -> String {
    use rand::distr::{Alphanumeric, SampleString};
    Alphanumeric.sample_string(&mut rand::rng(), 22)
}

#[cfg(test)]
mod tests {
    use super::{clip_logged_body, conservative_base_from_charge, utf8_prefix_len};

    #[test]
    fn conservative_base_cost_reverses_discount_rounding() {
        // 50 micro-USD 实收、50% 折扣时，向下取整可能来自 100 或 101 原价，
        // 返回 101 才能覆盖整数截断带来的一个微元区间。
        assert_eq!(conservative_base_from_charge(50, 5_000), 101);
        assert_eq!(conservative_base_from_charge(50, 10_000), 50);
        assert_eq!(conservative_base_from_charge(50, 0), 0);
    }

    #[test]
    fn clip_logged_body_stops_on_utf8_char_boundary() {
        // 「世」是 3 字节 E4 B8 96；截在第 7 字节会切断该字。
        let body = "hello 世界".as_bytes().to_vec();
        assert_eq!(utf8_prefix_len(&body, 7), 6);
        let clipped = clip_logged_body(Some(body), 7).expect("应有截断结果");
        assert_eq!(clipped, b"hello ");
    }

    /// 流式封顶可能让缓冲恰好停在字符中间且长度等于上限，仍须退下完整字符。
    #[test]
    fn clip_logged_body_floors_incomplete_utf8_at_exact_max() {
        let mut body = b"hello ".to_vec();
        body.push(0xe4);
        assert_eq!(body.len(), 7);
        assert_eq!(utf8_prefix_len(&body, 7), 6);
        let clipped = clip_logged_body(Some(body), 7).expect("应有截断结果");
        assert_eq!(clipped, b"hello ");
        assert_eq!(std::str::from_utf8(&clipped), Ok("hello "));
    }

    #[test]
    fn clip_logged_body_keeps_ascii_and_binary() {
        assert_eq!(
            clip_logged_body(Some(b"abcdef".to_vec()), 4).as_deref(),
            Some(b"abcd".as_slice())
        );
        assert_eq!(
            clip_logged_body(Some(vec![0xff, 0xfe, 0x00, 0x01]), 3).as_deref(),
            Some([0xff, 0xfe, 0x00].as_slice())
        );
    }
}
