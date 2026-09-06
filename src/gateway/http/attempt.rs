//! 出站计费尝试：预留生命周期与长流心跳。
//!
//! 一次物理出站在真正发送前原子预留费用；结果随入队消费预留（有 usage 按
//! 用量结算，缺失则释放），长流以心跳保活预留行不被孤儿恢复误收。

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;

use crate::{
    config::Protocol,
    core::billing::PriceSnapshot,
    core::ir::{ChatRequest, Usage},
    runtime::RuntimeSnapshot,
    store,
    store::resources::{Channel, StoredChannelKey, Token},
};

use super::{Deps, OutboundCall, estimate_attempt_cost_micros, outbound_model_for_log};
use crate::gateway::failover::Outbound;
use crate::gateway::logging::{
    Billing, RequestLogDraft, new_request_id, protocol_name, queue_request_log,
};

/// 一次即将发出的 provider 调用所需的账务与日志依赖。
pub(super) struct BillingAttemptStart<'a> {
    pub(super) deps: &'a Deps,
    pub(super) snapshot: &'a Arc<RuntimeSnapshot>,
    pub(super) request_id: &'a str,
    pub(super) request: &'a ChatRequest,
    pub(super) token: &'a Token,
    pub(super) routed_model: &'a str,
    pub(super) channel: &'a Channel,
    pub(super) channel_key: &'a StoredChannelKey,
    pub(super) started: i64,
    pub(super) price: PriceSnapshot,
    pub(super) inbound_protocol: Protocol,
    pub(super) request_body: Option<Bytes>,
    /// 请求级「曾出站」标记；物理尝试真正发送前置位。
    pub(super) ever_dispatched: &'a AtomicBool,
    /// 本渠道入口重锚的截止时刻；出站响应读取与结果持久化共享同一预算。
    pub(super) deadline: tokio::time::Instant,
}

/// 已持久化、等待进入出站阶段的一次物理尝试。
///
/// 创建成功后由调用方标记是否真正出站；进入出站后预留经结果入队消费：
/// 回报 usage 的按用量结算，缺失 usage 的随入队事务释放。未进入出站时
/// 仍可直接释放。成功响应由调用路径使用 `attempt_id` 入队；失败响应
/// 通过本类型的记录方法入队，使所有终止分支都消费同一条预留。
pub(super) struct BillingAttempt<'a> {
    pub(super) attempt_id: String,
    start: BillingAttemptStart<'a>,
}

