//! 流式出站的共享任务基础设施：探测、流水与结算。
//!
//! `StreamTask` 承载流式转换任务的计费与日志上下文；首块探测判定可重试
//! 错误，后台流水任务逐帧转换并兜底结算。

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use axum::response::sse::Event as SseEvent;
use bytes::Bytes;
use futures_util::Stream;
use serde_json::Value;
use tokio::sync::OwnedSemaphorePermit;

use crate::{
    config::Protocol,
    core::billing::PriceSnapshot,
    core::ir::{StreamEvent, Usage},
    core::stream::SseFrame,
    runtime::RuntimeSnapshot,
    store::resources::{Channel, Token},
};

use super::attempt::spawn_reservation_heartbeat;
use super::{Deps, channel_idle, outbound_model_for_log, remaining_timeout};
use crate::gateway::logging::{Billing, RequestLogDraft, queue_request_log};
use crate::gateway::sse::{data_frame_to_wire, event_from_frame, frame_to_wire, take_frame};

use crate::gateway::{protocol, routing};

/// 流式请求的共享任务数据：出站目标、计费与日志所需的请求侧信息。
pub(super) struct StreamTask {
    pub(super) deps: Deps,
    pub(super) snapshot: Arc<RuntimeSnapshot>,
    pub(super) token: Token,
    pub(super) request_model: String,
    pub(super) routed_model: String,
    pub(super) channel: Channel,
    pub(super) channel_key_name: String,
    /// 别名命中时入站模型名（用于重写响应模型名）；`None` 表示不覆盖。
    pub(super) inbound_model: Option<String>,
    /// 请求侧转换的信息损失，以 `stream-start` 事件在流首下发。
    pub(super) request_warnings: Vec<crate::core::ir::Warning>,
    pub(super) status_code: u16,
    pub(super) started: i64,
    pub(super) price: PriceSnapshot,
    /// 入站 wire 协议：响应重编码按此分派。
    pub(super) inbound_protocol: Protocol,
    pub(super) request_body: Option<Bytes>,
    pub(super) response_body: Vec<u8>,
    pub(super) request_id: String,
    /// 本流对应的实际出站尝试；与同一入站请求的其它重试互不覆盖。
    pub(super) billing_attempt_id: String,
    /// 首块 peek 阶段缓存的原始上游帧，流水任务开头重放（peek 用独立解码器
    /// 消费过，重放还原同一事件序列与解码器状态）。
    pub(super) peeked: Vec<Value>,
    /// 流任务持有 permit 直到上游消费结束、费用日志入队完成。
    pub(super) _active_permit: Option<OwnedSemaphorePermit>,
    /// 流内读取与下发不再受请求总时限约束（仅受渠道空闲超时约束）；本字段
    /// 只作为结算日志持久化的上界预算，长流结束时从当下重新起算。
    pub(super) log_deadline: tokio::time::Instant,
}

/// 上游 SSE 字节流的装箱形态：peek 与流水任务间传递所有权。
pub(super) type UpstreamByteStream =
    Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>;

/// 流首 peek 的结果。
#[derive(Debug)]
pub(super) enum PeekHead {
    /// 已见首个内容帧或正常收尾事件（空应答）：缓存帧交流水任务重放。
    Content(Vec<Value>),
    /// 首块前流内错误（如 overloaded_error），按可重试语义上抛。
    UpstreamError(String),
    /// 流未产出内容即中断：空流、EOF、读错误、首帧空闲超时或缺 `message_start`。
    Interrupted(String),
}

/// 事件是否产出下游可见内容。
pub(super) fn is_content_event(event: &StreamEvent) -> bool {
    matches!(
        event,
        StreamEvent::TextStart { .. }
            | StreamEvent::TextDelta { .. }
            | StreamEvent::TextEnd { .. }
            | StreamEvent::ReasoningStart { .. }
            | StreamEvent::ReasoningDelta { .. }
            | StreamEvent::ReasoningEnd { .. }
            | StreamEvent::ToolInputStart { .. }
            | StreamEvent::ToolInputDelta { .. }
            | StreamEvent::ToolInputEnd { .. }
            | StreamEvent::ToolCall { .. }
    )
}

