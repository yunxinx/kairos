//! Anthropic Messages 协议适配器：wire ↔ IR 双向编解码。
//!
//! wire 结构体全部私有，透过 `decode_*`/`encode_*` 公共函数暴露 IR 边界，
//! wire 类型不出本模块边界。
//!
//! 映射要点：
//! - 请求侧：首个 system 消息提升为顶层 `system`；assistant 内容块
//!   `text`/`thinking`/`redacted_thinking`/`tool_use` 与 user 内容块
//!   `text`/`tool_result`/`image`/`document`（base64/URL source）双向映射；
//!   thinking signature 经 part 逃生舱 `provider_options["anthropic"]["signature"]`
//!   无损往返。
//! - 响应侧：`stop_reason` 双轨映射，usage 输入侧为「input 不含缓存、
//!   缓存单独计」的加法约定（与口径一致）。
//! - 流式：事件名驱动的 SSE（`event:` 名），`signature_delta` 以零长增量携带
//!   signature，`message_delta` 携带最终 usage 与 stop_reason。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::core::ir::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, FinishReasonUnified, Message,
    PROVIDER_EXTRA_KEY, ReasoningEffort, Role, StreamEvent, Tool, ToolChoice, Usage, Warning,
    apply_provider_extra, capture_unknown_fields, warning_feature,
};
use crate::core::schema::normalize_object_root;
use crate::core::stream::{ErrorMessageShape, SseFrame, decode_failed_frame};

// ---- 错误 ----

/// wire 解码错误，网关映射为 400。
#[derive(Debug, Error)]
pub enum DecodeError {
    /// wire 形状不符：携带 serde 的具体原因与出错字段的 JSON 路径
    /// （如 `temperature: invalid type: string "hot"`；untagged 枚举处形如
    /// `messages[1].content: data did not match any variant of untagged
    /// enum WireContent`；顶层错误无路径前缀）。
    #[error("wire 形状不符: {detail}")]
    InvalidShape { detail: String },
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
    #[error("tool_choice 形状无法识别: {detail}")]
    InvalidToolChoice { detail: String },
    #[error("响应缺少 usage")]
    MissingUsage,
}

// ---- wire 请求类型 ----

/// 本协议已知顶层请求字段白名单；白名单外的顶层字段由入站解码收进
/// 未知字段逃生舱（`provider_options["anthropic"]["extra"]`）。
const KNOWN_REQUEST_FIELDS: &[&str] = &[
    "model",
    "max_tokens",
    "messages",
    "system",
    "temperature",
    "top_p",
    "top_k",
    "stop_sequences",
    "stream",
    "tools",
    "tool_choice",
    "cache_control",
    "thinking",
    "output_config",
];

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
    /// 请求级缓存断点，捕获进请求级 `provider_options["anthropic"]["cache_control"]`。
    #[serde(default)]
    cache_control: Option<Value>,
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
    /// 请求级逃生舱：`output_config`（原生 effort 档位、structured outputs 的
    /// format、task budget 等）。同协议族经 IR 出站时原样回传；`effort` 键
    /// 另捕获进类型化 reasoning 旋钮供跨族映射。
    #[serde(default)]
    output_config: Option<Value>,
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
///
/// 可缓存块（text/tool_use/tool_result/image/document）携带可选
/// `cache_control` 断点；thinking 类块不可缓存，无该字段。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireBlock {
    Text {
        text: String,
        #[serde(default)]
        cache_control: Option<Value>,
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
        #[serde(default)]
        cache_control: Option<Value>,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Option<Value>,
        #[serde(default)]
        is_error: Option<bool>,
        #[serde(default)]
        cache_control: Option<Value>,
    },
    /// 媒体内容块：`image`（图片）或 `document`（文档）。source 可为
    /// base64 字节、URL 或 provider 托管引用（`file_id`/`text`）。
    Image {
        #[serde(default)]
        source: Option<WireMediaSource>,
        #[serde(default)]
        cache_control: Option<Value>,
    },
    Document {
        #[serde(default)]
        source: Option<WireMediaSource>,
        #[serde(default)]
        cache_control: Option<Value>,
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
    /// 工具级缓存断点，捕获进 `Tool.provider_options["anthropic"]["cache_control"]`。
    #[serde(default)]
    cache_control: Option<Value>,
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
    /// 上游回报的 cache 写入 TTL 明细；仅 1h 档参与计费分档，缺省不出现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_creation: Option<WireCacheCreation>,
}

/// 上游 usage 的 `cache_creation` TTL 明细。
#[derive(Debug, Clone, Deserialize, Serialize)]
struct WireCacheCreation {
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
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
    #[serde(rename = "error")]
    Error {
        error: WireStreamError,
    },
    Ping,
}

/// `event: error` 的错误体（`type` 为 `overloaded_error` 等判别名，不消费）。
#[derive(Debug, Clone, Deserialize)]
struct WireStreamError {
    #[serde(default)]
    message: Option<String>,
}

/// `message_start` 的 message 首部：id/model 与输入侧 usage 早期值。
#[derive(Debug, Clone, Deserialize)]
struct WireStreamMessage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<WireUsage>,
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
    let wire: WireRequest = serde_path_to_error::deserialize(value.clone()).map_err(|err| {
        DecodeError::InvalidShape {
            detail: err.to_string(),
        }
    })?;

    let mut messages = Vec::new();
    // 顶层 `system` 提升为首条 System 消息；块数组尾块的 cache_control 断点
    // 进消息级逃生舱（出站挂到合并后 system 尾块）。
    if let Some(system) = &wire.system {
        let text = system_text(system);
        if let Some(text) = text {
            messages.push(Message {
                role: Role::System,
                content: vec![ContentPart::Text {
                    text,
                    provider_options: HashMap::new(),
                }],
                provider_options: system_cache_options(system),
            });
        }
    }

    for (index, wire_message) in wire.messages.iter().enumerate() {
        messages.extend(decode_message(wire_message, index)?);
    }

    let mut provider_options = HashMap::new();
    // 类型化 effort 从 thinking 配置派生（有损，仅供跨族映射与观测）；
    // 原始配置已进逃生舱，同族往返不受派生精度影响。
    let mut reasoning = wire.thinking.as_ref().and_then(|thinking| {
        match thinking.get("type").and_then(Value::as_str) {
            Some("disabled") => Some(ReasoningEffort::None),
            Some("enabled") => {
                thinking
                    .get("budget_tokens")
                    .and_then(Value::as_u64)
                    .map(|budget| {
                        ReasoningEffort::from_budget(u32::try_from(budget).unwrap_or(u32::MAX))
                    })
            }
            // adaptive 等无法枚举量化的配置仅由逃生舱承载。
            _ => None,
        }
    });
    if let Some(thinking) = &wire.thinking {
        provider_options.insert("anthropic".to_string(), json!({ "thinking": thinking }));
    }
    // 原生 effort 档位（adaptive/native-effort 模型的请求面）捕获进类型化
    // 旋钮；未识别的取值不拒绝——原始对象已在逃生舱，档位语义留空即可。
    // 显式 effort 比 budget 派生更直接，命中时覆盖。
    if let Some(effort) = wire
        .output_config
        .as_ref()
        .and_then(|config| config.get("effort"))
        .and_then(Value::as_str)
        .and_then(ReasoningEffort::parse_effort)
    {
        reasoning = Some(effort);
    }
    if let Some(output_config) = &wire.output_config {
        let entry = provider_options
            .entry("anthropic".to_string())
            .or_insert_with(|| json!({}));
        if let Value::Object(anthropic) = entry {
            anthropic.insert("output_config".into(), output_config.clone());
        }
    }
    // 白名单外的顶层字段收进未知字段逃生舱，同族出站原样回写。
    let extra = capture_unknown_fields(value, KNOWN_REQUEST_FIELDS);
    if !extra.is_empty() {
        let entry = provider_options
            .entry("anthropic".to_string())
            .or_insert_with(|| json!({}));
        if let Value::Object(anthropic) = entry {
            anthropic.insert(PROVIDER_EXTRA_KEY.to_string(), Value::Object(extra));
        }
    }
    // 请求级缓存断点原样捕获（ttl/scope 等子字段随值透传）。
    if let Some(cache_control) = &wire.cache_control {
        let entry = provider_options
            .entry("anthropic".to_string())
            .or_insert_with(|| json!({}));
        if let Value::Object(anthropic) = entry {
            anthropic.insert("cache_control".into(), cache_control.clone());
        }
    }
    let tool_choice = wire
        .tool_choice
        .as_ref()
        .map(|value| decode_tool_choice(value, &mut provider_options))
        .transpose()?;
    // 反语义取反：`disable_parallel_tool_use` 承载为 IR `parallel_tool_calls`
    // （true → 允许并行）；字段缺省时旋钮留空（Anthropic 缺省允许并行）。
    let parallel_tool_calls = wire
        .tool_choice
        .as_ref()
        .and_then(|choice| choice.get("disable_parallel_tool_use"))
        .and_then(Value::as_bool)
        .map(|disable| !disable);

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
                provider_options: cache_control_options(&t.cache_control),
            })
            .collect(),
        tool_choice,
        parallel_tool_calls,
        reasoning,
        provider_options,
        warnings: Vec::new(),
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

/// 顶层 `system` 块数组的缓存断点为消息级逃生舱。
///
/// 断点约定挂合并后 system 的尾块，多块各带断点时取最后出现的（后者为
/// 更靠后的前缘）；字符串 system 无块级断点，返回空集。
fn system_cache_options(system: &Value) -> crate::core::ir::ProviderOptions {
    let mut cache_control = None;
    if let Value::Array(blocks) = system {
        for block in blocks {
            if let Some(cc) = block.get("cache_control") {
                cache_control = Some(cc.clone());
            }
        }
    }
    cache_control_options(&cache_control)
}

/// 可选 cache_control 断点为 part/工具级逃生舱（约定键
/// `provider_options["anthropic"]["cache_control"]`）；缺席时空集。
fn cache_control_options(cache_control: &Option<Value>) -> crate::core::ir::ProviderOptions {
    cache_control
        .as_ref()
        .map(|cc| {
            [("anthropic".to_string(), json!({ "cache_control": cc }))]
                .into_iter()
                .collect()
        })
        .unwrap_or_default()
}

