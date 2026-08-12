//! Anthropic Messages 协议适配器：wire ↔ IR 双向编解码。
//!
//! wire 结构体全部私有，透过 `decode_*`/`encode_*` 公共函数暴露 IR 边界，
//! wire 类型不出本模块边界（ADR-0001 hub-and-spoke）。
//!
//! 映射对齐 Vercel AI SDK `convert-to-anthropic-prompt.ts` 与
//! `anthropic-messages-language-model.ts`：
//! - 请求侧：首个 system 消息提升为顶层 `system`；assistant 内容块
//!   `text`/`thinking`/`redacted_thinking`/`tool_use` 与 user 内容块
//!   `text`/`tool_result`/`image`/`document`（base64/URL source）双向映射；
//!   thinking signature 经 part 逃生舱 `provider_options["anthropic"]["signature"]`
//!   无损往返。
//! - 响应侧：`stop_reason` 双轨映射，usage 输入侧为「input 不含缓存、
//!   缓存单独计」的加法约定（与口径一致）。
//! - 流式：事件名驱动的 SSE（`event:` 名），`signature_delta` 以零长增量携带
//!   signature，`message_delta` 携带最终 usage 与 stop_reason。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::core::ir::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, FinishReasonUnified, Message, Role,
    StreamEvent, Tool, Usage, Warning,
};
use crate::core::stream::SseFrame;

// ---- 错误 ----

/// wire 解码错误，网关映射为 400。
#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("请求体不是合法 JSON 对象")]
    NotObject,
    #[error("缺少模型字段")]
    MissingModel,
    #[error("缺少 max_tokens")]
    MissingMaxTokens,
    #[error("消息 {index} 缺少角色")]
    MissingRole { index: usize },
    #[error("消息 {index} 角色未知")]
    UnknownRole { index: usize },
    #[error("消息 {index} 缺少内容")]
    MissingContent { index: usize },
    #[error("消息 {index} 的内容块类型未知")]
    UnknownContentBlock { index: usize },
    #[error("消息 {index} 的 tool_result 缺少 tool_use_id")]
    MissingToolUseId { index: usize },
    #[error("响应缺少 usage")]
    MissingUsage,
}

// ---- wire 请求类型 ----

/// Anthropic Messages 出站/入站请求体（wire）。
#[derive(Debug, Clone, Deserialize)]
struct WireRequest {
    model: String,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    messages: Vec<WireMessage>,
    /// `system` 可为字符串或文本块数组。
    #[serde(default)]
    system: Option<Value>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    top_p: Option<f64>,
    #[serde(default)]
    top_k: Option<u32>,
    #[serde(default)]
    stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    tools: Option<Vec<WireTool>>,
    #[serde(default)]
    tool_choice: Option<Value>,
    /// 请求级逃生舱：同协议族经 IR 出站时原样回传。
    #[serde(default)]
    thinking: Option<Value>,
}

/// wire 消息。
#[derive(Debug, Clone, Deserialize)]
struct WireMessage {
    role: String,
    #[serde(default)]
    content: Option<WireContent>,
}

/// user/assistant 的 content：字符串或有序块数组。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum WireContent {
    Text(String),
    Blocks(Vec<WireBlock>),
}

/// 内容块，按 `type` 判别。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Option<Value>,
        #[serde(default)]
        is_error: Option<bool>,
    },
    /// 媒体内容块：`image`（图片）或 `document`（文档）。source 可为
    /// base64 字节、URL 或 provider 托管引用（`file_id`/`text`）。
    Image {
        #[serde(default)]
        source: Option<WireMediaSource>,
    },
    Document {
        #[serde(default)]
        source: Option<WireMediaSource>,
    },
}

/// Anthropic 媒体 source：`base64`/`url` 两种网关承载载体，`file`（托管引用）
/// 与 `text`（纯文本文档）经逃生舱回传。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireMediaSource {
    Base64 {
        #[serde(default)]
        media_type: Option<String>,
        #[serde(default)]
        data: Option<String>,
    },
    Url {
        #[serde(default)]
        url: Option<String>,
    },
    File {
        #[serde(default)]
        file_id: Option<String>,
    },
    Text {
        #[serde(default)]
        media_type: Option<String>,
        #[serde(default)]
        data: Option<String>,
    },
}

/// 工具定义。
#[derive(Debug, Clone, Deserialize)]
struct WireTool {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    input_schema: Option<Value>,
}

// ---- wire 响应类型 ----

#[derive(Debug, Clone, Deserialize)]
struct WireResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    content: Vec<WireResponseBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireResponseBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

// ---- 流式 wire 事件 ----

/// Anthropic 流式事件，按 `type` 判别。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireStreamEvent {
    #[serde(rename = "message_start")]
    MessageStart {
        message: WireStreamMessage,
    },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: WireContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        index: usize,
        delta: WireStreamDelta,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop {
        index: usize,
    },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: WireMessageDelta,
        #[serde(default)]
        usage: Option<WireUsage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    Ping,
}

/// `message_start` 的 message 首部：id/model（usage 为输入侧早期值，非最终，
/// 此处不消费，由 `sniff_usage` 在直通路径处理）。
#[derive(Debug, Clone, Deserialize)]
struct WireStreamMessage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

/// `content_block_start` 的内容块。`Text`/`Thinking` 的文本以单元变体判别
/// （内容由后续 delta 提供），仅作 block 类型开启。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireContentBlock {
    Text,
    Thinking,
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Option<Value>,
    },
}

