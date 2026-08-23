//! 适配器单元测试共享的断言支撑，仅在 `cfg(test)` 下编译。
//!
//! 三个适配器的流式编码器都产出 [`SseFrame`] 序列，此前各自复制了一份把
//! `data:` 载荷解析成 JSON 的辅助函数，再逐字段断言。这里把「帧序列 → 可快照
//! 值」的规范化收敛为唯一声明，适配器测试只消费不重复实现。

use serde_json::{Value, json};

use super::stream::SseFrame;

/// 把流式编码器产出的帧序列规范化为可快照的 JSON 值。
///
/// 事件名与载荷一并保留：Anthropic 协议靠 `event:` 区分帧类型，OpenAI 协议则
/// 恒为 `null`，两者的差异本身就是需要被快照锁住的契约。
///
/// # Panics
///
/// 帧载荷不是合法 JSON 时 panic——编码器产出非法 JSON 属于测试要暴露的缺陷。
pub(crate) fn frames_to_snapshot(frames: &[SseFrame]) -> Value {
    Value::Array(
        frames
            .iter()
            .map(|frame| {
                json!({
                    "event": frame.event,
                    "data": serde_json::from_str::<Value>(&frame.data)
                        .expect("帧载荷应为合法 JSON"),
                })
            })
            .collect(),
    )
}

/// 解析单个帧的 `data:` 载荷为 JSON。
///
/// 供只关心某一帧内某个字段的定点断言使用；需要锁住整条序列时用
/// [`frames_to_snapshot`]。
///
/// # Panics
///
/// 帧载荷不是合法 JSON 时 panic，理由同 [`frames_to_snapshot`]。
pub(crate) fn frame_payload(frame: &SseFrame) -> Value {
    serde_json::from_str(&frame.data).expect("帧载荷应为合法 JSON")
}