/// spawn 流任务前 peek 首块：读帧并解码，直到首个内容帧（或正常 Finish）。
///
/// 首块前的流内错误、空流、首帧空闲超时与 Anthropic 流缺 `message_start`
/// 都在此归类为可重试——此刻尚未向下游转发任何帧，换渠道零痕迹。已读的
/// 原始帧随 [`PeekHead::Content`] 返回，由流任务重放；未读完的字节流原样
/// 交回，剩余部分由流水任务继续消费。
#[cfg(test)]
async fn peek_stream_head(
    byte_stream: UpstreamByteStream,
    protocol: Protocol,
    idle: Duration,
    max_bytes: usize,
) -> (PeekHead, UpstreamByteStream) {
    let deadline = tokio::time::Instant::now()
        .checked_add(Duration::from_millis(
            crate::store::resources::DEFAULT_REQUEST_TIMEOUT_MS,
        ))
        .unwrap_or_else(tokio::time::Instant::now);
    peek_stream_head_until(byte_stream, protocol, idle, max_bytes, deadline).await
}

pub(super) async fn peek_stream_head_until(
    byte_stream: UpstreamByteStream,
    protocol: Protocol,
    idle: Duration,
    max_bytes: usize,
    deadline: tokio::time::Instant,
) -> (PeekHead, UpstreamByteStream) {
    use futures_util::StreamExt as _;

    let mut byte_stream = byte_stream;
    let mut decoder = protocol::make_decoder(protocol);
    let mut buffer: Vec<u8> = Vec::new();
    let mut peeked: Vec<Value> = Vec::new();
    let mut peeked_bytes = 0usize;
    // Anthropic 的 message_start 对应 IR ResponseMetadata；内容帧先于它出现
    // 属协议破坏。
    let mut saw_message_start = false;
    loop {
        if let Some((event_name, frame)) = take_frame(&mut buffer) {
            if frame.is_empty() {
                continue;
            }
            peeked_bytes = peeked_bytes.saturating_add(frame.len());
            if peeked_bytes > max_bytes {
                return (
                    PeekHead::Interrupted(SSE_REASSEMBLY_OVERFLOW_MESSAGE.to_string()),
                    byte_stream,
                );
            }
            // 帧以 UTF-8 文本交给解码器直解 wire 类型（丢失字节的坏帧按
            // 解码失败留痕跳过，与原 Null 行为一致：不因个别坏帧中断 peek）。
            let frame_text = String::from_utf8_lossy(&frame);
            // 重放缓冲需要 Value 形态（PeekHead::Content），peek 只触及首块前的
            // 少量帧，此处解析不在整流热路径上。
            let chunk: Value = serde_json::from_str(&frame_text).unwrap_or(Value::Null);
            let decoded = decoder.process(&frame_text);
            let mut content = false;
            if matches!(protocol, Protocol::AnthropicMessages)
                && event_name.as_deref() == Some("message_start")
            {
                // 部分合法 Anthropic 流省略 `message.model`；SSE 事件名仍足以确认流首序言。
                saw_message_start = true;
            }
            for event in &decoded.events {
                match event {
                    StreamEvent::Error { message } => {
                        return (PeekHead::UpstreamError(message.clone()), byte_stream);
                    }
                    // 上游在产出任何内容前就以失败终态收尾（如 Gemini 的
                    // MALFORMED_FUNCTION_CALL）：与直通 peek 同判为上游错误，
                    // 在响应头前换渠道重试，避免 200 建流后中途错误帧——
                    // 同一上游失败不应因下游协议（直通 vs IR）而产生不同结果。
                    StreamEvent::Finish { finish_reason, .. }
                        if finish_reason.unified == crate::core::ir::FinishReasonUnified::Error =>
                    {
                        return (
                            PeekHead::UpstreamError("上游响应以失败状态结束".to_string()),
                            byte_stream,
                        );
                    }
                    // 空应答的正常收尾：合法流，交由流任务照常转发。
                    StreamEvent::Finish { .. } => content = true,
                    StreamEvent::ResponseMetadata { .. } => saw_message_start = true,
                    _ if is_content_event(event) => {
                        if matches!(protocol, Protocol::AnthropicMessages) && !saw_message_start {
                            return (
                                PeekHead::Interrupted("上游流缺少 message_start".to_string()),
                                byte_stream,
                            );
                        }
                        content = true;
                    }
                    _ => {}
                }
            }
            // 首帧只要已经形成一个非空但无法映射为 IR 事件的载荷，就视为
            // 已有上游字节可见；此后断流不能再安全地切换渠道。未知事件仍
            // 交给流水阶段按既有协议策略处理。
            if decoded.events.is_empty() {
                content = true;
            }
            peeked.push(chunk);
            if content {
                return (PeekHead::Content(peeked), byte_stream);
            }
            continue;
        }
        match tokio::time::timeout(
            remaining_timeout(deadline, idle),
            byte_stream.as_mut().next(),
        )
        .await
        {
            Ok(Some(Ok(bytes))) => {
                if buffer.len().saturating_add(bytes.len()) > max_bytes {
                    return (
                        PeekHead::Interrupted(SSE_REASSEMBLY_OVERFLOW_MESSAGE.to_string()),
                        byte_stream,
                    );
                }
                buffer.extend_from_slice(&bytes);
            }
            Ok(Some(Err(_))) => {
                return (
                    PeekHead::Interrupted("上游流读取失败".to_string()),
                    byte_stream,
                );
            }
            Ok(None) => {
                return (
                    PeekHead::Interrupted("上游流未产出内容即结束（空流）".to_string()),
                    byte_stream,
                );
            }
            Err(_) => {
                return (
                    PeekHead::Interrupted("上游流首帧超时".to_string()),
                    byte_stream,
                );
            }
        }
    }
}