/// `content_block_delta` 的增量，按 `type` 判别。
///
/// 变体名与 Anthropic 官方 delta 类型名对齐（`text_delta`、`signature_delta` 等），
/// 同名后缀是协议命名而非冗余。
#[derive(Debug, Clone, Deserialize)]
#[allow(clippy::enum_variant_names)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireStreamDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(rename = "signature_delta")]
    SignatureDelta { signature: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

/// `message_delta` 的 delta：stop_reason（stop_sequence 由 serde 忽略，不消费）。
#[derive(Debug, Clone, Deserialize)]
struct WireMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

// ---- 入站解码：wire 请求 → IR ----

/// 解码入站 Anthropic Messages 请求为 IR。
pub fn decode_request(value: &Value) -> Result<ChatRequest, DecodeError> {
    let wire = serde_json::from_value::<WireRequest>(value.clone()).map_err(|_| {
        // 区分 NotObject 与缺模型：都归为请求体不合法 JSON 对象。
        DecodeError::NotObject
    })?;

    let mut messages = Vec::new();
    // 顶层 `system` 提升为首条 System 消息。
    if let Some(system) = &wire.system {
        let text = system_text(system);
        if let Some(text) = text {
            messages.push(Message {
                role: Role::System,
                content: vec![ContentPart::Text {
                    text,
                    provider_options: HashMap::new(),
                }],
                provider_options: HashMap::new(),
            });
        }
    }

    for (index, wire_message) in wire.messages.iter().enumerate() {
        messages.extend(decode_message(wire_message, index)?);
    }

    let mut provider_options = HashMap::new();
    if let Some(thinking) = wire.thinking {
        provider_options.insert("anthropic".to_string(), json!({ "thinking": thinking }));
    }

    Ok(ChatRequest {
        model: wire.model,
        messages,
        stream: wire.stream,
        temperature: wire.temperature,
        top_p: wire.top_p,
        top_k: wire.top_k,
        max_tokens: wire.max_tokens,
        n: None,
        stop: wire.stop_sequences.unwrap_or_default(),
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        response_format: None,
        tools: wire
            .tools
            .unwrap_or_default()
            .into_iter()
            .map(|t| Tool {
                name: t.name,
                description: t.description,
                parameters: t.input_schema,
            })
            .collect(),
        tool_choice: wire.tool_choice,
        provider_options,
    })
}

/// 顶层 `system`（字符串或文本块数组）提取为纯文本。
fn system_text(system: &Value) -> Option<String> {
    match system {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => {
            let mut text = String::new();
            for block in blocks {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
            Some(text)
        }
        _ => None,
    }
}

/// 解码单条 wire 消息为 IR 消息（可产出多条：user 混含 tool_result 时拆分）。
fn decode_message(wire: &WireMessage, index: usize) -> Result<Vec<Message>, DecodeError> {
    let role = match wire.role.as_str() {
        "system" => Role::System,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return Err(DecodeError::UnknownRole { index }),
    };

    let content = wire
        .content
        .as_ref()
        .ok_or(DecodeError::MissingContent { index })?;

    match role {
        Role::System => {
            let text = content
                .text_value()
                .ok_or(DecodeError::MissingContent { index })?;
            Ok(vec![Message {
                role: Role::System,
                content: vec![ContentPart::Text {
                    text,
                    provider_options: HashMap::new(),
                }],
                provider_options: HashMap::new(),
            }])
        }
        Role::User => decode_user(content, index),
        Role::Assistant => {
            let blocks = content.blocks(index)?;
            let mut parts = Vec::new();
            for block in blocks {
                match block {
                    WireBlock::Text { text } => parts.push(ContentPart::Text {
                        text: text.clone(),
                        provider_options: HashMap::new(),
                    }),
                    WireBlock::Thinking {
                        thinking,
                        signature,
                    } => {
                        let mut provider_options = HashMap::new();
                        if let Some(sig) = signature {
                            provider_options
                                .insert("anthropic".to_string(), json!({ "signature": sig }));
                        }
                        parts.push(ContentPart::Reasoning {
                            text: thinking.clone(),
                            provider_options,
                        });
                    }
                    WireBlock::RedactedThinking { data } => {
                        parts.push(ContentPart::Reasoning {
                            // redacted_thinking 不含明文文本；逃生舱携带密文。
                            text: String::new(),
                            provider_options: [(
                                "anthropic".to_string(),
                                json!({ "redacted_data": data }),
                            )]
                            .into_iter()
                            .collect(),
                        });
                    }
                    WireBlock::ToolUse { id, name, input } => {
                        parts.push(ContentPart::ToolCall {
                            tool_call_id: id.clone(),
                            tool_name: name.clone(),
                            input: input.clone(),
                            provider_options: HashMap::new(),
                        });
                    }
                    WireBlock::ToolResult { .. } => {
                        return Err(DecodeError::UnknownContentBlock { index });
                    }
                    // assistant 消息不应携带媒体内容块；容错跳过（不产出）。
                    WireBlock::Image { .. } | WireBlock::Document { .. } => {}
                }
            }
            Ok(vec![Message {
                role: Role::Assistant,
                content: parts,
                provider_options: HashMap::new(),
            }])
        }
        Role::Tool => unreachable!("Anthropic wire role 无 tool"),
    }
}

/// user 消息：文本块进 User 消息，tool_result 块各自拆为 Tool 消息。
///
/// Anthropic 把 tool_result 放在 user 消息里，而 IR 的 tool 结果独立成 Tool 角色
/// （与 OpenAI 约定一致）。混含时文本与各 tool_result 分拆，保持 content 顺序。
fn decode_user(content: &WireContent, index: usize) -> Result<Vec<Message>, DecodeError> {
    // 纯字符串 user 消息 → 单个 text part。
    let blocks = match content {
        WireContent::Text(text) => {
            return Ok(vec![Message {
                role: Role::User,
                content: vec![ContentPart::Text {
                    text: text.clone(),
                    provider_options: HashMap::new(),
                }],
                provider_options: HashMap::new(),
            }]);
        }
        WireContent::Blocks(blocks) => blocks,
    };

    let mut messages = Vec::new();
    let mut text_parts = Vec::new();
    let mut tool_results = Vec::new();

    for block in blocks {
        match block {
            WireBlock::Text { text } => text_parts.push(ContentPart::Text {
                text: text.clone(),
                provider_options: HashMap::new(),
            }),
            WireBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let output = match content {
                    Some(Value::String(s)) => Value::String(s.clone()),
                    Some(other) => other.clone(),
                    None => Value::Null,
                };
                tool_results.push(ContentPart::ToolResult {
                    tool_call_id: tool_use_id.clone(),
                    tool_name: String::new(),
                    output,
                    provider_options: if *is_error == Some(true) {
                        [("anthropic".to_string(), json!({ "is_error": true }))]
                            .into_iter()
                            .collect()
                    } else {
                        HashMap::new()
                    },
                });
            }
            WireBlock::Image { source } | WireBlock::Document { source } => {
                let block_type = match block {
                    WireBlock::Image { .. } => "image",
                    _ => "document",
                };
                text_parts.push(decode_media_part(source, index, block_type)?);
            }
            _ => return Err(DecodeError::UnknownContentBlock { index }),
        }
    }

    if !text_parts.is_empty() {
        messages.push(Message {
            role: Role::User,
            content: text_parts,
            provider_options: HashMap::new(),
        });
    }
    for part in tool_results {
        messages.push(Message {
            role: Role::Tool,
            content: vec![part],
            provider_options: HashMap::new(),
        });
    }
    Ok(messages)
}

impl WireContent {
    fn text_value(&self) -> Option<String> {
        match self {
            WireContent::Text(text) => Some(text.clone()),
            WireContent::Blocks(_) => None,
        }
    }

    fn blocks(&self, index: usize) -> Result<&[WireBlock], DecodeError> {
        match self {
            WireContent::Blocks(blocks) => Ok(blocks),
            WireContent::Text(_) => Err(DecodeError::MissingContent { index }),
        }
    }
}

/// 解码 Anthropic 媒体 source 为 IR 媒体 part。
///
/// `base64` source → `MediaSource::Data`（media_type 缺省空串兜底）；`url` source →
/// `MediaSource::Url`。`file`（provider 托管引用）与 `text`（纯文本文档）网关不
/// 承载，以空 `MediaSource::Data` 占位跨协议族丢弃时记 warning（逃生舱哲学）。
/// `block_type`（`image`/`document`）在缺省 media_type 时兜底为顶层段。
fn decode_media_part(
    source: &Option<WireMediaSource>,
    index: usize,
    block_type: &str,
) -> Result<ContentPart, DecodeError> {
    let (media_type, data, provider_options) = match source {
        Some(WireMediaSource::Base64 {
            media_type,
            data: base64,
        }) => (
            media_type.clone().unwrap_or_else(|| block_type.to_string()),
            crate::core::ir::MediaSource::Data {
                base64: base64.clone().unwrap_or_default(),
            },
            HashMap::new(),
        ),
        Some(WireMediaSource::Url { url }) => (
            block_type.to_string(),
            crate::core::ir::MediaSource::Url {
                url: url.clone().unwrap_or_default(),
            },
            HashMap::new(),
        ),
        Some(WireMediaSource::File { file_id }) => (
            block_type.to_string(),
            crate::core::ir::MediaSource::Data {
                base64: String::new(),
            },
            [(
                "anthropic".to_string(),
                json!({ "media_source": "file", "file_id": file_id }),
            )]
            .into_iter()
            .collect(),
        ),
        Some(WireMediaSource::Text { media_type, data }) => (
            media_type.clone().unwrap_or_else(|| block_type.to_string()),
            crate::core::ir::MediaSource::Data {
                base64: String::new(),
            },
            [(
                "anthropic".to_string(),
                json!({ "media_source": "text", "data": data }),
            )]
            .into_iter()
            .collect(),
        ),
        None => return Err(DecodeError::UnknownContentBlock { index }),
    };
    Ok(ContentPart::Media {
        media_type,
        data,
        provider_options,
    })
}

// ---- 出站编码：IR → wire 请求 ----

/// 编码 IR 请求为出站 Anthropic Messages 请求体。
///
/// 首个 System 消息提升为顶层 `system`；请求级 `provider_options["anthropic"]`
/// 原样回传（thinking 配置经 IR 路径不丢失）。目标协议无法表达的内容追加到
/// `warnings`（对齐 AI SDK `doGenerate` 的 warnings 累积）。
pub fn encode_request(request: &ChatRequest, warnings: &mut Vec<Warning>) -> Value {
    let (system, messages) = encode_messages(&request.messages, warnings);

    let mut obj = serde_json::Map::new();
    obj.insert("model".into(), json!(request.model));
    if let Some(system) = system {
        obj.insert("system".into(), json!(system));
    }
    obj.insert("messages".into(), Value::Array(messages));
    // Anthropic 强制要求 max_tokens：缺省时补 4096（one-api 同款默认），
    // 否则跨协议请求（如 OpenAI 入站未带 max_tokens）会被上游 400 拒绝。
    let max_tokens = request.max_tokens.filter(|&v| v > 0).unwrap_or(4096);
    obj.insert("max_tokens".into(), json!(max_tokens));
    if let Some(v) = request.temperature {
        obj.insert("temperature".into(), json!(v));
    }
    if let Some(v) = request.top_p {
        obj.insert("top_p".into(), json!(v));
    }
    if let Some(v) = request.top_k {
        obj.insert("top_k".into(), json!(v));
    }
    if !request.stop.is_empty() {
        obj.insert("stop_sequences".into(), json!(request.stop));
    }
    if request.stream {
        obj.insert("stream".into(), Value::Bool(true));
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
                        tool.insert("name".into(), json!(t.name));
                        if let Some(d) = &t.description {
                            tool.insert("description".into(), json!(d));
                        }
                        if let Some(s) = &t.parameters {
                            tool.insert("input_schema".into(), s.clone());
                        }
                        Value::Object(tool)
                    })
                    .collect(),
            ),
        );
    }
    if let Some(tc) = &request.tool_choice {
        obj.insert("tool_choice".into(), tc.clone());
    }
    // 请求级逃生舱回传：Anthropic thinking 配置等。
    if let Some(anthropic) = request.provider_options.get("anthropic")
        && let Some(thinking) = anthropic.get("thinking")
    {
        obj.insert("thinking".into(), thinking.clone());
    }
    Value::Object(obj)
}

