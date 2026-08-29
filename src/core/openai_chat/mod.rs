//! OpenAI Chat Completions 协议适配器：wire ↔ IR 双向编解码。
//!
//! wire 结构体全部私有，透过 `decode_*`/`encode_*` 公共函数暴露 IR 边界，
//! wire 类型不出本模块边界。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::core::ir::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, FinishReasonUnified, MediaSource,
    Message, PROVIDER_EXTRA_KEY, ReasoningEffort, Role, StreamEvent, Tool, ToolChoice, Usage,
    Warning, apply_provider_extra, capture_unknown_fields, warning_feature,
};
use crate::core::stream::SseFrame;

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
    #[error("tool_choice 形状无法识别: {detail}")]
    InvalidToolChoice { detail: String },
    #[error("reasoning_effort 取值无法识别: {detail}")]
    InvalidReasoningEffort { detail: String },
    #[error("响应缺少 choices")]
    MissingChoices,
    #[error("响应的 choice 缺少 message")]
    MissingChoiceMessage,
}

// ---- wire 请求类型 ----

/// 本协议已知顶层请求字段白名单；白名单外的顶层字段由入站解码收进
/// 未知字段逃生舱（`provider_options["openai"]["extra"]`）。
const KNOWN_REQUEST_FIELDS: &[&str] = &[
    "model",
    "messages",
    "stream",
    "temperature",
    "top_p",
    "max_tokens",
    "max_completion_tokens",
    "n",
    "stop",
    "presence_penalty",
    "frequency_penalty",
    "seed",
    "response_format",
    "tools",
    "tool_choice",
    "reasoning_effort",
];

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
    /// o 系/gpt-5 客户端的输出上限字段（`max_tokens` 的事实标准继任者）。
    /// 捕获进 IR `max_tokens`（归一），原字段名经请求级逃生舱记忆供同族回写。
    #[serde(default)]
    max_completion_tokens: Option<u32>,
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
    #[serde(default)]
    reasoning_effort: Option<String>,
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
    /// 助手思维链（DeepSeek/OpenRouter/xAI 生态事实标准），`reasoning` 为
    /// OpenRouter 别名；解码归一为 IR Reasoning part，`reasoning_content`
    /// 优先。
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
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
    #[serde(default)]
    image_url: Option<WireImageUrl>,
}

/// `image_url` part 的载体：`url` 为远程 URL 或 base64 data URL；
/// `detail` 为 OpenAI 图片清晰度档位，可选。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireImageUrl {
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
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

// ---- 流式 wire 类型 ----

/// Chat Completions 流式 chunk（`chat.completion.chunk`）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireStreamChunk {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<WireStreamChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireStreamChoice {
    #[serde(default)]
    delta: Option<WireStreamDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireStreamDelta {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireStreamToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireStreamToolCall {
    /// 工具调用在流中的稳定序号（跨帧一致）。
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<WireStreamToolCallFunction>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct WireStreamToolCallFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

// ---- 入站解码：wire 请求 → IR ----

/// 解码入站 Chat Completions 请求为 IR。
pub fn decode_request(value: &Value) -> Result<ChatRequest, DecodeError> {
    let wire: WireChatRequest = serde_path_to_error::deserialize(value.clone()).map_err(|err| {
        DecodeError::InvalidShape {
            detail: err.to_string(),
        }
    })?;

    let mut warnings = Vec::new();
    let messages = wire
        .messages
        .iter()
        .enumerate()
        .map(|(index, m)| decode_message(m, index, &mut warnings))
        .collect::<Result<Vec<_>, _>>()?;

    // 输出上限归一进 IR `max_tokens`；两字段并存（客户端冲突）取事实标准
    // 继任字段。原字段名经逃生舱记忆，同族出站按请求原字段回写。
    let mut provider_options = HashMap::new();
    if let Some(value) = wire.max_completion_tokens {
        provider_options.insert(
            "openai".to_string(),
            json!({ "max_completion_tokens": value }),
        );
    }
    // 白名单外的顶层字段收进未知字段逃生舱，同族出站原样回写。
    let extra = capture_unknown_fields(value, KNOWN_REQUEST_FIELDS);
    if !extra.is_empty() {
        let entry = provider_options
            .entry("openai".to_string())
            .or_insert_with(|| json!({}));
        if let Value::Object(openai) = entry {
            openai.insert(PROVIDER_EXTRA_KEY.to_string(), Value::Object(extra));
        }
    }

    Ok(ChatRequest {
        model: wire.model,
        messages,
        stream: wire.stream,
        temperature: wire.temperature,
        top_p: wire.top_p,
        // Chat Completions 没有 top_k 字段；入站解码不产出该值。
        top_k: None,
        max_tokens: wire.max_completion_tokens.or(wire.max_tokens),
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
        tool_choice: wire
            .tool_choice
            .as_ref()
            .map(decode_tool_choice)
            .transpose()?,
        reasoning: wire
            .reasoning_effort
            .as_deref()
            .map(|value| {
                ReasoningEffort::parse_effort(value).ok_or_else(|| {
                    DecodeError::InvalidReasoningEffort {
                        detail: format!("未知档位 {value:?}"),
                    }
                })
            })
            .transpose()?,
        provider_options,
        warnings,
    })
}

/// 解码 wire `tool_choice` 为 IR 类型化枚举。
///
/// 已知形状之外直接拒绝：原样透传时代未知形状被静默忽略，跨协议转换后
/// 即成上游 400 雷，提前到入站面报错并指明字段。
fn decode_tool_choice(value: &Value) -> Result<ToolChoice, DecodeError> {
    match value {
        Value::String(s) => match s.as_str() {
            "auto" => Ok(ToolChoice::Auto),
            "none" => Ok(ToolChoice::None),
            "required" => Ok(ToolChoice::Required),
            other => Err(DecodeError::InvalidToolChoice {
                detail: format!("未知字符串值 {other:?}"),
            }),
        },
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) != Some("function") {
                return Err(DecodeError::InvalidToolChoice {
                    detail: "对象形状仅支持 {\"type\":\"function\"}".to_string(),
                });
            }
            let name = map
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if name.is_empty() {
                return Err(DecodeError::InvalidToolChoice {
                    detail: "type=function 缺少 function.name".to_string(),
                });
            }
            Ok(ToolChoice::Tool {
                name: name.to_string(),
            })
        }
        _ => Err(DecodeError::InvalidToolChoice {
            detail: "仅支持字符串或对象".to_string(),
        }),
    }
}