/// 解码 wire `tool_choice` 为 IR 类型化枚举。
///
/// `type`/`name` 之外的键是 Anthropic 附加语义：`disable_parallel_tool_use`
/// 取反映射为 IR 类型化 `parallel_tool_calls`（由调用方提取），其余键经请求级
/// 逃生舱 `provider_options["anthropic"]["tool_choice_extra"]` 保留，只在
/// Anthropic 出站写回；逃生舱并入已有的 `anthropic` 对象，thinking 等先前
/// 捕获的配置共存。
fn decode_tool_choice(
    value: &Value,
    provider_options: &mut crate::core::ir::ProviderOptions,
) -> Result<ToolChoice, DecodeError> {
    let Value::Object(map) = value else {
        return Err(DecodeError::InvalidToolChoice {
            detail: "应为对象形状".to_string(),
        });
    };
    let extra: serde_json::Map<String, Value> = map
        .iter()
        .filter(|(key, _)| {
            key.as_str() != "type"
                && key.as_str() != "name"
                && key.as_str() != "disable_parallel_tool_use"
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if !extra.is_empty() {
        let entry = provider_options
            .entry("anthropic".to_string())
            .or_insert_with(|| json!({}));
        if let Value::Object(anthropic) = entry {
            anthropic.insert("tool_choice_extra".into(), Value::Object(extra));
        }
    }
    match map.get("type").and_then(Value::as_str) {
        Some("auto") => Ok(ToolChoice::Auto),
        Some("any") => Ok(ToolChoice::Required),
        Some("none") => Ok(ToolChoice::None),
        Some("tool") => {
            let name = map.get("name").and_then(Value::as_str).unwrap_or_default();
            if name.is_empty() {
                Err(DecodeError::InvalidToolChoice {
                    detail: "type=tool 缺少 name".to_string(),
                })
            } else {
                Ok(ToolChoice::Tool {
                    name: name.to_string(),
                })
            }
        }
        other => Err(DecodeError::InvalidToolChoice {
            detail: format!("未知 type {other:?}"),
        }),
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
                    WireBlock::Text {
                        text,
                        cache_control,
                    } => {
                        parts.push(ContentPart::Text {
                            text: text.clone(),
                            provider_options: cache_control_options(cache_control),
                        });
                    }
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
                    WireBlock::ToolUse {
                        id,
                        name,
                        input,
                        cache_control,
                    } => {
                        parts.push(ContentPart::ToolCall {
                            tool_call_id: id.clone(),
                            tool_name: name.clone(),
                            input: input.clone(),
                            provider_options: cache_control_options(cache_control),
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
            WireBlock::Text {
                text,
                cache_control,
            } => {
                text_parts.push(ContentPart::Text {
                    text: text.clone(),
                    provider_options: cache_control_options(cache_control),
                });
            }
            WireBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                cache_control,
            } => {
                let output = match content {
                    Some(Value::String(s)) => Value::String(s.clone()),
                    Some(other) => other.clone(),
                    None => Value::Null,
                };
                let mut provider_options = HashMap::new();
                if *is_error == Some(true) {
                    provider_options.insert("anthropic".to_string(), json!({ "is_error": true }));
                }
                // 断点并入已有 anthropic 逃生舱（is_error 标记共存）。
                if let Some(cc) = cache_control {
                    let entry = provider_options
                        .entry("anthropic".to_string())
                        .or_insert_with(|| json!({}));
                    if let Value::Object(anthropic) = entry {
                        anthropic.insert("cache_control".into(), cc.clone());
                    }
                }
                tool_results.push(ContentPart::ToolResult {
                    tool_call_id: tool_use_id.clone(),
                    tool_name: String::new(),
                    output,
                    provider_options,
                });
            }
            WireBlock::Image {
                source,
                cache_control,
            }
            | WireBlock::Document {
                source,
                cache_control,
            } => {
                let block_type = match block {
                    WireBlock::Image { .. } => "image",
                    _ => "document",
                };
                text_parts.push(decode_media_part(source, index, block_type, cache_control)?);
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
/// `block_type`（`image`/`document`）在缺省 media_type 时兜底为顶层段；
/// 块级 cache_control 断点并入 part 逃生舱。
fn decode_media_part(
    source: &Option<WireMediaSource>,
    index: usize,
    block_type: &str,
    cache_control: &Option<Value>,
) -> Result<ContentPart, DecodeError> {
    let (media_type, data, mut provider_options) = match source {
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
    // 断点并入已有 anthropic 逃生舱（file/text source 的 media_source 等共存）。
    if let Some(cc) = cache_control {
        let entry = provider_options
            .entry("anthropic".to_string())
            .or_insert_with(|| json!({}));
        if let Value::Object(anthropic) = entry {
            anthropic.insert("cache_control".into(), cc.clone());
        }
    }
    Ok(ContentPart::Media {
        media_type,
        data,
        provider_options,
    })
}

// ---- 出站编码：IR → wire 请求 ----

/// 编码 IR 请求为出站 Anthropic Messages 请求体。
///
/// 全部 System 消息按序合并进顶层 `system`（`\n\n` 连接，空文本跳过）；请求级
/// `provider_options["anthropic"]` 原样回传（thinking 与 output_config 经 IR
/// 路径不丢失）。thinking 请求面按优先级解析：tool_choice 强制时整体剥离 >
/// 本族逃生舱 > 类型化 effort 按模型形态兜底；thinking 激活时整形采样参数
/// （temperature=1、top_p≥0.95、剥离 top_k）。目标协议无法表达或被约束整形
/// 的设置追加到 `warnings`。
pub fn encode_request(request: &ChatRequest, warnings: &mut Vec<Warning>) -> Value {
    let (system, messages) = encode_messages(&request.messages, warnings);

    let mut obj = serde_json::Map::new();
    obj.insert("model".into(), json!(request.model));
    if let Some(system) = system {
        obj.insert("system".into(), system);
    }
    obj.insert("messages".into(), Value::Array(messages));
    // Anthropic 强制要求 max_tokens：缺省时补 4096，
    // 否则跨协议请求（如 OpenAI 入站未带 max_tokens）会被上游 400 拒绝。
    let max_tokens = request.max_tokens.filter(|&v| v > 0).unwrap_or(4096);
    obj.insert("max_tokens".into(), json!(max_tokens));

    let anthropic_options = request.provider_options.get("anthropic");
    let hatch_thinking = anthropic_options.and_then(|options| options.get("thinking"));
    let hatch_output_config = anthropic_options.and_then(|options| options.get("output_config"));
    let adaptive_model = supports_adaptive_thinking(&request.model);

    // 类型化 effort 兜底出站：本族逃生舱缺席时把旋钮展开为请求模型形态的
    // 原生形状——adaptive/native-effort 模型出 `thinking: adaptive` + 原生
    // effort 档位；legacy 模型出 budget 阶梯（`None` 档两族均为 disabled）。
    // 逃生舱 thinking 在场时请求面由原始配置承载，effort 不再补写，避免
    // budget 与 effort 双表达。
    let typed_thinking = request.reasoning.map(|effort| {
        if adaptive_model {
            match effort.native_effort() {
                Some(_) => json!({ "type": "adaptive" }),
                None => json!({ "type": "disabled" }),
            }
        } else {
            match effort.budget_tokens() {
                Some(budget) => json!({ "type": "enabled", "budget_tokens": budget }),
                None => json!({ "type": "disabled" }),
            }
        }
    });
    let typed_effort = request
        .reasoning
        .filter(|_| hatch_thinking.is_none())
        .and_then(|effort| {
            if adaptive_model {
                effort
                    .native_effort()
                    .map(|native| json!({ "effort": native }))
            } else {
                None
            }
        });

    // tool_choice 强制（any/tool）时 Anthropic 拒绝 thinking 配置：整体剥离，
    // output_config 仅保留 effort 之外的键；发生实际剥离才记 warning。
    let would_emit_thinking = hatch_thinking.is_some() || typed_thinking.is_some();
    let forced_tool_choice = matches!(
        request.tool_choice,
        Some(ToolChoice::Required) | Some(ToolChoice::Tool { .. })
    );
    let thinking = if forced_tool_choice {
        None
    } else {
        hatch_thinking.cloned().or(typed_thinking)
    };
    let mut output_config = if forced_tool_choice {
        hatch_output_config.cloned()
    } else {
        hatch_output_config.cloned().or(typed_effort)
    };
    if forced_tool_choice {
        let effort_leaked = hatch_output_config
            .and_then(|config| config.get("effort"))
            .is_some();
        if would_emit_thinking || effort_leaked {
            warnings.push(Warning::compatibility(
                warning_feature::THINKING,
                "tool_choice 强制工具调用时 Anthropic 拒绝 thinking 配置，已剥离（含 output_config.effort）",
            ));
        }
        output_config = match output_config {
            Some(Value::Object(mut config)) => {
                config.remove("effort");
                (!config.is_empty()).then_some(Value::Object(config))
            }
            other => other,
        };
    }

    // thinking 激活时的采样约束（Anthropic 语义）：temperature 必须为 1、
    // top_p 不低于 0.95、top_k 不可用。整形动作记 warning，未激活时原样透传。
    let thinking_active = thinking
        .as_ref()
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "enabled" | "adaptive" | "auto"));
    let mut temperature = request.temperature;
    let mut top_p = request.top_p;
    let mut top_k = request.top_k;
    if thinking_active {
        if let Some(value) = temperature.filter(|&value| value != 1.0) {
            warnings.push(Warning::compatibility(
                warning_feature::TEMPERATURE,
                format!("thinking 激活时 temperature {value} 整形为 1"),
            ));
            temperature = Some(1.0);
        }
        if let Some(value) = top_p.filter(|&value| value < 0.95) {
            warnings.push(Warning::compatibility(
                warning_feature::TOP_P,
                format!("thinking 激活时 top_p {value} 整形为 0.95"),
            ));
            top_p = Some(0.95);
        }
        if request.top_k.is_some() {
            warnings.push(Warning::compatibility(
                warning_feature::TOP_K,
                "thinking 激活时 top_k 已剥离",
            ));
            top_k = None;
        }
    }
    if let Some(v) = temperature {
        obj.insert("temperature".into(), json!(v));
    }
    if let Some(v) = top_p {
        obj.insert("top_p".into(), json!(v));
    }
    if let Some(v) = top_k {
        obj.insert("top_k".into(), json!(v));
    }
    if !request.stop.is_empty() {
        obj.insert("stop_sequences".into(), json!(request.stop));
    }
    // Anthropic 无以下采样与输出控制参数：显式丢弃并记 warning（不静默吞掉，
    // 与 responses/gemini 出站面的同类丢弃同规）。
    for (feature, present) in [
        (warning_feature::N, request.n.is_some()),
        (warning_feature::SEED, request.seed.is_some()),
        (
            warning_feature::PRESENCE_PENALTY,
            request.presence_penalty.is_some(),
        ),
        (
            warning_feature::FREQUENCY_PENALTY,
            request.frequency_penalty.is_some(),
        ),
    ] {
        if present {
            warnings.push(Warning::unsupported(
                feature,
                format!("Anthropic 无 {feature} 承载，已丢弃"),
            ));
        }
    }
    // JSON 结构化输出在 Anthropic 无请求侧承载（官方走 tools 变体，未实现）；
    // type=text 等价于缺省（无输出形状约束），不告警。
    if let Some(response_format) = &request.response_format
        && response_format.get("type").and_then(Value::as_str) != Some("text")
    {
        warnings.push(Warning::unsupported(
            warning_feature::RESPONSE_FORMAT,
            "Anthropic Messages 无 response_format 承载，JSON 结构化输出已丢弃",
        ));
    }
    if request.stream {
        obj.insert("stream".into(), Value::Bool(true));
    }
    // Anthropic 没有 `none` 的 tool_choice 形状；禁用工具时同时省略工具声明与
    // 选择字段，避免把不可接受的组合发送给上游。
    let tools_enabled = !request
        .tool_choice
        .as_ref()
        .is_some_and(|choice| matches!(choice, ToolChoice::None));
    if tools_enabled && !request.tools.is_empty() {
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
                        let (schema, action) = normalize_object_root(t.parameters.as_ref());
                        if let Some(action) = action {
                            warnings.push(Warning::compatibility(
                                warning_feature::INPUT_SCHEMA,
                                format!("tool {} 的 input_schema {}", t.name, action),
                            ));
                        }
                        tool.insert("input_schema".into(), schema);
                        // 工具级缓存断点原样回传。
                        if let Some(cache_control) = t
                            .provider_options
                            .get("anthropic")
                            .and_then(|a| a.get("cache_control"))
                        {
                            tool.insert("cache_control".into(), cache_control.clone());
                        }
                        Value::Object(tool)
                    })
                    .collect(),
            ),
        );
    }
    if tools_enabled {
        if let Some(choice) = &request.tool_choice {
            obj.insert(
                "tool_choice".into(),
                encode_tool_choice(
                    choice,
                    &request.provider_options,
                    request.parallel_tool_calls,
                ),
            );
        } else if request.parallel_tool_calls == Some(false) && !request.tools.is_empty() {
            // Anthropic 的禁并行语义只挂在 tool_choice 上：请求未显式选择工具时
            // 按 auto 兜底合成（允许并行是缺省语义，true 不合成）。
            obj.insert(
                "tool_choice".into(),
                json!({ "type": "auto", "disable_parallel_tool_use": true }),
            );
        }
    }
    // 请求级缓存断点原样回传（ttl/scope 等子字段随值透传）。
    if let Some(cache_control) = anthropic_options.and_then(|o| o.get("cache_control")) {
        obj.insert("cache_control".into(), cache_control.clone());
    }
    if let Some(thinking) = thinking {
        obj.insert("thinking".into(), thinking);
    }
    if let Some(output_config) = output_config {
        obj.insert("output_config".into(), output_config);
    }
    // 未知字段逃生舱最后应用：本族字段回写不覆盖类型化字段，跨族字段丢弃告警。
    apply_provider_extra(&mut obj, request, "anthropic", warnings);
    if !tools_enabled {
        // `ToolChoice::None` 的协议承载是完全不发送工具面；逃生舱不能重新
        // 注入 Anthropic 不接受的 `tools`/`tool_choice` 字段。
        obj.remove("tools");
        obj.remove("tool_choice");
    }
    // 缓存断点预算钳制：超限时按 render order 保后弃前，动作可观测。
    let dropped = clamp_cache_breakpoints(&mut obj);
    if dropped > 0 {
        warnings.push(Warning::unsupported(
            warning_feature::CACHE_BREAKPOINT,
            format!(
                "Anthropic 缓存断点上限 {MAX_CACHE_BREAKPOINTS} 个，已保留靠后者，丢弃最早的 {dropped} 个"
            ),
        ));
    }
    Value::Object(obj)
}

/// Anthropic 单请求缓存断点预算。
const MAX_CACHE_BREAKPOINTS: usize = 4;

/// 出站对象中断点的位置（render order 采集序）。
enum BreakpointLocation {
    Tool(usize),
    SystemBlock(usize),
    MessageBlock { message: usize, block: usize },
}

/// 断点预算钳制：出站对象中断点超过 [`MAX_CACHE_BREAKPOINTS`] 时，按
/// render order（tools → system → messages）保留靠后者、牺牲最早者，返回
/// 丢弃数量。
///
/// 纯函数，作用于已编码的 wire 对象；只剥 `cache_control` 键，不动块本体，
/// 保留块形状（Anthropic 对无断点块数组同样接受）。
fn clamp_cache_breakpoints(obj: &mut serde_json::Map<String, Value>) -> usize {
    let locations = collect_breakpoint_locations(obj);

    let excess = locations.len().saturating_sub(MAX_CACHE_BREAKPOINTS);
    for location in locations.into_iter().take(excess) {
        let block = match location {
            BreakpointLocation::Tool(index) => obj
                .get_mut("tools")
                .and_then(Value::as_array_mut)
                .and_then(|tools| tools.get_mut(index)),
            BreakpointLocation::SystemBlock(index) => obj
                .get_mut("system")
                .and_then(Value::as_array_mut)
                .and_then(|blocks| blocks.get_mut(index)),
            BreakpointLocation::MessageBlock { message, block } => obj
                .get_mut("messages")
                .and_then(Value::as_array_mut)
                .and_then(|messages| messages.get_mut(message))
                .and_then(|message| message.get_mut("content"))
                .and_then(Value::as_array_mut)
                .and_then(|blocks| blocks.get_mut(block)),
        };
        if let Some(Value::Object(map)) = block {
            map.remove("cache_control");
        }
    }
    excess
}

/// 自动断点注入的默认标记：5 分钟 TTL 的 ephemeral。
fn ephemeral_cache_control() -> Value {
    json!({ "type": "ephemeral" })
}

/// 渲染顺序采集出站对象中已带断点的块位置（采集序同 render order）。
fn collect_breakpoint_locations(obj: &serde_json::Map<String, Value>) -> Vec<BreakpointLocation> {
    let mut locations = Vec::new();
    if let Some(tools) = obj.get("tools").and_then(Value::as_array) {
        for (index, tool) in tools.iter().enumerate() {
            if tool.get("cache_control").is_some() {
                locations.push(BreakpointLocation::Tool(index));
            }
        }
    }
    if let Some(system) = obj.get("system").and_then(Value::as_array) {
        for (index, block) in system.iter().enumerate() {
            if block.get("cache_control").is_some() {
                locations.push(BreakpointLocation::SystemBlock(index));
            }
        }
    }
    if let Some(messages) = obj.get("messages").and_then(Value::as_array) {
        for (message_index, message) in messages.iter().enumerate() {
            let Some(blocks) = message.get("content").and_then(Value::as_array) else {
                continue;
            };
            for (block_index, block) in blocks.iter().enumerate() {
                if block.get("cache_control").is_some() {
                    locations.push(BreakpointLocation::MessageBlock {
                        message: message_index,
                        block: block_index,
                    });
                }
            }
        }
    }
    locations
}

/// 自动缓存断点注入：按 tools 尾 → system 尾 → 末条消息尾块的顺序为出站
/// 对象补 `cache_control`，返回注入数量。
///
/// 预算为 [`MAX_CACHE_BREAKPOINTS`] 减已有断点数，预算用尽即止——超限不由
/// 本函数扩张，已有断点超限由出站钳制统一告警；已带断点的位置跳过（含
/// 「末条消息尾块已有断点时继续找更早消息」，显式标记视作调用方意图）。
/// 消息侧只标非 thinking 块：思维链内容随轮次更替，标在其上会让缓存前缀
/// 失去稳定前缘。纯函数，作用于已编码的 wire 对象。
pub fn inject_cache_breakpoints(obj: &mut serde_json::Map<String, Value>) -> usize {
    let existing = collect_breakpoint_locations(obj).len();
    let mut budget = MAX_CACHE_BREAKPOINTS.saturating_sub(existing);
    if budget == 0 {
        return 0;
    }
    let mut injected = 0usize;

    // tools 尾：工具定义位于每个请求前缀的最前段，最先锚定。
    if let Some(last_tool) = obj
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .and_then(|tools| tools.last_mut())
        && last_tool.get("cache_control").is_none()
        && last_tool.as_object_mut().is_some_and(|tool| {
            tool.insert("cache_control".into(), ephemeral_cache_control());
            true
        })
    {
        budget -= 1;
        injected += 1;
    }
    if budget == 0 {
        return injected;
    }

    // system 尾：系统提示紧随工具定义，同为全请求稳定前缀。字符串 system
    // 先转为块数组（该形状本就无块级断点），再标记尾块。
    if let Some(text) = obj
        .get("system")
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        obj.insert("system".into(), json!([{ "type": "text", "text": text }]));
    }
    if let Some(last_block) = obj
        .get_mut("system")
        .and_then(Value::as_array_mut)
        .and_then(|blocks| blocks.last_mut())
        && last_block.get("cache_control").is_none()
        && last_block.as_object_mut().is_some_and(|block| {
            block.insert("cache_control".into(), ephemeral_cache_control());
            true
        })
    {
        budget -= 1;
        injected += 1;
    }
    if budget == 0 {
        return injected;
    }

    // 末条可注入消息的尾块：从最新消息向前找第一个「尾块为非 thinking 且
    // 未带断点」的消息。工具循环通常以 user/tool_result 收尾，标记该处即
    // 把本轮工具结果纳入下一轮的稳定前缀。字符串 content（纯文本消息）先
    // 转为块数组再标记。
    if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut().rev() {
            if let Some(text) = message
                .get("content")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                message["content"] = json!([{ "type": "text", "text": text }]);
            }
            let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            let Some(block) = blocks.iter_mut().rev().find(|block| {
                !matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("thinking" | "redacted_thinking")
                )
            }) else {
                continue;
            };
            if block.get("cache_control").is_some() {
                continue;
            }
            if let Some(block) = block.as_object_mut() {
                block.insert("cache_control".into(), ephemeral_cache_control());
                injected += 1;
            }
            break;
        }
    }
    injected
}

