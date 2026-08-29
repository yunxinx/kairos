//! 网关协议分派：把渠道出入站 wire 协议映射到对应适配器的编解码与流处理。
//!
//! 网关按渠道 `protocol` 与入站协议挑选适配器行为：出站 URL、认证头、编解码、
//! usage 嗅探、流式帧形态（OpenAI 纯 `data:` + `[DONE]` 哨兵，Anthropic 事件名 +
//! `message_stop` 收尾）。适配器（`core::openai_chat`/`core::anthropic_messages`）
//! 的 wire 类型不出适配器边界，本模块是网关与适配器之间的薄分派层。

use serde_json::Value;

use crate::config::{Protocol, SessionCacheKeyMode};
use crate::core::ir::{ChatRequest, ChatResponse, StreamEvent, Usage, Warning};
use crate::core::stream::SseFrame;

/// 解码入站请求为 IR。
pub fn decode_request(value: &Value, protocol: Protocol) -> Result<ChatRequest, String> {
    match protocol {
        Protocol::OpenAiChat => {
            crate::core::openai_chat::decode_request(value).map_err(|e| e.to_string())
        }
        Protocol::AnthropicMessages => {
            crate::core::anthropic_messages::decode_request(value).map_err(|e| e.to_string())
        }
        Protocol::OpenAiResponses => {
            crate::core::openai_responses::decode_request(value).map_err(|e| e.to_string())
        }
    }
}

/// 编码 IR 请求为出站渠道协议，转换的信息损失追加到 `warnings`。
///
/// 渠道级 reasoning 兼容输出缺省开启；渠道关闭该开关时用
/// [`encode_request_with_reasoning`]。
pub fn encode_request(
    request: &ChatRequest,
    protocol: Protocol,
    warnings: &mut Vec<Warning>,
) -> Value {
    encode_request_with_reasoning(request, protocol, true, warnings)
}

/// 按渠道 reasoning 输出开关编码 IR 请求为出站渠道协议。
///
/// 开关仅被 OpenAI Chat 协议消费（`reasoning_content` 是 chat 侧字段），
/// 其余协议忽略。
pub fn encode_request_with_reasoning(
    request: &ChatRequest,
    protocol: Protocol,
    reasoning_content: bool,
    warnings: &mut Vec<Warning>,
) -> Value {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::encode_request_with(
            request,
            crate::core::openai_chat::ChatEncodeOptions { reasoning_content },
            warnings,
        ),
        Protocol::AnthropicMessages => {
            crate::core::anthropic_messages::encode_request(request, warnings)
        }
        Protocol::OpenAiResponses => {
            crate::core::openai_responses::encode_request(request, warnings)
        }
    }
}

/// 会话缓存键回写：按渠道开关把网关解析出的会话标识写为出站请求的
/// `prompt_cache_key`，让跨协议族的多轮请求也获得上游自动缓存的会话亲和。
///
/// 仅 OpenAI Chat 出站消费：`auto` 不覆盖下游显式携带的非空键，`always`
/// 无条件覆盖，`off` 不写。对已编码出站对象做目标性补丁，与适配器的
/// 类型化回写（下游显式值）正交。
pub fn write_session_cache_key(
    outbound: &mut Value,
    protocol: Protocol,
    mode: SessionCacheKeyMode,
    identity: &str,
) {
    if protocol != Protocol::OpenAiChat || mode == SessionCacheKeyMode::Off || identity.is_empty() {
        return;
    }
    let Some(map) = outbound.as_object_mut() else {
        return;
    };
    if mode == SessionCacheKeyMode::Auto {
        let explicit = map
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .is_some_and(|key| !key.trim().is_empty());
        if explicit {
            return;
        }
    }
    map.insert(
        "prompt_cache_key".to_string(),
        Value::String(identity.to_string()),
    );
}

/// 解码上游响应为 IR。
pub fn decode_response(value: &Value, protocol: Protocol) -> Result<ChatResponse, String> {
    match protocol {
        Protocol::OpenAiChat => {
            crate::core::openai_chat::decode_response(value).map_err(|e| e.to_string())
        }
        Protocol::AnthropicMessages => {
            crate::core::anthropic_messages::decode_response(value).map_err(|e| e.to_string())
        }
        Protocol::OpenAiResponses => {
            crate::core::openai_responses::decode_response(value).map_err(|e| e.to_string())
        }
    }
}

