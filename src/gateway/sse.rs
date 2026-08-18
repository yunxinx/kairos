//! SSE 传输辅助：直通快路径与 IR 流式路径共用的通道适配、帧解析与帧序列化。

use axum::response::sse::Event as SseEvent;
use bytes::Bytes;
use futures_util::Stream;

use crate::core::stream::SseFrame;

/// 把 tokio mpsc 接收端适配为 axum 响应体可消费的流。
pub(super) fn receiver_stream<T>(
    mut rx: tokio::sync::mpsc::Receiver<T>,
) -> impl Stream<Item = Result<T, std::convert::Infallible>> + Send + 'static
where
    T: Send + 'static,
{
    async_stream::stream! {
        while let Some(item) = rx.recv().await {
            yield Ok(item);
        }
    }
}

/// OpenAI 原始字节流的终止哨兵过滤器。
///
/// 正常 `data:` 行一旦确定不可能是 `[DONE]` 就立即释放短前缀，后续字节仍由原
/// `Bytes` 切片承载。只有可能构成哨兵的数据行与尚未判定的事件头会缓冲；独立
/// 哨兵事件整帧丢弃，多行事件中的哨兵数据行单独丢弃。
#[derive(Default)]
pub(super) struct OpenAiDoneFilter {
    event_prefix: Vec<u8>,
    line_prefix: Vec<u8>,
    event_phase: EventPhase,
    line_phase: LinePhase,
}

#[derive(Default)]
enum EventPhase {
    #[default]
    Pending,
    PendingDone,
    Forwarding,
    ForwardingAfterDone,
}

#[derive(Default)]
enum LinePhase {
    #[default]
    Classifying,
    Forwarding,
}

impl OpenAiDoneFilter {
    pub(super) fn push(&mut self, chunk: Bytes) -> Vec<Bytes> {
        let mut forwarded = Vec::new();
        let mut cursor = 0;

        while cursor < chunk.len() {
            if matches!(self.line_phase, LinePhase::Forwarding) {
                if let Some(line_end) = chunk[cursor..].iter().position(|&byte| byte == b'\n') {
                    let end = cursor + line_end + 1;
                    forwarded.push(chunk.slice(cursor..end));
                    cursor = end;
                    self.line_phase = LinePhase::Classifying;
                } else {
                    forwarded.push(chunk.slice(cursor..));
                    break;
                }
                continue;
            }

            let byte = chunk[cursor];
            self.line_prefix.push(byte);
            cursor += 1;

            if byte == b'\n' {
                self.finish_line(&mut forwarded);
            } else if is_definitely_non_done_data(&self.line_prefix) {
                self.forward_non_done_prefix(&mut forwarded);
                self.line_phase = LinePhase::Forwarding;
            }
        }

        forwarded
    }

    /// 刷新非哨兵尾部；若上游事件未闭合，补分隔符以保证网关哨兵独立成帧。
    pub(super) fn finish(&mut self) -> Vec<Bytes> {
        let mut forwarded = Vec::new();
        let line_is_done = is_done_data_line(&self.line_prefix);
        let has_forwarded_data = matches!(
            self.event_phase,
            EventPhase::Forwarding | EventPhase::ForwardingAfterDone
        );
        let should_drop_event = !has_forwarded_data
            && (line_is_done || matches!(self.event_phase, EventPhase::PendingDone));

        if should_drop_event {
            self.event_prefix.clear();
            self.line_prefix.clear();
            self.reset_event();
            return forwarded;
        }

        let has_open_event = has_forwarded_data
            || !self.event_prefix.is_empty()
            || !self.line_prefix.is_empty()
            || matches!(self.line_phase, LinePhase::Forwarding);
        if !line_is_done {
            let mut trailing = std::mem::take(&mut self.event_prefix);
            trailing.extend_from_slice(&self.line_prefix);
            if !trailing.is_empty() {
                forwarded.push(Bytes::from(trailing));
            }
        }
        self.line_prefix.clear();
        if has_open_event {
            let separator = if matches!(self.line_phase, LinePhase::Forwarding)
                || forwarded
                    .last()
                    .is_some_and(|trailing| !trailing.ends_with(b"\n"))
            {
                Bytes::from_static(b"\n\n")
            } else if forwarded
                .last()
                .is_some_and(|trailing| trailing.ends_with(b"\r\n"))
            {
                // 尾部以 CRLF 行终止：按上游风格补 CRLF 空行，不改写换行字节。
                Bytes::from_static(b"\r\n")
            } else {
                Bytes::from_static(b"\n")
            };
            forwarded.push(separator);
        }
        self.reset_event();
        forwarded
    }

    fn finish_line(&mut self, forwarded: &mut Vec<Bytes>) {
        if is_blank_line(&self.line_prefix) {
            if matches!(
                self.event_phase,
                EventPhase::Forwarding | EventPhase::ForwardingAfterDone
            ) {
                forwarded.push(Bytes::from(std::mem::take(&mut self.line_prefix)));
            } else if matches!(self.event_phase, EventPhase::Pending) {
                self.event_prefix.extend_from_slice(&self.line_prefix);
                self.line_prefix.clear();
                forwarded.push(Bytes::from(std::mem::take(&mut self.event_prefix)));
            } else {
                self.line_prefix.clear();
                self.event_prefix.clear();
            }
            self.reset_event();
        } else if is_done_data_line(&self.line_prefix) {
            self.event_phase = match self.event_phase {
                EventPhase::Pending | EventPhase::PendingDone => EventPhase::PendingDone,
                EventPhase::Forwarding | EventPhase::ForwardingAfterDone => {
                    EventPhase::ForwardingAfterDone
                }
            };
            self.line_prefix.clear();
        } else if matches!(
            self.event_phase,
            EventPhase::Forwarding | EventPhase::ForwardingAfterDone
        ) {
            forwarded.push(Bytes::from(std::mem::take(&mut self.line_prefix)));
        } else {
            self.event_prefix.extend_from_slice(&self.line_prefix);
            self.line_prefix.clear();
        }
    }