/// 解码单条 wire 消息为 IR 消息。
///
/// `developer` role 是 `system` 的事实标准继任者（o 系客户端普遍使用），
/// 与 Responses 适配器同规按 System 处理；角色名本身不做同族保留。
fn decode_message(
    wire: &WireMessage,
    index: usize,
    warnings: &mut Vec<Warning>,
) -> Result<Message, DecodeError> {
    let role = match wire.role.as_str() {
        "system" | "developer" => Role::System,
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
                    .map(|part| decode_user_part(part, index))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        Role::Assistant => decode_assistant(wire, index, warnings)?,
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

/// 助手消息：思维链归一为首个 Reasoning part（置于应答内容之前），text
/// parts 聚合成一个 text part，tool-call parts 各自保留。
fn decode_assistant(
    wire: &WireMessage,
    index: usize,
    warnings: &mut Vec<Warning>,
) -> Result<Vec<ContentPart>, DecodeError> {
    let mut parts = Vec::new();

    // 双别名归一：`reasoning_content` 为主、`reasoning` 为别名，并存时取主；
    // 空串视同缺席，不产出空 part。
    if let Some(reasoning) = wire
        .reasoning_content
        .as_ref()
        .or(wire.reasoning.as_ref())
        .filter(|text| !text.is_empty())
    {
        parts.push(ContentPart::Reasoning {
            text: reasoning.clone(),
            provider_options: HashMap::new(),
        });
    }

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
            // 非法 arguments 拒绝会让整轮工具调用 400 卡死；对齐流式累积侧
            // 兜底：合法 JSON 对象才透传，否则兜底空对象并记 warning。
            let input = match serde_json::from_str::<Value>(&tc.function.arguments) {
                Ok(input @ Value::Object(_)) => input,
                _ => {
                    warnings.push(Warning::compatibility(
                        warning_feature::TOOL_ARGUMENTS,
                        format!(
                            "tool call {} 的 arguments 非合法 JSON 对象，已兜底为空对象",
                            tc.function.name
                        ),
                    ));
                    json!({})
                }
            };
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

/// 解码单个 user content part：`text` 与 `image_url`（远程 URL 或 base64 data URL）。
///
/// `image_url` part 映射为 IR 媒体 part：data URL 解析出 media_type + base64 字节
/// 为 `MediaSource::Data`，远程 URL 为 `MediaSource::Url`。其余 part 类型拒绝
/// （未知 user part），与 v1 一致。
fn decode_user_part(part: &WireContentPart, index: usize) -> Result<ContentPart, DecodeError> {
    match part.part_type.as_str() {
        "text" => {
            let text = part
                .text
                .clone()
                .ok_or(DecodeError::UnknownUserContentPart { index })?;
            Ok(ContentPart::Text {
                text,
                provider_options: HashMap::new(),
            })
        }
        "image_url" => {
            let image = part
                .image_url
                .as_ref()
                .ok_or(DecodeError::UnknownUserContentPart { index })?;
            let (media_type, data) = split_data_url(&image.url);
            // `detail` 是 OpenAI 特有的图片档位，经逃生舱保留，跨协议转换不静默丢失。
            let mut provider_options = HashMap::new();
            if let Some(detail) = &image.detail {
                provider_options.insert("openai".to_string(), json!({ "detail": detail }));
            }
            Ok(ContentPart::Media {
                media_type,
                data,
                provider_options,
            })
        }
        _ => Err(DecodeError::UnknownUserContentPart { index }),
    }
}

/// 拆分 media 数据源：data URL → `MediaSource::Data`（base64），否则 → `MediaSource::Url`。
///
/// data URL 形如 `data:<media_type>;base64,<base64 字节>`；`media_type` 缺省时
/// 以空串占位（出站编码时按目标协议兜底）。远程 URL 原样保留。
fn split_data_url(url: &str) -> (String, MediaSource) {
    if let Some((media_type, base64)) = crate::core::ir::split_data_url(url) {
        return (
            media_type,
            MediaSource::Data {
                base64: base64.to_string(),
            },
        );
    }
    // 非 data URL 的 `image_url` 隐含图片：以顶层 `image` 兜底（出站按顶层段判定）。
    (
        "image".to_string(),
        MediaSource::Url {
            url: url.to_string(),
        },
    )
}

/// 顶层媒体段是否为图片。
fn is_image_media(media_type: &str) -> bool {
    crate::core::ir::top_level_media_type(media_type) == "image"
}

// ---- 出站编码：IR → wire 请求 ----

/// chat 出站编码选项：渠道级兼容输出开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatEncodeOptions {
    /// 把 assistant 消息的 IR Reasoning part 回写为 `reasoning_content`
    /// （生态事实标准，DeepSeek 系工具轮要求思维链随历史回放）。关闭时
    /// 丢弃并记 warning。缺省开启（同族保真优先）。
    pub reasoning_content: bool,
}

impl Default for ChatEncodeOptions {
    fn default() -> Self {
        Self {
            reasoning_content: true,
        }
    }
}

/// 编码 IR 请求为出站 Chat Completions 请求体。
///
/// 目标协议无法表达的内容（`top_k`、reasoning part）追加到 `warnings`，由网关
/// 随响应回传给下游。reasoning 回写缺省开启，渠道级关闭用
/// [`encode_request_with`]。
pub fn encode_request(request: &ChatRequest, warnings: &mut Vec<Warning>) -> Value {
    encode_request_with(request, ChatEncodeOptions::default(), warnings)
}

/// 按渠道选项编码 IR 请求为出站 Chat Completions 请求体。
///
/// 多条/散布的 System 消息归并为单条置顶（`\n\n` 连接，空文本跳过）；
/// reasoning 回写缺省开启，渠道级关闭用 [`encode_request_with`]。
pub fn encode_request_with(
    request: &ChatRequest,
    options: ChatEncodeOptions,
    warnings: &mut Vec<Warning>,
) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    let mut system_texts: Vec<String> = Vec::new();
    for message in &request.messages {
        match message.role {
            Role::System => {
                // System 消息仅承载文本；异常持有的 reasoning part 与其他
                // 非助手角色同规显式丢弃并记 warning。
                if message
                    .content
                    .iter()
                    .any(|p| matches!(p, ContentPart::Reasoning { .. }))
                {
                    warnings.push(Warning::unsupported(
                        warning_feature::REASONING,
                        "OpenAI Chat Completions 无 reasoning 内容块，system 消息中的推理内容已丢弃",
                    ));
                }
                if let Some(text) = text_parts(&message.content)
                    && !text.is_empty()
                {
                    system_texts.push(text);
                }
            }
            _ => messages.push(encode_message(message, options, warnings)),
        }
    }
    if !system_texts.is_empty() {
        messages.insert(
            0,
            json!({ "role": "system", "content": system_texts.join("\n\n") }),
        );
    }

    if request.top_k.is_some() {
        warnings.push(Warning::unsupported(
            warning_feature::TOP_K,
            "OpenAI Chat Completions 无 top_k 参数，已丢弃",
        ));
    }
    // 请求级逃生舱在 OpenAI Chat 无对应字段，显式丢弃；openai 逃生舱内的
    // max_completion_tokens 字段名记忆已按原字段回写，未知字段（extra）由
    // 专用逃生舱回写或告警，均不计丢弃。
    for (provider, options) in &request.provider_options {
        let unexpressed = match options.as_object() {
            Some(map) => map.keys().any(|key| {
                key.as_str() != PROVIDER_EXTRA_KEY
                    && (provider != "openai" || key.as_str() != "max_completion_tokens")
            }),
            None => true,
        };
        if unexpressed {
            warnings.push(Warning::unsupported(
                warning_feature::PROVIDER_OPTIONS,
                format!("{provider} 的请求级逃生舱设置无法表达，已丢弃"),
            ));
        }
    }
    let mut obj = serde_json::Map::new();
    obj.insert("model".into(), json!(request.model));
    obj.insert("messages".into(), Value::Array(messages));
    if let Some(v) = request.temperature {
        obj.insert("temperature".into(), json!(v));
    }
    if let Some(v) = request.top_p {
        obj.insert("top_p".into(), json!(v));
    }
    // 输出上限按请求原字段回写：入站走 max_completion_tokens 的请求（o 系
    // 上游普遍拒绝 max_tokens）同族出站保持原字段，其余出 max_tokens。
    let max_tokens_field = if request
        .provider_options
        .get("openai")
        .and_then(|openai| openai.get("max_completion_tokens"))
        .is_some()
    {
        "max_completion_tokens"
    } else {
        "max_tokens"
    };
    if let Some(v) = request.max_tokens {
        obj.insert(max_tokens_field.into(), json!(v));
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
    if let Some(choice) = &request.tool_choice {
        obj.insert("tool_choice".into(), encode_tool_choice(choice));
    }
    if let Some(effort) = request.reasoning {
        obj.insert("reasoning_effort".into(), json!(effort.as_str()));
    }
    // 未知字段逃生舱最后应用：本族字段回写不覆盖类型化字段，跨族字段丢弃告警。
    apply_provider_extra(&mut obj, request, "openai", warnings);
    Value::Object(obj)
}

/// 编码 IR tool_choice 为 Chat Completions wire 值。
fn encode_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool { name } => {
            json!({ "type": "function", "function": { "name": name } })
        }
    }
}