/// 编码 IR 响应为入站协议。
pub fn encode_response(response: &ChatResponse, protocol: Protocol) -> Value {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::encode_response(response),
        Protocol::AnthropicMessages => crate::core::anthropic_messages::encode_response(response),
        Protocol::OpenAiResponses => crate::core::openai_responses::encode_response(response),
    }
}

/// 直通快路径的 usage 嗅探：从单个 SSE 帧或非流式响应体提取 usage 折算为 IR
/// 四分量，供计费，不做完整解码。
pub fn sniff_usage(value: &Value, protocol: Protocol) -> Option<Usage> {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::sniff_chat_usage(value),
        Protocol::AnthropicMessages => crate::core::anthropic_messages::sniff_usage(value),
        Protocol::OpenAiResponses => crate::core::openai_responses::sniff_usage(value),
    }
}

/// 编码为入站协议的错误格式。
pub fn encode_error(status: u16, message: &str, protocol: Protocol) -> Value {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::encode_error(status, message),
        Protocol::AnthropicMessages => {
            crate::core::anthropic_messages::encode_error(status, message)
        }
        Protocol::OpenAiResponses => crate::core::openai_responses::encode_error(status, message),
    }
}

/// 流内错误的入站协议 SSE 帧（500 语义）。各适配器流式编码器消费 IR Error
/// 事件走同一形状，网关兜底路径（缓冲超限等）复用本函数。
pub fn stream_error_frame(protocol: Protocol, message: &str) -> SseFrame {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::stream_error_frame(message),
        Protocol::AnthropicMessages => crate::core::anthropic_messages::stream_error_frame(message),
        Protocol::OpenAiResponses => crate::core::openai_responses::stream_error_frame(message),
    }
}

/// 编码下游模型列表为入站协议的标准列出模型响应。
pub fn encode_model_list(ids: &[String], protocol: Protocol) -> Value {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::encode_model_list(ids),
        Protocol::AnthropicMessages => crate::core::anthropic_messages::encode_model_list(ids),
        Protocol::OpenAiResponses => crate::core::openai_responses::encode_model_list(ids),
    }
}

/// 出站渠道的 upstream 路径段（相对 base_url）。
pub fn upstream_path(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenAiChat => "/chat/completions",
        Protocol::AnthropicMessages => "/messages",
        Protocol::OpenAiResponses => "/responses",
    }
}

// ---- 流式解码/编码抽象 ----

/// 单个 chunk 解码结果：IR 事件序列。
pub struct DecodeChunk {
    pub events: Vec<StreamEvent>,
}

/// 流式解码器抽象：把上游 SSE 帧解码为 IR 流事件。
pub trait ChatStreamDecoder {
    fn process(&mut self, value: &Value) -> DecodeChunk;
}

/// 流式编码器抽象：把 IR 流事件还原为入站 SSE 帧。
pub trait ChatStreamEncoder {
    /// 流首帧（Anthropic 的 `message_start`）；无则返回 `None`。
    fn message_start(&self) -> Option<SseFrame>;
    /// 编码一个 IR 流事件为若干 SSE 帧。
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame>;
    /// 流终止哨兵（OpenAI 的 `data: [DONE]`）；Anthropic 以 `message_stop` 收尾，无哨兵。
    fn terminator(&self) -> Option<String>;
}

/// 按协议构造流式解码器。
pub fn make_decoder(protocol: Protocol) -> Box<dyn ChatStreamDecoder + Send> {
    match protocol {
        Protocol::OpenAiChat => Box::new(OpenAiStreamDecoder(
            crate::core::openai_chat::StreamDecoder::default(),
        )),
        Protocol::AnthropicMessages => Box::new(AnthropicStreamDecoder(
            crate::core::anthropic_messages::StreamDecoder::default(),
        )),
        Protocol::OpenAiResponses => Box::new(ResponsesStreamDecoder(
            crate::core::openai_responses::StreamDecoder::default(),
        )),
    }
}

/// 按协议构造流式编码器。
///
/// `reasoning_content` 为渠道级 reasoning 兼容输出开关，仅 OpenAI Chat
/// 编码器消费（ReasoningDelta 以 `delta.reasoning_content` 增量下发）。
pub fn make_encoder(
    protocol: Protocol,
    inbound_model: Option<String>,
    reasoning_content: bool,
) -> Box<dyn ChatStreamEncoder + Send> {
    match protocol {
        Protocol::OpenAiChat => Box::new(OpenAiStreamEncoder(
            crate::core::openai_chat::StreamEncoder::new(inbound_model, reasoning_content),
        )),
        Protocol::AnthropicMessages => Box::new(AnthropicStreamEncoder(
            crate::core::anthropic_messages::StreamEncoder::new(inbound_model),
        )),
        Protocol::OpenAiResponses => Box::new(ResponsesStreamEncoder(
            crate::core::openai_responses::StreamEncoder::new(inbound_model),
        )),
    }
}