    fn forward_non_done_prefix(&mut self, forwarded: &mut Vec<Bytes>) {
        if matches!(
            self.event_phase,
            EventPhase::Pending | EventPhase::PendingDone
        ) {
            self.event_prefix.extend_from_slice(&self.line_prefix);
            self.line_prefix.clear();
            forwarded.push(Bytes::from(std::mem::take(&mut self.event_prefix)));
            self.event_phase = match std::mem::take(&mut self.event_phase) {
                EventPhase::Pending => EventPhase::Forwarding,
                EventPhase::PendingDone => EventPhase::ForwardingAfterDone,
                forwarding @ (EventPhase::Forwarding | EventPhase::ForwardingAfterDone) => {
                    forwarding
                }
            };
        } else {
            forwarded.push(Bytes::from(std::mem::take(&mut self.line_prefix)));
        }
    }

    fn reset_event(&mut self) {
        self.event_phase = EventPhase::Pending;
        self.line_phase = LinePhase::Classifying;
    }
}

fn is_blank_line(line: &[u8]) -> bool {
    line == b"\n" || line == b"\r\n"
}

fn is_done_data_line(line: &[u8]) -> bool {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    line.strip_prefix(b"data:")
        .is_some_and(|data| data.trim_ascii() == b"[DONE]")
}

fn is_definitely_non_done_data(line: &[u8]) -> bool {
    let Some(data) = line.strip_prefix(b"data:") else {
        return false;
    };
    let data = data.trim_ascii_start();
    if data.is_empty() || b"[DONE]".starts_with(data) {
        return false;
    }
    !(data.starts_with(b"[DONE]") && data[b"[DONE]".len()..].trim_ascii().is_empty())
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

/// 从 SSE 缓冲中取出一帧：事件名与数据载荷；已消费字节从 `buffer` 头部 drain。
///
/// SSE 帧以 `\n\n` 或 `\r\n\r\n` 分隔。keep-alive、空数据和 `[DONE]`
/// 哨兵会被消费，但返回空载荷。全程字节操作，避免跨块 UTF-8 被截坏。
pub(super) fn take_frame(buffer: &mut Vec<u8>) -> Option<(Option<String>, Vec<u8>)> {
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
    let frame_text = buffer[..end].to_vec();
    buffer.drain(..end + separator_len);

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
    Some((event_name, data_lines.join(&b'\n')))
}

#[cfg(test)]
mod tests {
    use super::{OpenAiDoneFilter, data_frame_to_wire, frame_to_wire, take_frame};
    use crate::core::stream::SseFrame;

    /// EOF 未闭合的 CRLF 事件：补的空行保持 CRLF 风格，不改写换行字节。
    #[test]
    fn done_filter_finish_preserves_crlf_blank_line() {
        let mut filter = OpenAiDoneFilter::default();
        let forwarded = filter.push(bytes::Bytes::from_static(b"event: ping\r\n"));
        assert!(forwarded.is_empty(), "未判定事件头应先缓冲");
        let tail: Vec<u8> = filter.finish().into_iter().flatten().collect();
        assert_eq!(tail, b"event: ping\r\n\r\n", "应以 CRLF 风格补空行");
    }

    /// EOF 未闭合的普通事件：已接收字节直搬后补 LF 分隔，哨兵不粘连。
    #[test]
    fn done_filter_finish_separates_unclosed_lf_event() {
        let mut filter = OpenAiDoneFilter::default();
        let mut out: Vec<u8> = filter
            .push(bytes::Bytes::from_static(b"event: custom\ndata: partial"))
            .into_iter()
            .flatten()
            .collect();
        out.extend(filter.finish().into_iter().flatten());
        assert_eq!(out, b"event: custom\ndata: partial\n\n");
    }

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
        let mut input = b"event: message\r\ndata: {\"ok\":true}\r\n\r\nrest".to_vec();
        let (event, data) = take_frame(&mut input).expect("应解析完整 SSE 帧");
        assert_eq!(event.as_deref(), Some("message"));
        assert_eq!(data, br#"{"ok":true}"#);
        assert_eq!(input, b"rest");
    }

    #[test]
    fn joins_multiple_data_lines_and_consumes_done() {
        let mut input = b"data: first\ndata: second\n\n".to_vec();
        let (_, data) = take_frame(&mut input).expect("应解析多行 data");
        assert_eq!(data, b"first\nsecond");
        assert!(input.is_empty());

        let mut done = b"data: [DONE]\n\n".to_vec();
        let (_, payload) = take_frame(&mut done).expect("应消费 DONE 帧");
        assert!(payload.is_empty());
    }

    #[test]
    fn incomplete_frame_is_left_buffered() {
        let mut input = b"event: message\ndata: partial".to_vec();
        assert!(take_frame(&mut input).is_none());
        assert_eq!(input, b"event: message\ndata: partial");
    }
}