impl BillingAttempt<'_> {
    pub(super) fn id(&self) -> &str {
        &self.attempt_id
    }

    /// 在真正开始发送请求前标记物理尝试。
    ///
    /// 预留建立与 provider 发送之间存在一个取消窗口；只有发送即将开始时
    /// 才把尝试转为 dispatched。标记失败时仍可释放未派发预留，避免把本地
    /// 数据库故障误记成已经产生上游费用。
    pub(super) async fn mark_dispatched(&self) -> Result<(), Outbound> {
        let mark_result = tokio::time::timeout_at(
            self.start.deadline,
            store::settlement::mark_billing_attempt_dispatched(
                &self.start.deps.pool,
                &self.attempt_id,
            ),
        )
        .await;
        let mark_error = match mark_result {
            Ok(Ok(())) => {
                // 同一请求任务内顺序写后读，标记只是终局判定的输入；用
                // Release/Acquire 做保守一致，不依赖跨任务可见性。
                self.start.ever_dispatched.store(true, Ordering::Release);
                return Ok(());
            }
            Ok(Err(err)) => err.to_string(),
            Err(_) => "计费尝试标记出站状态超时".to_string(),
        };
        match tokio::time::timeout_at(
            self.start.deadline,
            store::settlement::release_billing_attempt(&self.start.deps.pool, &self.attempt_id),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(release_error)) => tracing::error!(
                error = %release_error,
                attempt_id = %self.attempt_id,
                "未派发的计费预留无法释放"
            ),
            Err(_) => tracing::error!(
                attempt_id = %self.attempt_id,
                "未派发的计费预留释放超时"
            ),
        }
        Err(Outbound::Fatal {
            channel: self.start.channel.name.clone(),
            status: 500,
            message: format!("计费尝试无法进入出站状态: {mark_error}"),
        })
    }

    /// 记录一个已经发出的尝试的结果。
    ///
    /// `send_error` 是 `.send()` 失败的错误对象：连接类错误（TCP 未建立）
    /// 判定为上游未达；收到响应或其余发送错误视为可能已产生费用。结果
    /// 缺失 usage 时不产生费用，预留随结果入队释放并告警。
    pub(super) async fn record_failure(
        &self,
        status: u16,
        response_body: Option<Vec<u8>>,
        usage: Option<Usage>,
        send_error: Option<&reqwest::Error>,
        outcome: Outbound,
    ) -> Outbound {
        let upstream_reached = !send_error.is_some_and(reqwest::Error::is_connect);
        let usage_reported = usage.is_some();
        let billing = Billing::calculated(
            usage.unwrap_or_default(),
            usage_reported,
            self.start.price,
            self.start.snapshot.discount_bp_for_token(self.start.token),
            self.start.request_body.clone(),
            self.start
                .snapshot
                .full_body
                .then_some(response_body)
                .flatten(),
        );
        match queue_request_log(
            self.start.deps,
            RequestLogDraft {
                token: self.start.token,
                model: &self.start.request.model,
                outbound_model: outbound_model_for_log(self.start.channel, self.start.routed_model),
                channel: &self.start.channel.name,
                channel_key: Some(&self.start.channel_key.name),
                status,
                started: self.start.started,
                billing,
                inbound_protocol: self.start.inbound_protocol,
                request_id: self.start.request_id,
                billing_attempt_id: Some(&self.attempt_id),
                upstream_reached,
                dispatched: true,
                deadline: Some(self.start.deadline),
            },
        )
        .await
        {
            Ok(()) => outcome,
            Err(err) => {
                tracing::error!(
                    error = %err,
                    attempt_id = %self.attempt_id,
                    "出站尝试结果无法立即持久化，将由恢复任务继续处理"
                );
                outcome
            }
        }
    }
}

/// 为一次即将发出的 provider 调用建立独立计费身份并冻结额度。
///
/// 预留只在目标地址和出站请求已经构造完成后创建；因此地址校验、协议编码等
/// 本地失败不会占用额度。标记为已发出后，调用方无论收到响应、网络错误还是
/// 超时，都必须把同一个 `attempt_id` 随结果写入持久化队列；结果缺失 usage
/// 时不产生费用，预留由入队事务释放。
pub(super) async fn begin_billing_attempt(
    start: BillingAttemptStart<'_>,
) -> Result<BillingAttempt<'_>, Outbound> {
    let discount_bp = start.snapshot.discount_bp_for_token(start.token);
    let reserved_cost = estimate_attempt_cost_micros(start.price, discount_bp, start.request)
        .map_err(|err| Outbound::Fatal {
            channel: start.channel.name.clone(),
            status: 500,
            message: err,
        })?;
    let recovery_metadata = serde_json::to_vec(&store::settlement::BillingAttemptRecovery {
        token_name: start.token.name.clone(),
        model: start.request.model.clone(),
        outbound_model: outbound_model_for_log(start.channel, start.routed_model)
            .map(str::to_string),
        channel: start.channel.name.clone(),
        channel_key: Some(start.channel_key.name.clone()),
        inbound_protocol: protocol_name(start.inbound_protocol).to_string(),
        started: start.started,
        price: start.price,
        discount_bp,
        request_body: start.request_body.as_ref().map(|body| body.to_vec()),
        result: None,
        result_settlement_error: None,
        // 预留建立时尚未发送，可达性未知；结果入队时按实际发送情况覆写。
        upstream_reached: true,
    })
    .map_err(|err| Outbound::Fatal {
        channel: start.channel.name.clone(),
        status: 500,
        message: format!("计费恢复元数据无法编码: {err}"),
    })?;
    let attempt_id = new_request_id();
    let reserved = tokio::time::timeout_at(
        start.deadline,
        store::settlement::reserve_billing_attempt(
            &start.deps.pool,
            store::settlement::BillingAttemptReservation {
                attempt_id: &attempt_id,
                request_id: start.request_id,
                token_key: &start.token.token_key,
                user_id: start.token.user_id,
                cost_usd_micros: reserved_cost,
                token_limit_usd_micros: start.token.limit_usd_micros,
                recovery_metadata: &recovery_metadata,
            },
        ),
    )
    .await
    .map_err(|_| Outbound::Fatal {
        channel: start.channel.name.clone(),
        status: 504,
        message: "计费预留持久化超时".to_string(),
    })?
    .map_err(|err| Outbound::Fatal {
        channel: start.channel.name.clone(),
        status: 500,
        message: format!("计费预留无法持久化: {err}"),
    })?;
    if !reserved {
        // 计费拒绝发生在出站之前，属于下游钱包/令牌额度域而非渠道故障：
        // 以独立变体上抛，避免被当成上游 402 记入渠道冷却。文案带上预留
        // 金额与口径——余额充足但低于保守预留的合法请求也能理解拒绝原因。
        return Err(Outbound::BillingDenied {
            channel: start.channel.name.clone(),
            message: format!(
                "余额或令牌累计上限不足以覆盖本次出站尝试的预检冻结额度 {}（按请求字节与缺省输出上限保守估算；实际费用按用量结算，结算后释放差额）",
                crate::gateway::admin::format_usd_micros(reserved_cost)
            ),
        });
    }
    Ok(BillingAttempt { attempt_id, start })
}

