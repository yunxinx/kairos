//! 请求日志与计费结果的持久化适配。

use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    config::Protocol,
    core::{billing::PriceSnapshot, ir::Usage},
    store,
    store::resources::Token,
};

use super::http::Deps;

/// 一次请求的计费结果，供日志落库。
///
/// 请求日志的 `settled` 由 [`log_request`] 按结算成败填写，调用方不必预置。
#[derive(Debug, Clone, Default)]
pub(super) struct Billing {
    pub(super) usage: Usage,
    pub(super) price: PriceSnapshot,
    pub(super) cost_usd_micros: i64,
    pub(super) request_body: Option<Vec<u8>>,
    pub(super) response_body: Option<Vec<u8>>,
}

/// 按独立的日志 body 上限截断落库字节，避免 full_body 把库撑爆。
fn clip_logged_body(body: Option<Vec<u8>>, max_bytes: u64) -> Option<Vec<u8>> {
    body.map(|mut bytes| {
        let max = max_bytes.min(usize::MAX as u64) as usize;
        if bytes.len() > max {
            bytes.truncate(max);
        }
        bytes
    })
}

/// 尽量在同一事务内结算并插入请求日志。
///
/// 结算成功后若插入失败，回滚扣费并尽力单独写入 `settled = false` 的请求日志。
/// 开事务或结算失败时同样尽力留下未结算请求日志，并记入系统日志。
#[allow(clippy::too_many_arguments)]
pub(super) async fn log_request(
    deps: &Deps,
    token: &Token,
    model: &str,
    outbound_model: Option<&str>,
    channel: &str,
    status: u16,
    started: i64,
    billing: Billing,
    inbound_protocol: Protocol,
) {
    let now = unix_millis();
    let max_bytes = deps.snapshot.read().await.log_body_max_bytes;
    let mut log = store::RequestLog {
        id: 0,
        created_at: now,
        token_name: token.name.clone(),
        token_key: token.token_key.clone(),
        inbound_protocol: protocol_name(inbound_protocol).to_string(),
        model: model.to_string(),
        outbound_model: outbound_model.map(str::to_string),
        channel: channel.to_string(),
        status_code: status as i64,
        latency_ms: now - started,
        input_tokens: billing.usage.input_tokens,
        output_tokens: billing.usage.output_tokens,
        cache_read_tokens: billing.usage.cache_read_tokens,
        cache_write_tokens: billing.usage.cache_write_tokens,
        price: billing.price,
        cost_usd_micros: billing.cost_usd_micros,
        settled: billing.cost_usd_micros <= 0,
        request_body: clip_logged_body(billing.request_body, max_bytes),
        response_body: clip_logged_body(billing.response_body, max_bytes),
    };

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
            store::record_system_error(&deps.pool, system_target, reason).await;
        }
        Err(err) => {
            store::record_system_error(
                &deps.pool,
                "request_log",
                &format!("{reason}；回退写入也失败: {err}"),
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
    }
}

/// 当前 unix 毫秒时间戳。
pub(super) fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