/// 把 IR 消息编码为（顶层 system，wire messages）。
///
/// 合并连续 assistant 消息（Anthropic 要求）与连续 tool 消息（拆为单条 user 的
/// 多个 tool_result 块）。首个 System 消息提升为 system；其余 System 消息以
/// user 文本夹在消息流中，保持顺序。
fn encode_messages(
    ir_messages: &[Message],
    warnings: &mut Vec<Warning>,
) -> (Option<String>, Vec<Value>) {
    let mut system_out: Option<String> = None;
    let mut wire_messages: Vec<Value> = Vec::new();

    for message in ir_messages {
        match message.role {
            Role::System => {
                // System 消息仅取文本；媒体等非文本 part 丢弃并记 warning。
                for part in &message.content {
                    if let ContentPart::Media { media_type, .. } = part {
                        warnings.push(Warning::unsupported(
                            "media",
                            format!(
                                "Anthropic Messages 系统消息不支持媒体内容（{media_type}），已丢弃"
                            ),
                        ));
                    }
                }
                let text = text_parts(&message.content).unwrap_or_default();
                if system_out.is_none() {
                    system_out = Some(text);
                } else {
                    // 后续 System 消息以 user 文本夹入，避免丢失。
                    push_user_text(&mut wire_messages, &text);
                }
            }
            Role::User => {
                let blocks = encode_user_blocks(&message.content, warnings);
                if blocks.is_empty() {
                    continue;
                }
                // 单一纯文本 user 消息编码为字符串（Anthropic 惯例，保持既有往返形状）；
                // 含媒体等非文本 part 时按序编码为数组，保持文本与媒体混排顺序。
                let single_text = (blocks.len() == 1)
                    .then(|| blocks[0].get("type").and_then(Value::as_str))
                    .flatten()
                    .filter(|t| *t == "text")
                    .map(|_| {
                        blocks[0]
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                    });
                if let Some(text) = single_text {
                    push_user_text(&mut wire_messages, text);
                } else {
                    push_user_blocks(&mut wire_messages, blocks);
                }
            }
            Role::Assistant => {
                let blocks = encode_assistant_blocks(&message.content, warnings);
                // 连续 assistant 消息合并为一条（Anthropic 要求）。
                if let Some(last) = wire_messages.last_mut()
                    && last.get("role").and_then(Value::as_str) == Some("assistant")
                {
                    append_blocks(last, blocks);
                } else {
                    let mut m = serde_json::Map::new();
                    m.insert("role".into(), json!("assistant"));
                    m.insert("content".into(), Value::Array(blocks));
                    wire_messages.push(Value::Object(m));
                }
            }
            Role::Tool => {
                let blocks = encode_tool_result_blocks(&message.content);
                // 连续 tool 消息合并为一条 user 的多个 tool_result 块。
                if let Some(last) = wire_messages.last_mut()
                    && last.get("role").and_then(Value::as_str) == Some("user")
                    && last
                        .get("content")
                        .and_then(Value::as_array)
                        .is_some_and(|c| {
                            c.iter().all(|b| {
                                b.get("type").and_then(Value::as_str) == Some("tool_result")
                            })
                        })
                {
                    append_blocks(last, blocks);
                } else {
                    let mut m = serde_json::Map::new();
                    m.insert("role".into(), json!("user"));
                    m.insert("content".into(), Value::Array(blocks));
                    wire_messages.push(Value::Object(m));
                }
            }
        }
    }

    // 末尾 assistant 文本块去除尾随空白（Anthropic 拒绝预置 assistant 的尾随空白）。
    trim_trailing_whitespace(&mut wire_messages);
    (system_out, wire_messages)
}

fn push_user_text(wire_messages: &mut Vec<Value>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = wire_messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("user")
        && let Some(content) = last.get("content").and_then(Value::as_str)
    {
        // 连续 user 文本合并；以换行保留消息边界，避免 "hi"+"there" 粘成 "hithere"。
        let new_text = format!("{content}\n{text}");
        *last.get_mut("content").expect("已确认 content 为字符串") = json!(new_text);
        return;
    }
    wire_messages.push(json!({ "role": "user", "content": text }));
}

/// 以内容块数组追加 user 消息（媒体混排时保留顺序）。
///
/// 与 `push_user_text` 并存：纯文本走字符串形状，含媒体等非文本 part 时按序
/// 编码为数组。与既有 user 文本消息合并时以换行分隔（保持消息边界）。
fn push_user_blocks(wire_messages: &mut Vec<Value>, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = wire_messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("user")
        && let Some(content) = last.get("content").and_then(Value::as_str)
    {
        // 连续 user 文本合并；以换行保留消息边界，避免 "hi"+"there" 粘成 "hithere"。
        let new_text = format!("{content}\n");
        let mut new_blocks = Vec::new();
        new_blocks.push(json!({ "type": "text", "text": new_text }));
        new_blocks.extend(blocks);
        *last.get_mut("content").expect("已确认 content 为字符串") = Value::Array(new_blocks);
        return;
    }
    wire_messages.push(json!({ "role": "user", "content": Value::Array(blocks) }));
}

/// 编码 user 消息的内容块序列（文本与媒体混排保持顺序）。
///
/// 文本 → `text` 块；媒体 part → `image`/`document` 块（base64/URL source 按
/// 数据源分派，媒体类型经顶层段判定）。目标协议不支持的媒体类型（非 image/
/// application/text 顶层段）丢弃并记 warning。
fn encode_user_blocks(parts: &[ContentPart], warnings: &mut Vec<Warning>) -> Vec<Value> {
    let mut blocks = Vec::new();
    for part in parts {
        match part {
            ContentPart::Text { text, .. } => {
                blocks.push(json!({ "type": "text", "text": text }));
            }
            ContentPart::Media {
                media_type,
                data,
                provider_options,
            } => {
                if let Some(block) =
                    encode_media_block(media_type, data, provider_options, warnings)
                {
                    blocks.push(block);
                }
            }
            ContentPart::Custom { kind, .. } => {
                warnings.push(Warning::unsupported(
                    "custom",
                    format!("Anthropic Messages 不支持 {kind} 内容块，已丢弃"),
                ));
            }
            _ => {}
        }
    }
    blocks
}

/// 编码单个 IR 媒体 part 为 Anthropic `image`/`document` 内容块。
///
/// 顶层段 `image` → `image` 块；`application`/`text` → `document` 块。base64
/// 数据源 → `base64` source（media_type 缺省空串直接拼装）；URL 数据源 → `url`
/// source。provider 托管形态（`file`/`text` source）经逃生舱回传。其余媒体类型
/// 丢弃并记 warning。
fn encode_media_block(
    media_type: &str,
    data: &crate::core::ir::MediaSource,
    provider_options: &crate::core::ir::ProviderOptions,
    warnings: &mut Vec<Warning>,
) -> Option<Value> {
    let top_level = crate::core::ir::top_level_media_type(media_type);
    // `document` 为入站 document 块 URL/file source 的缺省占位 media_type
    //（wire source 无 media_type 字段），必须放行否则往返断裂。
    if top_level != "image"
        && top_level != "application"
        && top_level != "text"
        && top_level != "document"
    {
        warnings.push(Warning::unsupported(
            "media",
            format!("Anthropic Messages 不支持 {top_level} 媒体类型（{media_type}），已丢弃"),
        ));
        return None;
    }
    let block_type = if top_level == "image" {
        "image"
    } else {
        "document"
    };

    // provider 托管形态（file/text source）经逃生舱回传（同协议族无损）。
    let anthropic = provider_options.get("anthropic");
    if let Some(Value::String(media_source)) = anthropic.and_then(|a| a.get("media_source"))
        && media_source == "file"
    {
        let file_id = anthropic
            .and_then(|a| a.get("file_id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Some(json!({
            "type": block_type,
            "source": { "type": "file", "file_id": file_id },
        }));
    }
    if let Some(Value::String(media_source)) = anthropic.and_then(|a| a.get("media_source"))
        && media_source == "text"
    {
        let text_data = anthropic
            .and_then(|a| a.get("data"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Some(json!({
            "type": block_type,
            "source": { "type": "text", "media_type": media_type, "data": text_data },
        }));
    }

    let source = match data {
        crate::core::ir::MediaSource::Data { base64 } => json!({
            "type": "base64",
            "media_type": media_type,
            "data": base64,
        }),
        crate::core::ir::MediaSource::Url { url } => json!({
            "type": "url",
            "url": url,
        }),
    };
    Some(json!({ "type": block_type, "source": source }))
}

/// 把内容块追加到既有 wire 消息的 `content` 数组。
fn append_blocks(message: &mut Value, blocks: Vec<Value>) {
    let content = message
        .get_mut("content")
        .and_then(Value::as_array_mut)
        .expect("合并目标应为数组 content");
    content.extend(blocks);
}

/// 编码 assistant 消息的内容块。
///
/// reasoning part 携带 `provider_options["anthropic"]["signature"]` 时回传为
/// thinking 块（同协议族无损）；`redacted_data` 回传为 redacted_thinking；
/// 无逃生舱的 reasoning 以 thinking 块无 signature 预置。跨协议族丢弃的
/// reasoning 已在出站前由调用方判定，此处只负责同协议族回传。末尾 assistant
/// 文本块的尾随空白裁剪由 `trim_trailing_whitespace` 统一处理。
fn encode_assistant_blocks(parts: &[ContentPart], warnings: &mut Vec<Warning>) -> Vec<Value> {
    let mut blocks = Vec::new();
    for part in parts {
        match part {
            ContentPart::Text { text, .. } => {
                blocks.push(json!({ "type": "text", "text": text }));
            }
            ContentPart::Reasoning {
                text,
                provider_options,
            } => {
                let anthropic = provider_options.get("anthropic");
                let signature = anthropic
                    .and_then(|a| a.get("signature"))
                    .and_then(Value::as_str);
                let redacted = anthropic
                    .and_then(|a| a.get("redacted_data"))
                    .and_then(Value::as_str);
                if let Some(redacted) = redacted {
                    blocks.push(json!({ "type": "redacted_thinking", "data": redacted }));
                } else {
                    let mut block = serde_json::Map::new();
                    block.insert("type".into(), json!("thinking"));
                    block.insert("thinking".into(), json!(text));
                    if let Some(sig) = signature {
                        block.insert("signature".into(), json!(sig));
                    }
                    blocks.push(Value::Object(block));
                }
            }
            ContentPart::ToolCall {
                tool_call_id,
                tool_name,
                input,
                ..
            } => {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": tool_call_id,
                    "name": tool_name,
                    "input": input,
                }));
            }
            ContentPart::Media { media_type, .. } => {
                // Anthropic 媒体内容块仅允许出现在 user 消息；assistant 侧媒体
                // 非标准，跨协议族转换时丢弃并记 warning。
                warnings.push(Warning::unsupported(
                    "media",
                    format!("Anthropic Messages 助手消息不支持媒体内容（{media_type}），已丢弃"),
                ));
            }
            ContentPart::Custom { kind, .. } => {
                warnings.push(Warning::unsupported(
                    "custom",
                    format!("Anthropic Messages 不支持 {kind} 内容块，已丢弃"),
                ));
            }
            ContentPart::ToolResult { .. } => {
                // assistant 消息不应携带 tool_result；忽略。
            }
        }
    }
    blocks
}