/// spawn 流式结算流水任务并挂轻量监视。
///
/// 任务 panic 或异常终止时，下游流只会随 mpsc 通道关闭而静默截断、结算与
/// 日志随之丢失——预留能由恢复任务按零费用释放，缺失的对账日志行无法重建。
/// 监视任务 await 业务 JoinHandle，在 JoinError 时补一条带请求上下文的
/// error 日志供人工对账。
///
/// 监视只观察终态：不调用 abort、不延长业务任务生命周期——业务任务始终是
/// detached 运行，监视的存在不影响其执行与取消语义；监视自身也无人持有
/// 句柄，随业务任务结束后自然退出。
pub(super) fn spawn_piped_stream_task<F>(
    request_id: String,
    billing_attempt_id: String,
    channel: String,
    task: F,
) where
    F: Future<Output = ()> + Send + 'static,
{
    let handle = tokio::spawn(task);
    tokio::spawn(async move {
        if let Err(join_error) = handle.await {
            // panic 载荷尽量还原为文本；取消只可能来自显式 abort（本网关从不
            // 调用），与 panic 一样意味着结算未完成，一律按 error 上报。
            let reason = if join_error.is_panic() {
                let panic = join_error.into_panic();
                panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|value| value.to_string()))
                    .unwrap_or_else(|| "无法识别的 panic 载荷".to_string())
            } else {
                "任务被外部 abort".to_string()
            };
            tracing::error!(
                request_id = %request_id,
                billing_attempt_id = %billing_attempt_id,
                channel = %channel,
                reason = %reason,
                "流式结算任务异常终止：下游流已静默截断且结算日志丢失，请人工对账"
            );
        }
    });
}