/// 编码单条 IR 消息为 wire 消息。
///
/// assistant 消息的 `reasoning` part 经 `reasoning_content` 字段回写（生态
/// 事实标准，多个 part 聚合为单字段、段间空行连接）；渠道开关关闭或出现
/// 在其他角色的消息中无法表达，丢弃并记 warning。
fn encode_message(
    message: &Message,
    options: ChatEncodeOptions,
    warnings: &mut Vec<Warning>,
) -> Value {
    // System 消息已在请求级归并为单条置顶，不进入本函数。
    if message
        .content
        .iter()
        .any(|p| matches!(p, ContentPart::Reasoning { .. }))
        && (message.role != Role::Assistant || !options.reasoning_content)
    {
        warnings.push(Warning::unsupported(
            warning_feature::REASONING,
            "OpenAI Chat Completions 无 reasoning 内容块，助手消息中的推理内容已丢弃",
        ));
    }
    match message.role {
        Role::System => unreachable!("System 消息已在请求级归并"),
        Role::User => {
            // 单一纯文本 user 消息编码为字符串（OpenAI 惯例，保持既有往返形状）；
            // 否则按 content 顺序编码为数组，保持文本与媒体混排顺序。
            let single_text = match message.content.as_slice() {
                [ContentPart::Text { text, .. }] => Some(text),
                _ => None,
            };
            let content: Value = if let Some(text) = single_text {
                Value::String(text.clone())
            } else {
                Value::Array(
                    message
                        .content
                        .iter()
                        .filter_map(|p| encode_user_part(p, warnings))
                        .collect(),
                )
            };
            json!({ "role": "user", "content": content })
        }
        Role::Assistant => {
            let reasoning: Vec<&str> = message
                .content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Reasoning { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
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
            if options.reasoning_content && !reasoning.is_empty() {
                wire.insert("reasoning_content".into(), json!(reasoning.join("\n\n")));
            }
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

/// 编码单个 IR user part 为 wire content part。
///
/// 文本 → `text` part；媒体 part → `image_url`（base64 数据拼接为 data URL，
/// URL 原样，逃生舱 `provider_options["openai"]["detail"]` 存在时写回）。
/// 目标协议无法表达的媒体类型（非 image）丢弃并记 warning——Chat
/// Completions 的 `image_url` 仅承载图片，其他媒体（audio/file）不支持。
fn encode_user_part(part: &ContentPart, warnings: &mut Vec<Warning>) -> Option<Value> {
    match part {
        ContentPart::Text { text, .. } => Some(json!({ "type": "text", "text": text })),
        ContentPart::Media {
            media_type,
            data,
            provider_options,
        } => {
            // OpenAI Chat Completions：仅 `image_url` 承载媒体，且数据源可为
            // 远程 URL 或 base64 data URL。非图片媒体类型丢弃并记 warning。
            if !is_image_media(media_type) {
                warnings.push(Warning::unsupported(
                    warning_feature::MEDIA,
                    format!("OpenAI Chat Completions 仅支持图片媒体，{media_type} 已丢弃"),
                ));
                return None;
            }
            let url = match data {
                MediaSource::Data { base64 } => {
                    format!("data:{media_type};base64,{base64}")
                }
                MediaSource::Url { url } => url.clone(),
            };
            let mut image_url = json!({ "url": url });
            if let Some(detail) = provider_options.get("openai").and_then(|o| o.get("detail")) {
                image_url["detail"] = detail.clone();
            }
            Some(json!({ "type": "image_url", "image_url": image_url }))
        }
        _ => None,
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
    let wire: WireChatResponse =
        serde_path_to_error::deserialize(value.clone()).map_err(|err| {
            DecodeError::InvalidShape {
                detail: err.to_string(),
            }
        })?;

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
        warnings: Vec::new(),
    })
}

/// 直通快路径的 usage 嗅探：从单个 SSE 帧或非流式响应体提取 `usage` 字段折算为 IR
/// usage 四分量，供计费，不做完整解码。
///
/// OpenAI Chat Completions 的 usage 在非流式响应顶层与流式帧顶层均为 `usage`；
/// 此处从任意 JSON 值顶层取 `usage`，缺失或非对象时返回 `None`（该帧无计费数据）。
/// 与 IR 完整路径共用 `convert_usage`，保证直通与 IR 计费口径一致。
pub fn sniff_chat_usage(value: &Value) -> Option<Usage> {
    let usage = value.get("usage")?.as_object()?;
    let wire =
        serde_json::from_value::<WireUsage>(Value::Object(usage.clone())).unwrap_or(WireUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            prompt_tokens_details: None,
        });
    Some(convert_usage(wire))
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

/// unified finish reason 映射为 Chat Completions wire 值。
fn map_finish_reason(raw: Option<&str>) -> FinishReasonUnified {
    match raw {
        Some("stop") => FinishReasonUnified::Stop,
        Some("length") => FinishReasonUnified::Length,
        Some("content_filter") => FinishReasonUnified::ContentFilter,
        Some("function_call") | Some("tool_calls") => FinishReasonUnified::ToolCalls,
        _ => FinishReasonUnified::Other,
    }
}

/// 把 IR unified finish reason 映射为 Chat Completions wire 值。
///
/// 跨协议族转换时 `finish_reason.raw` 是出站协议的值（如 Anthropic 的 `end_turn`），
/// 不能透传给入站；统一从 `unified` 映射，保证跨协议族语义正确。
fn encode_finish_reason(finish_reason: &FinishReason) -> &'static str {
    match finish_reason.unified {
        FinishReasonUnified::Stop => "stop",
        FinishReasonUnified::Length => "length",
        FinishReasonUnified::ContentFilter => "content_filter",
        FinishReasonUnified::ToolCalls => "tool_calls",
        FinishReasonUnified::Error | FinishReasonUnified::Other => "stop",
    }
}

// ---- 入站响应编码：IR → wire ----

/// 编码 IR 响应为入站 Chat Completions 响应体。
///
/// 转换过程的 warnings（跨协议族丢弃的 reasoning 等）以顶层 `gateway.warnings`
/// 暴露给下游，与错误体的 `error.gateway` 归因字段对称；无 warning 时不写该字段，
/// 响应与官方形状字节一致。响应 content 中的 reasoning part 在此丢弃并记 warning。
pub fn encode_response(response: &ChatResponse) -> Value {
    // OpenAI Chat Completions 无 reasoning 内容块：响应中的推理内容在重编码时
    // 被丢弃，显式记 warning。
    let mut warnings = response.warnings.clone();
    if response
        .content
        .iter()
        .any(|p| matches!(p, ContentPart::Reasoning { .. }))
    {
        warnings.push(Warning::unsupported(
            warning_feature::REASONING,
            "OpenAI Chat Completions 无 reasoning 内容块，响应中的推理内容已丢弃",
        ));
    }
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

    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(response.id));
    obj.insert("object".into(), json!("chat.completion"));
    obj.insert("model".into(), json!(response.model));
    obj.insert(
        "choices".into(),
        json!([{
            "index": 0,
            "message": message,
            "logprobs": null,
            "finish_reason": encode_finish_reason(&response.finish_reason),
        }]),
    );
    obj.insert("usage".into(), encode_usage(&response.usage));
    if let Some(gateway) = encode_warnings(&warnings) {
        obj.insert("gateway".into(), gateway);
    }
    Value::Object(obj)
}

/// 把 IR warnings 编码为 `gateway` 归因对象；无 warning 时返回 `None`。
///
/// 两个 OpenAI 协议面共用该形状（`{"warnings":[...]}`），Anthropic 面同理，
/// 让下游无论以哪种协议入站都能读到同一份信息损失说明。
pub fn encode_warnings(warnings: &[Warning]) -> Option<Value> {
    if warnings.is_empty() {
        return None;
    }
    let items: Vec<Value> = warnings
        .iter()
        .map(|w| serde_json::to_value(w).unwrap_or(Value::Null))
        .collect();
    Some(json!({ "warnings": items }))
}

/// 编码 IR usage 四分量 + 缓存细节为 wire usage 对象。
fn encode_usage(usage: &Usage) -> Value {
    let details = if usage.cache_read_tokens > 0 || usage.cache_write_tokens > 0 {
        let mut d = serde_json::Map::new();
        d.insert("cached_tokens".into(), json!(usage.cache_read_tokens));
        d.insert("cache_write_tokens".into(), json!(usage.cache_write_tokens));
        Some(Value::Object(d))
    } else {
        None
    };

    let mut obj = serde_json::Map::new();
    obj.insert(
        "prompt_tokens".into(),
        json!(usage.input_tokens + usage.cache_read_tokens + usage.cache_write_tokens),
    );
    obj.insert("completion_tokens".into(), json!(usage.output_tokens));
    obj.insert(
        "total_tokens".into(),
        json!(
            usage.input_tokens
                + usage.output_tokens
                + usage.cache_read_tokens
                + usage.cache_write_tokens
        ),
    );
    if let Some(details) = details {
        obj.insert("prompt_tokens_details".into(), details);
    }
    Value::Object(obj)
}

// ---- 流式：上游 chunk → IR 流事件 ----

/// 流式解码器：把上游 Chat Completions 流式 chunk 解码为 IR 流事件。
///
/// 跨帧维护 tool-call 的 index→id
/// 映射，后续只带 index 的增量帧能匹配到首帧记录的 id。text delta 产出
/// text-start/delta/end，tool-call delta 按 index 累积为 tool-input-start/delta/end，
/// usage 与 finish_reason 在出现时产出生命周期事件。
#[derive(Debug, Default)]
pub struct StreamDecoder {
    tool_ids_by_index: HashMap<usize, String>,
    /// 最近一次出现的 finish_reason：usage 独立末帧（无 finish_reason）复用，
    /// 避免 Finish 退化为 Other 并在下游编码时误落回 "stop"。
    last_finish_reason: Option<FinishReason>,
    /// 是否已产出 `ResponseMetadata`：Chat Completions 每个 chunk 都重复携带
    /// id/model，而该事件在 IR 中是「一次响应一次」的生命周期事件。
    metadata_emitted: bool,
}

impl StreamDecoder {
    /// 解码单个上游 chunk 为若干 IR 流事件。
    pub fn process(&mut self, chunk: &Value) -> DecodeStreamChunk {
        let wire = match serde_json::from_value::<WireStreamChunk>(chunk.clone()) {
            Ok(wire) => wire,
            Err(_) => return DecodeStreamChunk::delivery(Vec::new()),
        };

        let mut events = Vec::new();
        let mut is_output = false;

        // 每条 chunk 都重复携带 id/model，但 `ResponseMetadata` 是一次响应只发
        // 一次的生命周期事件：重复产出会让下游编码器把它当成新响应的开始，例如
        // Responses 编码器据此下发 `response.created`，重复即等于宣告响应重开。
        // 与 anthropic_messages（仅 message_start）、openai_responses（仅
        // response.created）两个解码器的产出时机对齐。
        if !self.metadata_emitted
            && let Some(id) = &wire.id
            && let Some(model) = &wire.model
        {
            self.metadata_emitted = true;
            events.push(StreamEvent::ResponseMetadata {
                id: id.clone(),
                model: model.clone(),
            });
        }

        let choice = wire.choices.first();
        if let Some(choice) = choice
            && let Some(delta) = &choice.delta
        {
            // role 只出现在文本流的首帧：以此开启文本块。
            if delta.role.is_some() {
                events.push(StreamEvent::TextStart {
                    id: "0".to_string(),
                    provider_options: HashMap::new(),
                });
            }
            if let Some(content) = &delta.content
                && !content.is_empty()
            {
                is_output = true;
                events.push(StreamEvent::TextDelta {
                    id: "0".to_string(),
                    delta: content.clone(),
                    provider_options: HashMap::new(),
                });
            }
            if let Some(tool_calls) = &delta.tool_calls {
                for tc in tool_calls {
                    let index = tc.index;
                    let id = match &tc.id {
                        Some(id) => {
                            // 首帧携带 id：记录 index→id 并产出工具起始。
                            self.tool_ids_by_index.insert(index, id.clone());
                            events.push(StreamEvent::ToolInputStart {
                                id: id.clone(),
                                tool_name: tc
                                    .function
                                    .as_ref()
                                    .and_then(|f| f.name.clone())
                                    .unwrap_or_default(),
                                provider_options: HashMap::new(),
                            });
                            id.clone()
                        }
                        // 后续帧只带 index：回查首帧记录的 id。
                        None => self
                            .tool_ids_by_index
                            .get(&index)
                            .cloned()
                            .unwrap_or_else(|| format!("{index}")),
                    };
                    if let Some(function) = &tc.function
                        && let Some(arguments) = &function.arguments
                        && !arguments.is_empty()
                    {
                        events.push(StreamEvent::ToolInputDelta {
                            id,
                            delta: arguments.clone(),
                            provider_options: HashMap::new(),
                        });
                    }
                    is_output = true;
                }
            }
        }

        // finish/usage：真实 OpenAI 把 usage 放在 `include_usage` 的独立末帧
        // （choices 为空），与 finish_reason 分离。只要出现 usage 或 finish_reason
        // 即产出 Finish，保证计费不因末帧形状而漏采。
        if choice.is_some_and(|c| c.finish_reason.is_some()) || wire.usage.is_some() {
            // 本帧带 finish_reason 则更新记忆；usage 独立末帧复用上一次的值，
            // 否则 Finish 退化为 Other、下游编码误落回 "stop"。
            if let Some(raw) = choice.and_then(|c| c.finish_reason.clone()) {
                self.last_finish_reason = Some(FinishReason {
                    unified: map_finish_reason(Some(raw.as_str())),
                    raw: Some(raw),
                });
            }
            let finish_reason = self.last_finish_reason.clone().unwrap_or(FinishReason {
                unified: FinishReasonUnified::Other,
                raw: None,
            });
            events.push(StreamEvent::Finish {
                finish_reason,
                usage: wire
                    .usage
                    .clone()
                    .map(convert_usage)
                    .unwrap_or_else(Usage::default),
                provider_metadata: HashMap::new(),
            });
        }

        DecodeStreamChunk { events, is_output }
    }
}

/// 单个 chunk 解码结果：IR 事件 + 是否产出任何输出内容。
#[derive(Debug)]
pub struct DecodeStreamChunk {
    pub events: Vec<StreamEvent>,
    pub is_output: bool,
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

/// 把 IR 流事件编码为入站 Chat Completions SSE 帧（`data:` 行，无事件名）。
///
/// 维护进行中的 text/tool-input 块状态，把事件还原为 `chat.completion.chunk`
/// wire 形状。`StreamStart` 的 warnings 以首帧 `gateway.warnings` 下发，
/// 与非流式响应的 `gateway` 字段对称。
#[derive(Debug)]
pub struct StreamEncoder {
    text_open: bool,
    tool_calls: Vec<OpenToolCall>,
    /// 从 ResponseMetadata 记录的响应 id 与 model，用于各 chunk 帧。
    id: String,
    model: String,
    /// 入站模型名覆盖：别名命中时，出站响应模型名须重写回入站短名。
    /// `Some` 时无视 ResponseMetadata 携带的上游模型名。
    inbound_model: Option<String>,
    /// 渠道级 reasoning 兼容输出开关：开启时 ReasoningDelta 以
    /// `delta.reasoning_content` 增量下发；关闭时丢弃并在 finish 帧显式
    /// warning。缺省开启（同族保真优先）。
    reasoning_content: bool,
    /// 流中是否出现过被丢弃的 reasoning 事件：关闭开关时在 finish 帧 warning。
    saw_reasoning: bool,
    /// reasoning 增量是否进行中（ReasoningStart 后）：首个内容增量补 `role`。
    reasoning_open: bool,
    /// 是否已随任一内容增量下发过 `role`（整个流恰好一次）。
    role_emitted: bool,
}

impl Default for StreamEncoder {
    fn default() -> Self {
        Self {
            inbound_model: None,
            text_open: false,
            tool_calls: Vec::new(),
            id: String::new(),
            model: String::new(),
            reasoning_content: true,
            saw_reasoning: false,
            reasoning_open: false,
            role_emitted: false,
        }
    }
}

impl StreamEncoder {
    /// 指定入站模型名覆盖（别名重写响应模型名）与渠道级 reasoning 输出开关。
    pub fn new(inbound_model: Option<String>, reasoning_content: bool) -> Self {
        Self {
            inbound_model,
            reasoning_content,
            ..Self::default()
        }
    }
}

/// 入站侧进行中的工具调用，按 OpenAI 约定的 index 排序。
#[derive(Debug)]
struct OpenToolCall {
    index: usize,
    id: String,
    arguments: String,
}

impl StreamEncoder {
    /// 编码一个 IR 流事件，返回需要下发的 SSE 帧（可能为空）。
    pub fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        self.encode_chunks(event)
            .into_iter()
            .map(|value| SseFrame::data(serde_json::to_string(&value).unwrap_or_default()))
            .collect()
    }

    /// 编码一个 IR 流事件为 `chat.completion.chunk` wire 值序列。
    fn encode_chunks(&mut self, event: &StreamEvent) -> Vec<Value> {
        match event {
            // warnings 以独立首帧下发，让下游在收到任何内容前就感知信息损失。
            StreamEvent::StreamStart { warnings } => match encode_warnings(warnings) {
                Some(gateway) => {
                    let mut obj = serde_json::Map::new();
                    obj.insert("id".into(), json!("chatcmpl-stream"));
                    obj.insert("object".into(), json!("chat.completion.chunk"));
                    obj.insert("choices".into(), Value::Array(Vec::new()));
                    obj.insert("gateway".into(), gateway);
                    vec![Value::Object(obj)]
                }
                None => Vec::new(),
            },
            StreamEvent::ResponseMetadata { id, model } => {
                self.id = id.clone();
                self.model = model.clone();
                Vec::new()
            }
            StreamEvent::TextStart { .. } => {
                self.text_open = true;
                Vec::new()
            }
            StreamEvent::TextDelta { delta, .. } => {
                let mut choice = serde_json::Map::new();
                choice.insert("index".into(), json!(0));
                let mut delta_obj = serde_json::Map::new();
                delta_obj.insert("content".into(), json!(delta));
                if self.text_open {
                    if !self.role_emitted {
                        delta_obj.insert("role".into(), json!("assistant"));
                        self.role_emitted = true;
                    }
                    self.text_open = false;
                }
                choice.insert("delta".into(), Value::Object(delta_obj));
                vec![Value::Object(self.build_chunk(choice))]
            }
            StreamEvent::TextEnd { .. } => Vec::new(),
            StreamEvent::ReasoningStart { .. } => {
                // 开关关闭时丢弃并在 finish 帧记 warning；开启时增量下发。
                if self.reasoning_content {
                    self.reasoning_open = true;
                } else {
                    self.saw_reasoning = true;
                }
                Vec::new()
            }
            StreamEvent::ReasoningDelta { delta, .. } => {
                // 空增量跳过（DeepSeek 系要求 reasoning_content 非空时才有意义）。
                if !self.reasoning_content || delta.is_empty() {
                    return Vec::new();
                }
                let mut choice = serde_json::Map::new();
                choice.insert("index".into(), json!(0));
                let mut delta_obj = serde_json::Map::new();
                delta_obj.insert("reasoning_content".into(), json!(delta));
                if (self.reasoning_open || self.text_open) && !self.role_emitted {
                    delta_obj.insert("role".into(), json!("assistant"));
                    self.role_emitted = true;
                }
                choice.insert("delta".into(), Value::Object(delta_obj));
                vec![Value::Object(self.build_chunk(choice))]
            }
            StreamEvent::ReasoningEnd { .. } => {
                self.reasoning_open = false;
                Vec::new()
            }
            StreamEvent::ToolInputStart { id, tool_name, .. } => {
                let index = self.tool_calls.len();
                self.tool_calls.push(OpenToolCall {
                    index,
                    id: id.clone(),
                    arguments: String::new(),
                });
                let mut function = serde_json::Map::new();
                function.insert("name".into(), json!(tool_name));
                function.insert("arguments".into(), json!(""));
                let mut tc = serde_json::Map::new();
                tc.insert("index".into(), json!(index));
                tc.insert("id".into(), json!(id));
                tc.insert("type".into(), json!("function"));
                tc.insert("function".into(), Value::Object(function));
                let mut delta_obj = serde_json::Map::new();
                delta_obj.insert("tool_calls".into(), Value::Array(vec![Value::Object(tc)]));
                let mut choice = serde_json::Map::new();
                choice.insert("index".into(), json!(0));
                choice.insert("delta".into(), Value::Object(delta_obj));
                vec![Value::Object(self.build_chunk(choice))]
            }
            StreamEvent::ToolInputDelta { id, delta, .. } => {
                if let Some(tool) = self.tool_calls.iter_mut().find(|t| t.id == *id) {
                    tool.arguments.push_str(delta);
                }
                let index = self
                    .tool_calls
                    .iter()
                    .find(|t| t.id == *id)
                    .map(|t| t.index)
                    .unwrap_or(0);
                let mut function = serde_json::Map::new();
                function.insert("arguments".into(), json!(delta));
                let mut tc = serde_json::Map::new();
                tc.insert("index".into(), json!(index));
                tc.insert("function".into(), Value::Object(function));
                let mut delta_obj = serde_json::Map::new();
                delta_obj.insert("tool_calls".into(), Value::Array(vec![Value::Object(tc)]));
                let mut choice = serde_json::Map::new();
                choice.insert("index".into(), json!(0));
                choice.insert("delta".into(), Value::Object(delta_obj));
                vec![Value::Object(self.build_chunk(choice))]
            }
            StreamEvent::ToolInputEnd { .. } => Vec::new(),
            StreamEvent::ToolCall { .. } => Vec::new(),
            StreamEvent::Finish {
                finish_reason,
                usage,
                ..
            } => {
                let mut choice = serde_json::Map::new();
                choice.insert("index".into(), json!(0));
                choice.insert(
                    "finish_reason".into(),
                    json!(encode_finish_reason(finish_reason)),
                );
                let mut obj = self.build_chunk(choice);
                obj.insert("usage".into(), encode_usage(usage));
                // 流中丢弃过 reasoning：finish 帧显式 warning，供下游感知信息损失。
                if self.saw_reasoning {
                    let warning = Warning::unsupported(
                        warning_feature::REASONING,
                        "OpenAI Chat Completions 无 reasoning 内容块，推理内容已丢弃",
                    );
                    if let Some(gateway) = encode_warnings(&[warning]) {
                        obj.insert("gateway".into(), gateway);
                    }
                }
                vec![Value::Object(obj)]
            }
            // 流内错误没有协议通道：以独立 `data:` 帧下发错误 JSON（与网关
            // 兜底错误帧同形状），由调用方感知并终止流。
            StreamEvent::Error { message } => vec![encode_error(500, message)],
        }
    }

    /// 构造一个 `chat.completion.chunk` 对象，携带记录的响应 id/model。
    ///
    /// 返回可继续追加顶层字段的对象而非 [`Value`]，供终止帧在同一信封上补
    /// `usage`——每个 chunk（含终止帧）都必须带齐 `id`/`object`/`model`，
    /// 下游按 id 关联响应、按 object 判定帧类型。
    fn build_chunk(
        &self,
        choice: serde_json::Map<String, Value>,
    ) -> serde_json::Map<String, Value> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "id".into(),
            json!(if self.id.is_empty() {
                "chatcmpl-stream"
            } else {
                self.id.as_str()
            }),
        );
        obj.insert("object".into(), json!("chat.completion.chunk"));
        // 别名命中时用入站模型名覆盖上游模型名，让下游看到稳定短名。
        let model = self.inbound_model.as_deref().unwrap_or(&self.model);
        obj.insert("model".into(), json!(model));
        obj.insert("choices".into(), Value::Array(vec![Value::Object(choice)]));
        obj
    }
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