/// 编码 tool 消息的内容块：每条 tool_result。
fn encode_tool_result_blocks(parts: &[ContentPart]) -> Vec<Value> {
    parts
        .iter()
        .filter_map(|part| {
            let (tool_call_id, output, is_error) = match part {
                ContentPart::ToolResult {
                    tool_call_id,
                    output,
                    provider_options,
                    ..
                } => {
                    let is_error = provider_options
                        .get("anthropic")
                        .and_then(|a| a.get("is_error"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    (tool_call_id.clone(), output.clone(), is_error)
                }
                _ => return None,
            };
            // 输出为字符串时直接用；否则 JSON 序列化（tool_result content 是文本）。
            let content_value = match output {
                Value::String(s) => json!(s),
                other => json!(other.to_string()),
            };
            let mut block = serde_json::Map::new();
            block.insert("type".into(), json!("tool_result"));
            block.insert("tool_use_id".into(), json!(tool_call_id));
            block.insert("content".into(), content_value);
            if is_error {
                block.insert("is_error".into(), Value::Bool(true));
            }
            Some(Value::Object(block))
        })
        .collect()
}

/// 裁剪末尾 assistant 消息末尾文本块的尾随空白；裁剪后为空则移除该块，
/// 整条消息因此变空则移除该消息。
///
/// Anthropic 拒绝预置 assistant 的尾随空白，也拒绝空文本内容块与空 content
/// 数组；裁剪后如整块变空，直接丢弃而非留下 `{"type":"text","text":""}`。
fn trim_trailing_whitespace(wire_messages: &mut Vec<Value>) {
    let last_now_empty = if let Some(last) = wire_messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("assistant")
        && let Some(blocks) = last.get_mut("content").and_then(Value::as_array_mut)
        && let Some(last_block) = blocks.last_mut()
        && last_block.get("type").and_then(Value::as_str) == Some("text")
        && let Value::String(text) = last_block.get_mut("text").expect("text 块应有 text")
    {
        *text = text.trim_end().to_string();
        if text.is_empty() {
            blocks.pop();
        }
        blocks.is_empty()
    } else {
        false
    };
    if last_now_empty {
        wire_messages.pop();
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

/// 解码上游 Anthropic Messages 响应为 IR。
pub fn decode_response(value: &Value) -> Result<ChatResponse, DecodeError> {
    let wire = serde_json::from_value::<WireResponse>(value.clone())
        .map_err(|_| DecodeError::NotObject)?;

    let mut content = Vec::new();
    for block in &wire.content {
        match block {
            WireResponseBlock::Text { text } => content.push(ContentPart::Text {
                text: text.clone(),
                provider_options: HashMap::new(),
            }),
            WireResponseBlock::Thinking {
                thinking,
                signature,
            } => {
                let mut provider_options = HashMap::new();
                if let Some(sig) = signature {
                    provider_options.insert("anthropic".to_string(), json!({ "signature": sig }));
                }
                content.push(ContentPart::Reasoning {
                    text: thinking.clone(),
                    provider_options,
                });
            }
            WireResponseBlock::RedactedThinking { data } => {
                content.push(ContentPart::Reasoning {
                    text: String::new(),
                    provider_options: [("anthropic".to_string(), json!({ "redacted_data": data }))]
                        .into_iter()
                        .collect(),
                });
            }
            WireResponseBlock::ToolUse { id, name, input } => {
                content.push(ContentPart::ToolCall {
                    tool_call_id: id.clone(),
                    tool_name: name.clone(),
                    input: input.clone(),
                    provider_options: HashMap::new(),
                });
            }
        }
    }

    let usage = wire.usage.map(convert_usage).unwrap_or_default();
    let raw = wire.stop_reason.clone();
    let unified = map_stop_reason(raw.as_deref());

    Ok(ChatResponse {
        id: wire.id.unwrap_or_default(),
        model: wire.model.unwrap_or_default(),
        content,
        finish_reason: FinishReason { unified, raw },
        usage,
        provider_metadata: HashMap::new(),
        warnings: Vec::new(),
    })
}

/// 直通快路径的 usage 嗅探：从任意 JSON 值提取 Anthropic usage 折算为 IR 四分量。
///
/// Anthropic 的 usage 分布：非流式在顶层 `usage`；流式在 `message_start.message.
/// usage`（输入侧，含缓存的早期值）与 `message_delta.usage`（最终值）。input 侧
/// 为「input 不含缓存、缓存单独计」的加法约定，与 IR 口径一致。
pub fn sniff_usage(value: &Value) -> Option<Usage> {
    // 顶层 usage（非流式响应/独立帧）。
    if let Some(usage) = value.get("usage").and_then(Value::as_object) {
        return parse_usage_object(usage);
    }
    // 流式 message_start.message.usage。
    if let Some(usage) = value
        .get("message")
        .and_then(|m| m.get("usage"))
        .and_then(Value::as_object)
    {
        return parse_usage_object(usage);
    }
    None
}

/// 从 usage 对象解析 IR 四分量。
fn parse_usage_object(usage: &serde_json::Map<String, Value>) -> Option<Usage> {
    let get = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(Usage {
        input_tokens: get("input_tokens"),
        output_tokens: get("output_tokens"),
        cache_read_tokens: get("cache_read_input_tokens"),
        cache_write_tokens: get("cache_creation_input_tokens"),
        raw: Some(Value::Object(usage.clone())),
    })
}

/// usage 四分量折算：input 侧为加法约定（input 不含缓存）。
fn convert_usage(wire: WireUsage) -> Usage {
    let raw = serde_json::to_value(&wire).ok();
    Usage {
        input_tokens: wire.input_tokens,
        output_tokens: wire.output_tokens,
        cache_read_tokens: wire.cache_read_input_tokens,
        cache_write_tokens: wire.cache_creation_input_tokens,
        raw,
    }
}

/// unified finish reason 映射，对齐 mapAnthropicStopReason。
fn map_stop_reason(raw: Option<&str>) -> FinishReasonUnified {
    match raw {
        Some("end_turn") | Some("stop_sequence") | Some("pause_turn") => FinishReasonUnified::Stop,
        Some("max_tokens") | Some("model_context_window_exceeded") => FinishReasonUnified::Length,
        Some("refusal") => FinishReasonUnified::ContentFilter,
        Some("tool_use") => FinishReasonUnified::ToolCalls,
        _ => FinishReasonUnified::Other,
    }
}

/// 把 IR unified finish reason 映射为 Anthropic stop_reason。
///
/// 跨协议族转换时 `finish_reason.raw` 是出站协议的值（如 OpenAI 的 `stop`），
/// 不能透传给入站；统一从 `unified` 映射，保证跨协议族语义正确。
fn encode_stop_reason(finish_reason: &FinishReason) -> &'static str {
    match finish_reason.unified {
        FinishReasonUnified::Stop => "end_turn",
        FinishReasonUnified::Length => "max_tokens",
        FinishReasonUnified::ContentFilter => "refusal",
        FinishReasonUnified::ToolCalls => "tool_use",
        FinishReasonUnified::Error | FinishReasonUnified::Other => "end_turn",
    }
}

// ---- 入站响应编码：IR → wire ----

/// 编码 IR 响应为入站 Anthropic Messages 响应体。
///
/// 转换过程的 warnings（跨协议族丢弃的 reasoning 等）以顶层 `gateway.warnings`
/// 暴露给下游（Anthropic 无标准 warnings 字段，属非标准但无害的扩展，SDK 会忽略
/// 未知字段）；无 warning 时不写，响应与官方形状一致。
pub fn encode_response(response: &ChatResponse) -> Value {
    let content: Vec<Value> = response
        .content
        .iter()
        .filter_map(encode_response_block)
        .collect();

    let stop_reason = encode_stop_reason(&response.finish_reason);

    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(response.id));
    obj.insert("type".into(), json!("message"));
    obj.insert("role".into(), json!("assistant"));
    obj.insert("content".into(), Value::Array(content));
    obj.insert("model".into(), json!(response.model));
    obj.insert("stop_reason".into(), json!(stop_reason));
    obj.insert("stop_sequence".into(), Value::Null);
    obj.insert("usage".into(), encode_usage(&response.usage));
    if let Some(gateway) = crate::core::openai_chat::encode_warnings(&response.warnings) {
        obj.insert("gateway".into(), gateway);
    }
    Value::Object(obj)
}

/// 编码单个 IR 内容 part 为响应块；请求侧 part 不属于响应 content，返回 `None`。
fn encode_response_block(part: &ContentPart) -> Option<Value> {
    match part {
        ContentPart::Text { text, .. } => Some(json!({ "type": "text", "text": text })),
        ContentPart::Reasoning {
            text,
            provider_options,
        } => {
            let anthropic = provider_options.get("anthropic");
            let redacted = anthropic
                .and_then(|a| a.get("redacted_data"))
                .and_then(Value::as_str);
            if let Some(redacted) = redacted {
                Some(json!({ "type": "redacted_thinking", "data": redacted }))
            } else {
                let signature = anthropic
                    .and_then(|a| a.get("signature"))
                    .and_then(Value::as_str);
                let mut block = serde_json::Map::new();
                block.insert("type".into(), json!("thinking"));
                block.insert("thinking".into(), json!(text));
                if let Some(sig) = signature {
                    block.insert("signature".into(), json!(sig));
                }
                Some(Value::Object(block))
            }
        }
        ContentPart::ToolCall {
            tool_call_id,
            tool_name,
            input,
            ..
        } => Some(json!({
            "type": "tool_use",
            "id": tool_call_id,
            "name": tool_name,
            "input": input,
        })),
        // 响应 content 不携带请求侧 part。
        ContentPart::ToolResult { .. } | ContentPart::Media { .. } | ContentPart::Custom { .. } => {
            None
        }
    }
}

/// 编码 IR usage 四分量 + 缓存细节为 wire usage 对象。
///
/// Anthropic 的 input_tokens 不含缓存（与 IR 加法约定一致），缓存单独计。
fn encode_usage(usage: &Usage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_creation_input_tokens": usage.cache_write_tokens,
        "cache_read_input_tokens": usage.cache_read_tokens,
    })
}