/// 请求模型是否支持 adaptive thinking 与原生 effort（`output_config.effort`）。
///
/// 网关暂无模型能力表，按模型名模式判定：opus 4.6/4.7/4.8/5+、sonnet 4.6/5+、
/// fable/mythos 家族，日期后缀与点分变体（bedrock/vertex/azure 接入形态）
/// 一并覆盖。判否时按 legacy budget 阶梯兜底——该形状对所有 budget 模型合法，
/// 误判只损失 effort 档位粒度，不产生非法请求。
fn supports_adaptive_thinking(model: &str) -> bool {
    let model = model.to_lowercase();
    let opus = model.contains("opus");
    let version_46 = model.contains("4-6") || model.contains("4.6");
    let opus_47_plus = opus
        && (model.contains("4-7")
            || model.contains("4.7")
            || model.contains("4-8")
            || model.contains("4.8")
            || model.contains("opus-5"));
    let sonnet_5_plus = model.contains("sonnet-5");
    let fable_family = model.contains("fable") || model.contains("mythos");
    opus_47_plus
        || sonnet_5_plus
        || fable_family
        || (version_46 && (opus || model.contains("sonnet")))
}

/// 编码 IR tool_choice 为 Anthropic wire 值；请求级逃生舱
/// `tool_choice_extra` 的附加键并回对象，类型化 `parallel_tool_calls` 以
/// 反语义 `disable_parallel_tool_use`（取反）最终定值。
fn encode_tool_choice(
    choice: &ToolChoice,
    provider_options: &crate::core::ir::ProviderOptions,
    parallel_tool_calls: Option<bool>,
) -> Value {
    let mut obj = serde_json::Map::new();
    match choice {
        ToolChoice::Auto => {
            obj.insert("type".into(), json!("auto"));
        }
        ToolChoice::None => {
            obj.insert("type".into(), json!("none"));
        }
        ToolChoice::Required => {
            obj.insert("type".into(), json!("any"));
        }
        ToolChoice::Tool { name } => {
            obj.insert("type".into(), json!("tool"));
            obj.insert("name".into(), json!(name));
        }
    }
    if let Some(extra) = provider_options
        .get("anthropic")
        .and_then(|anthropic| anthropic.get("tool_choice_extra"))
        .and_then(Value::as_object)
    {
        for (key, value) in extra {
            obj.insert(key.clone(), value.clone());
        }
    }
    if let Some(parallel) = parallel_tool_calls {
        obj.insert("disable_parallel_tool_use".into(), json!(!parallel));
    }
    Value::Object(obj)
}

