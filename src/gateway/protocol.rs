//! 网关协议分派：把渠道出入站 wire 协议映射到对应适配器的编解码与流处理。
//!
//! 网关按渠道 `protocol` 与入站协议挑选适配器行为：出站 URL、认证头、编解码、
//! usage 嗅探、流式帧形态（OpenAI 纯 `data:` + `[DONE]` 哨兵，Anthropic 事件名 +
//! `message_stop` 收尾）。适配器（`core::openai_chat`/`core::anthropic_messages`）
//! 的 wire 类型不出适配器边界，本模块是网关与适配器之间的薄分派层。

use serde_json::Value;

use crate::config::Protocol;
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
        Protocol::OpenAiResponses => Err("OpenAI Responses 协议尚未实现".to_string()),
    }
}

/// 编码 IR 请求为出站渠道协议，转换的信息损失追加到 `warnings`。
pub fn encode_request(
    request: &ChatRequest,
    protocol: Protocol,
    warnings: &mut Vec<Warning>,
) -> Value {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::encode_request(request, warnings),
        Protocol::AnthropicMessages => {
            crate::core::anthropic_messages::encode_request(request, warnings)
        }
        Protocol::OpenAiResponses => unreachable!("OpenAI Responses 出站尚未实现"),
    }
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
        Protocol::OpenAiResponses => Err("OpenAI Responses 协议尚未实现".to_string()),
    }
}

/// 编码 IR 响应为入站协议。
pub fn encode_response(response: &ChatResponse, protocol: Protocol) -> Value {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::encode_response(response),
        Protocol::AnthropicMessages => crate::core::anthropic_messages::encode_response(response),
        Protocol::OpenAiResponses => unreachable!("OpenAI Responses 入站尚未实现"),
    }
}

/// 直通快路径的 usage 嗅探：从单个 SSE 帧或非流式响应体提取 usage 折算为 IR
/// 四分量，供计费，不做完整解码。
pub fn sniff_usage(value: &Value, protocol: Protocol) -> Option<Usage> {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::sniff_chat_usage(value),
        Protocol::AnthropicMessages => crate::core::anthropic_messages::sniff_usage(value),
        Protocol::OpenAiResponses => None,
    }
}

/// 编码为入站协议的错误格式。
pub fn encode_error(status: u16, message: &str, protocol: Protocol) -> Value {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::encode_error(status, message),
        Protocol::AnthropicMessages => {
            crate::core::anthropic_messages::encode_error(status, message)
        }
        Protocol::OpenAiResponses => crate::core::openai_chat::encode_error(status, message),
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
        Protocol::OpenAiResponses => unreachable!("OpenAI Responses 流尚未实现"),
    }
}

/// 按协议构造流式编码器。
pub fn make_encoder(
    protocol: Protocol,
    inbound_model: Option<String>,
) -> Box<dyn ChatStreamEncoder + Send> {
    match protocol {
        Protocol::OpenAiChat => Box::new(OpenAiStreamEncoder(
            crate::core::openai_chat::StreamEncoder::new(inbound_model),
        )),
        Protocol::AnthropicMessages => Box::new(AnthropicStreamEncoder(
            crate::core::anthropic_messages::StreamEncoder::new(inbound_model),
        )),
        Protocol::OpenAiResponses => unreachable!("OpenAI Responses 流尚未实现"),
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