/// 流内错误的入站 SSE 帧（`data:` 纯帧，500 语义）。流式编码器消费 IR
/// Error 事件与网关兜底路径共用，保证形状一致。
pub fn stream_error_frame(message: &str) -> SseFrame {
    SseFrame::data(encode_error(500, message).to_string())
}

/// 编码为 OpenAI `GET /v1/models` 列表。`created` 未知时为 0。
pub fn encode_model_list(ids: &[String]) -> Value {
    json!({
        "object": "list",
        "data": ids.iter().map(|id| json!({
            "id": id,
            "object": "model",
            "created": 0,
            "owned_by": "kairos",
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::{frame_payload, frames_to_snapshot};
    use similar_asserts::assert_eq;

    /// 黄金样例请求 decode → encode 往返还原 wire。
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

    /// 黄金样例（max_completion_tokens 入站）decode → encode 往返还原 wire：
    /// 归一值经逃生舱记忆按原字段名回写，同族逐位稳定。
    #[test]
    fn max_completion_fixture_roundtrip() {
        let raw = include_str!("__fixtures__/request_max_completion.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let ir = decode_request(&wire).expect("fixture 应可解码为 IR");
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert_eq!(reencoded, wire, "往返应还原 wire 请求");
        assert!(warnings.is_empty(), "同协议往返不应产出 warning");
    }

    /// 黄金样例（思维链入站）decode → encode 往返还原 wire：Reasoning part
    /// 经 `reasoning_content` 同名回写，同族逐位稳定。
    #[test]
    fn reasoning_content_fixture_roundtrip() {
        let raw = include_str!("__fixtures__/request_reasoning_content.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let ir = decode_request(&wire).expect("fixture 应可解码为 IR");
        assert!(matches!(
            &ir.messages[1].content[0],
            ContentPart::Reasoning { text, .. }
                if text == "先算 900 ÷ 5 = 180，再算 25 ÷ 5 = 5，合起来 185。"
        ));
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert_eq!(reencoded, wire, "往返应还原 wire 请求");
        assert!(warnings.is_empty(), "同协议往返不应产出 warning");
    }

    /// `reasoning` 别名与 `reasoning_content` 归一为同一 Reasoning part；
    /// 并存时主字段优先。回写统一出规范字段名 `reasoning_content`。
    #[test]
    fn reasoning_alias_normalizes_to_reasoning_content() {
        let alias = json!({
            "model": "deepseek-reasoner",
            "messages": [{
                "role": "assistant",
                "content": "答案",
                "reasoning": "别名思维链"
            }]
        });
        let ir = decode_request(&alias).expect("别名应可解码");
        assert!(matches!(
            &ir.messages[0].content[0],
            ContentPart::Reasoning { text, .. } if text == "别名思维链"
        ));
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert!(warnings.is_empty());
        assert_eq!(
            reencoded["messages"][0]["reasoning_content"],
            json!("别名思维链")
        );

        let both = json!({
            "model": "deepseek-reasoner",
            "messages": [{
                "role": "assistant",
                "content": "答案",
                "reasoning": "别名",
                "reasoning_content": "主字段"
            }]
        });
        let ir = decode_request(&both).expect("并存应可解码");
        assert!(matches!(
            &ir.messages[0].content[0],
            ContentPart::Reasoning { text, .. } if text == "主字段"
        ));
    }

    /// 非法 tool arguments 不再拒绝整请求：解码成功、input 兜底空对象、
    /// warning 记录在请求上；合法 JSON 对象透传且零告警。
    #[test]
    fn illegal_tool_arguments_fall_back_to_empty_object() {
        let wire = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "get_weather", "arguments": "{oops" }
                }]
            }]
        });
        let ir = decode_request(&wire).expect("非法 arguments 应兜底解码而非拒绝");
        assert!(matches!(
            &ir.messages[0].content[0],
            ContentPart::ToolCall { input, .. } if *input == json!({})
        ));
        assert_eq!(
            ir.warnings,
            vec![Warning::compatibility(
                "tool_arguments",
                "tool call get_weather 的 arguments 非合法 JSON 对象，已兜底为空对象",
            )]
        );

        // 合法 JSON 但非对象（数组）同兜底。
        let wire = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "get_weather", "arguments": "[1, 2]" }
                }]
            }]
        });
        let ir = decode_request(&wire).expect("非对象 JSON 应兜底解码而非拒绝");
        assert!(matches!(
            &ir.messages[0].content[0],
            ContentPart::ToolCall { input, .. } if *input == json!({})
        ));
        assert_eq!(ir.warnings.len(), 1);

        // 合法 JSON 对象透传，零告警。
        let wire = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "get_weather", "arguments": "{\"city\":\"SF\"}" }
                }]
            }]
        });
        let ir = decode_request(&wire).expect("合法 arguments 应正常解码");
        assert!(matches!(
            &ir.messages[0].content[0],
            ContentPart::ToolCall { input, .. } if *input == json!({ "city": "SF" })
        ));
        assert!(ir.warnings.is_empty());
    }

    /// 多条/散布的 System 消息出站归并为单条置顶（`\n\n` 连接，空文本
    /// 跳过）；其余消息保持原序。无 System 消息时形状不变（fixture 往返覆盖）。
    #[test]
    fn scattered_system_messages_merge_to_single_top() {
        let request = ChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![
                Message {
                    role: Role::System,
                    content: vec![ContentPart::Text {
                        text: "你是天气助手".to_string(),
                        provider_options: HashMap::new(),
                    }],
                    provider_options: HashMap::new(),
                },
                Message {
                    role: Role::User,
                    content: vec![ContentPart::Text {
                        text: "上海天气如何？".to_string(),
                        provider_options: HashMap::new(),
                    }],
                    provider_options: HashMap::new(),
                },
                Message {
                    role: Role::System,
                    content: vec![ContentPart::Text {
                        text: "输出一律使用 JSON".to_string(),
                        provider_options: HashMap::new(),
                    }],
                    provider_options: HashMap::new(),
                },
                Message {
                    role: Role::System,
                    content: vec![ContentPart::Text {
                        text: String::new(),
                        provider_options: HashMap::new(),
                    }],
                    provider_options: HashMap::new(),
                },
            ],
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
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
        };
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        let messages = encoded["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0],
            json!({ "role": "system", "content": "你是天气助手\n\n输出一律使用 JSON" })
        );
        assert_eq!(messages[1]["role"], "user");
    }

    /// 渠道级开关控制请求历史回放：关闭时丢弃 assistant 的 reasoning part
    /// 并记 warning，开启时回写零告警（缺省开启由其余用例覆盖）。
    #[test]
    fn channel_gate_controls_reasoning_replay() {
        let request = ChatRequest {
            model: "deepseek-chat".to_string(),
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![
                    ContentPart::Reasoning {
                        text: "思考".to_string(),
                        provider_options: HashMap::new(),
                    },
                    ContentPart::Text {
                        text: "答案".to_string(),
                        provider_options: HashMap::new(),
                    },
                ],
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
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
        };

        let mut warnings = Vec::new();
        let encoded = encode_request_with(
            &request,
            ChatEncodeOptions {
                reasoning_content: false,
            },
            &mut warnings,
        );
        assert!(encoded["messages"][0].get("reasoning_content").is_none());
        assert!(
            warnings.iter().any(
                |w| matches!(w, Warning::Unsupported { feature, .. } if feature == "reasoning")
            )
        );

        let mut warnings = Vec::new();
        let encoded = encode_request_with(
            &request,
            ChatEncodeOptions {
                reasoning_content: true,
            },
            &mut warnings,
        );
        assert_eq!(encoded["messages"][0]["reasoning_content"], json!("思考"));
        assert!(warnings.is_empty());
    }

    /// 渠道级开关控制流式增量：开启时 ReasoningDelta 以
    /// `delta.reasoning_content` 下发（首个内容增量补 role，空增量跳过，
    /// finish 帧无告警）；关闭时丢弃并在 finish 帧记 warning。
    #[test]
    fn channel_gate_controls_reasoning_delta_streaming() {
        let reasoning_start = || StreamEvent::ReasoningStart {
            id: "0".to_string(),
            provider_options: HashMap::new(),
        };
        let reasoning_delta = |delta: &str| StreamEvent::ReasoningDelta {
            id: "0".to_string(),
            delta: delta.to_string(),
            provider_options: HashMap::new(),
        };
        let text_start = || StreamEvent::TextStart {
            id: "1".to_string(),
            provider_options: HashMap::new(),
        };
        let text_delta = || StreamEvent::TextDelta {
            id: "1".to_string(),
            delta: "答".to_string(),
            provider_options: HashMap::new(),
        };
        let finish = StreamEvent::Finish {
            finish_reason: FinishReason {
                unified: FinishReasonUnified::Stop,
                raw: None,
            },
            usage: Usage::default(),
            provider_metadata: HashMap::new(),
        };

        // 开启：增量逐帧下发，首个内容增量补 role，finish 无告警。
        let mut encoder = StreamEncoder::new(None, true);
        assert!(encoder.encode(&reasoning_start()).is_empty());
        let frames = encoder.encode(&reasoning_delta("思路"));
        assert_eq!(frames.len(), 1);
        let chunk = frame_payload(&frames[0]);
        assert_eq!(
            chunk["choices"][0]["delta"]["reasoning_content"],
            json!("思路")
        );
        assert_eq!(chunk["choices"][0]["delta"]["role"], json!("assistant"));
        assert!(
            encoder.encode(&reasoning_delta("")).is_empty(),
            "空增量不产帧"
        );
        assert!(
            encoder
                .encode(&StreamEvent::ReasoningEnd {
                    id: "0".to_string(),
                    provider_options: HashMap::new(),
                })
                .is_empty()
        );
        let frames = encoder.encode(&text_start());
        assert!(frames.is_empty());
        let frames = encoder.encode(&text_delta());
        let chunk = frame_payload(&frames[0]);
        assert_eq!(chunk["choices"][0]["delta"]["content"], json!("答"));
        assert!(
            chunk["choices"][0]["delta"].get("role").is_none(),
            "role 已随首个 reasoning 增量下发"
        );
        let frames = encoder.encode(&finish);
        assert!(
            frame_payload(&frames[0]).get("gateway").is_none(),
            "开启开关时 finish 帧不应有 reasoning 告警"
        );

        // 关闭：增量丢弃，finish 帧显式告警。
        let mut encoder = StreamEncoder::new(None, false);
        assert!(encoder.encode(&reasoning_start()).is_empty());
        assert!(encoder.encode(&reasoning_delta("思路")).is_empty());
        let frames = encoder.encode(&finish);
        let chunk = frame_payload(&frames[0]);
        assert_eq!(
            chunk["gateway"]["warnings"][0]["feature"],
            json!("reasoning")
        );
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

    /// 多模态黄金样例请求 decode → encode 往返还原 wire，文本与图片混排顺序不丢。
    #[test]
    fn multimodal_fixture_roundtrip() {
        let raw = include_str!("__fixtures__/request_multimodal.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let ir = decode_request(&wire).expect("fixture 应可解码为 IR");
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert_eq!(reencoded, wire, "往返应还原 wire 请求（含混排顺序）");
        assert!(warnings.is_empty(), "同协议图片往返不应产出 warning");

        // 混排顺序：text → 图片(data URL) → text → 图片(远程 URL)。
        let parts = &ir.messages[0].content;
        assert_eq!(parts.len(), 4, "应保留 4 个 part");
        assert!(matches!(parts[0], ContentPart::Text { .. }));
        assert!(matches!(
            &parts[1],
            ContentPart::Media {
                media_type,
                data: MediaSource::Data { base64 },
                ..
            } if media_type == "image/png" && base64 == "iVBORw0KGgoAAAANSUhEUg=="
        ));
        assert!(matches!(parts[2], ContentPart::Text { .. }));
        assert!(matches!(
            &parts[3],
            ContentPart::Media {
                data: MediaSource::Url { url },
                ..
            } if url == "https://example.com/image.png"
        ));
    }

    /// `image_url.detail` 档位经逃生舱往返：入站存 `provider_options["openai"]`，
    /// 出站写回，跨协议/跨渠道转换不静默丢失。
    #[test]
    fn image_detail_roundtrips_via_provider_options() {
        let wire = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image_url",
                    "image_url": { "url": "https://example.com/image.png", "detail": "low" }
                }]
            }]
        });
        let ir = decode_request(&wire).expect("带 detail 的 image_url 应可解码");
        assert!(matches!(
            &ir.messages[0].content[0],
            ContentPart::Media { provider_options, .. }
                if provider_options.get("openai").and_then(|o| o.get("detail"))
                    == Some(&Value::String("low".to_string()))
        ));
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert_eq!(reencoded, wire, "往返应还原 detail");
        assert!(warnings.is_empty());
    }

    /// 非文本/非 image_url 的 user content part 报错。
    #[test]
    fn unknown_user_content_is_rejected() {
        let wire = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [{ "type": "audio_url", "audio_url": { "url": "x" } }]
            }]
        });
        assert!(matches!(
            decode_request(&wire),
            Err(DecodeError::UnknownUserContentPart { index: 0 })
        ));
    }

    /// `developer` role 按 System 处理（o 系客户端的事实标准继任角色）；
    /// 其余未知角色仍在入站面拒绝。
    #[test]
    fn developer_role_decodes_as_system_and_unknown_role_is_rejected() {
        let developer = json!({
            "model": "gpt-4o",
            "messages": [{ "role": "developer", "content": "指令" }]
        });
        let ir = decode_request(&developer).expect("developer 角色应可解码");
        assert!(matches!(
            ir.messages.as_slice(),
            [Message {
                role: Role::System,
                ..
            }]
        ));

        let unknown = json!({
            "model": "gpt-4o",
            "messages": [{ "role": "bogus", "content": "hi" }]
        });
        assert!(matches!(
            decode_request(&unknown),
            Err(DecodeError::UnknownRole { index: 0 })
        ));
    }

    /// `max_completion_tokens` 归一进 IR `max_tokens`（与 `max_tokens` 并存时
    /// 取事实标准继任字段），原字段名经逃生舱记忆供同族出站回写。
    #[test]
    fn max_completion_tokens_normalizes_and_writes_back_original_field() {
        let wire = json!({
            "model": "o4-mini",
            "messages": [{ "role": "user", "content": "hi" }],
            "max_completion_tokens": 2048
        });
        let ir = decode_request(&wire).expect("应可解码");
        assert_eq!(ir.max_tokens, Some(2048));

        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert!(warnings.is_empty());
        assert_eq!(reencoded["max_completion_tokens"], json!(2048));
        assert!(reencoded.get("max_tokens").is_none(), "不应双写旧字段");

        // 仅 max_tokens 的请求不受影响：出 max_tokens，无逃生舱记忆。
        let legacy = json!({
            "model": "gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }],
            "max_tokens": 512
        });
        let ir = decode_request(&legacy).expect("应可解码");
        assert_eq!(ir.max_tokens, Some(512));
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert!(warnings.is_empty());
        assert_eq!(reencoded["max_tokens"], json!(512));
        assert!(reencoded.get("max_completion_tokens").is_none());

        // 两字段并存（客户端冲突）：归一取 max_completion_tokens。
        let conflict = json!({
            "model": "gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }],
            "max_tokens": 512,
            "max_completion_tokens": 2048
        });
        let ir = decode_request(&conflict).expect("应可解码");
        assert_eq!(ir.max_tokens, Some(2048));
    }

    /// wire 形状错误指明出错字段的 JSON 路径，而非笼统的「不是合法 JSON 对象」。
    #[test]
    fn invalid_wire_shape_reports_field_path() {
        let wire = json!({
            "model": "gpt-4o",
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

    /// 黄金样例流式往返：解码流式 chunk → 累积，与非流式 `response.json` 解码结果同构。
    #[test]
    fn stream_fixture_accumulates_to_response() {
        use crate::core::stream::StreamAccumulator;

        let mut decoder = StreamDecoder::default();
        let mut accumulator = StreamAccumulator::new();

        let frames = [
            include_str!("__fixtures__/stream_text_1.json"),
            include_str!("__fixtures__/stream_text_2.json"),
            include_str!("__fixtures__/stream_tool_start.json"),
            include_str!("__fixtures__/stream_tool_args.json"),
            include_str!("__fixtures__/stream_finish.json"),
        ];
        for raw in frames {
            let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
            for event in decoder.process(&wire).events {
                accumulator.push(event);
            }
        }
        let streamed = accumulator.finish();

        // 非流式黄金样例：response.json（同一文本 + 一个 tool_call + usage）。
        let raw = include_str!("__fixtures__/response.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let non_stream = decode_response(&wire).expect("fixture 应可解码");

        // 同构：流式累积结果与非流式解码完全一致（text + tool-call + usage + finish_reason）。
        assert_eq!(streamed, non_stream);
    }

    /// 上游流式 chunk 解码为 IR 流事件：text delta → text-delta，finish 帧带 usage。
    #[test]
    fn stream_chunk_decodes_to_ir_events() {
        let text_chunk = json!({
            "id": "chatcmpl-9", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "Hel" } }]
        });
        let decoded = StreamDecoder::default().process(&text_chunk);
        assert!(decoded.is_output);
        assert_eq!(
            decoded.events,
            vec![
                StreamEvent::ResponseMetadata {
                    id: "chatcmpl-9".to_string(),
                    model: "gpt-4o".to_string(),
                },
                // 首帧带 role，开启文本块。
                StreamEvent::TextStart {
                    id: "0".to_string(),
                    provider_options: HashMap::new(),
                },
                StreamEvent::TextDelta {
                    id: "0".to_string(),
                    delta: "Hel".to_string(),
                    provider_options: HashMap::new(),
                },
            ]
        );

        let finish_chunk = json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7 }
        });
        let decoded = StreamDecoder::default().process(&finish_chunk);
        assert!(!decoded.is_output);
        assert_eq!(decoded.events.len(), 1);
        match &decoded.events[0] {
            StreamEvent::Finish {
                finish_reason,
                usage,
                ..
            } => {
                assert_eq!(finish_reason.unified, FinishReasonUnified::Stop);
                assert_eq!(usage.input_tokens, 5);
                assert_eq!(usage.output_tokens, 2);
            }
            other => panic!("应产出 Finish 事件，实际 {other:?}"),
        }
    }

    /// 同一解码器连续处理多个 chunk：`ResponseMetadata` 只产出一次。
    ///
    /// Chat Completions 每个 chunk 都重复携带 id/model，但该事件在 IR 中是一次
    /// 响应一次的生命周期事件。重复产出会被下游编码器当成新响应开始——Responses
    /// 编码器据此下发 `response.created`，客户端收到第二帧就会丢弃已累积的内容。
    #[test]
    fn response_metadata_is_emitted_once_per_stream() {
        let mut decoder = StreamDecoder::default();
        let metadata_count = ["Hel", "lo", "!"]
            .into_iter()
            .flat_map(|delta| {
                decoder
                    .process(&json!({
                        "id": "chatcmpl-9",
                        "object": "chat.completion.chunk",
                        "model": "gpt-4o",
                        "choices": [{ "index": 0, "delta": { "content": delta } }]
                    }))
                    .events
            })
            .filter(|event| matches!(event, StreamEvent::ResponseMetadata { .. }))
            .count();
        assert_eq!(
            metadata_count, 1,
            "三个都带 id/model 的 chunk 只应产出一个 ResponseMetadata"
        );
    }

    /// usage 独立末帧（`include_usage` 真实帧型：choices 为空、仅带 usage）仍产出
    /// Finish，保证计费不漏采。
    #[test]
    fn usage_only_frame_emits_finish() {
        // 真实 OpenAI 把 usage 放在独立末帧，choices 为空数组。
        let usage_chunk = json!({
            "id": "chatcmpl-9", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7 }
        });
        let decoded = StreamDecoder::default().process(&usage_chunk);
        // 帧含 id/model，先产出 ResponseMetadata，再产出 Finish。
        assert_eq!(
            decoded.events.len(),
            2,
            "usage-only 帧应产出 ResponseMetadata + Finish"
        );
        match &decoded.events[1] {
            StreamEvent::Finish { usage, .. } => {
                assert_eq!(usage.input_tokens, 5);
                assert_eq!(usage.output_tokens, 2);
            }
            other => panic!("应产出 Finish 事件，实际 {other:?}"),
        }
    }

    /// usage 独立末帧复用先前帧的 finish_reason（真实流 finish 与 usage 分离）。
    #[test]
    fn usage_only_frame_reuses_finish_reason() {
        let finish_chunk = json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }]
        });
        let usage_chunk = json!({
            "choices": [],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7 }
        });
        let mut decoder = StreamDecoder::default();
        decoder.process(&finish_chunk);
        let decoded = decoder.process(&usage_chunk);
        match decoded.events.last().expect("应产出 Finish") {
            StreamEvent::Finish { finish_reason, .. } => {
                assert_eq!(finish_reason.unified, FinishReasonUnified::ToolCalls);
                assert_eq!(finish_reason.raw.as_deref(), Some("tool_calls"));
            }
            other => panic!("应产出 Finish 事件，实际 {other:?}"),
        }
    }

    /// 工具调用跨多帧累积：首帧带 id 与 name，后续帧只带 arguments 片段。
    #[test]
    fn stream_tool_call_deltas_accumulate() {
        let first = json!({
            "choices": [{ "index": 0, "delta": { "tool_calls": [{
                "index": 0, "id": "call_1", "type": "function",
                "function": { "name": "get_weather", "arguments": "" }
            }] } }]
        });
        let second = json!({
            "choices": [{ "index": 0, "delta": { "tool_calls": [{
                "index": 0, "function": { "arguments": r#"{"city":"SF"}"# }
            }] } }]
        });

        let mut decoder = StreamDecoder::default();
        let first_events = decoder.process(&first).events;
        let second_events = decoder.process(&second).events;
        assert!(matches!(
            &first_events[0],
            StreamEvent::ToolInputStart { id, tool_name, .. }
                if id == "call_1" && tool_name == "get_weather"
        ));
        assert!(matches!(
            &second_events[0],
            StreamEvent::ToolInputDelta { id, delta, .. }
                if id == "call_1" && delta == r#"{"city":"SF"}"#
        ));
    }

    /// 直通快路径 usage 嗅探：从非流式响应顶层与流式帧顶层提取四分量，与 IR
    /// 完整路径共用 `convert_usage`，计费口径一致。
    #[test]
    fn sniff_chat_usage_extracts_four_components() {
        // 非流式响应顶层 usage（带缓存细节）。
        let resp = json!({
            "usage": {
                "prompt_tokens": 1250, "completion_tokens": 100, "total_tokens": 1350,
                "prompt_tokens_details": { "cached_tokens": 200, "cache_write_tokens": 50 }
            }
        });
        let usage = sniff_chat_usage(&resp).expect("应提取 usage");
        assert_eq!(
            usage.input_tokens, 1000,
            "input = prompt - cached - cache_write"
        );
        assert_eq!(usage.output_tokens, 100);
        assert_eq!(usage.cache_read_tokens, 200);
        assert_eq!(usage.cache_write_tokens, 50);

        // 流式独立末帧（`include_usage` 帧型：顶层 usage）。
        let frame = json!({
            "id": "chatcmpl-s", "object": "chat.completion.chunk", "model": "gpt-4o",
            "choices": [],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        });
        let usage = sniff_chat_usage(&frame).expect("流式帧应提取 usage");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 2);

        // 无 usage 字段的帧返回 None。
        let no_usage = json!({ "choices": [{ "index": 0, "delta": { "content": "hi" } }] });
        assert!(sniff_chat_usage(&no_usage).is_none());
    }

    /// 流内错误编码：chat 无协议内错误通道，以独立 `data:` 帧下发错误 JSON，
    /// 与网关兜底错误帧（`stream_error_frame`）同形状。
    #[test]
    fn stream_error_event_encodes_to_data_error_frame() {
        let mut encoder = StreamEncoder::default();
        let frames = encoder.encode(&StreamEvent::Error {
            message: "Overloaded".to_string(),
        });
        assert_eq!(frames, vec![stream_error_frame("Overloaded")]);
        assert!(frames[0].event.is_none());
        let body: Value = serde_json::from_str(&frames[0].data).expect("错误帧载荷应为 JSON");
        assert_eq!(
            body,
            json!({ "error": { "message": "Overloaded", "type": "api_error", "code": null } })
        );
    }

    /// IR 流事件编码为入站 chunk 帧。
    ///
    /// 快照锁住整条帧序列：`text-start` 不产帧、首个 delta 补 `role`、finish 帧
    /// 携带 usage，以及全程 `event` 恒为 `null`（Chat Completions 不写事件名）。
    #[test]
    fn stream_events_encode_to_chunk_frames() {
        let mut encoder = StreamEncoder::default();
        let mut frames = encoder.encode(&StreamEvent::TextStart {
            id: "0".to_string(),
            provider_options: HashMap::new(),
        });
        frames.extend(encoder.encode(&StreamEvent::TextDelta {
            id: "0".to_string(),
            delta: "Hi".to_string(),
            provider_options: HashMap::new(),
        }));
        frames.extend(encoder.encode(&StreamEvent::Finish {
            finish_reason: FinishReason {
                unified: FinishReasonUnified::Stop,
                raw: Some("stop".to_string()),
            },
            usage: Usage {
                input_tokens: 3,
                output_tokens: 2,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                raw: None,
            },
            provider_metadata: HashMap::new(),
        }));

        insta::assert_json_snapshot!(frames_to_snapshot(&frames));
    }

    /// 目标协议不支持的媒体类型（非图片）出站时丢弃并记 warning。
    #[test]
    fn non_image_media_is_dropped_with_warning() {
        use crate::core::ir::Message;
        let request = ChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentPart::Media {
                    media_type: "audio/mp3".to_string(),
                    data: MediaSource::Url {
                        url: "https://example.com/a.mp3".to_string(),
                    },
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
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
        };
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        // 非图片媒体被丢弃：user content 为空数组。
        assert_eq!(
            encoded["messages"][0]["content"].as_array().map(Vec::len),
            Some(0)
        );
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, Warning::Unsupported { feature, .. } if feature == "media")),
            "媒体丢弃应记 warning"
        );
    }

    /// 无 warning 时响应体不含 `gateway` 字段（与官方形状一致）；有 warning 时
    /// 以 `gateway.warnings` 暴露，不静默吞掉。
    #[test]
    fn warnings_surface_in_response_and_stream_start() {
        let mut response = ChatResponse {
            id: "chatcmpl-w".to_string(),
            model: "gpt-4o".to_string(),
            content: vec![ContentPart::Text {
                text: "ok".to_string(),
                provider_options: HashMap::new(),
            }],
            finish_reason: FinishReason {
                unified: FinishReasonUnified::Stop,
                raw: Some("stop".to_string()),
            },
            usage: Usage::default(),
            provider_metadata: HashMap::new(),
            warnings: Vec::new(),
        };
        assert!(
            encode_response(&response).get("gateway").is_none(),
            "无 warning 不应写 gateway 字段"
        );

        response.warnings = vec![Warning::unsupported("reasoning", "跨协议族丢弃")];
        let encoded = encode_response(&response);
        assert_eq!(encoded["gateway"]["warnings"][0]["type"], "unsupported");
        assert_eq!(encoded["gateway"]["warnings"][0]["feature"], "reasoning");

        // 流式以独立首帧下发同一份 warnings。
        let mut encoder = StreamEncoder::default();
        let frames = encoder.encode(&StreamEvent::StreamStart {
            warnings: response.warnings.clone(),
        });
        assert_eq!(frames.len(), 1, "有 warning 时 stream-start 应产出一帧");
        let chunk = frame_payload(&frames[0]);
        assert_eq!(chunk["gateway"]["warnings"][0]["feature"], "reasoning");

        // 无 warning 时 stream-start 不产出帧。
        let mut encoder = StreamEncoder::default();
        assert!(
            encoder
                .encode(&StreamEvent::StreamStart {
                    warnings: Vec::new()
                })
                .is_empty()
        );
    }

    /// 出站编码时 IR 的 top_k 无法表达：丢弃并记 warning；assistant 消息的
    /// reasoning part 回写为 `reasoning_content`（零告警），非 assistant 角色
    /// 的 reasoning part 仍丢弃并记 warning。
    #[test]
    fn unsupported_ir_features_produce_warnings() {
        let mut request = ChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![ContentPart::Reasoning {
                    text: "思考".to_string(),
                    provider_options: HashMap::new(),
                }],
                provider_options: HashMap::new(),
            }],
            stream: false,
            temperature: None,
            top_p: None,
            top_k: Some(40),
            max_tokens: None,
            n: None,
            stop: Vec::new(),
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
            response_format: None,
            tools: Vec::new(),
            tool_choice: None,
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
        };
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        assert!(
            encoded.get("top_k").is_none(),
            "Chat Completions 无 top_k 字段"
        );
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, Warning::Unsupported { feature, .. } if feature == "top_k"))
        );
        assert_eq!(
            encoded["messages"][0]["reasoning_content"],
            json!("思考"),
            "assistant reasoning part 应回写为 reasoning_content"
        );
        assert!(
            !warnings.iter().any(
                |w| matches!(w, Warning::Unsupported { feature, .. } if feature == "reasoning")
            )
        );

        // 非 assistant 角色携带 reasoning part：无法表达，丢弃并记 warning。
        request.messages[0].role = Role::User;
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        assert!(
            warnings.iter().any(
                |w| matches!(w, Warning::Unsupported { feature, .. } if feature == "reasoning")
            ),
            "非 assistant 角色的 reasoning part 丢弃应记 warning"
        );
        assert!(encoded["messages"][0].get("reasoning_content").is_none());
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
