//! SSE 传输辅助：直通快路径与 IR 流式路径共用的通道适配、帧解析与帧序列化。

use axum::response::sse::Event as SseEvent;
use futures_util::Stream;

use crate::core::stream::SseFrame;

/// 把 tokio mpsc 接收端适配为 axum SSE 可消费的流。
pub(super) fn receiver_stream(
    mut rx: tokio::sync::mpsc::Receiver<SseEvent>,
) -> impl Stream<Item = Result<SseEvent, std::convert::Infallible>> + Send + 'static {
    async_stream::stream! {
        while let Some(event) = rx.recv().await {
            yield Ok(event);
        }
    }
}

/// 把适配器产出的 SSE 帧转为 axum SSE 事件。
pub(super) fn event_from_frame(frame: &SseFrame) -> SseEvent {
    let event = SseEvent::default().data(&frame.data);
    match &frame.event {
        Some(name) => event.event(name),
        None => event,
    }
}

/// 把一个 SSE 帧序列化为 wire 文本字节，供 full_body 记录实际下发的入站响应。
///
/// 格式与 axum `SseEvent` 的序列化一致：可选 `event: <名>` 行 + 每行 data 一个
/// `data: <行>` + 空行收尾。
pub(super) fn frame_to_wire(frame: &SseFrame) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(name) = &frame.event {
        out.extend_from_slice(b"event: ");
        out.extend_from_slice(name.as_bytes());
        out.push(b'\n');
    }
    append_data_lines(&mut out, &frame.data);
    out.push(b'\n');
    out
}

/// 把纯 data 帧（如 `[DONE]` 终止哨兵）序列化为 wire 文本字节。
pub(super) fn data_frame_to_wire(data: &str) -> Vec<u8> {
    let mut out = Vec::new();
    append_data_lines(&mut out, data);
    out.push(b'\n');
    out
}

/// 按 SSE 规范逐行写 `data:` 字段：载荷内换行拆为多个 data 行。
fn append_data_lines(out: &mut Vec<u8>, data: &str) {
    for line in data.split('\n') {
        out.extend_from_slice(b"data: ");
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
}

/// 从 SSE 缓冲中取出一帧：事件名、数据载荷与剩余缓冲。
///
/// SSE 帧以 `\n\n` 或 `\r\n\r\n` 分隔。keep-alive、空数据和 `[DONE]`
/// 哨兵会被消费，但返回空载荷。全程字节操作，避免跨块 UTF-8 被截坏。
pub(super) fn take_frame(buffer: &[u8]) -> Option<(Option<String>, Vec<u8>, Vec<u8>)> {
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4));
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2));
    let (end, separator_len) = match (crlf, lf) {
        (Some(first), Some(second)) => first.min(second),
        (Some(frame), None) | (None, Some(frame)) => frame,
        (None, None) => return None,
    };
    let frame_text = &buffer[..end];
    let rest = buffer[end + separator_len..].to_vec();

    let mut event_name = None;
    let mut data_lines = Vec::new();
    for line in frame_text.split(|&byte| byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(name) = line.strip_prefix(b"event:") {
            event_name = Some(String::from_utf8_lossy(name.trim_ascii()).into_owned());
        } else if let Some(data) = line.strip_prefix(b"data:") {
            let data = data.trim_ascii();
            if !data.is_empty() && data != b"[DONE]" {
                data_lines.push(data);
            }
        }
    }
    Some((event_name, data_lines.join(&b'\n'), rest))
}

#[cfg(test)]
mod tests {
    use super::{data_frame_to_wire, frame_to_wire, take_frame};
    use crate::core::stream::SseFrame;

    #[test]
    fn frame_to_wire_matches_sse_format() {
        let frame = SseFrame {
            event: Some("message_delta".to_string()),
            data: r#"{"ok":true}"#.to_string(),
        };
        assert_eq!(
            frame_to_wire(&frame),
            b"event: message_delta\ndata: {\"ok\":true}\n\n"
        );

        let unnamed = SseFrame {
            event: None,
            data: "first\nsecond".to_string(),
        };
        assert_eq!(frame_to_wire(&unnamed), b"data: first\ndata: second\n\n");
    }

    #[test]
    fn data_frame_to_wire_wraps_terminator() {
        assert_eq!(data_frame_to_wire("[DONE]"), b"data: [DONE]\n\n");
    }

    #[test]
    fn parses_event_and_data_with_crlf() {
        let input = b"event: message\r\ndata: {\"ok\":true}\r\n\r\nrest";
        let (event, data, rest) = take_frame(input).expect("应解析完整 SSE 帧");
        assert_eq!(event.as_deref(), Some("message"));
        assert_eq!(data, br#"{"ok":true}"#);
        assert_eq!(rest, b"rest");
    }

    #[test]
    fn joins_multiple_data_lines_and_consumes_done() {
        let input = b"data: first\ndata: second\n\n";
        let (_, data, rest) = take_frame(input).expect("应解析多行 data");
        assert_eq!(data, b"first\nsecond");
        assert!(rest.is_empty());

        let (_, done, _) = take_frame(b"data: [DONE]\n\n").expect("应消费 DONE 帧");
        assert!(done.is_empty());
    }

    #[test]
    fn incomplete_frame_is_left_buffered() {
        assert!(take_frame(b"event: message\ndata: partial").is_none());
    }
}