/// 把上游 SSE 字节流逐帧解码 → 提取 usage → 重编码，推送到下游通道。
///
/// 每收到一个完整 SSE 数据帧，解码为 IR 流事件；仅保留 finish 携带的 usage，
/// 同时把事件重编码为入站协议 chunk 帧推给下游。上游读取与下游下发均只受
/// 渠道空闲超时约束，不受请求总时限约束。上游流结束时结算并落日志；下游
/// 断连时按渠道 `abort_on_disconnect` 开关决定是否立即取消上游消费。
pub(super) async fn pipe_stream<S>(
    byte_stream: S,
    tx: tokio::sync::mpsc::Sender<SseEvent>,
    mut ctx: StreamTask,
) where
    S: Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    use futures_util::StreamExt as _;

    let mut decoder = protocol::make_decoder(ctx.channel.protocol);
    // 长流心跳：流式消费不受请求总时限约束，期间持续刷新预留行，防止恢复
    // 任务把仍在消费中的长流误判为孤儿。结算完成后终止。
    let reservation_heartbeat = spawn_reservation_heartbeat(&ctx.deps, &ctx.billing_attempt_id);
    // reasoning 兼容输出的自动挡按出站模型名判定：上游回报的思维链是否
    // 以 `delta.reasoning_content` 转发 chat 下游，由该渠道的开关决定。
    let outbound_model = routing::outbound_model(&ctx.channel, &ctx.routed_model);
    let reasoning_content = ctx
        .channel
        .reasoning_output
        .enables_reasoning_content(outbound_model, &ctx.channel.base_url);
    let mut encoder = protocol::make_encoder(
        ctx.inbound_protocol,
        ctx.inbound_model.clone(),
        reasoning_content,
    );
    let terminator = encoder.terminator();
    let mut usage = Usage::default();
    let mut usage_reported = false;
    // 以字节缓冲、帧边界后再转文本：多字节 UTF-8 可能被拆在两个字节块里，
    // 提前转换会截坏字符。
    let mut sse_buffer: Vec<u8> = Vec::new();
    // peek 阶段缓存的首批原始帧先入解码缓冲：重放还原同一事件序列与解码器
    // 状态，随后与实时字节流无缝衔接。
    for chunk in &ctx.peeked {
        sse_buffer.extend_from_slice(format!("data: {chunk}\n\n").as_bytes());
    }
    let mut saw_finish = false;
    let mut downstream_open = true;
    let mut truncated = false;
    // 流内错误：已向下游下发错误帧，终止消费且不再合成 Finish。
    let mut stream_errored = false;
    // 断连即取消：下游断开且渠道开关开启时立即停止上游消费。
    let mut aborted = false;
    let idle = channel_idle(ctx.channel.timeout_ms);
    let mut byte_stream = Box::pin(byte_stream);

    // 流首先下发 message_start（Anthropic 需要；OpenAI 无）与 stream-start 的
    // warnings，让下游在任何内容之前就感知信息损失（跨协议族丢弃的 reasoning 等）。
    if let Some(frame) = encoder.message_start() {
        record_frame_wire(&mut ctx, &frame);
        if !send_to_downstream(&tx, event_from_frame(&frame), idle).await {
            downstream_open = false;
            aborted = ctx.channel.abort_on_disconnect;
        }
    }
    let start_event = StreamEvent::StreamStart {
        warnings: ctx.request_warnings.clone(),
    };
    for frame in encoder.encode(&start_event) {
        if downstream_open {
            record_frame_wire(&mut ctx, &frame);
        }
        if !send_to_downstream(&tx, event_from_frame(&frame), idle).await {
            downstream_open = false;
            if ctx.channel.abort_on_disconnect {
                aborted = true;
                break;
            }
            break;
        }
    }

    loop {
        if aborted {
            break;
        }
        // 尝试从已缓冲字节提取完整 SSE 数据帧。
        if let Some((_event_name, frame)) = take_frame(&mut sse_buffer) {
            // 空载荷帧（keep-alive 注释、[DONE] 哨兵）直接消费。
            if frame.is_empty() {
                continue;
            }
            // 帧以 UTF-8 文本交给解码器直解 wire 类型；usage 嗅探先经子串
            // 门控再由适配器按需物化（chat/Gemini 只建 usage 子树，不整树
            // 建 DOM——流式 delta 帧占绝对多数，整树解析是热路径主要浪费）。
            let frame_text = String::from_utf8_lossy(&frame);
            if frame_text.contains("usage")
                && let Some(sniffed) = protocol::sniff_usage_str(&frame_text, ctx.channel.protocol)
            {
                usage_reported = true;
                usage.union_max(sniffed);
            }
            let decoded = decoder.process(&frame_text);
            for event in &decoded.events {
                let mut suppress_event = false;
                if let StreamEvent::Finish {
                    finish_reason,
                    usage: reported,
                    ..
                } = event
                {
                    // Finish 可能由 provider 的 failed/incomplete 事件产生。
                    // 这不是正常收尾：保留已观察 usage，但向下游发错误帧，
                    // 不再补成功终止哨兵，也不把 200 + failed 伪装成 stop。
                    usage.union_max(reported.clone());
                    if finish_reason.unified == crate::core::ir::FinishReasonUnified::Error {
                        ctx.status_code = 502;
                        stream_errored = true;
                        suppress_event = true;
                        if downstream_open {
                            let frame = inbound_stream_error_frame(
                                ctx.inbound_protocol,
                                "上游响应以失败状态结束",
                            );
                            record_frame_wire(&mut ctx, &frame);
                            if !send_to_downstream(&tx, event_from_frame(&frame), idle).await {
                                downstream_open = false;
                                if ctx.channel.abort_on_disconnect {
                                    aborted = true;
                                }
                            }
                        }
                    } else {
                        saw_finish = true;
                    }
                }
                if suppress_event {
                    break;
                }
                if downstream_open {
                    for frame in encoder.encode(event) {
                        record_frame_wire(&mut ctx, &frame);
                        if !send_to_downstream(&tx, event_from_frame(&frame), idle).await {
                            // 下游断开：停止发送；开关开启时立即取消上游消费，
                            // 关闭则维持原语义，继续消费上游直至结算。
                            downstream_open = false;
                            if ctx.channel.abort_on_disconnect {
                                aborted = true;
                            }
                            break;
                        }
                    }
                }
                if matches!(event, StreamEvent::Error { .. }) {
                    stream_errored = true;
                    break;
                }
            }
            if aborted || stream_errored {
                break;
            }
            continue;
        }

        // 缓冲不足一帧：从上游读取更多字节。
        match tokio::time::timeout(idle, byte_stream.next()).await {
            Ok(Some(Ok(bytes))) => {
                sse_buffer.extend_from_slice(&bytes);
                if sse_buffer.len() > ctx.snapshot.sse_reassembly_max() {
                    truncated = true;
                    break;
                }
            }
            Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
        }
    }
    // 上游消费已结束（自然收尾或断连取消）：先释放字节流再结算。
    drop(byte_stream);

    if truncated && downstream_open {
        let frame =
            inbound_stream_error_frame(ctx.inbound_protocol, SSE_REASSEMBLY_OVERFLOW_MESSAGE);
        record_frame_wire(&mut ctx, &frame);
        if !send_to_downstream(&tx, event_from_frame(&frame), idle).await {
            downstream_open = false;
        }
    }

    // 流结束：若上游未发 finish 帧（异常中断），不合成成功 Finish——以入站
    // 协议错误帧告知下游截断（缺收尾归类，杜绝「200 + 异常流」被当成完整
    // 成功）。缓冲超限与流内错误已发过各自的错误帧，不再重复。
    if !saw_finish && downstream_open && !truncated && !stream_errored {
        let frame = inbound_stream_error_frame(ctx.inbound_protocol, STREAM_UNTERMINATED_MESSAGE);
        record_frame_wire(&mut ctx, &frame);
        if !send_to_downstream(&tx, event_from_frame(&frame), idle).await {
            downstream_open = false;
        }
    }
    // 终止哨兵也是入站响应的一部分，full_body 开启时先记入再结算，
    // 保证日志带全实际下发的字节（结算仍先于哨兵下发）。
    if downstream_open
        && ctx.snapshot.full_body
        && let Some(terminator) = terminator.as_ref()
    {
        append_logged_body(
            &mut ctx.response_body,
            &data_frame_to_wire(terminator),
            ctx.snapshot.log_body_max(),
        );
    }
    // 先结算再发终止哨兵：下游读到终止时计费必定已落库。
    settle_and_log(&ctx, usage, usage_reported).await;
    reservation_heartbeat.abort();
    // OpenAI 协议约定以 `data: [DONE]` 结束；Anthropic 以 message_stop 收尾。
    if downstream_open && let Some(terminator) = terminator {
        let _ = send_to_downstream(&tx, SseEvent::default().data(terminator), idle).await;
    }
}