// ---- 流式：上游 chunk → IR 流事件 ----

/// 流式解码器：把上游 Anthropic SSE 事件解码为 IR 流事件。
///
/// 对齐 AI SDK 的流式处理：`content_block_start` 开启块，`content_block_delta`
/// 产出增量（`signature_delta` 以零长增量携带 signature），`content_block_stop`
/// 收尾（tool_use 在此解析出完整 input），`message_delta` 产出 Finish（最终
/// usage + stop_reason）。
#[derive(Debug, Default)]
pub struct StreamDecoder {
    /// 按块 index 维护进行中的块状态。
    blocks: HashMap<usize, OpenBlock>,
}

/// 进行中的内容块。
#[derive(Debug)]
enum OpenBlock {
    Text,
    Reasoning,
    ToolCall { id: String, input: String },
}

/// 单个 chunk 解码结果：IR 事件 + 是否产出任何输出内容。
#[derive(Debug)]
pub struct DecodeStreamChunk {
    pub events: Vec<StreamEvent>,
    pub is_output: bool,
}

impl StreamDecoder {
    /// 解码单个上游 SSE 事件为若干 IR 流事件。
    pub fn process(&mut self, value: &Value) -> DecodeStreamChunk {
        let wire = match serde_json::from_value::<WireStreamEvent>(value.clone()) {
            Ok(wire) => wire,
            Err(_) => return DecodeStreamChunk::delivery(Vec::new()),
        };

        let mut events = Vec::new();
        let mut is_output = false;

        match wire {
            WireStreamEvent::Ping => {}
            WireStreamEvent::MessageStart { message } => {
                if let (Some(id), Some(model)) = (message.id, message.model) {
                    events.push(StreamEvent::ResponseMetadata { id, model });
                }
                // message_start 的 usage 是输入侧早期值，非最终；不在此产出 Finish。
            }
            WireStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => match content_block {
                WireContentBlock::Text => {
                    self.blocks.insert(index, OpenBlock::Text);
                    events.push(StreamEvent::TextStart {
                        id: index.to_string(),
                        provider_options: HashMap::new(),
                    });
                }
                WireContentBlock::Thinking => {
                    self.blocks.insert(index, OpenBlock::Reasoning);
                    events.push(StreamEvent::ReasoningStart {
                        id: index.to_string(),
                        provider_options: HashMap::new(),
                    });
                }
                WireContentBlock::RedactedThinking { data } => {
                    self.blocks.insert(index, OpenBlock::Reasoning);
                    events.push(StreamEvent::ReasoningStart {
                        id: index.to_string(),
                        provider_options: [(
                            "anthropic".to_string(),
                            json!({ "redacted_data": data }),
                        )]
                        .into_iter()
                        .collect(),
                    });
                }
                WireContentBlock::ToolUse { id, name, input } => {
                    let initial_input = input
                        .filter(|v| v.is_object() && !v.as_object().is_some_and(|m| m.is_empty()))
                        .map(|v| v.to_string())
                        .unwrap_or_default();
                    self.blocks.insert(
                        index,
                        OpenBlock::ToolCall {
                            id: id.clone(),
                            input: initial_input.clone(),
                        },
                    );
                    events.push(StreamEvent::ToolInputStart {
                        id: id.clone(),
                        tool_name: name,
                        provider_options: HashMap::new(),
                    });
                    // 预置的非空 input 以首个增量下发，累积器才能拼出完整参数。
                    if !initial_input.is_empty() {
                        events.push(StreamEvent::ToolInputDelta {
                            id,
                            delta: initial_input,
                            provider_options: HashMap::new(),
                        });
                    }
                }
            },
            WireStreamEvent::ContentBlockDelta { index, delta } => match delta {
                WireStreamDelta::TextDelta { text } => {
                    if !text.is_empty() {
                        is_output = true;
                        events.push(StreamEvent::TextDelta {
                            id: index.to_string(),
                            delta: text,
                            provider_options: HashMap::new(),
                        });
                    }
                }
                WireStreamDelta::ThinkingDelta { thinking } => {
                    if !thinking.is_empty() {
                        is_output = true;
                        events.push(StreamEvent::ReasoningDelta {
                            id: index.to_string(),
                            delta: thinking,
                            provider_options: HashMap::new(),
                        });
                    }
                }
                WireStreamDelta::SignatureDelta { signature } => {
                    // 零长增量仅携带 signature，供同协议族多轮 thinking 回传。
                    events.push(StreamEvent::ReasoningDelta {
                        id: index.to_string(),
                        delta: String::new(),
                        provider_options: [(
                            "anthropic".to_string(),
                            json!({ "signature": signature }),
                        )]
                        .into_iter()
                        .collect(),
                    });
                }
                WireStreamDelta::InputJsonDelta { partial_json } => {
                    if partial_json.is_empty() {
                        return DecodeStreamChunk::delivery(Vec::new());
                    }
                    is_output = true;
                    if let Some(OpenBlock::ToolCall { id, input, .. }) = self.blocks.get_mut(&index)
                    {
                        input.push_str(&partial_json);
                        events.push(StreamEvent::ToolInputDelta {
                            id: id.clone(),
                            delta: partial_json,
                            provider_options: HashMap::new(),
                        });
                    }
                }
            },
            WireStreamEvent::ContentBlockStop { index } => {
                match self.blocks.remove(&index) {
                    Some(OpenBlock::Text) => events.push(StreamEvent::TextEnd {
                        id: index.to_string(),
                        provider_options: HashMap::new(),
                    }),
                    Some(OpenBlock::Reasoning) => {
                        events.push(StreamEvent::ReasoningEnd {
                            id: index.to_string(),
                            provider_options: HashMap::new(),
                        });
                    }
                    Some(OpenBlock::ToolCall { id, .. }) => {
                        // 终端事件为 ToolInputEnd：累积器在此把已拼接的 arguments
                        // 解析为 tool-call（与 openai_chat 解码器一致），避免重复产出。
                        events.push(StreamEvent::ToolInputEnd {
                            id,
                            provider_options: HashMap::new(),
                        });
                    }
                    None => {}
                }
            }
            WireStreamEvent::MessageDelta { delta, usage } => {
                let raw = delta.stop_reason;
                let unified = map_stop_reason(raw.as_deref());
                events.push(StreamEvent::Finish {
                    finish_reason: FinishReason {
                        unified,
                        raw: raw.clone(),
                    },
                    usage: usage.map(convert_usage).unwrap_or_default(),
                    provider_metadata: HashMap::new(),
                });
            }
            WireStreamEvent::MessageStop => {}
        }

        DecodeStreamChunk { events, is_output }
    }
}

