//! OpenAI Chat Completions 协议适配器：wire ↔ IR 双向编解码。
//!
//! wire 结构体全部私有，透过 `decode_*`/`encode_*` 公共函数暴露 IR 边界，
//! wire 类型不出本模块边界（ADR-0001 hub-and-spoke）。
//!
//! 映射对齐 Vercel AI SDK `convert-to-openai-chat-messages.ts` 与
//! `openai-chat-language-model.ts`。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::core::ir::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, FinishReasonUnified, Message, Role, Tool,
    Usage,
};

// ---- 错误 ----

/// wire 解码错误，网关映射为 400。
#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("请求体不是合法 JSON 对象")]
    NotObject,
    #[error("缺少模型字段")]
    MissingModel,
    #[error("缺少消息列表")]
    MissingMessages,
    #[error("消息 {index} 缺少角色")]
    MissingRole { index: usize },
    #[error("消息 {index} 角色未知")]
    UnknownRole { index: usize },
    #[error("消息 {index} 缺少内容")]
    MissingContent { index: usize },
    #[error("消息 {index} 的 user 内容类型未知")]
    UnknownUserContentPart { index: usize },
    #[error("消息 {index} 的 assistant 内容类型未知")]
    UnknownAssistantContentPart { index: usize },
    #[error("消息 {index} 的 tool 消息缺少 tool_call_id")]
    MissingToolCallId { index: usize },
    #[error("消息 {index} 的 tool 消息内容不是字符串")]
    ToolContentNotString { index: usize },
    #[error("消息 {index} 的 tool_call 参数不是 JSON 字符串")]
    ToolCallArgumentsNotString { index: usize },
    #[error("响应缺少 choices")]
    MissingChoices,
    #[error("响应的 choice 缺少 message")]
    MissingChoiceMessage,
}

// ---- wire 请求类型 ----

/// OpenAI Chat Completions 出站/入站请求体（wire）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireChatRequest {
    model: String,
    messages: Vec<WireMessage>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    top_p: Option<f64>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    n: Option<u32>,
    #[serde(default)]
    stop: Option<Vec<String>>,
    #[serde(default)]
    presence_penalty: Option<f64>,
    #[serde(default)]
    frequency_penalty: Option<f64>,
    #[serde(default)]
    seed: Option<u64>,
    #[serde(default)]
    response_format: Option<Value>,
    #[serde(default)]
    tools: Option<Vec<WireTool>>,
    #[serde(default)]
    tool_choice: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireTool {
    function: WireFunctionTool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireFunctionTool {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<Value>,
}

/// wire 消息：role 区分系统/用户/助手/工具，content 形态各异。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireMessage {
    role: String,
    #[serde(default)]
    content: Option<WireContent>,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCall>>,
}