/// 把 IR 消息编码为（顶层 system，wire messages）。
///
/// 全部 System 消息按序合并进顶层 system（`\n\n` 连接，空文本跳过）；
/// 连续 assistant 消息合并为一条（Anthropic 要求）；连续 tool 消息拆为
/// 单条 user 的多个 tool_result 块。tool 身份配对经 [`ToolAlignment`] 整形。
fn encode_messages(
    ir_messages: &[Message],
    warnings: &mut Vec<Warning>,
) -> (Option<Value>, Vec<Value>) {
    // tool 身份整形：Anthropic 要求 tool_use.id 合法且与 tool_result 一一
    // 配对；重复 tool 消息按原始 id 取最后一条、只产出一次。重排在编码后
    // 统一执行（align_tool_results），IR 保持中立。
    let mut alignment = ToolAlignment::scan(ir_messages);
    let mut system_out: Option<String> = None;
    // System 缓存断点：合并后挂在 system 尾块；多条 System 各带断点时取
    // 最后出现的（后者为更靠后的缓存前缘）。
    let mut system_cache: Option<Value> = None;
    let mut wire_messages: Vec<Value> = Vec::new();

    for message in ir_messages {
        match message.role {
            Role::System => {
                // System 消息仅取文本；媒体等非文本 part 丢弃并记 warning。
                for part in &message.content {
                    if let ContentPart::Media { media_type, .. } = part {
                        warnings.push(Warning::unsupported(
                            warning_feature::MEDIA,
                            format!(
                                "Anthropic Messages 系统消息不支持媒体内容（{media_type}），已丢弃"
                            ),
                        ));
                    }
                }
                if let Some(cache_control) = message
                    .provider_options
                    .get("anthropic")
                    .and_then(|a| a.get("cache_control"))
                {
                    system_cache = Some(cache_control.clone());
                }
                let text = text_parts(&message.content).unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                // 全部 System 消息按序合并进顶层 system（\n\n 连接）；中段
                // system 以消息形式夹入会扰动上游缓存前缀并改变消息序列。
                system_out = Some(match system_out.take() {
                    Some(existing) => format!("{existing}\n\n{text}"),
                    None => text,
                });
            }
            Role::User => {
                let blocks = encode_user_blocks(&message.content, warnings);
                if blocks.is_empty() {
                    continue;
                }
                // 单一纯文本 user 消息编码为字符串（Anthropic 惯例，保持既有往返形状）；
                // 含媒体等非文本 part 时按序编码为数组，保持文本与媒体混排顺序；
                // 带 cache_control 断点的文本块必须保持块形状（字符串无处挂断点）。
                let single_text = (blocks.len() == 1 && blocks[0].get("cache_control").is_none())
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
                let blocks = encode_assistant_blocks(&message.content, &mut alignment, warnings);
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
                let blocks = encode_tool_result_blocks(&message.content, &mut alignment);
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

    align_tool_results(&mut wire_messages);
    // assistant 块整形：thinking 归位块首（Anthropic 要求 thinking 紧邻
    // 消息前缀），合法序列恒等。
    move_thinking_blocks_to_front(&mut wire_messages);
    // 末尾 assistant 文本块去除尾随空白（Anthropic 拒绝预置 assistant 的尾随空白）。
    trim_trailing_whitespace(&mut wire_messages);
    // 有断点时 system 以单文本块数组出站，断点挂尾块；否则保持字符串形状。
    let system = system_out.map(|text| match system_cache {
        Some(cache_control) => {
            json!([{ "type": "text", "text": text, "cache_control": cache_control }])
        }
        None => json!(text),
    });
    (system, wire_messages)
}

/// 单次出站请求的 tool 身份整形状态。
///
/// Anthropic 要求 `tool_use.id` 匹配 `^[a-zA-Z0-9_-]+$` 且与紧随的
/// `tool_result.tool_use_id` 一一配对。原始 id → 合法 id 经同一 memo 映射，
/// 空 id 只生成一次（配对不因生成而断裂）；重复 tool 消息按原始 id 取
/// 最后一条并去重，把客户端脏序列整形为合法配对结构。
struct ToolAlignment {
    /// 原始 tool_call_id → 合法 wire id。
    memo: HashMap<String, String>,
    /// 空 id 兜底生成的计数器（附请求内序号避免纳秒碰撞）。
    generated: u64,
    /// 已产出 tool_result 的原始 id：重复出现只发一次。
    emitted: HashSet<String>,
    /// 原始 tool_call_id → 最后一条 tool 消息中的 ToolResult part。
    last_result: HashMap<String, ContentPart>,
}

impl ToolAlignment {
    /// 前置扫描：记录每个原始 tool_call_id 最后一次出现的 ToolResult part。
    fn scan(ir_messages: &[Message]) -> Self {
        let mut last_result = HashMap::new();
        for message in ir_messages {
            if message.role != Role::Tool {
                continue;
            }
            for part in &message.content {
                if let ContentPart::ToolResult { tool_call_id, .. } = part {
                    last_result.insert(tool_call_id.clone(), part.clone());
                }
            }
        }
        Self {
            memo: HashMap::new(),
            generated: 0,
            emitted: HashSet::new(),
            last_result,
        }
    }

    /// 原始 id 对应的合法 wire id：非空清洗（幂等纯函数），空结果生成
    /// `toolu_<纳秒>_<序号>`；同一原始 id 在 tool_use 与 tool_result 两侧
    /// 得到同一 wire id。
    fn wire_id(&mut self, original: &str) -> String {
        if let Some(wire) = self.memo.get(original) {
            return wire.clone();
        }
        let sanitized = sanitize_tool_id(original);
        let wire = if sanitized.is_empty() {
            self.generated += 1;
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            format!("toolu_{nanos}_{}", self.generated)
        } else {
            sanitized
        };
        self.memo.insert(original.to_string(), wire.clone());
        wire
    }
}

/// 清洗单个 tool id 为 Anthropic 合法形状：`^[a-zA-Z0-9_-]+$` 之外的字符
/// 替换 `_`，空结果由调用方生成兜底。合法输入逐字节不变。
fn sanitize_tool_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
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

/// 把 part 级缓存断点写入 wire 内容块（约定键 anthropic.cache_control）。
fn attach_cache_control(block: &mut Value, provider_options: &crate::core::ir::ProviderOptions) {
    if let Some(cache_control) = provider_options
        .get("anthropic")
        .and_then(|a| a.get("cache_control"))
        && let Value::Object(map) = block
    {
        map.insert("cache_control".into(), cache_control.clone());
    }
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
            ContentPart::Text {
                text,
                provider_options,
            } => {
                let mut block = json!({ "type": "text", "text": text });
                attach_cache_control(&mut block, provider_options);
                blocks.push(block);
            }
            ContentPart::Media {
                media_type,
                data,
                provider_options,
            } => {
                if let Some(mut block) =
                    encode_media_block(media_type, data, provider_options, warnings)
                {
                    attach_cache_control(&mut block, provider_options);
                    blocks.push(block);
                }
            }
            ContentPart::Custom { kind, .. } => {
                warnings.push(Warning::unsupported(
                    warning_feature::CUSTOM,
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
            warning_feature::MEDIA,
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
fn encode_assistant_blocks(
    parts: &[ContentPart],
    alignment: &mut ToolAlignment,
    warnings: &mut Vec<Warning>,
) -> Vec<Value> {
    let mut blocks = Vec::new();
    for part in parts {
        match part {
            ContentPart::Text {
                text,
                provider_options,
            } => {
                let mut block = json!({ "type": "text", "text": text });
                attach_cache_control(&mut block, provider_options);
                blocks.push(block);
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
                provider_options,
            } => {
                let mut block = json!({
                    "type": "tool_use",
                    "id": alignment.wire_id(tool_call_id),
                    "name": tool_name,
                    "input": input,
                });
                attach_cache_control(&mut block, provider_options);
                blocks.push(block);
            }
            ContentPart::Media { media_type, .. } => {
                // Anthropic 媒体内容块仅允许出现在 user 消息；assistant 侧媒体
                // 非标准，跨协议族转换时丢弃并记 warning。
                warnings.push(Warning::unsupported(
                    warning_feature::MEDIA,
                    format!("Anthropic Messages 助手消息不支持媒体内容（{media_type}），已丢弃"),
                ));
            }
            ContentPart::Custom { kind, .. } => {
                warnings.push(Warning::unsupported(
                    warning_feature::CUSTOM,
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
/// 编码 Tool 消息的 ToolResult parts 为 tool_result 块。
///
/// 重复 tool 消息按原始 id 去重（重复出现只发一次），内容一律取该 id
/// 最后一条 tool 消息——客户端重发同 id 结果时以最新为准；wire id 经
/// 共享映射清洗，与前置 assistant 消息的 tool_use 保持配对。
fn encode_tool_result_blocks(parts: &[ContentPart], alignment: &mut ToolAlignment) -> Vec<Value> {
    parts
        .iter()
        .filter_map(|part| {
            let tool_call_id = match part {
                ContentPart::ToolResult { tool_call_id, .. } => tool_call_id.clone(),
                _ => return None,
            };
            if !alignment.emitted.insert(tool_call_id.clone()) {
                return None;
            }
            // 内容与 is_error 取该 id 的最后一条 tool 消息。
            let (output, is_error, cache_control) = match alignment.last_result.get(&tool_call_id) {
                Some(ContentPart::ToolResult {
                    output,
                    provider_options,
                    ..
                }) => {
                    let is_error = provider_options
                        .get("anthropic")
                        .and_then(|a| a.get("is_error"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let cache_control = provider_options
                        .get("anthropic")
                        .and_then(|a| a.get("cache_control"))
                        .cloned();
                    (output.clone(), is_error, cache_control)
                }
                _ => (part_output(part), false, None),
            };
            // 输出为字符串时直接用；否则 JSON 序列化（tool_result content 是文本）。
            let content_value = match output {
                Value::String(s) => json!(s),
                other => json!(other.to_string()),
            };
            let mut block = serde_json::Map::new();
            block.insert("type".into(), json!("tool_result"));
            block.insert(
                "tool_use_id".into(),
                json!(alignment.wire_id(&tool_call_id)),
            );
            block.insert("content".into(), content_value);
            if is_error {
                block.insert("is_error".into(), Value::Bool(true));
            }
            if let Some(cache_control) = cache_control {
                block.insert("cache_control".into(), cache_control);
            }
            Some(Value::Object(block))
        })
        .collect()
}

/// 取 ToolResult part 的 output 字段；part 形态异常时回退空串
/// （调用方已由前置扫描保证命中，此为防御性兜底）。
fn part_output(part: &ContentPart) -> Value {
    match part {
        ContentPart::ToolResult { output, .. } => output.clone(),
        _ => Value::String(String::new()),
    }
}

/// 把纯 tool_result 块的 user 消息按前置 assistant 消息的 tool_use 顺序重排。
///
/// 结果块与 tool_use id 构成一对一匹配时才重排（匹配失败保持原序，交由
/// 上游判定）。合法已对齐的序列重排为恒等，同族往返逐字节稳定。
fn align_tool_results(wire_messages: &mut [Value]) {
    for index in 1..wire_messages.len() {
        let is_tool_result_message = wire_messages[index].get("role").and_then(Value::as_str)
            == Some("user")
            && wire_messages[index]
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|blocks| {
                    !blocks.is_empty()
                        && blocks
                            .iter()
                            .all(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                });
        if !is_tool_result_message {
            continue;
        }
        let Some(tool_use_ids) = wire_messages[..index]
            .iter()
            .rev()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
            .map(|assistant| {
                assistant["content"]
                    .as_array()
                    .expect("assistant 消息应为块数组")
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                    .filter_map(|b| b.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .filter(|ids| !ids.is_empty())
        else {
            continue;
        };
        let results = wire_messages[index]
            .get_mut("content")
            .and_then(Value::as_array_mut)
            .expect("已确认 content 为块数组");
        if results.len() != tool_use_ids.len() {
            continue;
        }
        let mut ordered = Vec::with_capacity(results.len());
        let mut used = vec![false; results.len()];
        for use_id in &tool_use_ids {
            let matched = results.iter().enumerate().position(|(index, block)| {
                !used[index] && block.get("tool_use_id").and_then(Value::as_str) == Some(use_id)
            });
            let Some(matched) = matched else {
                // 无完整一对一匹配：保持原序，交由上游判定。
                return;
            };
            used[matched] = true;
            ordered.push(results[matched].clone());
        }
        *results = ordered;
    }
}

/// 把 assistant 消息内的 `thinking`/`redacted_thinking` 块稳定挪到块序列最前。
///
/// Anthropic 要求 thinking 块位于助手消息开头；跨协议族历史可能产出
/// tool_use/text 在前的块序（如 Responses 的 function_call 项先于
/// reasoning 项、或客户端原样回放的脏序列），原样出站会被上游拒绝。
/// 各类块的相对顺序保持不变，thinking 已在前缀的合法序列恒等。
fn move_thinking_blocks_to_front(wire_messages: &mut [Value]) {
    for message in wire_messages.iter_mut() {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        let mut leading = Vec::with_capacity(blocks.len());
        let mut trailing = Vec::new();
        for block in blocks.drain(..) {
            if matches!(
                block.get("type").and_then(Value::as_str),
                Some("thinking") | Some("redacted_thinking")
            ) {
                leading.push(block);
            } else {
                trailing.push(block);
            }
        }
        leading.append(&mut trailing);
        *blocks = leading;
    }
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
    crate::core::ir::text_content(parts)
}

// ---- 上游响应解码：wire → IR ----

/// 解码上游 Anthropic Messages 响应为 IR。
pub fn decode_response(value: &Value) -> Result<ChatResponse, DecodeError> {
    let wire: WireResponse = serde_path_to_error::deserialize(value.clone()).map_err(|err| {
        DecodeError::InvalidShape {
            detail: err.to_string(),
        }
    })?;

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
    let warnings = if raw.as_deref() == Some("pause_turn") {
        vec![pause_turn_warning()]
    } else {
        Vec::new()
    };

    Ok(ChatResponse {
        id: wire.id.unwrap_or_default(),
        model: wire.model.unwrap_or_default(),
        content,
        finish_reason: FinishReason { unified, raw },
        usage,
        provider_metadata: HashMap::new(),
        warnings,
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

/// 从 usage 对象解析 IR 四分量与 1h 写入明细。
fn parse_usage_object(usage: &serde_json::Map<String, Value>) -> Option<Usage> {
    let mut has_metric = false;
    let mut get = |key: &str| {
        usage.get(key).map_or(Some(0), |value| {
            has_metric = true;
            value.as_u64()
        })
    };
    let input_tokens = get("input_tokens")?;
    let output_tokens = get("output_tokens")?;
    let cache_read_tokens = get("cache_read_input_tokens")?;
    let cache_write_tokens = get("cache_creation_input_tokens")?;
    let cache_write_1h = match usage.get("cache_creation") {
        None | Some(Value::Null) => 0,
        Some(value) => {
            let object = value.as_object()?;
            match object.get("ephemeral_1h_input_tokens") {
                None => 0,
                Some(value) => {
                    has_metric = true;
                    value.as_u64()?
                }
            }
        }
    };
    if !has_metric {
        return None;
    }
    Some(Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cache_write_1h_tokens: cache_write_1h,
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
        cache_write_1h_tokens: wire
            .cache_creation
            .map(|c| c.ephemeral_1h_input_tokens)
            .unwrap_or(0),
        raw,
    }
}

/// unified stop reason 映射（Anthropic stop_reason 值）。
fn map_stop_reason(raw: Option<&str>) -> FinishReasonUnified {
    match raw {
        // `pause_turn` 是暂停待续而非正常结束：经网关无法向下游表达续传语义，
        // 折叠为 Other（直通路径原样透传 raw），由调用方附 compatibility warning。
        Some("end_turn") | Some("stop_sequence") => FinishReasonUnified::Stop,
        Some("pause_turn") => FinishReasonUnified::Other,
        Some("max_tokens") | Some("model_context_window_exceeded") => FinishReasonUnified::Length,
        Some("refusal") => FinishReasonUnified::ContentFilter,
        Some("tool_use") => FinishReasonUnified::ToolCalls,
        _ => FinishReasonUnified::Other,
    }
}

/// `pause_turn` 终态的告警：经 IR 的响应无法承载续传语义，下游按普通流结束
/// 处理；需要续传能力的客户端应使用直通路径（同协议同渠道）。
fn pause_turn_warning() -> Warning {
    Warning::unsupported(
        warning_feature::PAUSE_TURN,
        "Anthropic pause_turn（暂停待续）经网关无法续传，已按流结束处理",
    )
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
/// 流式事件处理：`content_block_start` 开启块，`content_block_delta`
/// 产出增量（`signature_delta` 以零长增量携带 signature），`content_block_stop`
/// 收尾（tool_use 在此解析出完整 input），`message_delta` 产出 Finish（最终
/// usage + stop_reason），`error` 产出 IR Error 事件（overloaded_error 等
/// 流内错误，不再静默吞掉）；反序列化失败的事件留痕后跳过，错误语义可
/// 安全提取时映射 IR Error。
#[derive(Debug, Default)]
pub struct StreamDecoder {
    /// 按块 index 维护进行中的块状态。
    blocks: HashMap<usize, OpenBlock>,
    /// 分散在 message_start/message_delta 的 usage，按分量取最大值合并。
    usage: Usage,
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
            Err(err) => {
                return DecodeStreamChunk::delivery(decode_failed_frame(
                    &err,
                    value,
                    ErrorMessageShape::NestedOnly,
                ));
            }
        };

        let mut events = Vec::new();
        let mut is_output = false;

        match wire {
            WireStreamEvent::Ping => {}
            WireStreamEvent::Error { error } => {
                events.push(StreamEvent::Error {
                    message: error.message.unwrap_or_default(),
                });
            }
            WireStreamEvent::MessageStart { message } => {
                if let (Some(id), Some(model)) = (message.id, message.model) {
                    events.push(StreamEvent::ResponseMetadata { id, model });
                }
                if let Some(usage) = message.usage {
                    let usage = convert_usage(usage);
                    self.usage.union_max(usage.clone());
                    self.usage.raw = usage.raw;
                }
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
                if let Some(usage) = usage {
                    let usage = convert_usage(usage);
                    self.usage.union_max(usage.clone());
                    self.usage.raw = usage.raw;
                }
                events.push(StreamEvent::Finish {
                    finish_reason: FinishReason {
                        unified,
                        raw: raw.clone(),
                    },
                    usage: self.usage.clone(),
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
            // 流内错误以 `event: error` 下发（与网关兜底错误帧同形状），
            // 由调用方感知并终止流。
            StreamEvent::Error { message } => vec![stream_error_frame(message)],
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

/// 流内错误的入站 SSE 帧（`event: error`，500 语义）。流式编码器消费 IR
/// Error 事件与网关兜底路径共用，保证形状一致。
pub fn stream_error_frame(message: &str) -> SseFrame {
    SseFrame::named("error", encode_error(500, message).to_string())
}

/// 编码为 Anthropic `GET /v1/models` 列表。
///
/// `display_name` 与 `id` 相同；`created_at` 未知时用 epoch（官方允许）。
/// 本网关一次返回全部可见 ID，`has_more` 恒为 false。
pub fn encode_model_list(ids: &[String]) -> Value {
    let data: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({
                "id": id,
                "type": "model",
                "display_name": id,
                "created_at": "1970-01-01T00:00:00Z",
            })
        })
        .collect();
    json!({
        "data": data,
        "has_more": false,
        "first_id": ids.first(),
        "last_id": ids.last(),
    })
}

#[cfg(test)]
mod tests {
    /// `pause_turn` 终态：unified 归为 Other（非正常结束）并携带
    /// pause_turn warning，下游可观测到续传语义的丢失。
    #[test]
    fn pause_turn_decodes_to_other_with_warning() {
        let wire = json!({
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "content": [{ "type": "text", "text": "partial" }],
            "stop_reason": "pause_turn",
            "stop_sequence": null,
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        });
        let response = decode_response(&wire).expect("pause_turn 响应应可解码");
        assert_eq!(
            response.finish_reason.unified,
            crate::core::ir::FinishReasonUnified::Other
        );
        assert_eq!(response.finish_reason.raw.as_deref(), Some("pause_turn"));
        assert!(matches!(
            response.warnings.as_slice(),
            [Warning::Unsupported { feature, .. }] if feature == warning_feature::PAUSE_TURN
        ));
    }

    use super::*;
    use crate::core::stream::StreamAccumulator;
    use crate::core::testing::frames_to_snapshot;
    use similar_asserts::assert_eq;

    /// 模型形态判定：adaptive/native-effort 家族覆盖官方名、日期后缀与
    /// bedrock/vertex 变体；legacy 模型与异族模型名判否。
    #[test]
    fn supports_adaptive_thinking_matches_model_forms() {
        for model in [
            "claude-opus-4-6",
            "claude-opus-4-6-20260201",
            "claude-opus-4.6",
            "claude-sonnet-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-fable-5",
            "claude-mythos-5",
            "us.anthropic.claude-opus-4-6-v1:0",
        ] {
            assert!(
                supports_adaptive_thinking(model),
                "{model} 应判为 adaptive 形态"
            );
        }
        for model in [
            "claude-sonnet-4-5",
            "claude-opus-4-5",
            "claude-opus-4-1",
            "claude-haiku-4-5",
            "claude-3-7-sonnet",
            "gpt-4o",
        ] {
            assert!(
                !supports_adaptive_thinking(model),
                "{model} 应判为 legacy 形态"
            );
        }
    }

    /// wire 形状错误指明出错字段的 JSON 路径，而非笼统的「不是合法 JSON 对象」。
    #[test]
    fn invalid_wire_shape_reports_field_path() {
        let wire = json!({
            "model": "claude-sonnet-4",
            "max_tokens": 64,
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "user", "content": 42 }
            ]
        });
        match decode_request(&wire) {
            Err(DecodeError::InvalidShape { detail }) => {
                assert!(
                    detail.contains("messages[1].content"),
                    "报错应含字段路径: {detail}"
                );
            }
            other => panic!("应报 InvalidShape: {other:?}"),
        }
    }

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

    /// 缓存断点黄金样例：system 尾块（含 TTL）、工具、user 文本、tool_result
    /// 与请求级五处断点解码捕获进约定键 `anthropic.cache_control`，出站逐位
    /// 还原断点位置与 TTL，同族往返零告警。
    #[test]
    fn cache_control_fixture_roundtrip() {
        let raw = include_str!("__fixtures__/request_cache_control.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let ir = decode_request(&wire).expect("fixture 应可解码为 IR");

        // 解码捕获：各层级断点落在约定键，值原样（ttl 随值透传）。
        let system = ir.messages.first().expect("应有 system 消息");
        assert_eq!(
            system.provider_options["anthropic"]["cache_control"],
            json!({ "type": "ephemeral", "ttl": "1h" })
        );
        assert_eq!(
            ir.tools[0].provider_options["anthropic"]["cache_control"],
            json!({ "type": "ephemeral" })
        );
        let user_text = &ir.messages[1].content[0];
        assert!(matches!(
            user_text,
            ContentPart::Text { provider_options, .. }
                if provider_options["anthropic"]["cache_control"] == json!({ "type": "ephemeral" })
        ));
        let tool_result = &ir.messages[3].content[0];
        assert!(matches!(
            tool_result,
            ContentPart::ToolResult { provider_options, .. }
                if provider_options["anthropic"]["cache_control"] == json!({ "type": "ephemeral" })
        ));
        assert_eq!(
            ir.provider_options["anthropic"]["cache_control"],
            json!({ "type": "ephemeral" }),
            "请求级断点应捕获"
        );

        // 出站还原：断点位置与 TTL 逐位一致。
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert!(warnings.is_empty(), "同族断点往返不应产出 warning");
        assert_eq!(reencoded, wire, "断点位置与 TTL 应逐位还原");
    }

    /// System 断点合并语义：多条 System 消息各带断点时取最后出现的（尾块
    /// 前缘），断点在场时 system 以单文本块数组出站，缺席保持字符串形状。
    /// 最小化请求骨架（各测试按需覆写消息/工具）。
    fn bare_request(messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: "claude-sonnet-4-5".to_string(),
            messages,
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
            parallel_tool_calls: None,
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn system_cache_control_merges_to_tail_block() {
        let cached = json!({ "type": "ephemeral" });
        let make = |text: &str, cc: Option<&Value>| Message {
            role: Role::System,
            content: vec![ContentPart::Text {
                text: text.to_string(),
                provider_options: HashMap::new(),
            }],
            provider_options: match cc {
                Some(cc) => [("anthropic".to_string(), json!({ "cache_control": cc }))]
                    .into_iter()
                    .collect(),
                None => HashMap::new(),
            },
        };

        let request = bare_request(vec![
            make("第一段", None),
            make("第二段", Some(&cached)),
            make("第三段", Some(&json!({ "type": "ephemeral", "ttl": "1h" }))),
        ]);
        let mut warnings = Vec::new();
        let wire = encode_request(&request, &mut warnings);
        assert!(warnings.is_empty());
        assert_eq!(
            wire["system"],
            json!([{
                "type": "text",
                "text": "第一段\n\n第二段\n\n第三段",
                "cache_control": { "type": "ephemeral", "ttl": "1h" }
            }])
        );

        let request = bare_request(vec![make("无断点", None)]);
        let wire = encode_request(&request, &mut Vec::new());
        assert_eq!(wire["system"], json!("无断点"), "无断点保持字符串形状");
    }

    /// 自动断点注入：按 tools 尾 → system 尾 → 末条消息尾块补 `cache_control`。
    #[test]
    fn auto_injection_marks_tool_system_and_message_tails_in_order() {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "tools".into(),
            json!([
                { "name": "a", "input_schema": {} },
                { "name": "b", "input_schema": {} },
            ]),
        );
        obj.insert("system".into(), json!([{ "type": "text", "text": "s" }]));
        obj.insert(
            "messages".into(),
            json!([
                { "role": "user", "content": [{ "type": "text", "text": "旧问" }] },
                { "role": "assistant", "content": [{ "type": "text", "text": "旧答" }] },
                {
                    "role": "user",
                    "content": [
                        { "type": "tool_result", "content": "r" },
                        { "type": "text", "text": "新问" },
                    ],
                },
            ]),
        );

        let injected = inject_cache_breakpoints(&mut obj);

        assert_eq!(injected, 3);
        let tools = obj["tools"].as_array().unwrap();
        assert!(tools[0].get("cache_control").is_none(), "只标 tools 尾");
        assert_eq!(tools[1]["cache_control"], json!({ "type": "ephemeral" }));
        let system = obj["system"].as_array().unwrap();
        assert_eq!(system[0]["cache_control"], json!({ "type": "ephemeral" }));
        let messages = obj["messages"].as_array().unwrap();
        assert!(
            messages[0]["content"][0].get("cache_control").is_none(),
            "更早消息不应被标记"
        );
        assert!(
            messages[1]["content"][0].get("cache_control").is_none(),
            "assistant 消息不应被标记"
        );
        assert_eq!(
            messages[2]["content"][1]["cache_control"],
            json!({ "type": "ephemeral" }),
            "末条消息的最后一个块被标记"
        );
    }

    /// 尾块为 thinking 的消息跳过，向前找非 thinking 尾块的消息；尾块已带
    /// 断点的消息同样跳过（显式标记视作调用方意图）。
    #[test]
    fn auto_injection_skips_thinking_and_marked_tails() {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "messages".into(),
            json!([
                { "role": "user", "content": [{ "type": "text", "text": "旧问" }] },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "想", "signature": "s" },
                ] },
                { "role": "user", "content": [
                    { "type": "text", "text": "已标" },
                    { "type": "text", "text": "尾块", "cache_control": { "type": "ephemeral" } },
                ] },
            ]),
        );

        let injected = inject_cache_breakpoints(&mut obj);

        assert_eq!(injected, 1, "thinking 尾块与已标尾块都不可用，锚定更早消息");
        assert_eq!(
            obj["messages"][0]["content"][0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
        assert!(
            obj["messages"][2]["content"][1]
                .get("cache_control")
                .is_some(),
            "已标尾块保持原样"
        );
    }

    /// 预算为 4 减已有断点：预算用尽即止；已有断点达上限时零注入。
    #[test]
    fn auto_injection_respects_budget_minus_existing() {
        let marked_tool =
            |name: &str| json!({ "name": name, "cache_control": { "type": "ephemeral" } });
        let mut full = serde_json::Map::new();
        full.insert(
            "tools".into(),
            json!([
                marked_tool("a"),
                marked_tool("b"),
                marked_tool("c"),
                marked_tool("d")
            ]),
        );
        full.insert("system".into(), json!([{ "type": "text", "text": "s" }]));
        full.insert(
            "messages".into(),
            json!([{ "role": "user", "content": [{ "type": "text", "text": "问" }] }]),
        );
        assert_eq!(
            inject_cache_breakpoints(&mut full),
            0,
            "已有断点达上限时零注入"
        );
        assert!(
            full["system"][0].get("cache_control").is_none(),
            "预算用尽后 system 不被标记"
        );

        // 已有 3 个：预算剩 1，只注入 tools 尾。
        let mut almost = serde_json::Map::new();
        almost.insert(
            "tools".into(),
            json!([
                marked_tool("a"),
                marked_tool("b"),
                marked_tool("c"),
                { "name": "d", "input_schema": {} },
            ]),
        );
        almost.insert("system".into(), json!([{ "type": "text", "text": "s" }]));
        almost.insert(
            "messages".into(),
            json!([{ "role": "user", "content": [{ "type": "text", "text": "问" }] }]),
        );
        assert_eq!(inject_cache_breakpoints(&mut almost), 1);
        assert_eq!(
            almost["tools"][2]["cache_control"],
            json!({ "type": "ephemeral" })
        );
        assert!(
            almost["system"][0].get("cache_control").is_none(),
            "预算用尽后 system 不被标记"
        );
    }

    /// 断点预算钳制：超 4 上限时按 render order（tools → system → messages）
    /// 保后弃前，牺牲最早者并记 cache_breakpoint warning；恰好 4 个时不超限
    /// 零改动零告警。
    #[test]
    fn cache_breakpoints_clamp_to_budget_keeping_latest() {
        let hatch = || {
            [(
                "anthropic".to_string(),
                json!({ "cache_control": { "type": "ephemeral" } }),
            )]
            .into_iter()
            .collect::<crate::core::ir::ProviderOptions>()
        };
        let cached_tool = |name: &str| crate::core::ir::Tool {
            name: name.to_string(),
            description: None,
            parameters: Some(json!({
                "type": "object",
                "properties": { "x": { "type": "string" } },
            })),
            provider_options: hatch(),
        };
        let cached_system = |text: &str| Message {
            role: Role::System,
            content: vec![ContentPart::Text {
                text: text.to_string(),
                provider_options: HashMap::new(),
            }],
            provider_options: hatch(),
        };
        let cached_user = |text: &str| Message {
            role: Role::User,
            content: vec![ContentPart::Text {
                text: text.to_string(),
                provider_options: hatch(),
            }],
            provider_options: HashMap::new(),
        };

        // render order 共 7 个断点：tool0、tool1、system、消息 0..3；
        // 保留靠后 4 个（四条消息），牺牲最早的工具与 system 断点。
        let mut request = bare_request(vec![
            cached_system("你是天气助手"),
            cached_user("问 1"),
            cached_user("问 2"),
            cached_user("问 3"),
            cached_user("问 4"),
        ]);
        request.tools = vec![cached_tool("tool_a"), cached_tool("tool_b")];
        let mut warnings = Vec::new();
        let wire = encode_request(&request, &mut warnings);

        assert!(wire["tools"][0].get("cache_control").is_none());
        assert!(wire["tools"][1].get("cache_control").is_none());
        assert!(
            wire["system"]
                .as_array()
                .expect("块形状保持")
                .iter()
                .all(|block| block.get("cache_control").is_none()),
            "system 断点应被牺牲"
        );
        let kept = wire["messages"]
            .as_array()
            .expect("应有消息数组")
            .iter()
            .flat_map(|message| message["content"].as_array().expect("块数组").iter())
            .filter(|block| block.get("cache_control").is_some())
            .count();
        assert_eq!(kept, 4, "应保留靠后的 4 个消息断点");
        assert_eq!(
            warnings,
            vec![Warning::unsupported(
                warning_feature::CACHE_BREAKPOINT,
                "Anthropic 缓存断点上限 4 个，已保留靠后者，丢弃最早的 3 个",
            )]
        );

        // 恰好 4 个：零改动零告警。
        let request = bare_request(vec![
            cached_user("问 1"),
            cached_user("问 2"),
            cached_user("问 3"),
            cached_user("问 4"),
        ]);
        let mut warnings = Vec::new();
        let wire = encode_request(&request, &mut warnings);
        assert!(warnings.is_empty(), "不超限不应告警");
        let kept = wire["messages"]
            .as_array()
            .expect("应有消息数组")
            .iter()
            .flat_map(|message| message["content"].as_array().expect("块数组").iter())
            .filter(|block| block.get("cache_control").is_some())
            .count();
        assert_eq!(kept, 4, "预算内断点零改动");
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

    /// chat 的 response_format 在 Anthropic 出站无请求侧承载：非缺省形状
    /// 记 warning；type=text 等价缺省不告警。
    #[test]
    fn response_format_without_carrier_warns() {
        let mut request = {
            use crate::core::ir::Message;
            ChatRequest {
                model: "claude-sonnet-4-5".to_string(),
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
                max_tokens: Some(100),
                n: None,
                stop: Vec::new(),
                presence_penalty: None,
                frequency_penalty: None,
                seed: None,
                response_format: None,
                tools: Vec::new(),
                tool_choice: None,
                parallel_tool_calls: None,
                reasoning: None,
                provider_options: HashMap::new(),
                warnings: Vec::new(),
            }
        };

        request.response_format = Some(json!({ "type": "json_object" }));
        let mut warnings = Vec::new();
        encode_request(&request, &mut warnings);
        assert!(
            warnings.iter().any(|w| matches!(
                w,
                Warning::Unsupported { feature: f, .. } if f == warning_feature::RESPONSE_FORMAT
            )),
            "JSON 结构化输出无承载应记 response_format 告警"
        );

        request.response_format = Some(json!({ "type": "text" }));
        let mut warnings = Vec::new();
        encode_request(&request, &mut warnings);
        assert!(
            warnings.is_empty(),
            "type=text 等价缺省，不应告警: {warnings:?}"
        );
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
            parallel_tool_calls: None,
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
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

    /// usage 的 cache_creation TTL 明细解码为 1h 写入分量；缺失时为 0。
    #[test]
    fn response_decodes_cache_creation_1h_detail() {
        let wire = json!({
            "id": "msg_02", "type": "message", "role": "assistant", "model": "claude-sonnet",
            "content": [{ "type": "text", "text": "ok" }],
            "stop_reason": "end_turn", "stop_sequence": null,
            "usage": {
                "input_tokens": 100, "output_tokens": 10,
                "cache_creation_input_tokens": 300, "cache_read_input_tokens": 40,
                "cache_creation": { "ephemeral_5m_input_tokens": 100, "ephemeral_1h_input_tokens": 200 }
            }
        });
        let ir = decode_response(&wire).expect("应可解码");
        assert_eq!(ir.usage.cache_write_tokens, 300);
        assert_eq!(ir.usage.cache_write_1h_tokens, 200);

        // 无 cache_creation 明细的上游（旧形状）1h 分量为 0，写入总数不变。
        let wire = json!({
            "id": "msg_03", "type": "message", "role": "assistant", "model": "claude-sonnet",
            "content": [{ "type": "text", "text": "ok" }],
            "stop_reason": "end_turn", "stop_sequence": null,
            "usage": { "input_tokens": 1, "output_tokens": 1, "cache_creation_input_tokens": 7 }
        });
        let ir = decode_response(&wire).expect("应可解码");
        assert_eq!(ir.usage.cache_write_tokens, 7);
        assert_eq!(ir.usage.cache_write_1h_tokens, 0);
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
            parallel_tool_calls: None,
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
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

    /// tool id 清洗：非法字符替换 `_`、空 id 生成 `toolu_` 前缀兜底，
    /// tool_use 与 tool_result 经同一映射保持配对且形状合法。
    #[test]
    fn invalid_and_empty_tool_ids_are_sanitized_with_pairing() {
        let wire = json!({
            "model": "claude-sonnet",
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "", "tool_calls": [
                    { "id": "we!rd@id", "type": "function",
                      "function": { "name": "f", "arguments": "{}" } },
                    { "id": "", "type": "function",
                      "function": { "name": "g", "arguments": "{}" } }
                ]},
                { "role": "tool", "tool_call_id": "we!rd@id", "content": "ok1" },
                { "role": "tool", "tool_call_id": "", "content": "ok2" }
            ]
        });
        let request = crate::core::openai_chat::decode_request(&wire).expect("应可解码");
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        assert!(warnings.is_empty());

        let assistant = encoded["messages"]
            .as_array()
            .expect("应有消息数组")
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("应有 assistant 消息");
        let use_ids: Vec<&str> = assistant["content"]
            .as_array()
            .expect("assistant 应为块数组")
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .map(|b| b["id"].as_str().expect("tool_use 应有 id"))
            .collect();
        assert_eq!(use_ids[0], "we_rd_id", "非法字符应替换为下划线");
        assert!(
            use_ids[1].starts_with("toolu_") && use_ids[1].len() > "toolu_".len(),
            "空 id 应生成 toolu_ 前缀兜底: {}",
            use_ids[1]
        );
        for id in &use_ids {
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "wire id 应匹配 Anthropic 合法形状: {id}"
            );
        }

        let result_message = encoded["messages"]
            .as_array()
            .expect("应有消息数组")
            .iter()
            .find(|m| m["role"] == "user" && m["content"].is_array())
            .expect("应有 tool_result 消息");
        let result_ids: Vec<&str> = result_message["content"]
            .as_array()
            .expect("应为块数组")
            .iter()
            .map(|b| b["tool_use_id"].as_str().expect("tool_result 应有 id"))
            .collect();
        assert_eq!(
            result_ids, use_ids,
            "tool_use 与 tool_result 应经同一映射配对且按 tool_use 顺序对齐"
        );
    }

    /// 重复 tool 消息取每 id 最后一条内容且只产出一次，乱序结果按前置
    /// tool_use 顺序重排（每条 tool_result 紧随对应 tool_use 的序列）。
    #[test]
    fn duplicate_and_out_of_order_tool_results_are_deduped_and_aligned() {
        let wire = json!({
            "model": "claude-sonnet",
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "", "tool_calls": [
                    { "id": "call_A", "type": "function",
                      "function": { "name": "f", "arguments": "{}" } },
                    { "id": "call_B", "type": "function",
                      "function": { "name": "g", "arguments": "{}" } }
                ]},
                { "role": "tool", "tool_call_id": "call_B", "content": "B-first" },
                { "role": "tool", "tool_call_id": "call_A", "content": "A-final" },
                { "role": "tool", "tool_call_id": "call_B", "content": "B-final" }
            ]
        });
        let request = crate::core::openai_chat::decode_request(&wire).expect("应可解码");
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        assert!(warnings.is_empty());

        let results = encoded["messages"]
            .as_array()
            .expect("应有消息数组")
            .iter()
            .find(|m| m["role"] == "user" && m["content"].is_array())
            .expect("应有 tool_result 消息")["content"]
            .as_array()
            .expect("应为块数组")
            .clone();
        assert_eq!(results.len(), 2, "重复 tool 消息应去重");
        assert_eq!(results[0]["tool_use_id"], json!("call_A"));
        assert_eq!(results[0]["content"], json!("A-final"));
        assert_eq!(results[1]["tool_use_id"], json!("call_B"));
        assert_eq!(
            results[1]["content"],
            json!("B-final"),
            "同 id 重发取最后一条内容"
        );
    }

    /// tool id 清洗为幂等纯函数：合法输入逐字节不变，非法输入收敛到合法形状。
    #[test]
    fn sanitize_tool_id_is_idempotent_and_legality_preserving() {
        for (raw, expected) in [
            ("toolu_01", "toolu_01"),
            ("call-9_X", "call-9_X"),
            ("we!rd@id", "we_rd_id"),
            ("中文名", "___"),
        ] {
            assert_eq!(sanitize_tool_id(raw), expected);
            assert_eq!(sanitize_tool_id(&sanitize_tool_id(raw)), expected, "应幂等");
        }
        assert_eq!(sanitize_tool_id("!!"), "__", "全非法字符清洗后仍为合法形状");
        assert_eq!(sanitize_tool_id(""), "", "空串清洗为空，生成由调用方兜底");
    }

    /// 请求编码：散布的多条 System 消息按序合并进顶层 `system`（`\n\n`
    /// 连接），不再以 user 文本夹入消息流；空文本 System 跳过。
    #[test]
    fn multiple_system_messages_merge_into_top_system() {
        let system_message = |text: &str| Message {
            role: Role::System,
            content: vec![ContentPart::Text {
                text: text.to_string(),
                provider_options: HashMap::new(),
            }],
            provider_options: HashMap::new(),
        };
        let request = ChatRequest {
            model: "claude-sonnet".to_string(),
            messages: vec![
                system_message("你是天气助手"),
                Message {
                    role: Role::User,
                    content: vec![ContentPart::Text {
                        text: "上海天气如何？".to_string(),
                        provider_options: HashMap::new(),
                    }],
                    provider_options: HashMap::new(),
                },
                system_message("输出一律使用 JSON"),
                system_message(""),
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
            parallel_tool_calls: None,
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
        };
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        assert_eq!(
            encoded["system"],
            json!("你是天气助手\n\n输出一律使用 JSON")
        );
        // 消息序列不含夹入的 system 文本：仅剩一条 user。
        let messages = encoded["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "上海天气如何？");
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
            parallel_tool_calls: None,
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
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

    /// 出站编码接线：根级 union schema 的 tool 摊平出站并记 warning；
    /// schema 缺席的 tool 出站为空 object schema（Anthropic 必有 input_schema）。
    #[test]
    fn tool_input_schema_normalized_on_encode() {
        let request = ChatRequest {
            model: "claude-sonnet".to_string(),
            messages: Vec::new(),
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
            tools: vec![
                Tool {
                    name: "flattened".to_string(),
                    description: None,
                    parameters: Some(json!({
                        "anyOf": [
                            { "type": "object", "properties": { "a": { "type": "string" } } },
                        ],
                    })),
                    provider_options: HashMap::new(),
                },
                Tool {
                    name: "bare".to_string(),
                    description: None,
                    parameters: None,
                    provider_options: HashMap::new(),
                },
            ],
            tool_choice: None,
            parallel_tool_calls: None,
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
        };
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        assert_eq!(
            encoded["tools"][0]["input_schema"],
            json!({ "type": "object", "properties": { "a": { "type": "string" } } })
        );
        assert_eq!(
            encoded["tools"][1]["input_schema"],
            json!({ "type": "object", "properties": {} })
        );
        assert!(matches!(
            warnings.as_slice(),
            [Warning::Compatibility { feature: f1, .. }, Warning::Compatibility { feature: f2, .. }]
                if f1 == "input_schema" && f2 == "input_schema"
        ));
    }

    /// `none` 表示禁用工具；Anthropic 不接受对应的 wire 形状，故连同工具声明
    /// 一并省略。
    #[test]
    fn tool_choice_none_omits_tools_and_choice() {
        let mut request = bare_request(Vec::new());
        request.tools = vec![Tool {
            name: "get_weather".to_string(),
            description: None,
            parameters: Some(json!({ "type": "object" })),
            provider_options: HashMap::new(),
        }];
        request.tool_choice = Some(ToolChoice::None);

        let encoded = encode_request(&request, &mut Vec::new());
        assert!(encoded.get("tools").is_none());
        assert!(encoded.get("tool_choice").is_none());
    }

    /// `event: error`（200 后流内错误，如 overloaded_error）解码为 IR Error
    /// 事件，不再静默吞掉；错误不贡献内容，累积器照常保留已累积 usage。
    #[test]
    fn stream_error_event_decodes_to_ir_error() {
        let raw = include_str!("__fixtures__/stream_error.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let mut decoder = StreamDecoder::default();
        let chunk = decoder.process(&wire);
        assert!(matches!(
            chunk.events.as_slice(),
            [StreamEvent::Error { message }] if message == "Overloaded"
        ));
        assert!(!chunk.is_output);
    }

    /// 形状畸形的错误帧仍提取错误语义：缺 `type` 判别键（部分代理以裸
    /// `{"error": {...}}` 报错）导致整帧解析失败时，留痕后以顶层 error.message
    /// 映射 IR Error，不静默为空。
    #[test]
    fn malformed_error_event_still_surfaces_error_semantics() {
        let event = json!({
            "error": { "type": "overloaded_error", "message": "Overloaded" },
        });
        let decoded = StreamDecoder::default().process(&event);
        assert!(matches!(
            decoded.events.as_slice(),
            [StreamEvent::Error { message }] if message == "Overloaded"
        ));
    }

    /// 解析失败且无错误语义的事件：留痕后跳过（空事件），不中断流。
    #[test]
    fn malformed_event_without_error_semantics_is_skipped() {
        let event = json!({
            "type": "content_block_delta",
            "index": "not-a-number",
            "delta": { "type": "text_delta", "text": "hi" },
        });
        let decoded = StreamDecoder::default().process(&event);
        assert_eq!(decoded.events, Vec::new());

        // error 对象在场但 message 类型不符：无字符串可提取，同样跳过。
        let event = json!({
            "type": "error",
            "error": { "type": "overloaded_error", "message": 42 },
        });
        let decoded = StreamDecoder::default().process(&event);
        assert_eq!(decoded.events, Vec::new());
    }

    /// 流内错误编码：以 `event: error` 帧下发，与网关兜底错误帧
    /// （`stream_error_frame`）同形状。
    #[test]
    fn stream_error_event_encodes_to_named_error_frame() {
        let mut encoder = StreamEncoder::new(None);
        let frames = encoder.encode(&StreamEvent::Error {
            message: "Overloaded".to_string(),
        });
        assert_eq!(frames, vec![stream_error_frame("Overloaded")]);
        assert_eq!(frames[0].event.as_deref(), Some("error"));
        let body: Value = serde_json::from_str(&frames[0].data).expect("错误帧载荷应为 JSON");
        assert_eq!(
            body,
            json!({ "type": "error", "error": { "type": "api_error", "message": "Overloaded" } })
        );
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

    /// 流式 usage 分布在首部和收尾时，必须合并输入、缓存与输出各分量。
    #[test]
    fn stream_usage_merges_message_start_and_delta() {
        let mut decoder = StreamDecoder::default();
        let start = json!({
            "type": "message_start",
            "message": {
                "id": "msg_1",
                "model": "claude-sonnet",
                "usage": {
                    "input_tokens": 120,
                    "cache_read_input_tokens": 30,
                    "cache_creation_input_tokens": 10
                }
            }
        });
        let delta = json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" },
            "usage": { "output_tokens": 45 }
        });

        let start_chunk = decoder.process(&start);
        assert!(start_chunk.events.iter().any(|event| matches!(
            event,
            StreamEvent::ResponseMetadata { id, model }
                if id == "msg_1" && model == "claude-sonnet"
        )));
        let finish = decoder.process(&delta).events;
        assert!(matches!(
            finish.as_slice(),
            [StreamEvent::Finish { usage, .. }]
                if usage.input_tokens == 120
                    && usage.output_tokens == 45
                    && usage.cache_read_tokens == 30
                    && usage.cache_write_tokens == 10
        ));
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
        // 恰好一个 tool-call，无重复（流式/非流式同构）。
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

    /// 流式 IR 事件编码为入站 Anthropic SSE 帧。
    ///
    /// 快照锁住整条帧序列：事件名、块索引、`content_block_start`/`delta`/`stop`
    /// 的配对、以及 `message_delta` + `message_stop` 的两帧收尾。逐字段断言只能
    /// 抽查其中几处，帧数量与顺序的回归会漏掉。
    #[test]
    fn stream_events_encode_to_anthropic_frames() {
        let mut encoder = StreamEncoder::default();
        let mut frames = vec![encoder.message_start()];
        frames.extend(encoder.encode(&StreamEvent::TextStart {
            id: "0".to_string(),
            provider_options: HashMap::new(),
        }));
        frames.extend(encoder.encode(&StreamEvent::TextDelta {
            id: "0".to_string(),
            delta: "Hi".to_string(),
            provider_options: HashMap::new(),
        }));
        frames.extend(encoder.encode(&StreamEvent::TextEnd {
            id: "0".to_string(),
            provider_options: HashMap::new(),
        }));
        frames.extend(encoder.encode(&StreamEvent::Finish {
            finish_reason: FinishReason {
                unified: FinishReasonUnified::ToolCalls,
                raw: Some("tool_use".to_string()),
            },
            usage: Usage {
                input_tokens: 3,
                output_tokens: 2,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_write_1h_tokens: 0,
                raw: None,
            },
            provider_metadata: HashMap::new(),
        }));

        insta::assert_json_snapshot!(frames_to_snapshot(&frames));
    }

    /// 流式 signature_delta 编码：附随进行中 thinking 块的增量事件。
    ///
    /// 快照覆盖两种增量的分帧结果——有内容无 signature 走 `thinking_delta`，
    /// 零长增量仅携带 signature 走 `signature_delta`，两者都只应产出单帧。
    #[test]
    fn stream_encodes_signature_delta() {
        let mut encoder = StreamEncoder::default();
        let mut frames = encoder.encode(&StreamEvent::ReasoningStart {
            id: "0".to_string(),
            provider_options: HashMap::new(),
        });
        frames.extend(encoder.encode(&StreamEvent::ReasoningDelta {
            id: "0".to_string(),
            delta: "先想".to_string(),
            provider_options: HashMap::new(),
        }));
        frames.extend(
            encoder.encode(&StreamEvent::ReasoningDelta {
                id: "0".to_string(),
                delta: String::new(),
                provider_options: [("anthropic".to_string(), json!({ "signature": "sigX" }))]
                    .into_iter()
                    .collect(),
            }),
        );

        insta::assert_json_snapshot!(frames_to_snapshot(&frames));
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
        assert!(sniff_usage(&json!({ "usage": { "output_tokens": "invalid" } })).is_none());
        assert!(sniff_usage(&json!({ "usage": { "unrelated": 1 } })).is_none());
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
            parallel_tool_calls: None,
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
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

    /// 纯函数整形：乱序 assistant 块内 thinking/redacted_thinking 稳定归位
    /// 块首（相对顺序保持），user 消息不参与，合法序列恒等。
    #[test]
    fn thinking_blocks_move_to_front_stably() {
        let mut messages = vec![
            json!({ "role": "user", "content": "hi" }),
            json!({ "role": "assistant", "content": [
                { "type": "text", "text": "a" },
                { "type": "thinking", "thinking": "t1", "signature": "s1" },
                { "type": "tool_use", "id": "toolu_1", "name": "f", "input": {} },
                { "type": "redacted_thinking", "data": "d" },
                { "type": "text", "text": "b" }
            ]}),
        ];
        move_thinking_blocks_to_front(&mut messages);
        let types: Vec<&str> = messages[1]["content"]
            .as_array()
            .expect("content 应为数组")
            .iter()
            .map(|block| block["type"].as_str().expect("块应有 type"))
            .collect();
        assert_eq!(
            types,
            ["thinking", "redacted_thinking", "text", "tool_use", "text"],
            "thinking 归位块首，其余块相对顺序不变"
        );
        assert_eq!(messages[0]["role"], "user", "user 消息不参与整形");

        let mut valid = vec![json!({ "role": "assistant", "content": [
            { "type": "thinking", "thinking": "t", "signature": "s" },
            { "type": "redacted_thinking", "data": "d" },
            { "type": "tool_use", "id": "toolu_1", "name": "f", "input": {} }
        ]})];
        let before = valid.clone();
        move_thinking_blocks_to_front(&mut valid);
        assert_eq!(valid, before, "合法序列应恒等");
    }

    /// 编码接线：连续 assistant 消息（tool_use 在前、thinking 在后，跨族
    /// 常见脏序列）合并后 thinking 归位块首；末尾 prefill 文本裁去尾随空白。
    #[test]
    fn encode_shapes_merged_assistant_blocks() {
        let request = ChatRequest {
            model: "claude-sonnet-4-5".to_string(),
            messages: vec![
                Message {
                    role: Role::User,
                    content: vec![ContentPart::Text {
                        text: "hi".to_string(),
                        provider_options: HashMap::new(),
                    }],
                    provider_options: HashMap::new(),
                },
                Message {
                    role: Role::Assistant,
                    content: vec![ContentPart::ToolCall {
                        tool_call_id: "call-1".to_string(),
                        tool_name: "get_weather".to_string(),
                        input: json!({}),
                        provider_options: HashMap::new(),
                    }],
                    provider_options: HashMap::new(),
                },
                Message {
                    role: Role::Assistant,
                    content: vec![
                        ContentPart::Reasoning {
                            text: "think".to_string(),
                            provider_options: [(
                                "anthropic".to_string(),
                                json!({ "signature": "sig-1" }),
                            )]
                            .into_iter()
                            .collect(),
                        },
                        ContentPart::Text {
                            text: "prefill  ".to_string(),
                            provider_options: HashMap::new(),
                        },
                    ],
                    provider_options: HashMap::new(),
                },
            ],
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
            parallel_tool_calls: None,
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
        };
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        assert!(warnings.is_empty());

        // 连续 assistant 已合并为一条；thinking 归位块首，tool_use 随后，
        // prefill 尾随空白被裁剪（前导空白保留）。
        let assistant = &encoded["messages"][1];
        assert_eq!(assistant["role"], "assistant");
        let content = assistant["content"].as_array().expect("content 应为数组");
        let types: Vec<&str> = content
            .iter()
            .map(|block| block["type"].as_str().expect("块应有 type"))
            .collect();
        assert_eq!(types, ["thinking", "tool_use", "text"]);
        assert_eq!(content[0]["signature"], "sig-1");
        assert_eq!(content[2]["text"], "prefill");
    }

    /// 模型列表编码对齐官方 `GET /v1/models` 黄金样例。
    #[test]
    fn model_list_fixture_matches_wire() {
        let raw = include_str!("__fixtures__/model_list.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let encoded = encode_model_list(&["fast".to_string(), "gpt-4o".to_string()]);
        assert_eq!(encoded, wire, "列表编码应与黄金样例一致");
    }
}