impl DecodeStreamChunk {
    fn delivery(events: Vec<StreamEvent>) -> Self {
        Self {
            events,
            is_output: false,
        }
    }
}

// ---- 流式：IR 流事件 → 入站 SSE 帧 ----

/// 把 IR 流事件编码为入站 Anthropic SSE 帧（带 `event:` 名）。
///
/// 维护块 index 与进行中的 tool_use id，把事件还原为 Anthropic 事件流：
/// `message_start`/`content_block_start`/`content_block_delta`/`content_block_stop`/
/// `message_delta`/`message_stop`。调用方负责把每帧包成 SSE 发送。
#[derive(Debug, Default)]
pub struct StreamEncoder {
    /// 内容块 index 序号（按出现顺序递增）。
    block_index: usize,
    /// 进行中的 tool_use 块 id（按块 index 记录）。
    tool_id_by_block: HashMap<usize, String>,
    /// 从 ResponseMetadata 记录的响应 id 与 model。
    id: String,
    model: String,
    /// 入站模型名覆盖：别名命中时重写响应模型名。
    inbound_model: Option<String>,
}

impl StreamEncoder {
    /// 指定入站模型名覆盖（别名重写响应模型名）；`None` 表示不覆盖。
    pub fn new(inbound_model: Option<String>) -> Self {
        Self {
            inbound_model,
            ..Self::default()
        }
    }

    /// 编码一个 IR 流事件，返回需要下发的 SSE 帧（可能为空）。
    pub fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        match event {
            StreamEvent::StreamStart { warnings } => {
                // Anthropic 无标准 warnings 通道：以 `ping` 事件携带，SDK 忽略。
                if warnings.is_empty() {
                    Vec::new()
                } else {
                    let gateway = crate::core::openai_chat::encode_warnings(warnings)
                        .unwrap_or_else(|| json!({ "warnings": [] }));
                    vec![SseFrame::named("ping", gateway.to_string())]
                }
            }
            StreamEvent::ResponseMetadata { id, model } => {
                self.id = id.clone();
                self.model = model.clone();
                Vec::new()
            }
            StreamEvent::TextStart { .. } => {
                let index = self.next_index();
                let frame = self.content_block_start(index, json!({ "type": "text", "text": "" }));
                vec![frame]
            }
            StreamEvent::TextDelta { delta, .. } => {
                let index = self.current_index();
                vec![SseFrame::named(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "text_delta", "text": delta },
                    })
                    .to_string(),
                )]
            }
            StreamEvent::TextEnd { .. } => {
                vec![self.content_block_stop(self.current_index())]
            }
            StreamEvent::ReasoningStart {
                provider_options, ..
            } => {
                let index = self.next_index();
                let redacted = provider_options
                    .get("anthropic")
                    .and_then(|a| a.get("redacted_data"))
                    .and_then(Value::as_str);
                let block = match redacted {
                    Some(data) => json!({ "type": "redacted_thinking", "data": data }),
                    None => json!({ "type": "thinking", "thinking": "", "signature": "" }),
                };
                vec![self.content_block_start(index, block)]
            }
            StreamEvent::ReasoningDelta {
                delta,
                provider_options,
                ..
            } => {
                let index = self.current_index();
                let mut frames = Vec::new();
                if !delta.is_empty() {
                    frames.push(SseFrame::named(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "thinking_delta", "thinking": delta },
                        })
                        .to_string(),
                    ));
                }
                if let Some(signature) = provider_options
                    .get("anthropic")
                    .and_then(|a| a.get("signature"))
                    .and_then(Value::as_str)
                {
                    frames.push(SseFrame::named(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "signature_delta", "signature": signature },
                        })
                        .to_string(),
                    ));
                }
                frames
            }
            StreamEvent::ReasoningEnd { .. } => {
                vec![self.content_block_stop(self.current_index())]
            }
            StreamEvent::ToolInputStart { id, tool_name, .. } => {
                let index = self.next_index();
                self.tool_id_by_block.insert(index, id.clone());
                let block = json!({
                    "type": "tool_use",
                    "id": id,
                    "name": tool_name,
                    "input": {},
                });
                vec![self.content_block_start(index, block)]
            }
            StreamEvent::ToolInputDelta { id, delta, .. } => {
                let index = self.block_index_for(id);
                vec![SseFrame::named(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "input_json_delta", "partial_json": delta },
                    })
                    .to_string(),
                )]
            }
            StreamEvent::ToolInputEnd { id, .. } => {
                let index = self.block_index_for(id);
                self.tool_id_by_block.remove(&index);
                vec![self.content_block_stop(index)]
            }
            // ToolCall 是 tool_use 汇聚完成的完整表示；输入已由增量流下发，无需额外帧。
            StreamEvent::ToolCall { .. } => Vec::new(),
            StreamEvent::Finish {
                finish_reason,
                usage,
                ..
            } => {
                let stop_reason = encode_stop_reason(finish_reason);
                let delta = json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                    "usage": encode_usage(usage),
                });
                let message_stop = json!({ "type": "message_stop" });
                vec![
                    SseFrame::named("message_delta", delta.to_string()),
                    SseFrame::named("message_stop", message_stop.to_string()),
                ]
            }
        }
    }

    /// 请求流首的 `message_start` 帧（含 id/model 与空内容）。
    pub fn message_start(&self) -> SseFrame {
        let message = json!({
            "type": "message",
            "role": "assistant",
            "id": if self.id.is_empty() { "msg_stream" } else { self.id.as_str() },
            "model": self.inbound_model.as_deref().unwrap_or(&self.model),
            "content": [],
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
            },
        });
        SseFrame::named(
            "message_start",
            json!({ "type": "message_start", "message": message }).to_string(),
        )
    }

    fn next_index(&mut self) -> usize {
        let index = self.block_index;
        self.block_index += 1;
        index
    }

    fn current_index(&self) -> usize {
        self.block_index.saturating_sub(1)
    }

    fn block_index_for(&self, tool_id: &str) -> usize {
        self.tool_id_by_block
            .iter()
            .find(|(_, id)| id.as_str() == tool_id)
            .map(|(index, _)| *index)
            .unwrap_or_else(|| self.current_index())
    }

    fn content_block_start(&self, index: usize, block: Value) -> SseFrame {
        SseFrame::named(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": block,
            })
            .to_string(),
        )
    }

    fn content_block_stop(&self, index: usize) -> SseFrame {
        SseFrame::named(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": index }).to_string(),
        )
    }
}

// ---- 错误编码 ----