/// full_body 开启时把一个即将下发的入站协议帧记入响应字节；关闭时无操作。
pub(super) fn record_frame_wire(ctx: &mut StreamTask, frame: &SseFrame) {
    if ctx.snapshot.full_body {
        append_logged_body(
            &mut ctx.response_body,
            &frame_to_wire(frame),
            ctx.snapshot.log_body_max(),
        );
    }
}

/// 把一帧交给下游响应通道，受渠道空闲超时约束。
///
/// 流建立后下发不再受请求总时限约束：通道背压超过空闲预算同样视为下游
/// 断开，返回 `false` 交由调用方按渠道断连开关决定去留。
pub(super) async fn send_to_downstream<T>(
    tx: &tokio::sync::mpsc::Sender<T>,
    value: T,
    idle: Duration,
) -> bool {
    matches!(tokio::time::timeout(idle, tx.send(value)).await, Ok(Ok(())))
}

/// SSE 重装缓冲超限时写入日志与下游错误事件的固定文案。
pub(super) const SSE_REASSEMBLY_OVERFLOW_MESSAGE: &str = "SSE 重装缓冲超过上限，流已截断";

/// 上游流缺收尾事件（未以 finish 类事件正常结束即中断）时的错误文案。
pub(super) const STREAM_UNTERMINATED_MESSAGE: &str = "上游流未正常收尾，已中断";