/// 从一次出站调用上下文构造物理尝试账务记录。
///
/// 所有协议路径都经过这个组合点创建预留，保证请求标识、用户归属、价格快照
/// 和恢复元数据的字段保持一致；具体发送时机仍由调用方显式标记 dispatched。
pub(super) async fn begin_attempt_for_call<'a>(
    ctx: &'a OutboundCall<'a>,
    request: &'a ChatRequest,
    channel: &'a Channel,
    key: &'a StoredChannelKey,
    price: PriceSnapshot,
) -> Result<BillingAttempt<'a>, Outbound> {
    begin_billing_attempt(BillingAttemptStart {
        deps: ctx.deps,
        snapshot: ctx.snapshot,
        request_id: ctx.request_id,
        request,
        token: ctx.token,
        routed_model: ctx.routed_model,
        channel,
        channel_key: key,
        started: ctx.started,
        price,
        inbound_protocol: ctx.inbound_protocol,
        request_body: ctx.request_body.clone(),
        ever_dispatched: ctx.ever_dispatched,
        deadline: ctx.deadline,
    })
    .await
}

/// 长流预留心跳间隔：远小于恢复任务的孤儿阈值，流任务存活期间持续刷新
/// 预留行的 `updated_at`，使恢复扫描始终视为新鲜。
const BILLING_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// 为仍在消费中的流式尝试启动预留心跳。
///
/// 流建立后的上游消费不受请求总时限约束，可能远超恢复任务的孤儿阈值；心跳
/// 以固定间隔把预留行 `updated_at` 推进到当前时刻，避免长流被误判为孤儿而
/// 释放预留。预留离开待恢复状态时心跳自行退出；任务结束时由调用方 abort。
/// 单次刷新失败只记 warn，不阻塞流转发。
pub(super) fn spawn_reservation_heartbeat(
    deps: &Deps,
    attempt_id: &str,
) -> tokio::task::JoinHandle<()> {
    let pool = deps.pool.clone();
    let attempt_id = attempt_id.to_string();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(BILLING_HEARTBEAT_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // interval 的首次 tick 立即完成：跳过它，让首次刷新发生在一个完整间隔之后。
        interval.tick().await;
        loop {
            interval.tick().await;
            match store::settlement::touch_billing_attempt_heartbeat(&pool, &attempt_id).await {
                Ok(true) => {}
                // 预留已终结（已结算/已释放/结果已入队）：无需继续心跳。
                Ok(false) => break,
                Err(err) => tracing::warn!(
                    error = %err,
                    attempt_id = %attempt_id,
                    "长流预留心跳刷新失败"
                ),
            }
        }
    })
}