/// 编码为 Anthropic 错误格式 `{"type":"error","error":{"type":...,"message":...}}`。
///
/// `error.type` 固定为 `api_error`（Anthropic 官方约定）；`error` 内层 `type` 按
/// 状态码映射，客户端错误为 `invalid_request_error`。
pub fn encode_error(status: u16, message: &str) -> Value {
    let error_type = if (400..500).contains(&status) {
        "invalid_request_error"
    } else {
        "api_error"
    };
    json!({
        "type": "error",
        "error": {
            "type": error_type,
            "message": message,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::stream::StreamAccumulator;

    /// 黄金样例请求 decode → encode 往返还原 wire。
    ///
    /// 覆盖：system 提升、user 纯文本、assistant 文本 + tool_use、tool 消息的
    /// tool_result 还原为 user 消息、temperature/max_tokens。
    #[test]
    fn request_fixture_roundtrip() {
        let raw = include_str!("__fixtures__/request.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let ir = decode_request(&wire).expect("fixture 应可解码为 IR");
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert_eq!(reencoded, wire, "往返应还原 wire 请求");
        assert!(warnings.is_empty(), "同协议往返不应产出 warning");
    }

    /// 多模态黄金样例请求 decode → encode 往返还原 wire，文本与媒体混排顺序不丢。
    ///
    /// 覆盖 base64/URL 两种 source、image/document 两种块、6 part 混排顺序。
    #[test]
    fn multimodal_fixture_roundtrip() {
        let raw = include_str!("__fixtures__/request_multimodal.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let ir = decode_request(&wire).expect("fixture 应可解码为 IR");
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert_eq!(reencoded, wire, "往返应还原 wire 请求（含混排顺序）");
        assert!(warnings.is_empty(), "同协议图片/文档往返不应产出 warning");

        // 混排顺序：text → 图片(base64) → text → 文档(base64) → text → 图片(URL)。
        let parts = &ir.messages[0].content;
        assert_eq!(parts.len(), 6, "应保留 6 个 part");
        assert!(matches!(parts[0], ContentPart::Text { .. }));
        assert!(matches!(
            &parts[1],
            ContentPart::Media {
                media_type,
                data: crate::core::ir::MediaSource::Data { base64 },
                ..
            } if media_type == "image/png" && base64 == "iVBORw0KGgoAAAANSUhEUg=="
        ));
        assert!(matches!(parts[2], ContentPart::Text { .. }));
        assert!(matches!(
            &parts[3],
            ContentPart::Media {
                media_type,
                data: crate::core::ir::MediaSource::Data { base64 },
                ..
            } if media_type == "application/pdf" && base64 == "JVBERi0xLjQK"
        ));
        assert!(matches!(parts[4], ContentPart::Text { .. }));
        assert!(matches!(
            &parts[5],
            ContentPart::Media {
                data: crate::core::ir::MediaSource::Url { url },
                ..
            } if url == "https://example.com/image.png"
        ));
    }

    /// URL source 的 image 块 decode → encode 往返：media_type 缺省空串，出站按
    /// image 顶层段兜底为 `image` 块。
    #[test]
    fn url_image_roundtrips_with_default_media_type() {
        let wire = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 100,
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image",
                    "source": { "type": "url", "url": "https://example.com/x.png" }
                }]
            }]
        });
        let ir = decode_request(&wire).expect("应可解码");
        assert!(matches!(
            &ir.messages[0].content[0],
            ContentPart::Media {
                data: crate::core::ir::MediaSource::Url { url },
                ..
            } if url == "https://example.com/x.png"
        ));
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert_eq!(reencoded, wire, "URL source 往返应还原 wire");
        assert!(warnings.is_empty());
    }

    /// URL source 的 document 块 decode → encode 往返：media_type 缺省占位为
    /// `document`，出站经顶层段放行还原为 document 块（不丢弃记 warning）。
    #[test]
    fn url_document_roundtrips_with_placeholder_media_type() {
        let wire = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 100,
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "document",
                    "source": { "type": "url", "url": "https://example.com/doc.pdf" }
                }]
            }]
        });
        let ir = decode_request(&wire).expect("应可解码");
        assert!(matches!(
            &ir.messages[0].content[0],
            ContentPart::Media {
                media_type,
                data: crate::core::ir::MediaSource::Url { url },
                ..
            } if media_type == "document" && url == "https://example.com/doc.pdf"
        ));
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert_eq!(reencoded, wire, "document URL source 往返应还原 wire");
        assert!(warnings.is_empty(), "document URL 往返不应记 warning");
    }

    /// 目标协议不支持的媒体类型（非 image/application/text 顶层段）出站时丢弃并记 warning。
    #[test]
    fn unsupported_media_is_dropped_with_warning() {
        use crate::core::ir::Message;
        let request = ChatRequest {
            model: "claude-sonnet-4-5".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentPart::Media {
                    media_type: "video/mp4".to_string(),
                    data: crate::core::ir::MediaSource::Url {
                        url: "https://example.com/v.mp4".to_string(),
                    },
                    provider_options: HashMap::new(),
                }],
                provider_options: HashMap::new(),
            }],
            stream: false,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: Some(100),
            n: None,
            stop: Vec::new(),
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
            response_format: None,
            tools: Vec::new(),
            tool_choice: None,
            provider_options: HashMap::new(),
        };
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        // 非支持媒体被丢弃：user 消息整体跳过（无内容可编码）。
        assert!(
            encoded["messages"]
                .as_array()
                .map(Vec::is_empty)
                .unwrap_or(true),
            "全 media 丢弃后 user 消息应整体跳过"
        );
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, Warning::Unsupported { feature, .. } if feature == "media")),
            "媒体丢弃应记 warning"
        );
    }

    /// 黄金样例响应 decode → encode 往返还原 wire。
    ///
    /// 覆盖：assistant 消息解码为 IR → 重编码。响应顶层的 `gateway` 字段在解码
    /// 时被忽略，重编码不含（同协议往返无 warning）。
    #[test]
    fn response_fixture_roundtrip() {
        let raw = include_str!("__fixtures__/response.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let ir = decode_response(&wire).expect("fixture 应可解码为 IR");
        let reencoded = encode_response(&ir);
        // 响应 fixture 无 gateway 字段，重编码亦不含。
        assert_eq!(reencoded, wire, "往返应还原 wire 响应");
    }

    /// 响应解码：text/thinking(signature)/tool_use 映射为 IR，usage 加法约定。
    #[test]
    fn response_decodes_blocks_and_usage() {
        let wire = json!({
            "id": "msg_01", "type": "message", "role": "assistant", "model": "claude-sonnet",
            "content": [
                { "type": "thinking", "thinking": "先想", "signature": "sig123" },
                { "type": "text", "text": "结果" },
                { "type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": { "city": "SF" } }
            ],
            "stop_reason": "tool_use", "stop_sequence": null,
            "usage": {
                "input_tokens": 200, "output_tokens": 50,
                "cache_creation_input_tokens": 30, "cache_read_input_tokens": 40
            }
        });
        let ir = decode_response(&wire).expect("应可解码");
        assert_eq!(ir.content.len(), 3);
        assert!(matches!(
            &ir.content[0],
            ContentPart::Reasoning { text, provider_options }
                if text == "先想" && provider_options["anthropic"]["signature"] == "sig123"
        ));
        assert!(matches!(&ir.content[1], ContentPart::Text { text, .. } if text == "结果"));
        assert!(matches!(
            &ir.content[2],
            ContentPart::ToolCall { tool_call_id, tool_name, .. }
                if tool_call_id == "toolu_1" && tool_name == "get_weather"
        ));
        assert_eq!(ir.finish_reason.unified, FinishReasonUnified::ToolCalls);
        assert_eq!(ir.finish_reason.raw.as_deref(), Some("tool_use"));
        // input 加法约定：input 不含缓存，缓存单独计。
        assert_eq!(ir.usage.input_tokens, 200);
        assert_eq!(ir.usage.cache_read_tokens, 40);
        assert_eq!(ir.usage.cache_write_tokens, 30);
        assert_eq!(ir.usage.output_tokens, 50);
    }

    /// 请求编码：同协议族回传 thinking signature（多轮不被上游拒绝）。
    #[test]
    fn request_encodes_thinking_signature() {
        let request = ChatRequest {
            model: "claude-sonnet".to_string(),
            messages: vec![
                Message {
                    role: Role::User,
                    content: vec![ContentPart::Text {
                        text: "再算一次".to_string(),
                        provider_options: HashMap::new(),
                    }],
                    provider_options: HashMap::new(),
                },
                Message {
                    role: Role::Assistant,
                    content: vec![ContentPart::Reasoning {
                        text: "重算 925 ÷ 5".to_string(),
                        provider_options: [(
                            "anthropic".to_string(),
                            json!({ "signature": "ErUBCkY" }),
                        )]
                        .into_iter()
                        .collect(),
                    }],
                    provider_options: HashMap::new(),
                },
            ],
            stream: false,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: Some(1024),
            n: None,
            stop: Vec::new(),
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
            response_format: None,
            tools: Vec::new(),
            tool_choice: None,
            provider_options: HashMap::new(),
        };
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        let assistant = encoded["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("应有 assistant 消息");
        assert_eq!(assistant["content"][0]["type"], "thinking");
        assert_eq!(assistant["content"][0]["signature"], "ErUBCkY");
        assert!(warnings.is_empty());
    }

    /// 请求编码：tool 消息的 tool_result 还原为 user 消息，assistant 连续消息合并。
    #[test]
    fn request_encodes_tool_results_into_user() {
        let request = ChatRequest {
            model: "claude-sonnet".to_string(),
            messages: vec![
                Message {
                    role: Role::Assistant,
                    content: vec![ContentPart::ToolCall {
                        tool_call_id: "toolu_1".to_string(),
                        tool_name: "get_weather".to_string(),
                        input: json!({ "city": "SF" }),
                        provider_options: HashMap::new(),
                    }],
                    provider_options: HashMap::new(),
                },
                Message {
                    role: Role::Tool,
                    content: vec![ContentPart::ToolResult {
                        tool_call_id: "toolu_1".to_string(),
                        tool_name: "get_weather".to_string(),
                        output: json!("sunny, 72F"),
                        provider_options: HashMap::new(),
                    }],
                    provider_options: HashMap::new(),
                },
            ],
            stream: false,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: Some(1024),
            n: None,
            stop: Vec::new(),
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
            response_format: None,
            tools: Vec::new(),
            tool_choice: None,
            provider_options: HashMap::new(),
        };
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        let messages = encoded["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"][0]["type"], "tool_use");
        // tool 消息 → user + tool_result。
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"][0]["type"], "tool_result");
        assert_eq!(messages[1]["content"][0]["tool_use_id"], "toolu_1");
        assert_eq!(messages[1]["content"][0]["content"], "sunny, 72F");
    }

    /// 黄金样例流式往返：解码流式事件 → 累积，与非流式 `response.json` 解码同构。
    #[test]
    fn stream_fixture_accumulates_to_response() {
        let mut decoder = StreamDecoder::default();
        let mut accumulator = StreamAccumulator::new();

        for raw in [
            include_str!("__fixtures__/stream_message_start.json"),
            include_str!("__fixtures__/stream_thinking_start.json"),
            include_str!("__fixtures__/stream_thinking_delta.json"),
            include_str!("__fixtures__/stream_thinking_signature.json"),
            include_str!("__fixtures__/stream_thinking_stop.json"),
            include_str!("__fixtures__/stream_text_start.json"),
            include_str!("__fixtures__/stream_text_delta.json"),
            include_str!("__fixtures__/stream_text_stop.json"),
            include_str!("__fixtures__/stream_message_delta.json"),
        ] {
            let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
            for event in decoder.process(&wire).events {
                accumulator.push(event);
            }
        }
        let streamed = accumulator.finish();

        // 非流式黄金样例：response.json（同一 thinking + text + usage + stop_reason）。
        let raw = include_str!("__fixtures__/response.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let non_stream = decode_response(&wire).expect("fixture 应可解码");

        // 同构：流式累积结果与非流式解码一致（含 thinking 的 signature 逃生舱）。
        assert_eq!(streamed, non_stream);
    }

    /// 流式 tool_use 输入跨帧累积，`content_block_stop` 收尾为完整 tool-call。
    #[test]
    fn stream_tool_use_input_accumulates() {
        let mut decoder = StreamDecoder::default();
        let mut accumulator = StreamAccumulator::new();

        for raw in [
            include_str!("__fixtures__/stream_tool_start.json"),
            include_str!("__fixtures__/stream_tool_args.json"),
            include_str!("__fixtures__/stream_tool_stop.json"),
            include_str!("__fixtures__/stream_finish.json"),
        ] {
            let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
            for event in decoder.process(&wire).events {
                accumulator.push(event);
            }
        }
        let response = accumulator.finish();
        // 恰好一个 tool-call，无重复（ADR-0001 流式/非流式同构）。
        assert_eq!(
            response.content.len(),
            1,
            "tool_use 块应只产出一次 tool-call"
        );
        assert!(matches!(
            &response.content[0],
            ContentPart::ToolCall { tool_call_id, tool_name, input, .. }
                if tool_call_id == "toolu_01" && tool_name == "get_weather"
                    && input == &json!({ "city": "San Francisco" })
        ));
        assert_eq!(
            response.finish_reason.unified,
            FinishReasonUnified::ToolCalls
        );
    }

    /// 空参数 tool_use（`content_block_start` 与 `stop` 之间无增量）收尾为 `{}`。
    #[test]
    fn stream_empty_tool_input_defaults_to_empty_object() {
        let mut decoder = StreamDecoder::default();
        let mut accumulator = StreamAccumulator::new();
        for raw in [
            include_str!("__fixtures__/stream_tool_start.json"),
            include_str!("__fixtures__/stream_tool_stop.json"),
        ] {
            let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
            for event in decoder.process(&wire).events {
                accumulator.push(event);
            }
        }
        let response = accumulator.finish();
        assert_eq!(response.content.len(), 1);
        assert!(
            matches!(
                &response.content[0],
                ContentPart::ToolCall { input, .. } if input == &json!({})
            ),
            "空参数 tool_use 应收尾为 {{}}"
        );
    }

    /// 解析 SSE 帧的 `data:` 载荷为 JSON，供帧内容断言。
    fn frame_json(frame: &SseFrame) -> Value {
        serde_json::from_str(&frame.data).expect("帧载荷应为合法 JSON")
    }

    /// 流式 IR 事件编码为入站 Anthropic SSE 帧：带事件名，message_finish 收尾。
    #[test]
    fn stream_events_encode_to_anthropic_frames() {
        let mut encoder = StreamEncoder::default();
        let start = encoder.message_start();
        assert_eq!(start.event.as_deref(), Some("message_start"));
        assert!(start.data.contains("msg_stream"));

        let frames = encoder.encode(&StreamEvent::TextStart {
            id: "0".to_string(),
            provider_options: HashMap::new(),
        });
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("content_block_start"));
        assert_eq!(frame_json(&frames[0])["content_block"]["type"], "text");

        let frames = encoder.encode(&StreamEvent::TextDelta {
            id: "0".to_string(),
            delta: "Hi".to_string(),
            provider_options: HashMap::new(),
        });
        assert_eq!(frames[0].event.as_deref(), Some("content_block_delta"));
        assert_eq!(frame_json(&frames[0])["delta"]["type"], "text_delta");
        assert_eq!(frame_json(&frames[0])["delta"]["text"], "Hi");

        let frames = encoder.encode(&StreamEvent::TextEnd {
            id: "0".to_string(),
            provider_options: HashMap::new(),
        });
        assert_eq!(frames[0].event.as_deref(), Some("content_block_stop"));

        let frames = encoder.encode(&StreamEvent::Finish {
            finish_reason: FinishReason {
                unified: FinishReasonUnified::ToolCalls,
                raw: Some("tool_use".to_string()),
            },
            usage: Usage {
                input_tokens: 3,
                output_tokens: 2,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                raw: None,
            },
            provider_metadata: HashMap::new(),
        });
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event.as_deref(), Some("message_delta"));
        assert_eq!(frame_json(&frames[0])["delta"]["stop_reason"], "tool_use");
        assert_eq!(frame_json(&frames[0])["usage"]["output_tokens"], 2);
        assert_eq!(frames[1].event.as_deref(), Some("message_stop"));
    }

    /// 流式 signature_delta 编码：附随进行中 thinking 块的增量事件。
    #[test]
    fn stream_encodes_signature_delta() {
        let mut encoder = StreamEncoder::default();
        encoder.encode(&StreamEvent::ReasoningStart {
            id: "0".to_string(),
            provider_options: HashMap::new(),
        });
        let frames = encoder.encode(&StreamEvent::ReasoningDelta {
            id: "0".to_string(),
            delta: "先想".to_string(),
            provider_options: HashMap::new(),
        });
        // 内容增量 + 无 signature → 单帧 thinking_delta。
        assert_eq!(frames.len(), 1);
        assert_eq!(frame_json(&frames[0])["delta"]["type"], "thinking_delta");

        let frames = encoder.encode(&StreamEvent::ReasoningDelta {
            id: "0".to_string(),
            delta: String::new(),
            provider_options: [("anthropic".to_string(), json!({ "signature": "sigX" }))]
                .into_iter()
                .collect(),
        });
        assert_eq!(frames.len(), 1, "零长增量仅携带 signature");
        assert_eq!(frame_json(&frames[0])["delta"]["type"], "signature_delta");
        assert_eq!(frame_json(&frames[0])["delta"]["signature"], "sigX");
    }

    /// 直通 usage 嗅探：顶层、message_start.message、message_delta 三种分布都提取。
    #[test]
    fn sniff_usage_covers_all_usage_locations() {
        // 非流式顶层 usage。
        let resp = json!({
            "usage": {
                "input_tokens": 200, "output_tokens": 50,
                "cache_creation_input_tokens": 30, "cache_read_input_tokens": 40
            }
        });
        let usage = sniff_usage(&resp).expect("顶层 usage 应提取");
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 40);
        assert_eq!(usage.cache_write_tokens, 30);

        // 流式 message_start.message.usage。
        let start = json!({
            "type": "message_start",
            "message": { "usage": { "input_tokens": 10, "output_tokens": 0 } }
        });
        let usage = sniff_usage(&start).expect("message_start usage 应提取");
        assert_eq!(usage.input_tokens, 10);

        // 无 usage 的帧返回 None。
        assert!(sniff_usage(&json!({ "content_block_start": true })).is_none());
    }

    /// Anthropic 强制要求 max_tokens：IR 缺省时补默认 4096，避免跨协议请求被拒。
    #[test]
    fn request_defaults_max_tokens_when_absent() {
        let request = ChatRequest {
            model: "claude-sonnet".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentPart::Text {
                    text: "hi".to_string(),
                    provider_options: HashMap::new(),
                }],
                provider_options: HashMap::new(),
            }],
            stream: false,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            n: None,
            stop: Vec::new(),
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
            response_format: None,
            tools: Vec::new(),
            tool_choice: None,
            provider_options: HashMap::new(),
        };
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        assert_eq!(
            encoded["max_tokens"], 4096,
            "缺 max_tokens 应补 Anthropic 默认"
        );
    }

    /// 错误编码为 Anthropic 格式；内层 type 按状态码：客户端错误为
    /// invalid_request_error，服务端错误为 api_error。
    #[test]
    fn encode_error_is_anthropic_shape() {
        let err = encode_error(429, "rate limited");
        assert_eq!(err["type"], "error");
        assert_eq!(err["error"]["type"], "invalid_request_error");
        assert_eq!(err["error"]["message"], "rate limited");

        let err = encode_error(500, "boom");
        assert_eq!(err["error"]["type"], "api_error");
    }
}