/// user/assistant 的 content：字符串或有序 part 数组。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum WireContent {
    Text(String),
    Parts(Vec<WireContentPart>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireContentPart {
    #[serde(rename = "type")]
    part_type: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireToolCall {
    id: String,
    function: WireToolCallFunction,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireToolCallFunction {
    name: String,
    arguments: String,
}

// ---- wire 响应类型 ----

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireChatResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireChoice {
    #[serde(default)]
    message: Option<WireResponseMessage>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCall>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<WirePromptTokensDetails>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
struct WirePromptTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
}

// ---- 入站解码：wire 请求 → IR ----

/// 解码入站 Chat Completions 请求为 IR。
pub fn decode_request(value: &Value) -> Result<ChatRequest, DecodeError> {
    let wire = serde_json::from_value::<WireChatRequest>(value.clone())
        .map_err(|_| DecodeError::NotObject)?;

    let messages = wire
        .messages
        .iter()
        .enumerate()
        .map(|(index, m)| decode_message(m, index))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ChatRequest {
        model: wire.model,
        messages,
        stream: wire.stream,
        temperature: wire.temperature,
        top_p: wire.top_p,
        max_tokens: wire.max_tokens,
        n: wire.n,
        stop: wire.stop.unwrap_or_default(),
        presence_penalty: wire.presence_penalty,
        frequency_penalty: wire.frequency_penalty,
        seed: wire.seed,
        response_format: wire.response_format,
        tools: wire
            .tools
            .unwrap_or_default()
            .into_iter()
            .map(|t| Tool {
                name: t.function.name,
                description: t.function.description,
                parameters: t.function.parameters,
            })
            .collect(),
        tool_choice: wire.tool_choice,
    })
}

/// 解码单条 wire 消息为 IR 消息。
fn decode_message(wire: &WireMessage, index: usize) -> Result<Message, DecodeError> {
    let role = match wire.role.as_str() {
        "system" => Role::System,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => return Err(DecodeError::UnknownRole { index }),
    };

    let content = match role {
        Role::System => {
            let text = wire
                .content
                .as_ref()
                .ok_or(DecodeError::MissingContent { index })?
                .text_value()
                .map_err(|_| DecodeError::MissingContent { index })?;
            vec![ContentPart::Text {
                text,
                provider_options: HashMap::new(),
            }]
        }
        Role::User => {
            let content = wire
                .content
                .as_ref()
                .ok_or(DecodeError::MissingContent { index })?;
            match content {
                // 纯字符串 user 消息解码为单个 text part。
                WireContent::Text(text) => vec![ContentPart::Text {
                    text: text.clone(),
                    provider_options: HashMap::new(),
                }],
                WireContent::Parts(parts) => parts
                    .iter()
                    .map(|part| {
                        if part.part_type != "text" || part.text.is_none() {
                            return Err(DecodeError::UnknownUserContentPart { index });
                        }
                        Ok(ContentPart::Text {
                            text: part.text.clone().unwrap_or_default(),
                            provider_options: HashMap::new(),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        Role::Assistant => decode_assistant(wire, index)?,
        Role::Tool => {
            let tool_call_id = wire
                .tool_call_id
                .clone()
                .ok_or(DecodeError::MissingToolCallId { index })?;
            let text = wire
                .content
                .as_ref()
                .ok_or(DecodeError::MissingContent { index })?
                .text_value()
                .map_err(|_| DecodeError::ToolContentNotString { index })?;
            vec![ContentPart::ToolResult {
                tool_call_id,
                tool_name: String::new(),
                output: Value::String(text),
                provider_options: HashMap::new(),
            }]
        }
    };

    Ok(Message {
        role,
        content,
        provider_options: HashMap::new(),
    })
}

/// 助手消息：text parts 聚合成一个 text part，tool-call parts 各自保留。
fn decode_assistant(wire: &WireMessage, index: usize) -> Result<Vec<ContentPart>, DecodeError> {
    let mut parts = Vec::new();

    if let Some(content) = &wire.content {
        match content {
            WireContent::Text(text) => {
                if !text.is_empty() {
                    parts.push(ContentPart::Text {
                        text: text.clone(),
                        provider_options: HashMap::new(),
                    });
                }
            }
            WireContent::Parts(text_parts) => {
                for part in text_parts {
                    if part.part_type != "text" || part.text.is_none() {
                        return Err(DecodeError::UnknownAssistantContentPart { index });
                    }
                    parts.push(ContentPart::Text {
                        text: part.text.clone().unwrap_or_default(),
                        provider_options: HashMap::new(),
                    });
                }
            }
        }
    }

    if let Some(tool_calls) = &wire.tool_calls {
        for tc in tool_calls {
            let input = serde_json::from_str::<Value>(&tc.function.arguments)
                .map_err(|_| DecodeError::ToolCallArgumentsNotString { index })?;
            parts.push(ContentPart::ToolCall {
                tool_call_id: tc.id.clone(),
                tool_name: tc.function.name.clone(),
                input,
                provider_options: HashMap::new(),
            });
        }
    }

    Ok(parts)
}

impl WireContent {
    fn text_value(&self) -> Result<String, ()> {
        match self {
            WireContent::Text(text) => Ok(text.clone()),
            WireContent::Parts(_) => Err(()),
        }
    }
}

// ---- 出站编码：IR → wire 请求 ----

/// 编码 IR 请求为出站 Chat Completions 请求体。
pub fn encode_request(request: &ChatRequest) -> Value {
    let messages: Vec<Value> = request.messages.iter().map(encode_message).collect();

    let mut obj = serde_json::Map::new();
    obj.insert("model".into(), json!(request.model));
    obj.insert("messages".into(), Value::Array(messages));
    if let Some(v) = request.temperature {
        obj.insert("temperature".into(), json!(v));
    }
    if let Some(v) = request.top_p {
        obj.insert("top_p".into(), json!(v));
    }
    if let Some(v) = request.max_tokens {
        obj.insert("max_tokens".into(), json!(v));
    }
    if let Some(v) = request.n {
        obj.insert("n".into(), json!(v));
    }
    if !request.stop.is_empty() {
        obj.insert("stop".into(), json!(request.stop));
    }
    if let Some(v) = request.presence_penalty {
        obj.insert("presence_penalty".into(), json!(v));
    }
    if let Some(v) = request.frequency_penalty {
        obj.insert("frequency_penalty".into(), json!(v));
    }
    if let Some(v) = request.seed {
        obj.insert("seed".into(), json!(v));
    }
    if let Some(v) = &request.response_format {
        obj.insert("response_format".into(), v.clone());
    }
    if !request.tools.is_empty() {
        obj.insert(
            "tools".into(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .map(|t| {
                        let mut tool = serde_json::Map::new();
                        tool.insert("type".into(), json!("function"));
                        let mut function = serde_json::Map::new();
                        function.insert("name".into(), json!(t.name));
                        if let Some(d) = &t.description {
                            function.insert("description".into(), json!(d));
                        }
                        if let Some(p) = &t.parameters {
                            function.insert("parameters".into(), p.clone());
                        }
                        tool.insert("function".into(), Value::Object(function));
                        Value::Object(tool)
                    })
                    .collect(),
            ),
        );
    }
    if let Some(v) = &request.tool_choice {
        obj.insert("tool_choice".into(), v.clone());
    }
    Value::Object(obj)
}

/// 编码单条 IR 消息为 wire 消息。
fn encode_message(message: &Message) -> Value {
    match message.role {
        Role::System => {
            let text = text_parts(&message.content).unwrap_or_default();
            json!({ "role": "system", "content": text })
        }
        Role::User => {
            let content: String = message
                .content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            json!({ "role": "user", "content": content })
        }
        Role::Assistant => {
            let text = text_parts(&message.content).unwrap_or_default();
            let tool_calls: Vec<Value> = message
                .content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::ToolCall {
                        tool_call_id,
                        tool_name,
                        input,
                        ..
                    } => Some(json!({
                        "id": tool_call_id,
                        "type": "function",
                        "function": {
                            "name": tool_name,
                            "arguments": input.to_string(),
                        }
                    })),
                    _ => None,
                })
                .collect();

            let mut wire = serde_json::Map::new();
            wire.insert("role".into(), json!("assistant"));
            // 有 tool_calls 时 content 可为 null（OpenAI 语义）。
            let content_value: Value = if tool_calls.is_empty() {
                Value::String(text)
            } else if text.is_empty() {
                Value::Null
            } else {
                Value::String(text)
            };
            wire.insert("content".into(), content_value);
            if !tool_calls.is_empty() {
                wire.insert("tool_calls".into(), Value::Array(tool_calls));
            }
            Value::Object(wire)
        }
        Role::Tool => {
            // tool 消息只携带 tool_result parts。
            let part = message
                .content
                .iter()
                .find(|p| matches!(p, ContentPart::ToolResult { .. }));
            let (tool_call_id, output) = match part {
                Some(ContentPart::ToolResult {
                    tool_call_id,
                    output,
                    ..
                }) => (tool_call_id.clone(), output),
                _ => (String::new(), &Value::String(String::new())),
            };
            let content = match output {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            json!({ "role": "tool", "tool_call_id": tool_call_id, "content": content })
        }
    }
}

/// 聚合消息中所有 text part 为一个字符串。
fn text_parts(parts: &[ContentPart]) -> Option<String> {
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.concat())
    }
}

// ---- 上游响应解码：wire → IR ----

/// 解码上游 Chat Completions 响应为 IR。
pub fn decode_response(value: &Value) -> Result<ChatResponse, DecodeError> {
    let wire = serde_json::from_value::<WireChatResponse>(value.clone())
        .map_err(|_| DecodeError::NotObject)?;

    let choice = wire.choices.first().ok_or(DecodeError::MissingChoices)?;
    let message = choice
        .message
        .as_ref()
        .ok_or(DecodeError::MissingChoiceMessage)?;

    let mut content = Vec::new();
    if let Some(text) = &message.content
        && !text.is_empty()
    {
        content.push(ContentPart::Text {
            text: text.clone(),
            provider_options: HashMap::new(),
        });
    }
    if let Some(tool_calls) = &message.tool_calls {
        for tc in tool_calls {
            let input = serde_json::from_str::<Value>(&tc.function.arguments)
                .map_err(|_| DecodeError::ToolCallArgumentsNotString { index: 0 })?;
            content.push(ContentPart::ToolCall {
                tool_call_id: tc.id.clone(),
                tool_name: tc.function.name.clone(),
                input,
                provider_options: HashMap::new(),
            });
        }
    }

    let usage = wire.usage.map(convert_usage).unwrap_or(Usage {
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        raw: None,
    });

    Ok(ChatResponse {
        id: wire.id.unwrap_or_default(),
        model: wire.model.unwrap_or_default(),
        content,
        finish_reason: FinishReason {
            unified: map_finish_reason(choice.finish_reason.as_deref()),
            raw: choice.finish_reason.clone(),
        },
        usage,
        provider_metadata: HashMap::new(),
    })
}

/// usage 四分量折算：input = prompt - cached - cache_write，output = completion。
/// `raw` 保留上游原始 usage 形状。
fn convert_usage(wire: WireUsage) -> Usage {
    let raw = serde_json::to_value(&wire).ok();
    let cached = wire
        .prompt_tokens_details
        .as_ref()
        .map(|d| d.cached_tokens)
        .unwrap_or(0);
    let cache_write = wire
        .prompt_tokens_details
        .as_ref()
        .map(|d| d.cache_write_tokens)
        .unwrap_or(0);
    Usage {
        input_tokens: wire
            .prompt_tokens
            .saturating_sub(cached)
            .saturating_sub(cache_write),
        output_tokens: wire.completion_tokens,
        cache_read_tokens: cached,
        cache_write_tokens: cache_write,
        raw,
    }
}

/// unified finish reason 映射，对齐 mapOpenAIFinishReason。
fn map_finish_reason(raw: Option<&str>) -> FinishReasonUnified {
    match raw {
        Some("stop") => FinishReasonUnified::Stop,
        Some("length") => FinishReasonUnified::Length,
        Some("content_filter") => FinishReasonUnified::ContentFilter,
        Some("function_call") | Some("tool_calls") => FinishReasonUnified::ToolCalls,
        _ => FinishReasonUnified::Other,
    }
}

// ---- 入站响应编码：IR → wire ----

/// 编码 IR 响应为入站 Chat Completions 响应体。
pub fn encode_response(response: &ChatResponse) -> Value {
    let text = text_parts(&response.content).unwrap_or_default();
    let tool_calls: Vec<Value> = response
        .content
        .iter()
        .filter_map(|p| match p {
            ContentPart::ToolCall {
                tool_call_id,
                tool_name,
                input,
                ..
            } => Some(json!({
                "id": tool_call_id,
                "type": "function",
                "function": {
                    "name": tool_name,
                    "arguments": input.to_string(),
                }
            })),
            _ => None,
        })
        .collect();

    let mut message = serde_json::Map::new();
    message.insert("role".into(), json!("assistant"));
    let content_value: Value = if tool_calls.is_empty() {
        Value::String(text)
    } else if text.is_empty() {
        Value::Null
    } else {
        Value::String(text)
    };
    message.insert("content".into(), content_value);
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }

    let details = if response.usage.cache_read_tokens > 0 || response.usage.cache_write_tokens > 0 {
        let mut d = serde_json::Map::new();
        d.insert(
            "cached_tokens".into(),
            json!(response.usage.cache_read_tokens),
        );
        d.insert(
            "cache_write_tokens".into(),
            json!(response.usage.cache_write_tokens),
        );
        Some(Value::Object(d))
    } else {
        None
    };

    let mut usage = serde_json::Map::new();
    usage.insert(
        "prompt_tokens".into(),
        json!(
            response.usage.input_tokens
                + response.usage.cache_read_tokens
                + response.usage.cache_write_tokens
        ),
    );
    usage.insert(
        "completion_tokens".into(),
        json!(response.usage.output_tokens),
    );
    usage.insert(
        "total_tokens".into(),
        json!(
            response.usage.input_tokens
                + response.usage.output_tokens
                + response.usage.cache_read_tokens
                + response.usage.cache_write_tokens
        ),
    );
    if let Some(details) = details {
        usage.insert("prompt_tokens_details".into(), details);
    }

    json!({
        "id": response.id,
        "object": "chat.completion",
        "model": response.model,
        "choices": [{
            "index": 0,
            "message": message,
            "logprobs": null,
            "finish_reason": response.finish_reason.raw.clone().unwrap_or_else(|| "stop".into()),
        }],
        "usage": usage,
    })
}

// ---- 错误编码 ----

/// 编码为 OpenAI 错误格式 `{"error":{...}}`。
///
/// `type` 按状态码映射：客户端错误为 `invalid_request_error`，服务端错误为
/// `api_error`（对齐 OpenAI 官方错误类型约定）。
pub fn encode_error(status: u16, message: &str) -> Value {
    let error_type = if (400..500).contains(&status) {
        "invalid_request_error"
    } else {
        "api_error"
    };
    json!({
        "error": {
            "message": message,
            "type": error_type,
            "code": null,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 黄金样例请求 decode → encode 往返还原 wire。
    #[test]
    fn request_fixture_roundtrip() {
        let raw = include_str!("__fixtures__/request.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let ir = decode_request(&wire).expect("fixture 应可解码为 IR");
        let reencoded = encode_request(&ir);
        assert_eq!(reencoded, wire, "往返应还原 wire 请求");
    }

    /// 黄金样例响应 decode → encode 往返还原 wire。
    #[test]
    fn response_fixture_roundtrip() {
        let raw = include_str!("__fixtures__/response.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let ir = decode_response(&wire).expect("fixture 应可解码为 IR");
        let reencoded = encode_response(&ir);
        assert_eq!(reencoded, wire, "往返应还原 wire 响应");
    }

    /// 非文本 user content part 报错。
    #[test]
    fn unknown_user_content_is_rejected() {
        let wire = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [{ "type": "image_url", "image_url": { "url": "x" } }]
            }]
        });
        assert!(matches!(
            decode_request(&wire),
            Err(DecodeError::UnknownUserContentPart { index: 0 })
        ));
    }

    /// 未知角色报错。
    #[test]
    fn unknown_role_is_rejected() {
        let wire = json!({
            "model": "gpt-4o",
            "messages": [{ "role": "developer", "content": "hi" }]
        });
        assert!(matches!(
            decode_request(&wire),
            Err(DecodeError::UnknownRole { index: 0 })
        ));
    }
}
