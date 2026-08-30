//! 请求日志与计费结果的持久化适配。

use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;

use crate::{
    config::Protocol,
    core::billing::{self, BillingError, PriceSnapshot},
    core::ir::Usage,
    store,
    store::resources::Token,
};

use super::http::Deps;

/// 一次请求的计费结果，供日志落库。
///
/// 请求日志的 `settled` 由 [`log_request`] 按结算成败填写，调用方不必预置。
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
            request_body: None,
            response_body: None,
        }
    }
}

impl Billing {
    /// 从 usage 与价格受检构造计费结果；失败时保留原始 usage/价格并标记未结算。
    pub(super) fn try_calculated(
        usage: Usage,
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
            request_body,
            response_body,
        })
    }

    /// 从 usage 与价格受检构造计费结果；失败时保留原始 usage/价格并标记未结算。
    pub(super) fn calculated(
        usage: Usage,
        price: PriceSnapshot,
        discount_bp: i64,
        request_body: Option<Bytes>,
        response_body: Option<Vec<u8>>,
    ) -> Self {
        match Self::try_calculated(
            usage.clone(),
            price,
            discount_bp,
            request_body.clone(),
            response_body.clone(),
        ) {
            Ok(billing) => billing,
            Err(err) => Self {
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

/// 尽量在同一事务内结算并插入请求日志；最后使用时间在提交后再尽力刷新。
///
/// `channel` 非空表示该请求已通过计费准入并选定出站渠道：此时刷新
/// `last_used_at`。准入拒绝（402）与尚未路由的错误 `channel` 为空，不刷新。
/// `last_used_at` 只是展示元数据，失败不得回滚已成功的扣费与日志。
///
/// 结算成功后若插入失败，回滚扣费并尽力单独写入 `settled = false` 的请求日志。
/// 开事务或结算失败时同样尽力留下未结算请求日志，并记入系统日志。
/// HTTP 2xx 且 usage 四分量全零时另记一条 warn 系统日志，使上游漏报 usage 可观测。
#[allow(clippy::too_many_arguments)]
pub(super) async fn log_request(
    deps: &Deps,
    token: &Token,
    model: &str,
    outbound_model: Option<&str>,
    channel: &str,
    channel_key: Option<&str>,
    status: u16,
    started: i64,
    billing: Billing,
    inbound_protocol: Protocol,
    request_id: &str,
) {
    let now = unix_millis();
    let max_bytes = deps.snapshot.read().await.log_body_max_bytes;
    if (200..300).contains(&status) && billing.usage.is_zero() {
        let inbound = protocol_name(inbound_protocol);
        store::record_system_warn(
            &deps.pool,
            "billing",
            &store::SystemLogEvent::new(
                "billing.usage_missing",
                serde_json::json!({
                    "request_id": request_id,
                    "token_name": token.name,
                    "model": model,
                    "channel": channel,
                    "inbound_protocol": inbound,
                }),
                format!(
                    "上游未回报 usage，本次按零计费（request_id={request_id} \
                     token={} model={model} channel={channel} protocol={inbound}）",
                    token.name
                ),
            ),
        )
        .await;
    }
    let mut log = store::RequestLog {
        id: 0,
        created_at: now,
        token_name: token.name.clone(),
        token_key: token.token_key.clone(),
        user_id: token.user_id,
        inbound_protocol: protocol_name(inbound_protocol).to_string(),
        model: model.to_string(),
        outbound_model: outbound_model.map(str::to_string),
        channel: channel.to_string(),
        channel_key: channel_key.map(str::to_string),
        status_code: status as i64,
        latency_ms: now - started,
        input_tokens: billing.usage.input_tokens,
        output_tokens: billing.usage.output_tokens,
        cache_read_tokens: billing.usage.cache_read_tokens,
        cache_write_tokens: billing.usage.cache_write_tokens,
        cache_write_1h_tokens: billing.usage.cache_write_1h_tokens,
        price: billing.price,
        base_cost_usd_micros: billing.base_cost_usd_micros,
        discount_bp: billing.discount_bp,
        cost_usd_micros: billing.cost_usd_micros,
        settled: billing.calculation_error.is_none() && billing.cost_usd_micros == 0,
        request_id: Some(request_id.to_string()),
        request_body: clip_logged_body(billing.request_body.map(|bytes| bytes.to_vec()), max_bytes),
        response_body: clip_logged_body(billing.response_body, max_bytes),
    };

    if let Some(err) = billing.calculation_error {
        log.settled = false;
        write_unsettled_request_log(
            deps,
            log,
            "billing",
            &format!("费用计算失败，未执行结算: {err}"),
        )
        .await;
        return;
    }

    let mut tx = match deps.pool.begin().await {
        Ok(tx) => tx,
        Err(err) => {
            log.settled = false;
            write_unsettled_request_log(
                deps,
                log,
                "request_log",
                &format!("结算/日志事务开启失败: {err}"),
            )
            .await;
            return;
        }
    };

    if billing.cost_usd_micros > 0 {
        match store::settle_charge(&mut tx, &token.token_key, billing.cost_usd_micros).await {
            Ok(_) => log.settled = true,
            Err(err) => {
                rollback_and_write_unsettled(deps, tx, log, "settle", format!("结算失败: {err}"))
                    .await;
                return;
            }
        }
    }

    if let Err(err) = store::insert_request_log_on(&mut tx, &log).await {
        rollback_and_write_unsettled(
            deps,
            tx,
            log,
            "request_log",
            format!("请求日志同事务插入失败，事务已回滚: {err}"),
        )
        .await;
        return;
    }

    if let Err(err) = tx.commit().await {
        log.settled = false;
        write_unsettled_request_log(
            deps,
            log,
            "request_log",
            &format!("结算/日志提交失败: {err}"),
        )
        .await;
        return;
    }

    touch_last_used_best_effort(&deps.pool, &log).await;
}

/// 已出站的请求刷新 `last_used_at`；尚未路由则跳过。失败只记系统日志。
async fn touch_last_used_best_effort(pool: &sqlx::SqlitePool, log: &store::RequestLog) {
    if log.channel.is_empty() {
        return;
    }
    let mut conn = match pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => {
            store::record_system_warn(
                pool,
                "request_log",
                &store::SystemLogEvent::new(
                    "request_log.token_last_used_update_failed",
                    serde_json::json!({ "error": err.to_string() }),
                    format!("刷新令牌最后使用时间失败: {err}"),
                ),
            )
            .await;
            return;
        }
    };
    if let Err(err) =
        store::resources::touch_token_used(&mut conn, &log.token_key, log.created_at).await
    {
        store::record_system_warn(
            pool,
            "request_log",
            &store::SystemLogEvent::new(
                "request_log.token_last_used_update_failed",
                serde_json::json!({ "error": err.to_string() }),
                format!("刷新令牌最后使用时间失败: {err}"),
            ),
        )
        .await;
    }
}

/// 回滚进行中的结算事务后，尽力留下未结算请求日志。
async fn rollback_and_write_unsettled(
    deps: &Deps,
    tx: sqlx::Transaction<'_, sqlx::Sqlite>,
    mut log: store::RequestLog,
    system_target: &str,
    reason: String,
) {
    log.settled = false;
    let reason = match tx.rollback().await {
        Ok(()) => reason,
        Err(err) => format!("{reason}；事务回滚也失败: {err}"),
    };
    write_unsettled_request_log(deps, log, system_target, &reason).await;
}

/// 尽力写入未结算请求日志，并记一条系统日志。请求日志落库失败时系统日志带上两次错误。
async fn write_unsettled_request_log(
    deps: &Deps,
    log: store::RequestLog,
    system_target: &str,
    reason: &str,
) {
    match store::insert_request_log(&deps.pool, &log).await {
        Ok(_) => {
            touch_last_used_best_effort(&deps.pool, &log).await;
            store::record_system_error(
                &deps.pool,
                system_target,
                &store::SystemLogEvent::new(
                    "request_log.unsettled",
                    serde_json::json!({ "reason": reason }),
                    reason.to_string(),
                ),
            )
            .await;
        }
        Err(err) => {
            store::record_system_error(
                &deps.pool,
                "request_log",
                &store::SystemLogEvent::new(
                    "request_log.fallback_write_failed",
                    serde_json::json!({ "reason": reason, "error": err.to_string() }),
                    format!("{reason}；回退写入也失败: {err}"),
                ),
            )
            .await;
        }
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
    use super::{clip_logged_body, utf8_prefix_len};

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