struct OpenAiStreamDecoder(crate::core::openai_chat::StreamDecoder);
impl ChatStreamDecoder for OpenAiStreamDecoder {
    fn process(&mut self, value: &Value) -> DecodeChunk {
        DecodeChunk {
            events: self.0.process(value).events,
        }
    }
}

struct AnthropicStreamDecoder(crate::core::anthropic_messages::StreamDecoder);
impl ChatStreamDecoder for AnthropicStreamDecoder {
    fn process(&mut self, value: &Value) -> DecodeChunk {
        DecodeChunk {
            events: self.0.process(value).events,
        }
    }
}

struct ResponsesStreamDecoder(crate::core::openai_responses::StreamDecoder);
impl ChatStreamDecoder for ResponsesStreamDecoder {
    fn process(&mut self, value: &Value) -> DecodeChunk {
        DecodeChunk {
            events: self.0.process(value).events,
        }
    }
}

struct OpenAiStreamEncoder(crate::core::openai_chat::StreamEncoder);
impl ChatStreamEncoder for OpenAiStreamEncoder {
    fn message_start(&self) -> Option<SseFrame> {
        None
    }
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        self.0.encode(event)
    }
    fn terminator(&self) -> Option<String> {
        Some("[DONE]".to_string())
    }
}

struct AnthropicStreamEncoder(crate::core::anthropic_messages::StreamEncoder);
impl ChatStreamEncoder for AnthropicStreamEncoder {
    fn message_start(&self) -> Option<SseFrame> {
        Some(self.0.message_start())
    }
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        self.0.encode(event)
    }
    fn terminator(&self) -> Option<String> {
        None
    }
}

struct ResponsesStreamEncoder(crate::core::openai_responses::StreamEncoder);
impl ChatStreamEncoder for ResponsesStreamEncoder {
    fn message_start(&self) -> Option<SseFrame> {
        None
    }
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        self.0.encode(event)
    }
    fn terminator(&self) -> Option<String> {
        // Responses 以 `response.completed` 事件收尾，无 `[DONE]` 哨兵。
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 回写三态：off 不写；auto 不覆盖下游显式键、缺席时写；always 无条件
    /// 覆盖。非 chat 协议一概不写。
    #[test]
    fn session_cache_key_writeback_follows_mode() {
        let identity = "sess-1";

        let mut outbound = json!({ "model": "gpt-4o", "messages": [] });
        write_session_cache_key(
            &mut outbound,
            Protocol::OpenAiChat,
            SessionCacheKeyMode::Off,
            identity,
        );
        assert!(outbound.get("prompt_cache_key").is_none(), "off 不应回写");

        let mut outbound = json!({ "model": "gpt-4o", "messages": [] });
        write_session_cache_key(
            &mut outbound,
            Protocol::OpenAiChat,
            SessionCacheKeyMode::Auto,
            identity,
        );
        assert_eq!(outbound["prompt_cache_key"], json!("sess-1"));

        let mut outbound = json!({
            "model": "gpt-4o",
            "messages": [],
            "prompt_cache_key": "downstream-key"
        });
        write_session_cache_key(
            &mut outbound,
            Protocol::OpenAiChat,
            SessionCacheKeyMode::Auto,
            identity,
        );
        assert_eq!(
            outbound["prompt_cache_key"],
            json!("downstream-key"),
            "auto 不应覆盖下游显式键"
        );

        let mut outbound = json!({
            "model": "gpt-4o",
            "messages": [],
            "prompt_cache_key": "downstream-key"
        });
        write_session_cache_key(
            &mut outbound,
            Protocol::OpenAiChat,
            SessionCacheKeyMode::Always,
            identity,
        );
        assert_eq!(
            outbound["prompt_cache_key"],
            json!("sess-1"),
            "always 应覆盖下游显式键"
        );

        let mut outbound = json!({ "model": "claude", "messages": [] });
        write_session_cache_key(
            &mut outbound,
            Protocol::AnthropicMessages,
            SessionCacheKeyMode::Always,
            identity,
        );
        assert!(
            outbound.get("prompt_cache_key").is_none(),
            "非 chat 协议不写"
        );
    }
}