/// 把即将落库的响应字节封顶追加，达到 `log_body_max_bytes` 后停止。
pub(super) fn append_logged_body(buf: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) {
    if buf.len() >= max_bytes {
        return;
    }
    let take = (max_bytes - buf.len()).min(chunk.len());
    buf.extend_from_slice(&chunk[..take]);
}

/// 流中途失败时的入站协议错误 SSE 帧，让下游能感知截断。
///
/// 委托到适配器统一形状：流式编码器消费 IR Error 事件产出同一帧。
pub(super) fn inbound_stream_error_frame(protocol: Protocol, message: &str) -> SseFrame {
    protocol::stream_error_frame(protocol, message)
}

/// 结算流式请求费用并落日志。
pub(super) async fn settle_and_log(ctx: &StreamTask, usage: Usage, usage_reported: bool) {
    let discount_bp = ctx.snapshot.discount_bp_for_token(&ctx.token);
    if let Err(err) = queue_request_log(
        &ctx.deps,
        RequestLogDraft {
            token: &ctx.token,
            model: &ctx.request_model,
            outbound_model: outbound_model_for_log(&ctx.channel, &ctx.routed_model),
            channel: &ctx.channel.name,
            channel_key: Some(&ctx.channel_key_name),
            status: ctx.status_code,
            started: ctx.started,
            billing: Billing::calculated(
                usage,
                usage_reported,
                ctx.price,
                discount_bp,
                ctx.request_body.clone(),
                ctx.snapshot.full_body.then(|| ctx.response_body.clone()),
            ),
            inbound_protocol: ctx.inbound_protocol,
            request_id: &ctx.request_id,
            billing_attempt_id: Some(&ctx.billing_attempt_id),
            upstream_reached: true,
            dispatched: true,
            deadline: Some(ctx.log_deadline),
        },
    )
    .await
    {
        tracing::error!(error = %err, request_id = %ctx.request_id, "流式请求结果无法持久化");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_logged_body_stops_at_cap() {
        let mut buf = Vec::new();
        append_logged_body(&mut buf, b"hello ", 8);
        append_logged_body(&mut buf, b"world!!!", 8);
        assert_eq!(buf, b"hello wo");
        append_logged_body(&mut buf, b"more", 8);
        assert_eq!(buf, b"hello wo");
    }

    #[test]
    fn inbound_stream_error_frame_names_responses_event() {
        let chat = inbound_stream_error_frame(Protocol::OpenAiChat, "截断");
        assert!(chat.event.is_none(), "Chat Completions 只用 data 行");
        let responses = inbound_stream_error_frame(Protocol::OpenAiResponses, "截断");
        assert_eq!(responses.event.as_deref(), Some("error"));
        let anthropic = inbound_stream_error_frame(Protocol::AnthropicMessages, "截断");
        assert_eq!(anthropic.event.as_deref(), Some("error"));
    }

    /// 构造 SSE 字节流：每帧为 `data: {json}\n\n`。
    fn sse_stream(frames: Vec<serde_json::Value>) -> super::UpstreamByteStream {
        use futures_util::stream;
        let data: Vec<Result<bytes::Bytes, reqwest::Error>> = frames
            .into_iter()
            .map(|frame| Ok(bytes::Bytes::from(format!("data: {frame}\n\n"))))
            .collect();
        Box::pin(stream::iter(data))
    }

    fn anthropic_message_start() -> serde_json::Value {
        serde_json::json!({
            "type": "message_start",
            "message": { "type": "message", "role": "assistant", "id": "msg_1", "model": "claude-sonnet", "content": [] }
        })
    }

    fn anthropic_text_delta() -> serde_json::Value {
        serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "ok" }
        })
    }

    /// 首块前流内错误归类为 UpstreamError（可重试语义）。
    #[tokio::test]
    async fn peek_classifies_pre_first_chunk_error_as_retryable() {
        let stream = sse_stream(vec![
            anthropic_message_start(),
            serde_json::json!({
                "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" }
            }),
        ]);
        let (peek, _rest) = super::peek_stream_head(
            stream,
            Protocol::AnthropicMessages,
            std::time::Duration::from_millis(100),
            4096,
        )
        .await;
        assert!(matches!(peek, super::PeekHead::UpstreamError(m) if m == "Overloaded"));
    }

    /// 空流（未产出内容即 EOF）归类为 Interrupted。
    #[tokio::test]
    async fn peek_classifies_empty_stream_as_interrupted() {
        let stream = sse_stream(vec![]);
        let (peek, _rest) = super::peek_stream_head(
            stream,
            Protocol::AnthropicMessages,
            std::time::Duration::from_millis(100),
            4096,
        )
        .await;
        assert!(matches!(peek, super::PeekHead::Interrupted(m) if m.contains("空流")));
    }

    /// Anthropic 内容帧先于 message_start 属协议破坏，归类为 Interrupted。
    #[tokio::test]
    async fn peek_classifies_missing_message_start_as_interrupted() {
        let stream = sse_stream(vec![anthropic_text_delta()]);
        let (peek, _rest) = super::peek_stream_head(
            stream,
            Protocol::AnthropicMessages,
            std::time::Duration::from_millis(100),
            4096,
        )
        .await;
        assert!(
            matches!(&peek, super::PeekHead::Interrupted(m) if m.contains("message_start")),
            "缺 message_start 应归类为 Interrupted: {peek:?}"
        );
    }

    /// 正常流：读到首个内容事件（content_block_start）即返回 Content，
    /// 缓存帧含此前全部原始帧，后续帧留给流水任务。
    #[tokio::test]
    async fn peek_stops_at_first_content_event_and_caches() {
        let stream = sse_stream(vec![
            anthropic_message_start(),
            serde_json::json!({
                "type": "content_block_start", "index": 0, "content_block": { "type": "text" }
            }),
            anthropic_text_delta(),
        ]);
        let (peek, mut rest) = super::peek_stream_head(
            stream,
            Protocol::AnthropicMessages,
            std::time::Duration::from_millis(100),
            4096,
        )
        .await;
        match peek {
            super::PeekHead::Content(frames) => assert_eq!(frames.len(), 2),
            other => panic!("应归类为 Content: {other:?}"),
        }
        // 未消费的剩余帧仍可读。
        use futures_util::StreamExt as _;
        let mut remaining = Vec::new();
        while let Some(chunk) = rest.next().await {
            remaining.push(chunk.expect("剩余流应可读"));
        }
        assert_eq!(remaining.len(), 1, "text_delta 帧应留在剩余流中");
    }

    /// 空应答（无内容、直接正常收尾）是合法流：归类为 Content。
    #[tokio::test]
    async fn peek_treats_bare_finish_as_content() {
        let stream = sse_stream(vec![
            anthropic_message_start(),
            serde_json::json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn", "stop_sequence": null },
                "usage": { "input_tokens": 10, "output_tokens": 1 }
            }),
            serde_json::json!({ "type": "message_stop" }),
        ]);
        let (peek, _rest) = super::peek_stream_head(
            stream,
            Protocol::AnthropicMessages,
            std::time::Duration::from_millis(100),
            4096,
        )
        .await;
        assert!(matches!(peek, super::PeekHead::Content(_)));
    }

    /// 首帧探测与流水阶段使用同一字节上限，避免控制帧或超大首帧持续累积。
    #[tokio::test]
    async fn peek_rejects_buffer_overflow() {
        let stream = sse_stream(vec![anthropic_message_start()]);
        let (peek, _rest) = super::peek_stream_head(
            stream,
            Protocol::AnthropicMessages,
            std::time::Duration::from_millis(100),
            16,
        )
        .await;
        assert!(
            matches!(peek, super::PeekHead::Interrupted(message) if message.contains("超过上限"))
        );
    }
}
