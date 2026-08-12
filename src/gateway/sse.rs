//! SSE transport helpers shared by IR and passthrough streaming paths.

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

/// 从 SSE 缓冲中取出一帧：事件名、数据载荷与剩余缓冲。
///
/// SSE 帧以 `\\n\\n` 或 `\\r\\n\\r\\n` 分隔。keep-alive、空数据和 `[DONE]`
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
    use super::take_frame;

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
