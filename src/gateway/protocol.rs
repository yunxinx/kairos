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
        Protocol::Gemini => crate::core::gemini::decode_request(value).map_err(|e| e.to_string()),
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
    encode_request_with_model(
        request,
        protocol,
        &request.model,
        reasoning_content,
        warnings,
    )
}

/// 按最终出站模型编码请求。
///
/// 大多数协议把模型名作为请求字段补丁写回；Gemini 的 thinking budget
/// 需要在编码阶段依据 URL 中的最终模型名裁剪，因此调用方必须传入已解析
/// 别名后的名称。
pub fn encode_request_with_model(
    request: &ChatRequest,
    protocol: Protocol,
    outbound_model: &str,
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
        Protocol::Gemini => {
            crate::core::gemini::encode_request_for_model(request, outbound_model, warnings)
        }
    }
}

/// 会话缓存键回写：按渠道开关把网关解析出的隔离标识写为出站请求的
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

/// 渠道级自动缓存断点注入：对已编码出站对象按序补 `cache_control`。
///
/// 仅 Anthropic Messages 出站消费（断点是 Anthropic 语义），注入顺序与
/// 预算语义见适配器同名函数。对已编码出站对象做目标性补丁，直通快路径
/// 字节直搬，不经过本开关。
pub fn inject_cache_breakpoints(outbound: &mut Value, protocol: Protocol, enabled: bool) {
    if !enabled || protocol != Protocol::AnthropicMessages {
        return;
    }
    if let Some(map) = outbound.as_object_mut() {
        crate::core::anthropic_messages::inject_cache_breakpoints(map);
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
        Protocol::OpenAiResponses => {
            crate::core::openai_responses::decode_response(value).map_err(|e| e.to_string())
        }
        Protocol::Gemini => crate::core::gemini::decode_response(value).map_err(|e| e.to_string()),
    }
}

/// 编码 IR 响应为入站协议。
pub fn encode_response(response: &ChatResponse, protocol: Protocol) -> Value {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::encode_response(response),
        Protocol::AnthropicMessages => crate::core::anthropic_messages::encode_response(response),
        Protocol::OpenAiResponses => crate::core::openai_responses::encode_response(response),
        Protocol::Gemini => crate::core::gemini::encode_response(response),
    }
}

/// 直通快路径的 usage 嗅探：从单个 SSE 帧或非流式响应体提取 usage 折算为 IR
/// 四分量，供计费，不做完整解码。
pub fn sniff_usage(value: &Value, protocol: Protocol) -> Option<Usage> {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::sniff_chat_usage(value),
        Protocol::AnthropicMessages => crate::core::anthropic_messages::sniff_usage(value),
        Protocol::OpenAiResponses => crate::core::openai_responses::sniff_usage(value),
        Protocol::Gemini => crate::core::gemini::sniff_usage(value),
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
        Protocol::Gemini => crate::core::gemini::encode_error(status, message),
    }
}

/// 流内错误的入站协议 SSE 帧（500 语义）。各适配器流式编码器消费 IR Error
/// 事件走同一形状，网关兜底路径（缓冲超限等）复用本函数。
pub fn stream_error_frame(protocol: Protocol, message: &str) -> SseFrame {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::stream_error_frame(message),
        Protocol::AnthropicMessages => crate::core::anthropic_messages::stream_error_frame(message),
        Protocol::OpenAiResponses => crate::core::openai_responses::stream_error_frame(message),
        Protocol::Gemini => crate::core::gemini::stream_error_frame(message),
    }
}

/// 编码下游模型列表为入站协议的标准列出模型响应。
pub fn encode_model_list(ids: &[String], protocol: Protocol) -> Value {
    match protocol {
        Protocol::OpenAiChat => crate::core::openai_chat::encode_model_list(ids),
        Protocol::AnthropicMessages => crate::core::anthropic_messages::encode_model_list(ids),
        Protocol::OpenAiResponses => crate::core::openai_responses::encode_model_list(ids),
        Protocol::Gemini => crate::core::gemini::encode_model_list(ids),
    }
}

/// 出站渠道的 upstream 路径段（相对 base_url）。
///
/// Gemini 的模型名承载在路径上：非流式为
/// `/v1beta/models/{model}:generateContent`，流式为
/// `:streamGenerateContent?alt=sse`；其余协议的路径为静态段，忽略模型名。
pub fn upstream_path(protocol: Protocol, model: &str, stream: bool) -> String {
    match protocol {
        Protocol::OpenAiChat => "/chat/completions".to_string(),
        Protocol::OpenAiResponses => "/responses".to_string(),
        Protocol::AnthropicMessages => "/messages".to_string(),
        Protocol::Gemini if stream => {
            format!("/v1beta/models/{model}:streamGenerateContent?alt=sse")
        }
        Protocol::Gemini => format!("/v1beta/models/{model}:generateContent"),
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
        Protocol::Gemini => Box::new(GeminiStreamDecoder(
            crate::core::gemini::StreamDecoder::default(),
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
        Protocol::Gemini => Box::new(GeminiStreamEncoder(
            crate::core::gemini::StreamEncoder::new(inbound_model),
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

struct GeminiStreamDecoder(crate::core::gemini::StreamDecoder);
impl ChatStreamDecoder for GeminiStreamDecoder {
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

struct GeminiStreamEncoder(crate::core::gemini::StreamEncoder);
impl ChatStreamEncoder for GeminiStreamEncoder {
    fn message_start(&self) -> Option<SseFrame> {
        None
    }
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        self.0.encode(event)
    }
    fn terminator(&self) -> Option<String> {
        // Gemini 流以服务器关闭收尾（末 chunk 自带 finishReason），无哨兵行。
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 出站路径构造：静态协议忽略模型名与流式；Gemini 的模型名承载在路径
    /// 端点上，流式走 `:streamGenerateContent?alt=sse`。
    #[test]
    fn upstream_path_builds_per_protocol() {
        assert_eq!(
            upstream_path(Protocol::OpenAiChat, "gpt-4o", true),
            "/chat/completions"
        );
        assert_eq!(
            upstream_path(Protocol::AnthropicMessages, "claude", false),
            "/messages"
        );
        assert_eq!(
            upstream_path(Protocol::Gemini, "gemini-2.5-pro", false),
            "/v1beta/models/gemini-2.5-pro:generateContent"
        );
        assert_eq!(
            upstream_path(Protocol::Gemini, "gemini-2.5-pro", true),
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
    }

    /// 注入分派：仅 Anthropic 协议且开关开启时生效，其余一概不动出站对象。
    #[test]
    fn cache_breakpoint_injection_follows_protocol_and_switch() {
        let base = || json!({ "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }] });

        // 开关关闭：不动。
        let mut outbound = base();
        inject_cache_breakpoints(&mut outbound, Protocol::AnthropicMessages, false);
        assert!(outbound["messages"][0].get("cache_control").is_none());

        // 非 Anthropic 协议：不动（即使开关开启）。
        let mut outbound = base();
        inject_cache_breakpoints(&mut outbound, Protocol::OpenAiChat, true);
        assert!(outbound["messages"][0].get("cache_control").is_none());

        // Anthropic 协议且开启：末条消息尾块被标记。
        let mut outbound = base();
        inject_cache_breakpoints(&mut outbound, Protocol::AnthropicMessages, true);
        assert_eq!(
            outbound["messages"][0]["content"][0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
    }

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
