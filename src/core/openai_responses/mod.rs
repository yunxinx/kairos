//! OpenAI Responses 协议适配器：wire ↔ IR 双向编解码。
//!
//! wire 结构体全部私有，透过 `decode_*`/`encode_*` 公共函数暴露 IR 边界，
//! wire 类型不出本模块边界。
//!
//! 映射要点：
//! - 请求侧：`input` 数组的 message/function_call/function_call_output/reasoning
//!   项映射为对应 IR 消息；顶层 `instructions` 提升为首条 System 消息；`text`/
//!   `reasoning` 面板经请求级逃生舱 `provider_options["openai"]` 无损往返。
//!   多模态 input_image/input_file 解码为 IR 媒体 part（base64 data URL / 远程
//!   URL 两种载体）。
//! - 响应侧：`output` 数组的 message/reasoning/function_call 项映射为 IR content；
//!   reasoning 的 encrypted_content 经 part 逃生舱无损回传；finish_reason 由
//!   `status`/`incomplete_details.reason` 双轨映射。
//! - 流式：事件名驱动的 SSE（`response.created`/`response.output_text.delta`/
//!   `response.completed` 等），`response.completed` 携带最终 usage。
//! - Responses 有状态特性（`store`/`previous_response_id`/`conversation`）Out of
//!   Scope：入站留存到逃生舱，出站时显式 warning 并丢弃（不静默吞掉）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::core::ir::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, FinishReasonUnified, Message,
    ReasoningEffort, Role, StreamEvent, Tool, ToolChoice, Usage, Warning,
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
    #[error("input 必须是字符串或数组")]
    InvalidInput,
    #[error("input 数组第 {index} 项不是对象")]
    InputItemNotObject { index: usize },
    #[error("input 数组第 {index} 项类型未知")]
    UnknownInputItem { index: usize },
    #[error("消息 {index} 缺少角色")]
    MissingRole { index: usize },
    #[error("消息 {index} 角色未知")]
    UnknownRole { index: usize },
    #[error("消息 {index} 缺少内容")]
    MissingContent { index: usize },
    #[error("消息 {index} 的内容部分缺少文本")]
    MissingText { index: usize },
    #[error("function_call 项 {index} 的 arguments 不是字符串")]
    ArgumentsNotString { index: usize },
    #[error("tool_choice 形状无法识别: {detail}")]
    InvalidToolChoice { detail: String },
    #[error("reasoning.effort 取值无法识别: {detail}")]
    InvalidReasoningEffort { detail: String },
    #[error("响应缺少 output")]
    MissingOutput,
}

// ---- wire 请求类型 ----

/// OpenAI Responses 出站/入站请求体（wire）。
///
/// `input` 可为字符串或数组（`Vec<Value>` 手动分派）；`store`/`previous_response_id`/
/// `conversation` 为有状态特性，Out of Scope 时入站留存、出站丢弃。
#[derive(Debug, Clone, Deserialize)]
struct WireRequest {
    model: String,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    top_p: Option<f64>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    tools: Option<Vec<WireTool>>,
    #[serde(default)]
    tool_choice: Option<Value>,
    #[serde(default)]
    text: Option<Value>,
    #[serde(default)]
    reasoning: Option<Value>,
    #[serde(default)]
    store: Option<bool>,
    #[serde(default)]
    previous_response_id: Option<String>,
    #[serde(default)]
    conversation: Option<Value>,
}

/// 函数工具定义（wire）。
#[derive(Debug, Clone, Deserialize)]
struct WireTool {
    #[serde(rename = "type")]
    tool_type: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<Value>,
}

// ---- wire 响应类型 ----

#[derive(Debug, Clone, Deserialize)]
struct WireResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    output: Vec<Value>,
    #[serde(default)]
    incomplete_details: Option<WireIncompleteDetails>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct WireIncompleteDetails {
    reason: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<WireInputTokensDetails>,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    output_tokens_details: Option<WireOutputTokensDetails>,
    #[serde(default)]
    total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
struct WireInputTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
struct WireOutputTokensDetails {
    #[serde(default)]
    reasoning_tokens: u64,
}

// ---- 流式 wire 事件 ----

/// Responses 流式事件，按 `type` 判别。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireStreamEvent {
    #[serde(rename = "response.created")]
    Created {
        #[serde(default)]
        response: Option<WireStreamResponse>,
    },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded {
        #[serde(default)]
        output_index: Option<usize>,
        #[serde(default)]
        item: Option<Value>,
    },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone {
        #[serde(default)]
        output_index: Option<usize>,
        #[serde(default)]
        item: Option<Value>,
    },
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta {
        #[serde(default)]
        item_id: Option<String>,
        #[serde(default)]
        delta: Option<String>,
    },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta {
        #[serde(default)]
        output_index: Option<usize>,
        #[serde(default)]
        delta: Option<String>,
    },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta {
        #[serde(default)]
        item_id: Option<String>,
        #[serde(default)]
        summary_index: Option<usize>,
        #[serde(default)]
        delta: Option<String>,
    },
    #[serde(rename = "response.completed")]
    Completed {
        #[serde(default)]
        response: Option<Value>,
    },
    #[serde(rename = "response.incomplete")]
    Incomplete {
        #[serde(default)]
        response: Option<Value>,
    },
    #[serde(rename = "response.failed")]
    Failed {
        #[serde(default)]
        response: Option<Value>,
    },
}

/// `response.created` 的 response 首部：id/model。
#[derive(Debug, Clone, Deserialize)]
struct WireStreamResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

// ---- 逃生舱键 ----

/// `provider_options["openai"]` 的响应签名键（reasoning encrypted_content）。
const OPENAI_PROVIDER: &str = "openai";
const REASONING_ENCRYPTED: &str = "reasoning_encrypted_content";
const ITEM_ID: &str = "item_id";
const IMAGE_DETAIL: &str = "image_detail";
const FILE_NAME: &str = "filename";
/// 有状态特性（Out of Scope）的留存键，出站时显式警告并丢弃。
const STATEFUL_STORE: &str = "store";
const STATEFUL_PREVIOUS_RESPONSE_ID: &str = "previous_response_id";
const STATEFUL_CONVERSATION: &str = "conversation";

// ---- 入站解码：wire 请求 → IR ----

/// 解码入站 Responses 请求为 IR。
pub fn decode_request(value: &Value) -> Result<ChatRequest, DecodeError> {
    let wire: WireRequest = serde_path_to_error::deserialize(value.clone()).map_err(|err| {
        DecodeError::InvalidShape {
            detail: err.to_string(),
        }
    })?;

    let mut messages = Vec::new();
    // 顶层 `instructions` 提升为首条 System 消息。
    if let Some(text) = &wire.instructions
        && !text.is_empty()
    {
        messages.push(Message {
            role: Role::System,
            content: vec![ContentPart::Text {
                text: text.clone(),
                provider_options: HashMap::new(),
            }],
            provider_options: HashMap::new(),
        });
    }
    if let Some(input) = &wire.input {
        messages.extend(decode_input(input)?);
    }

    // 请求级逃生舱：`text`/`reasoning` 面板与有状态特性（留存，出站丢弃）。
    let mut provider_options = HashMap::new();
    let mut openai = serde_json::Map::new();
    if let Some(text) = &wire.text {
        openai.insert("text".into(), text.clone());
    }
    if let Some(reasoning) = &wire.reasoning {
        openai.insert("reasoning".into(), reasoning.clone());
    }
    if let Some(store) = wire.store {
        openai.insert(STATEFUL_STORE.into(), json!(store));
    }
    if let Some(prev) = &wire.previous_response_id {
        openai.insert(STATEFUL_PREVIOUS_RESPONSE_ID.into(), json!(prev));
    }
    if let Some(conv) = &wire.conversation {
        openai.insert(STATEFUL_CONVERSATION.into(), conv.clone());
    }
    if !openai.is_empty() {
        provider_options.insert(OPENAI_PROVIDER.into(), Value::Object(openai));
    }

    Ok(ChatRequest {
        model: wire.model,
        messages,
        stream: wire.stream,
        temperature: wire.temperature,
        top_p: wire.top_p,
        // Responses 无 top_k 字段；入站解码不产出该值。
        top_k: None,
        max_tokens: wire.max_output_tokens,
        n: None,
        stop: Vec::new(),
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        response_format: None,
        tools: wire
            .tools
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| {
                // 仅函数工具映射为 IR Tool；Provider 托管工具（web_search 等）v1 不实现。
                if t.tool_type != "function" {
                    return None;
                }
                Some(Tool {
                    name: t.name.unwrap_or_default(),
                    description: t.description,
                    parameters: t.parameters,
                })
            })
            .collect(),
        tool_choice: wire
            .tool_choice
            .as_ref()
            .map(decode_tool_choice)
            .transpose()?,
        reasoning: wire
            .reasoning
            .as_ref()
            .and_then(|panel| panel.get("effort"))
            .and_then(Value::as_str)
            .map(|value| {
                ReasoningEffort::parse_effort(value).ok_or_else(|| {
                    DecodeError::InvalidReasoningEffort {
                        detail: format!("未知档位 {value:?}"),
                    }
                })
            })
            .transpose()?,
        provider_options,
    })
}

/// 解码 `input`（字符串或数组）为 IR 消息序列。
fn decode_input(input: &Value) -> Result<Vec<Message>, DecodeError> {
    match input {
        Value::String(text) => Ok(vec![Message {
            role: Role::User,
            content: vec![ContentPart::Text {
                text: text.clone(),
                provider_options: HashMap::new(),
            }],
            provider_options: HashMap::new(),
        }]),
        Value::Array(items) => {
            let mut messages = Vec::new();
            for (index, item) in items.iter().enumerate() {
                messages.extend(decode_input_item(item, index)?);
            }
            Ok(messages)
        }
        _ => Err(DecodeError::InvalidInput),
    }
}

/// 解码单个 input 项，按 `type` 分派。
fn decode_input_item(item: &Value, index: usize) -> Result<Vec<Message>, DecodeError> {
    let object = item
        .as_object()
        .ok_or(DecodeError::InputItemNotObject { index })?;
    match object.get("type").and_then(Value::as_str) {
        Some("message") => Ok(vec![decode_message_item(item, index)?]),
        Some("function_call") => Ok(vec![decode_function_call(item, index)?]),
        Some("function_call_output") => Ok(vec![decode_function_call_output(item)?]),
        Some("reasoning") => Ok(vec![decode_reasoning_item(item)?]),
        // 未实现的 input 项类型（item_reference、provider 托管工具输出等）静默跳过。
        _ => Ok(Vec::new()),
    }
}

/// 解码 message 项（system/developer/user/assistant）。
fn decode_message_item(item: &Value, index: usize) -> Result<Message, DecodeError> {
    let role = item
        .get("role")
        .and_then(Value::as_str)
        .ok_or(DecodeError::MissingRole { index })?;
    let role = match role {
        "system" | "developer" => Role::System,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return Err(DecodeError::UnknownRole { index }),
    };
    let content = item
        .get("content")
        .ok_or(DecodeError::MissingContent { index })?;
    let parts = decode_content_parts(content, index)?;
    Ok(Message {
        role,
        content: parts,
        provider_options: HashMap::new(),
    })
}

/// 解码消息 content（字符串或 part 数组）为 IR parts。
///
/// 文本 part（`input_text`/`output_text`/`text`）映射为 Text；多模态 part
/// （`input_image`/`image`/`input_file`/`file`）映射为 IR 媒体 part，携带真实
/// 数据源（base64 或 URL）。跨协议族不支持的媒体类型在出站时记 warning。
fn decode_content_parts(content: &Value, index: usize) -> Result<Vec<ContentPart>, DecodeError> {
    match content {
        Value::String(text) => Ok(vec![ContentPart::Text {
            text: text.clone(),
            provider_options: HashMap::new(),
        }]),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for part in parts {
                let part_type = part.get("type").and_then(Value::as_str).unwrap_or_default();
                let text = part.get("text").and_then(Value::as_str);
                match part_type {
                    "input_text" | "output_text" | "text" => {
                        let text = text
                            .map(str::to_string)
                            .ok_or(DecodeError::MissingText { index })?;
                        out.push(ContentPart::Text {
                            text,
                            provider_options: HashMap::new(),
                        });
                    }
                    // 多模态 part：input_image/image → 图片，input_file/file → 文件。
                    // base64 data URL 拆为 Data，远程 URL 为 Url；filename/detail
                    // 等 OpenResponses 特有字段经逃生舱保留。
                    "input_image" | "image" | "input_file" | "file" => {
                        out.push(decode_media_part(part, index)?);
                    }
                    // 音频 part：Responses 出站无一等音频 part（出站按 input_file
                    // 编码），入站仍需承载，否则下游音频输入被静默丢弃。
                    "input_audio" | "audio" => {
                        out.push(decode_audio_part(part, index)?);
                    }
                    _ => {}
                }
            }
            Ok(out)
        }
        _ => Err(DecodeError::MissingContent { index }),
    }
}

/// 解码单个多模态 part 为 IR 媒体 part。
///
/// `image_url`/`file_url` → `MediaSource::Url`；`file_data` → `MediaSource::Data`
/// （data URL 拆出 media_type + base64）。`file_id` 为 provider 托管引用，网关不
/// 承载，以空 `MediaSource::Data` 占位跨协议族丢弃时记 warning。
fn decode_media_part(part: &Value, index: usize) -> Result<ContentPart, DecodeError> {
    let part_type = part.get("type").and_then(Value::as_str).unwrap_or_default();
    let image = part_type == "input_image" || part_type == "image";
    let mut provider_options = HashMap::new();
    let mut openai = serde_json::Map::new();

    // 图片 `detail` 档位与文件 `filename` 经逃生舱保留（跨协议转换不静默丢失）。
    if let Some(detail) = part.get("detail").and_then(Value::as_str) {
        openai.insert(IMAGE_DETAIL.into(), json!(detail));
    }
    if let Some(filename) = part.get("filename").and_then(Value::as_str) {
        openai.insert(FILE_NAME.into(), json!(filename));
    }

    let (media_type, data) = if let Some(url) = part.get("image_url").and_then(Value::as_str) {
        if let Some((media_type, base64)) = crate::core::ir::split_data_url(url) {
            (media_type, crate::core::ir::MediaSource::Data { base64 })
        } else {
            // 远程图片 URL 隐含图片：以顶层 `image` 兜底。
            (
                "image".to_string(),
                crate::core::ir::MediaSource::Url {
                    url: url.to_string(),
                },
            )
        }
    } else if let Some(url) = part.get("file_url").and_then(Value::as_str) {
        if let Some((media_type, base64)) = crate::core::ir::split_data_url(url) {
            (media_type, crate::core::ir::MediaSource::Data { base64 })
        } else {
            (
                "file".to_string(),
                crate::core::ir::MediaSource::Url {
                    url: url.to_string(),
                },
            )
        }
    } else if let Some(file_data) = part.get("file_data").and_then(Value::as_str) {
        if let Some((media_type, base64)) = crate::core::ir::split_data_url(file_data) {
            (media_type, crate::core::ir::MediaSource::Data { base64 })
        } else {
            // 无 data URL 标记的 file_data（OpenAI 规范要求 data URL，容忍畸形输入）。
            (
                "file".to_string(),
                crate::core::ir::MediaSource::Data {
                    base64: file_data.to_string(),
                },
            )
        }
    } else if let Some(file_id) = part.get("file_id").and_then(Value::as_str) {
        // provider 托管引用：以空 Data 占位，逃生舱保留 file_id。
        openai.insert("file_id".into(), json!(file_id));
        (
            if image {
                "image".to_string()
            } else {
                "file".to_string()
            },
            crate::core::ir::MediaSource::Data {
                base64: String::new(),
            },
        )
    } else {
        return Err(DecodeError::UnknownInputItem { index });
    };

    if !openai.is_empty() {
        provider_options.insert(OPENAI_PROVIDER.into(), Value::Object(openai));
    }
    Ok(ContentPart::Media {
        media_type,
        data,
        provider_options,
    })
}

/// 解码音频 part 为 IR 媒体 part。
///
/// 载荷形状对齐 OpenAI chat 的 `input_audio`：`{ data, format }`（嵌套于同名子
/// 对象或平铺均容忍）；`data` 为 base64 字节，`format` 缺省兜底 `wav`。出站
/// 无对应 part 类型时按 `input_file` 编码（见 `encode_media_part`）。
fn decode_audio_part(part: &Value, index: usize) -> Result<ContentPart, DecodeError> {
    let nested = part.get("input_audio").unwrap_or(part);
    let base64 = nested
        .get("data")
        .and_then(Value::as_str)
        .ok_or(DecodeError::UnknownInputItem { index })?;
    let format = nested
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("wav");
    Ok(ContentPart::Media {
        media_type: format!("audio/{format}"),
        data: crate::core::ir::MediaSource::Data {
            base64: base64.to_string(),
        },
        provider_options: HashMap::new(),
    })
}

/// 解码 function_call 项为 Assistant 消息（含 ToolCall part）。
fn decode_function_call(item: &Value, index: usize) -> Result<Message, DecodeError> {
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .ok_or(DecodeError::ArgumentsNotString { index })?;
    let input = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
    Ok(Message {
        role: Role::Assistant,
        content: vec![ContentPart::ToolCall {
            tool_call_id: call_id,
            tool_name: name,
            input,
            provider_options: HashMap::new(),
        }],
        provider_options: HashMap::new(),
    })
}

/// 解码 function_call_output 项为 Tool 消息（含 ToolResult part）。
fn decode_function_call_output(item: &Value) -> Result<Message, DecodeError> {
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // Responses 的 output 为字符串；IR 的 ToolResult.output 为任意 JSON 值。
    let output = item.get("output").cloned().unwrap_or(Value::Null);
    Ok(Message {
        role: Role::Tool,
        content: vec![ContentPart::ToolResult {
            tool_call_id: call_id,
            tool_name: String::new(),
            output,
            provider_options: HashMap::new(),
        }],
        provider_options: HashMap::new(),
    })
}

/// 解码 reasoning 项为 Assistant 消息（含 Reasoning part，encrypted_content 逃生舱）。
fn decode_reasoning_item(item: &Value) -> Result<Message, DecodeError> {
    let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
    let encrypted = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // summary 文本聚合；无 summary 时以空文本占位。
    let text: String = item
        .get("summary")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let mut openai = serde_json::Map::new();
    if !item_id.is_empty() {
        openai.insert(ITEM_ID.into(), json!(item_id));
    }
    if !encrypted.is_empty() {
        openai.insert(REASONING_ENCRYPTED.into(), json!(encrypted));
    }
    let provider_options = if openai.is_empty() {
        HashMap::new()
    } else {
        [(OPENAI_PROVIDER.to_string(), Value::Object(openai))]
            .into_iter()
            .collect()
    };
    Ok(Message {
        role: Role::Assistant,
        content: vec![ContentPart::Reasoning {
            text,
            provider_options,
        }],
        provider_options: HashMap::new(),
    })
}

// ---- 出站编码：IR → wire 请求 ----

/// 编码 IR 请求为出站 Responses 请求体。
///
/// System 消息提升为顶层 `instructions`；`provider_options["openai"]` 的
/// `text`/`reasoning` 面板原样回传（经 IR 路径不丢失）；有状态特性（store/
/// previous_response_id/conversation）显式警告并丢弃。目标协议无法表达的内容
/// 追加到 `warnings`。
pub fn encode_request(request: &ChatRequest, warnings: &mut Vec<Warning>) -> Value {
    // Responses 无以下采样与输出控制参数：显式丢弃并记 warning（不静默吞掉）。
    if request.top_k.is_some() {
        warnings.push(Warning::unsupported(
            "top_k",
            "OpenAI Responses 无 top_k 参数，已丢弃",
        ));
    }
    if request.n.is_some() {
        warnings.push(Warning::unsupported(
            "n",
            "OpenAI Responses 无 n 参数，已丢弃",
        ));
    }
    if request.seed.is_some() {
        warnings.push(Warning::unsupported(
            "seed",
            "OpenAI Responses 无 seed 参数，已丢弃",
        ));
    }
    if !request.stop.is_empty() {
        warnings.push(Warning::unsupported(
            "stop",
            "OpenAI Responses 无 stop 序列参数，已丢弃",
        ));
    }
    if request.presence_penalty.is_some() {
        warnings.push(Warning::unsupported(
            "presence_penalty",
            "OpenAI Responses 无 presence_penalty 参数，已丢弃",
        ));
    }
    if request.frequency_penalty.is_some() {
        warnings.push(Warning::unsupported(
            "frequency_penalty",
            "OpenAI Responses 无 frequency_penalty 参数，已丢弃",
        ));
    }
    if request.response_format.is_some() {
        warnings.push(Warning::unsupported(
            "response_format",
            "OpenAI Responses 无 response_format 字段（JSON 输出需以 text.format 表达），已丢弃",
        ));
    }

    // System 消息聚合为 instructions；非文本 part 丢弃并记 warning。
    for message in request.messages.iter().filter(|m| m.role == Role::System) {
        for part in &message.content {
            match part {
                ContentPart::Media { media_type, .. } => {
                    warnings.push(Warning::unsupported(
                        "media",
                        format!("OpenAI Responses 系统消息不支持媒体内容（{media_type}），已丢弃"),
                    ));
                }
                ContentPart::Custom { kind, .. } => {
                    warnings.push(Warning::unsupported(
                        "custom",
                        format!("OpenAI Responses 不支持 {kind} 内容块，已丢弃"),
                    ));
                }
                _ => {}
            }
        }
    }
    let system_text: String = request
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .filter_map(|m| text_parts(&m.content))
        .collect::<Vec<_>>()
        .join("\n");

    let mut input = Vec::new();
    for message in request.messages.iter().filter(|m| m.role != Role::System) {
        match message.role {
            Role::User => input.extend(encode_user_item(message, warnings)),
            Role::Assistant => input.extend(encode_assistant_items(message, warnings)),
            Role::Tool => input.extend(encode_tool_items(message, warnings)),
            Role::System => unreachable!("已过滤 System 消息"),
        }
    }

    let mut obj = serde_json::Map::new();
    obj.insert("model".into(), json!(request.model));
    if !system_text.is_empty() {
        obj.insert("instructions".into(), json!(system_text));
    }
    obj.insert("input".into(), Value::Array(input));
    if request.stream {
        obj.insert("stream".into(), Value::Bool(true));
    }
    if let Some(v) = request.temperature {
        obj.insert("temperature".into(), json!(v));
    }
    if let Some(v) = request.top_p {
        obj.insert("top_p".into(), json!(v));
    }
    if let Some(v) = request.max_tokens {
        obj.insert("max_output_tokens".into(), json!(v));
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
                        tool.insert("name".into(), json!(t.name));
                        if let Some(d) = &t.description {
                            tool.insert("description".into(), json!(d));
                        }
                        if let Some(p) = &t.parameters {
                            tool.insert("parameters".into(), p.clone());
                        }
                        Value::Object(tool)
                    })
                    .collect(),
            ),
        );
    }
    if let Some(choice) = &request.tool_choice {
        obj.insert("tool_choice".into(), encode_tool_choice(choice));
    }

    // 请求级逃生舱回传；有状态特性出站丢弃并显式 warning。
    let mut reasoning_emitted = false;
    if let Some(openai) = request.provider_options.get(OPENAI_PROVIDER) {
        if let Some(text) = openai.get("text") {
            obj.insert("text".into(), text.clone());
        }
        if let Some(reasoning) = openai.get("reasoning") {
            obj.insert("reasoning".into(), reasoning.clone());
            reasoning_emitted = true;
        }
        for (key, label) in [
            (STATEFUL_STORE, "store"),
            (STATEFUL_PREVIOUS_RESPONSE_ID, "previous_response_id"),
            (STATEFUL_CONVERSATION, "conversation"),
        ] {
            if openai.get(key).is_some() {
                warnings.push(Warning::unsupported(
                    label,
                    "Responses 有状态会话特性（网关侧会话存储）Out of Scope，已忽略",
                ));
            }
        }
    }
    // 面板逃生舱缺席时以类型化 effort 兜底，保持「旋钮跨请求不丢失」。
    if !reasoning_emitted && let Some(effort) = request.reasoning {
        obj.insert("reasoning".into(), json!({ "effort": effort.as_str() }));
    }
    Value::Object(obj)
}

/// 解码 wire `tool_choice` 为 IR 类型化枚举（Responses 的工具选择为扁平
/// `{"type":"function","name"}` 形状）。已知形状之外直接拒绝，避免跨协议
/// 转换时静默降级为上游 400。
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
            let name = map.get("name").and_then(Value::as_str).unwrap_or_default();
            if name.is_empty() {
                return Err(DecodeError::InvalidToolChoice {
                    detail: "type=function 缺少 name".to_string(),
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

/// 编码 IR tool_choice 为 Responses wire 值。
fn encode_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool { name } => json!({ "type": "function", "name": name }),
    }
}

/// 编码 user 消息为 message 项；媒体 part 映射为 `input_image`/`input_file`。
///
/// 图片（顶层段 `image`）→ `input_image`（base64 data URL / 远程 URL，detail
/// 逃生舱写回）；其他媒体与音频 → `input_file`（file_data data URL / file_url，
/// filename 逃生舱写回）。目标协议不支持的媒体类型丢弃并记 warning。
fn encode_user_item(message: &Message, warnings: &mut Vec<Warning>) -> Vec<Value> {
    let mut parts = Vec::new();
    for part in &message.content {
        match part {
            ContentPart::Text { text, .. } => {
                parts.push(json!({ "type": "input_text", "text": text }));
            }
            ContentPart::Media {
                media_type,
                data,
                provider_options,
            } => {
                if let Some(part) = encode_media_part(media_type, data, provider_options) {
                    parts.push(part);
                }
            }
            ContentPart::Custom { kind, .. } => {
                warnings.push(Warning::unsupported(
                    "custom",
                    format!("OpenAI Responses 不支持 {kind} 内容块，已丢弃"),
                ));
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        return Vec::new();
    }
    vec![json!({ "type": "message", "role": "user", "content": Value::Array(parts) })]
}

/// 编码单个 IR 媒体 part 为 Responses `input_image`/`input_file` part。
///
/// 顶层段 `image` → `input_image`（base64 数据源拼 data URL，URL 数据源原样）；
/// 其余媒体（含音频）→ `input_file`（base64 → file_data，URL → file_url）。图片
/// `detail` 与文件 `filename` 从逃生舱写回。`file_id` 引用类 provider 托管形态
/// 经逃生舱回传。
fn encode_media_part(
    media_type: &str,
    data: &crate::core::ir::MediaSource,
    provider_options: &crate::core::ir::ProviderOptions,
) -> Option<Value> {
    let openai = provider_options.get(OPENAI_PROVIDER);
    // provider 托管引用（file_id）经逃生舱回传（同协议族无损）。
    if let Some(file_id) = openai
        .and_then(|o| o.get("file_id"))
        .and_then(Value::as_str)
    {
        let top_level = crate::core::ir::top_level_media_type(media_type);
        let part_type = if top_level == "image" {
            "input_image"
        } else {
            "input_file"
        };
        let mut part = serde_json::Map::new();
        part.insert("type".into(), json!(part_type));
        part.insert("file_id".into(), json!(file_id));
        return Some(Value::Object(part));
    }

    let is_image = crate::core::ir::top_level_media_type(media_type) == "image";
    let part_type = if is_image {
        "input_image"
    } else {
        "input_file"
    };

    let mut part = serde_json::Map::new();
    part.insert("type".into(), json!(part_type));
    match data {
        crate::core::ir::MediaSource::Data { base64 } => {
            if is_image {
                part.insert(
                    "image_url".into(),
                    json!(format!("data:{media_type};base64,{base64}")),
                );
                if let Some(detail) = openai.and_then(|o| o.get(IMAGE_DETAIL)) {
                    part.insert("detail".into(), detail.clone());
                }
            } else {
                let filename = openai
                    .and_then(|o| o.get(FILE_NAME))
                    .and_then(Value::as_str)
                    .unwrap_or("data");
                part.insert("filename".into(), json!(filename));
                part.insert(
                    "file_data".into(),
                    json!(format!("data:{media_type};base64,{base64}")),
                );
            }
        }
        crate::core::ir::MediaSource::Url { url } => {
            if is_image {
                part.insert("image_url".into(), json!(url));
                if let Some(detail) = openai.and_then(|o| o.get(IMAGE_DETAIL)) {
                    part.insert("detail".into(), detail.clone());
                }
            } else {
                part.insert("file_url".into(), json!(url));
            }
        }
    }
    Some(Value::Object(part))
}

/// 编码 assistant 消息为 message/function_call/reasoning 项。
///
/// 文本 part 聚合为 message 项，tool-call part 各自为 function_call 项，
/// reasoning part 为 reasoning 项（encrypted_content 逃生舱回传）。
fn encode_assistant_items(message: &Message, warnings: &mut Vec<Warning>) -> Vec<Value> {
    let mut items = Vec::new();
    let text = text_parts(&message.content);
    if let Some(text) = text
        && !text.is_empty()
    {
        items.push(json!({
            "type": "message",
            "role": "assistant",
            "content": [ { "type": "output_text", "text": text, "annotations": [] } ],
        }));
    }
    for part in &message.content {
        match part {
            ContentPart::ToolCall {
                tool_call_id,
                tool_name,
                input,
                ..
            } => {
                items.push(json!({
                    "type": "function_call",
                    "call_id": tool_call_id,
                    "name": tool_name,
                    "arguments": input.to_string(),
                }));
            }
            ContentPart::Reasoning {
                text,
                provider_options,
            } => {
                let openai = provider_options.get(OPENAI_PROVIDER);
                let item_id = openai
                    .and_then(|o| o.get(ITEM_ID))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let encrypted = openai
                    .and_then(|o| o.get(REASONING_ENCRYPTED))
                    .and_then(Value::as_str);
                let mut item = serde_json::Map::new();
                item.insert("type".into(), json!("reasoning"));
                if !item_id.is_empty() {
                    item.insert("id".into(), json!(item_id));
                }
                match encrypted {
                    Some(enc) => item.insert("encrypted_content".into(), json!(enc)),
                    None => item.insert("encrypted_content".into(), Value::Null),
                };
                item.insert(
                    "summary".into(),
                    json!([{ "type": "summary_text", "text": text }]),
                );
                items.push(Value::Object(item));
            }
            ContentPart::Media { media_type, .. } => {
                warnings.push(Warning::unsupported(
                    "media",
                    format!("OpenAI Responses 助手消息不支持媒体内容（{media_type}），已丢弃"),
                ));
            }
            ContentPart::Custom { kind, .. } => {
                warnings.push(Warning::unsupported(
                    "custom",
                    format!("OpenAI Responses 不支持 {kind} 内容块，已丢弃"),
                ));
            }
            _ => {}
        }
    }
    items
}

/// 编码 tool 消息为 function_call_output 项（每条 ToolResult 一项）。
fn encode_tool_items(message: &Message, warnings: &mut Vec<Warning>) -> Vec<Value> {
    let mut items = Vec::new();
    for part in &message.content {
        match part {
            ContentPart::ToolResult {
                tool_call_id,
                output,
                ..
            } => {
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": output,
                }));
            }
            _ => {
                warnings.push(Warning::unsupported(
                    "tool_result",
                    "tool 消息含非 ToolResult part，已丢弃",
                ));
            }
        }
    }
    items
}

/// 把 openai 逃生舱字段并入 part 的 `provider_options["openai"]`。
///
/// message 项 id 等响应侧元数据写入 Text part 逃生舱，同协议族出站时回传；
/// 跨协议族转换时该逃生舱随响应丢弃（openai_chat/anthropic 出站忽略）。
fn content_options(part: &mut ContentPart, openai: Value) {
    if let ContentPart::Text {
        provider_options, ..
    } = part
    {
        let existing = provider_options
            .get_mut(OPENAI_PROVIDER)
            .and_then(Value::as_object_mut);
        match existing {
            Some(map) => {
                if let Value::Object(new) = openai {
                    map.extend(new);
                }
            }
            None => {
                provider_options.insert(OPENAI_PROVIDER.to_string(), openai);
            }
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

/// 解码上游 Responses 响应为 IR。
pub fn decode_response(value: &Value) -> Result<ChatResponse, DecodeError> {
    let wire: WireResponse = serde_path_to_error::deserialize(value.clone()).map_err(|err| {
        DecodeError::InvalidShape {
            detail: err.to_string(),
        }
    })?;

    let mut content = Vec::new();
    let mut has_function_call = false;
    for item in &wire.output {
        let object = item
            .as_object()
            .ok_or(DecodeError::InputItemNotObject { index: 0 })?;
        match object.get("type").and_then(Value::as_str) {
            Some("message") => {
                let item_id = object.get("id").and_then(Value::as_str).unwrap_or_default();
                let parts = decode_output_content(item.get("content"))
                    .map_err(|_| DecodeError::MissingContent { index: 0 })?;
                for mut part in parts {
                    // message 项 id 写入 part 逃生舱，同协议族出站时回传。
                    if !item_id.is_empty() {
                        let mut openai = serde_json::Map::new();
                        openai.insert(ITEM_ID.into(), json!(item_id));
                        content_options(&mut part, Value::Object(openai));
                    }
                    content.push(part);
                }
            }
            Some("reasoning") => {
                content.push(decode_output_reasoning(item)?);
            }
            Some("function_call") => {
                has_function_call = true;
                content.push(decode_output_function_call(item)?);
            }
            // Provider 托管工具项（web_search_call 等）与未实现项 v1 不产出。
            _ => {}
        }
    }

    let (unified, raw) = map_finish_reason(
        wire.incomplete_details.as_ref().map(|d| d.reason.as_str()),
        has_function_call,
        wire.status.as_deref(),
    );
    let usage = wire.usage.map(convert_usage).unwrap_or_default();
    let mut provider_metadata = HashMap::new();
    if let Some(id) = &wire.id {
        provider_metadata.insert(OPENAI_PROVIDER.to_string(), json!({ "response_id": id }));
    }

    Ok(ChatResponse {
        id: wire.id.unwrap_or_default(),
        model: wire.model.unwrap_or_default(),
        content,
        finish_reason: FinishReason { unified, raw },
        usage,
        provider_metadata,
        warnings: Vec::new(),
    })
}

/// 解码 message 项的 content 数组为 IR parts（仅 output_text → Text）。
fn decode_output_content(content: Option<&Value>) -> Result<Vec<ContentPart>, ()> {
    let mut parts = Vec::new();
    let Some(content) = content else {
        return Ok(parts);
    };
    let Value::Array(items) = content else {
        return Err(());
    };
    for item in items {
        let content_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        if content_type == "output_text" {
            let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
            parts.push(ContentPart::Text {
                text: text.to_string(),
                provider_options: HashMap::new(),
            });
        }
    }
    Ok(parts)
}

/// 解码 reasoning 项为 Reasoning part（encrypted_content 逃生舱）。
fn decode_output_reasoning(item: &Value) -> Result<ContentPart, DecodeError> {
    let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
    let encrypted = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text: String = item
        .get("summary")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let mut openai = serde_json::Map::new();
    if !item_id.is_empty() {
        openai.insert(ITEM_ID.into(), json!(item_id));
    }
    if !encrypted.is_empty() {
        openai.insert(REASONING_ENCRYPTED.into(), json!(encrypted));
    }
    let provider_options = if openai.is_empty() {
        HashMap::new()
    } else {
        [(OPENAI_PROVIDER.to_string(), Value::Object(openai))]
            .into_iter()
            .collect()
    };
    Ok(ContentPart::Reasoning {
        text,
        provider_options,
    })
}

/// 解码 function_call 项为 ToolCall part。
fn decode_output_function_call(item: &Value) -> Result<ContentPart, DecodeError> {
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .ok_or(DecodeError::ArgumentsNotString { index: 0 })?;
    let input = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
    Ok(ContentPart::ToolCall {
        tool_call_id: call_id,
        tool_name: name,
        input,
        provider_options: HashMap::new(),
    })
}

/// 直通快路径的 usage 嗅探：从任意 JSON 值提取 Responses usage 折算为 IR 四分量。
///
/// Responses 的 usage 分布：非流式在顶层 `usage`；流式在 `response.completed`/
/// `response.incomplete`/`response.failed` 的 `response.usage`。input 侧为「input
/// 不含缓存、缓存单独计」的减法约定（与 OpenAI Chat 口径一致）。
pub fn sniff_usage(value: &Value) -> Option<Usage> {
    // 非流式响应顶层 usage。
    if let Some(usage) = value.get("usage").and_then(Value::as_object) {
        return parse_usage_object(usage);
    }
    // 流式终端事件（response.completed 等）的 response.usage。
    let terminal_type = value.get("type").and_then(Value::as_str).is_some_and(|t| {
        t == "response.completed" || t == "response.incomplete" || t == "response.failed"
    });
    if terminal_type
        && let Some(usage) = value
            .get("response")
            .and_then(|r| r.get("usage"))
            .and_then(Value::as_object)
    {
        return parse_usage_object(usage);
    }
    None
}

/// 从 usage 对象解析 IR 四分量（input 不含缓存的减法约定）。
fn parse_usage_object(usage: &serde_json::Map<String, Value>) -> Option<Usage> {
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = usage
        .get("input_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = usage
        .get("input_tokens_details")
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(Usage {
        input_tokens: input.saturating_sub(cached).saturating_sub(cache_write),
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_tokens: cached,
        cache_write_tokens: cache_write,
        raw: Some(Value::Object(usage.clone())),
    })
}

/// usage 四分量折算：input 侧为减法约定（input 不含缓存）。
fn convert_usage(wire: WireUsage) -> Usage {
    let raw = serde_json::to_value(&wire).ok();
    let cached = wire
        .input_tokens_details
        .as_ref()
        .map(|d| d.cached_tokens)
        .unwrap_or(0);
    let cache_write = wire
        .input_tokens_details
        .as_ref()
        .map(|d| d.cache_write_tokens)
        .unwrap_or(0);
    Usage {
        input_tokens: wire
            .input_tokens
            .saturating_sub(cached)
            .saturating_sub(cache_write),
        output_tokens: wire.output_tokens,
        cache_read_tokens: cached,
        cache_write_tokens: cache_write,
        raw,
    }
}

/// unified finish reason 映射，对齐 mapOpenAIResponseFinishReason。
///
/// Responses 无顶层 finish_reason 字段：由 `incomplete_details.reason` 双轨映射，
/// 无 reason 时按是否含 function_call 判定（tool-calls / stop）。
fn map_finish_reason(
    reason: Option<&str>,
    has_function_call: bool,
    status: Option<&str>,
) -> (FinishReasonUnified, Option<String>) {
    match reason {
        Some("max_output_tokens") => (
            FinishReasonUnified::Length,
            Some("max_output_tokens".into()),
        ),
        Some("content_filter") => (
            FinishReasonUnified::ContentFilter,
            Some("content_filter".into()),
        ),
        Some(other) => {
            if has_function_call {
                (FinishReasonUnified::ToolCalls, Some(other.into()))
            } else {
                (FinishReasonUnified::Other, Some(other.into()))
            }
        }
        None => {
            let unified = if has_function_call {
                FinishReasonUnified::ToolCalls
            } else if status == Some("failed") {
                FinishReasonUnified::Error
            } else {
                FinishReasonUnified::Stop
            };
            (unified, None)
        }
    }
}

/// 把 IR unified finish reason 映射为 Responses `status` 与 `incomplete_details`。
///
/// 跨协议族转换时 `finish_reason.raw` 是出站协议的值，不能透传；统一从 `unified`
/// 映射，保证跨协议族语义正确。
fn encode_status(finish_reason: &FinishReason) -> (&'static str, Option<&'static str>) {
    match finish_reason.unified {
        FinishReasonUnified::Length => ("incomplete", Some("max_output_tokens")),
        FinishReasonUnified::ContentFilter => ("incomplete", Some("content_filter")),
        FinishReasonUnified::Error => ("failed", None),
        _ => ("completed", None),
    }
}

// ---- 入站响应编码：IR → wire ----

/// 编码 IR 响应为入站 Responses 响应体。
///
/// 转换过程的 warnings（跨协议族丢弃的 reasoning 等）以顶层 `gateway.warnings`
/// 暴露给下游；无 warning 时不写，响应与官方形状一致。响应 content 中无法表达的
/// part（file/custom）丢弃并记 warning。
pub fn encode_response(response: &ChatResponse) -> Value {
    let mut warnings = response.warnings.clone();
    let mut output = Vec::new();
    for part in &response.content {
        match part {
            ContentPart::Text {
                text,
                provider_options,
            } => {
                let item_id = provider_options
                    .get(OPENAI_PROVIDER)
                    .and_then(|o| o.get(ITEM_ID))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mut item = serde_json::Map::new();
                if !item_id.is_empty() {
                    item.insert("id".into(), json!(item_id));
                }
                item.insert("type".into(), json!("message"));
                item.insert("role".into(), json!("assistant"));
                item.insert(
                    "content".into(),
                    json!([{ "type": "output_text", "text": text, "annotations": [] }]),
                );
                output.push(Value::Object(item));
            }
            ContentPart::Reasoning {
                text,
                provider_options,
            } => {
                let openai = provider_options.get(OPENAI_PROVIDER);
                let item_id = openai
                    .and_then(|o| o.get(ITEM_ID))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let encrypted = openai
                    .and_then(|o| o.get(REASONING_ENCRYPTED))
                    .and_then(Value::as_str);
                let mut item = serde_json::Map::new();
                if !item_id.is_empty() {
                    item.insert("id".into(), json!(item_id));
                }
                item.insert("type".into(), json!("reasoning"));
                match encrypted {
                    Some(enc) => item.insert("encrypted_content".into(), json!(enc)),
                    None => item.insert("encrypted_content".into(), Value::Null),
                };
                item.insert(
                    "summary".into(),
                    json!([{ "type": "summary_text", "text": text }]),
                );
                output.push(Value::Object(item));
            }
            ContentPart::ToolCall {
                tool_call_id,
                tool_name,
                input,
                ..
            } => {
                output.push(json!({
                    "type": "function_call",
                    "call_id": tool_call_id,
                    "name": tool_name,
                    "arguments": input.to_string(),
                }));
            }
            ContentPart::ToolResult { .. } => {}
            ContentPart::Media { media_type, .. } => {
                warnings.push(Warning::unsupported(
                    "media",
                    format!("OpenAI Responses 响应输出不支持媒体内容（{media_type}），已丢弃"),
                ));
            }
            ContentPart::Custom { kind, .. } => {
                warnings.push(Warning::unsupported(
                    "custom",
                    format!("OpenAI Responses 不支持 {kind} 内容块，已丢弃"),
                ));
            }
        }
    }

    let (status, incomplete) = encode_status(&response.finish_reason);
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(response.id));
    obj.insert("object".into(), json!("response"));
    obj.insert("status".into(), json!(status));
    obj.insert("model".into(), json!(response.model));
    obj.insert("output".into(), Value::Array(output));
    if let Some(reason) = incomplete {
        obj.insert("incomplete_details".into(), json!({ "reason": reason }));
    }
    obj.insert("usage".into(), encode_usage(&response.usage));
    if let Some(gateway) = crate::core::openai_chat::encode_warnings(&warnings) {
        obj.insert("gateway".into(), gateway);
    }
    Value::Object(obj)
}

/// 编码 IR usage 四分量 + 缓存细节为 wire usage 对象。
///
/// Responses 的 input_tokens 含命中缓存（与 OpenAI Chat 相似，但与 Anthropic 的
/// 加法约定不同）；编码时把 cache 分量加回 input，缓存单独计。
fn encode_usage(usage: &Usage) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "input_tokens".into(),
        json!(usage.input_tokens + usage.cache_read_tokens + usage.cache_write_tokens),
    );
    // 缓存细节仅在非零时写出，避免污染无缓存的响应形状。
    if usage.cache_read_tokens > 0 || usage.cache_write_tokens > 0 {
        let mut input_details = serde_json::Map::new();
        input_details.insert("cached_tokens".into(), json!(usage.cache_read_tokens));
        if usage.cache_write_tokens > 0 {
            input_details.insert("cache_write_tokens".into(), json!(usage.cache_write_tokens));
        }
        obj.insert("input_tokens_details".into(), Value::Object(input_details));
    }
    obj.insert("output_tokens".into(), json!(usage.output_tokens));
    obj.insert(
        "total_tokens".into(),
        json!(
            usage.input_tokens
                + usage.output_tokens
                + usage.cache_read_tokens
                + usage.cache_write_tokens
        ),
    );
    Value::Object(obj)
}

// ---- 流式：上游 chunk → IR 流事件 ----

/// 流式解码器：把上游 Responses SSE 事件解码为 IR 流事件。
///
/// `output_item.added` 开启块（message→text、function_call→tool、
/// reasoning→reasoning），`output_text.delta`/`function_call_arguments.delta`/
/// `reasoning_summary_text.delta` 产出增量，`output_item.done` 收尾（function_call
/// 在此解析出完整 arguments），`response.completed`/`incomplete` 产出 Finish。
#[derive(Default)]
pub struct StreamDecoder {
    /// 按 output_index 维护进行中的工具调用（arguments 跨帧累积）。
    tools: HashMap<usize, OpenToolCall>,
    /// 流中是否出现过 function_call（持久标记，用于 finish_reason 判定）。
    saw_function_call: bool,
}

/// 进行中的工具调用。
#[derive(Debug)]
struct OpenToolCall {
    call_id: String,
    arguments: String,
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
            WireStreamEvent::Created { response } => {
                if let Some(resp) = response
                    && let (Some(id), Some(model)) = (resp.id, resp.model)
                {
                    events.push(StreamEvent::ResponseMetadata { id, model });
                }
            }
            WireStreamEvent::OutputItemAdded { output_index, item } => {
                let Some(item) = item else {
                    return DecodeStreamChunk::delivery(Vec::new());
                };
                let Some(object) = item.as_object() else {
                    return DecodeStreamChunk::delivery(Vec::new());
                };
                match object.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        let id = object.get("id").and_then(Value::as_str).unwrap_or_default();
                        events.push(StreamEvent::TextStart {
                            id: id.to_string(),
                            provider_options: if id.is_empty() {
                                HashMap::new()
                            } else {
                                [(OPENAI_PROVIDER.to_string(), json!({ ITEM_ID: id }))]
                                    .into_iter()
                                    .collect()
                            },
                        });
                    }
                    Some("function_call") => {
                        let call_id = object
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let tool_name = object
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let index = output_index.unwrap_or(0);
                        self.tools.insert(
                            index,
                            OpenToolCall {
                                call_id: call_id.clone(),
                                arguments: String::new(),
                            },
                        );
                        events.push(StreamEvent::ToolInputStart {
                            id: call_id,
                            tool_name,
                            provider_options: HashMap::new(),
                        });
                    }
                    Some("reasoning") => {
                        let id = object.get("id").and_then(Value::as_str).unwrap_or_default();
                        let encrypted = object
                            .get("encrypted_content")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let mut openai = serde_json::Map::new();
                        if !id.is_empty() {
                            openai.insert(ITEM_ID.into(), json!(id));
                        }
                        if !encrypted.is_empty() {
                            openai.insert(REASONING_ENCRYPTED.into(), json!(encrypted));
                        }
                        events.push(StreamEvent::ReasoningStart {
                            // reasoning-start id 为 `${itemId}:${summaryIndex}`。
                            id: format!("{id}:0"),
                            provider_options: if openai.is_empty() {
                                HashMap::new()
                            } else {
                                [(OPENAI_PROVIDER.to_string(), Value::Object(openai))]
                                    .into_iter()
                                    .collect()
                            },
                        });
                    }
                    _ => {}
                }
            }
            WireStreamEvent::OutputItemDone { output_index, item } => {
                let Some(item) = item else {
                    return DecodeStreamChunk::delivery(Vec::new());
                };
                let Some(object) = item.as_object() else {
                    return DecodeStreamChunk::delivery(Vec::new());
                };
                match object.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        let id = object.get("id").and_then(Value::as_str).unwrap_or_default();
                        events.push(StreamEvent::TextEnd {
                            id: id.to_string(),
                            provider_options: HashMap::new(),
                        });
                    }
                    Some("function_call") => {
                        self.saw_function_call = true;
                        let index = output_index.unwrap_or(0);
                        if let Some(tool) = self.tools.remove(&index) {
                            // 终端事件为 ToolInputEnd：累积器把已拼接的 arguments
                            // 解析为 tool-call（与 openai_chat/anthropic 解码器一致），
                            // 避免重复产出 ToolCall part。
                            events.push(StreamEvent::ToolInputEnd {
                                id: tool.call_id,
                                provider_options: HashMap::new(),
                            });
                        }
                    }
                    Some("reasoning") => {
                        let id = object.get("id").and_then(Value::as_str).unwrap_or_default();
                        events.push(StreamEvent::ReasoningEnd {
                            id: format!("{id}:0"),
                            provider_options: HashMap::new(),
                        });
                    }
                    _ => {}
                }
            }
            WireStreamEvent::OutputTextDelta { item_id, delta, .. } => {
                if let Some(delta) = delta
                    && !delta.is_empty()
                {
                    is_output = true;
                    events.push(StreamEvent::TextDelta {
                        id: item_id.unwrap_or_default(),
                        delta,
                        provider_options: HashMap::new(),
                    });
                }
            }
            WireStreamEvent::FunctionCallArgumentsDelta {
                output_index,
                delta,
            } => {
                let Some(delta) = delta else {
                    return DecodeStreamChunk::delivery(Vec::new());
                };
                if delta.is_empty() {
                    return DecodeStreamChunk::delivery(Vec::new());
                }
                is_output = true;
                let index = output_index.unwrap_or(0);
                if let Some(tool) = self.tools.get_mut(&index) {
                    tool.arguments.push_str(&delta);
                    events.push(StreamEvent::ToolInputDelta {
                        id: tool.call_id.clone(),
                        delta,
                        provider_options: HashMap::new(),
                    });
                }
            }
            WireStreamEvent::ReasoningSummaryTextDelta {
                item_id,
                summary_index,
                delta,
                ..
            } => {
                if let Some(delta) = delta
                    && !delta.is_empty()
                {
                    is_output = true;
                    let id = item_id.unwrap_or_default();
                    let summary = summary_index.unwrap_or(0);
                    events.push(StreamEvent::ReasoningDelta {
                        id: format!("{id}:{summary}"),
                        delta,
                        provider_options: HashMap::new(),
                    });
                }
            }
            WireStreamEvent::Completed { response } | WireStreamEvent::Incomplete { response } => {
                let parsed = response.as_ref().and_then(|r| r.as_object());
                let reason = parsed
                    .and_then(|r| r.get("incomplete_details"))
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str);
                let (unified, raw) = map_finish_reason(reason, self.saw_function_call, None);
                // 复用非流式的 `convert_usage`，保证流式累积与非流式解码的 usage
                //（含 raw 兜底）口径一致。
                let usage = parsed
                    .and_then(|r| r.get("usage"))
                    .and_then(|u| serde_json::from_value::<WireUsage>(u.clone()).ok())
                    .map(convert_usage)
                    .unwrap_or_default();
                let mut provider_metadata = HashMap::new();
                if let Some(id) = parsed.and_then(|r| r.get("id")).and_then(Value::as_str) {
                    provider_metadata
                        .insert(OPENAI_PROVIDER.to_string(), json!({ "response_id": id }));
                }
                events.push(StreamEvent::Finish {
                    finish_reason: FinishReason { unified, raw },
                    usage,
                    provider_metadata,
                });
            }
            WireStreamEvent::Failed { response } => {
                let usage = response
                    .as_ref()
                    .and_then(|r| r.get("usage"))
                    .and_then(Value::as_object)
                    .and_then(parse_usage_object)
                    .unwrap_or_default();
                events.push(StreamEvent::Finish {
                    finish_reason: FinishReason {
                        unified: FinishReasonUnified::Error,
                        raw: Some("error".to_string()),
                    },
                    usage,
                    provider_metadata: HashMap::new(),
                });
            }
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

/// 把 IR 流事件编码为入站 Responses SSE 帧（带 `event:` 名）。
///
/// 维护进行中的块状态，把事件还原为 Responses 事件流：`response.created`/
/// `response.output_item.added`/`response.output_text.delta`/`response.output_text.
/// done`/`response.output_item.done`/`response.completed`。调用方负责把每帧包成
/// SSE 发送；`StreamStart` 的 warnings 随 `response.created` 的 `gateway` 字段下发。
#[derive(Default)]
pub struct StreamEncoder {
    /// 进行中的文本块 id（`output_item.added` 记录，`output_item.done` 消费）。
    text_id: Option<String>,
    /// 按 call_id 维护进行中的工具调用（arguments 跨帧累积，`output_item.done` 收尾）。
    tools: HashMap<String, OpenStreamTool>,
    /// 从 ResponseMetadata 记录的响应 id 与 model。
    id: String,
    model: String,
    /// 入站模型名覆盖：别名命中时重写响应模型名。
    inbound_model: Option<String>,
    /// `StreamStart` 转换的 warnings，随首个 `response.created` 的 `gateway` 字段下发。
    pending_warnings: Vec<Warning>,
    /// output_index 分配：每个 output item 一个递增的唯一索引。下游 SDK 按
    /// output_index 索引进行中的工具调用与活跃项，多 item 共用 0 会互相覆盖。
    next_item_index: usize,
    /// 进行中文本块的 output_index。
    text_index: Option<usize>,
    /// 进行中 reasoning 块的 output_index（按 item_id）。
    reasoning_indexes: HashMap<String, usize>,
    /// 进行中工具调用的 output_index（按 call_id）。
    tool_indexes: HashMap<String, usize>,
}

/// 进行中的工具调用（出站侧）。
#[derive(Debug)]
struct OpenStreamTool {
    tool_name: String,
    arguments: String,
}

impl StreamEncoder {
    /// 指定入站模型名覆盖（别名重写响应模型名）；`None` 表示不覆盖。
    pub fn new(inbound_model: Option<String>) -> Self {
        Self {
            inbound_model,
            ..Self::default()
        }
    }

    /// 分配下一个 output_index：每个 output item 唯一，供下游按索引定位。
    fn next_output_index(&mut self) -> usize {
        let index = self.next_item_index;
        self.next_item_index += 1;
        index
    }

    /// 编码一个 IR 流事件，返回需要下发的 SSE 帧（可能为空）。
    pub fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        match event {
            StreamEvent::StreamStart { warnings } => {
                // warnings 留存，待首个 `response.created`（含真实 id）下发出。
                self.pending_warnings.extend(warnings.clone());
                Vec::new()
            }
            StreamEvent::ResponseMetadata { id, model } => {
                self.id = id.clone();
                self.model = model.clone();
                // 首个 `response.created` 在拿到真实 id/model 后下发，避免占位 id
                // 与 `response.completed` 的真实 id 不一致。
                let mut response = serde_json::Map::new();
                response.insert("id".into(), json!(id));
                response.insert("object".into(), json!("response"));
                response.insert("status".into(), json!("in_progress"));
                let model = self.inbound_model.as_deref().unwrap_or(model);
                response.insert("model".into(), json!(model));
                if let Some(gateway) =
                    crate::core::openai_chat::encode_warnings(&self.pending_warnings)
                {
                    response.insert("gateway".into(), gateway);
                }
                self.pending_warnings.clear();
                vec![SseFrame::named(
                    "response.created",
                    json!({ "type": "response.created", "response": response }).to_string(),
                )]
            }
            StreamEvent::TextStart {
                id,
                provider_options,
            } => {
                self.text_id = Some(id.clone());
                let output_index = self.next_output_index();
                self.text_index = Some(output_index);
                let item_id = provider_options
                    .get(OPENAI_PROVIDER)
                    .and_then(|o| o.get(ITEM_ID))
                    .and_then(Value::as_str)
                    .unwrap_or(id);
                vec![
                    SseFrame::named(
                        "response.output_item.added",
                        json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": { "type": "message", "id": item_id, "phase": "final_answer" },
                        })
                        .to_string(),
                    ),
                    SseFrame::named(
                        "response.content_part.added",
                        json!({
                            "type": "response.content_part.added",
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": 0,
                            "part": { "type": "output_text", "text": "", "annotations": [] },
                        })
                        .to_string(),
                    ),
                ]
            }
            StreamEvent::TextDelta { id, delta, .. } => {
                let item_id = self.text_id.as_deref().unwrap_or(id);
                let output_index = self.text_index.unwrap_or(0);
                vec![SseFrame::named(
                    "response.output_text.delta",
                    json!({
                        "type": "response.output_text.delta",
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "delta": delta,
                    })
                    .to_string(),
                )]
            }
            StreamEvent::TextEnd { id, .. } => {
                let item_id = self.text_id.as_deref().unwrap_or(id).to_string();
                let output_index = self.text_index.take().unwrap_or(0);
                self.text_id = None;
                vec![
                    SseFrame::named(
                        "response.output_text.done",
                        json!({
                            "type": "response.output_text.done",
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": 0,
                            "text": "",
                        })
                        .to_string(),
                    ),
                    SseFrame::named(
                        "response.content_part.done",
                        json!({
                            "type": "response.content_part.done",
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": 0,
                            "part": { "type": "output_text", "text": "", "annotations": [] },
                        })
                        .to_string(),
                    ),
                    SseFrame::named(
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": output_index,
                            "item": { "type": "message", "id": item_id, "role": "assistant" },
                        })
                        .to_string(),
                    ),
                ]
            }
            StreamEvent::ReasoningStart {
                id,
                provider_options,
            } => {
                let item_id = provider_options
                    .get(OPENAI_PROVIDER)
                    .and_then(|o| o.get(ITEM_ID))
                    .and_then(Value::as_str)
                    .unwrap_or(id);
                let encrypted = provider_options
                    .get(OPENAI_PROVIDER)
                    .and_then(|o| o.get(REASONING_ENCRYPTED));
                let output_index = self.next_output_index();
                self.reasoning_indexes
                    .insert(strip_summary_suffix(id).to_string(), output_index);
                let mut item = serde_json::Map::new();
                item.insert("type".into(), json!("reasoning"));
                item.insert("id".into(), json!(item_id));
                match encrypted {
                    Some(enc) => item.insert("encrypted_content".into(), enc.clone()),
                    None => item.insert("encrypted_content".into(), Value::Null),
                };
                vec![SseFrame::named(
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": Value::Object(item),
                    })
                    .to_string(),
                )]
            }
            StreamEvent::ReasoningDelta { id, delta, .. } => {
                let item_id = strip_summary_suffix(id);
                let output_index = self.reasoning_indexes.get(item_id).copied().unwrap_or(0);
                vec![SseFrame::named(
                    "response.reasoning_summary_text.delta",
                    json!({
                        "type": "response.reasoning_summary_text.delta",
                        "item_id": item_id,
                        "output_index": output_index,
                        "summary_index": 0,
                        "delta": delta,
                    })
                    .to_string(),
                )]
            }
            StreamEvent::ReasoningEnd { id, .. } => {
                let item_id = strip_summary_suffix(id);
                let output_index = self.reasoning_indexes.remove(item_id).unwrap_or(0);
                vec![
                    SseFrame::named(
                        "response.reasoning_summary_part.done",
                        json!({
                            "type": "response.reasoning_summary_part.done",
                            "item_id": item_id,
                            "output_index": output_index,
                            "summary_index": 0,
                        })
                        .to_string(),
                    ),
                    SseFrame::named(
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": output_index,
                            "item": { "type": "reasoning", "id": item_id },
                        })
                        .to_string(),
                    ),
                ]
            }
            StreamEvent::ToolInputStart { id, tool_name, .. } => {
                let output_index = self.next_output_index();
                self.tool_indexes.insert(id.clone(), output_index);
                self.tools.insert(
                    id.clone(),
                    OpenStreamTool {
                        tool_name: tool_name.clone(),
                        arguments: String::new(),
                    },
                );
                vec![SseFrame::named(
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": {
                            "type": "function_call",
                            "id": format!("fc_{id}"),
                            "call_id": id,
                            "name": tool_name,
                            "arguments": "",
                        },
                    })
                    .to_string(),
                )]
            }
            StreamEvent::ToolInputDelta { id, delta, .. } => {
                if let Some(tool) = self.tools.get_mut(id) {
                    tool.arguments.push_str(delta);
                }
                let output_index = self.tool_indexes.get(id).copied().unwrap_or(0);
                vec![SseFrame::named(
                    "response.function_call_arguments.delta",
                    json!({
                        "type": "response.function_call_arguments.delta",
                        "item_id": format!("fc_{id}"),
                        "output_index": output_index,
                        "delta": delta,
                    })
                    .to_string(),
                )]
            }
            StreamEvent::ToolInputEnd { id, .. } => {
                // 终端事件：以累积的完整 arguments 下发 `output_item.done`，关闭
                // function_call 项（Responses 客户端在 `.done` 前不触发工具调用）。
                let Some(tool) = self.tools.remove(id) else {
                    return Vec::new();
                };
                let output_index = self.tool_indexes.remove(id).unwrap_or(0);
                vec![SseFrame::named(
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": {
                            "type": "function_call",
                            "id": format!("fc_{id}"),
                            "call_id": id,
                            "name": tool.tool_name,
                            "arguments": tool.arguments,
                        },
                    })
                    .to_string(),
                )]
            }
            StreamEvent::ToolCall {
                tool_call_id,
                tool_name,
                input,
                ..
            } => {
                // 兜底：非增量路径（如直接以完整工具调用编码）同样关闭 function_call 项。
                let output_index = self.tool_indexes.remove(tool_call_id).unwrap_or(0);
                vec![SseFrame::named(
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": {
                            "type": "function_call",
                            "id": format!("fc_{tool_call_id}"),
                            "call_id": tool_call_id,
                            "name": tool_name,
                            "arguments": input.to_string(),
                        },
                    })
                    .to_string(),
                )]
            }
            StreamEvent::Finish {
                finish_reason,
                usage,
                ..
            } => {
                // 上游解码器（如 openai_chat）可能不发 ToolInputEnd，靠累积器 flush 收尾；
                // 流结束前把仍打开的 function_call 项逐个关闭，避免下游收到未闭合工具。
                let pending: Vec<(String, OpenStreamTool)> =
                    std::mem::take(&mut self.tools).into_iter().collect();
                let mut pending_tools = Vec::new();
                for (call_id, tool) in pending {
                    let output_index = self.tool_indexes.remove(&call_id).unwrap_or(0);
                    pending_tools.push(SseFrame::named(
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": output_index,
                            "item": {
                                "type": "function_call",
                                "id": format!("fc_{call_id}"),
                                "call_id": call_id,
                                "name": tool.tool_name,
                                "arguments": tool.arguments,
                            },
                        })
                        .to_string(),
                    ));
                }
                let (status, incomplete) = encode_status(finish_reason);
                let mut response = serde_json::Map::new();
                response.insert(
                    "id".into(),
                    json!(if self.id.is_empty() {
                        "resp_stream"
                    } else {
                        self.id.as_str()
                    }),
                );
                response.insert("object".into(), json!("response"));
                response.insert("status".into(), json!(status));
                let model = self.inbound_model.as_deref().unwrap_or(&self.model);
                response.insert("model".into(), json!(model));
                response.insert("output".into(), Value::Array(Vec::new()));
                if let Some(reason) = incomplete {
                    response.insert("incomplete_details".into(), json!({ "reason": reason }));
                }
                response.insert("usage".into(), encode_usage(usage));
                let mut frames = pending_tools;
                frames.push(SseFrame::named(
                    "response.completed",
                    json!({ "type": "response.completed", "response": response }).to_string(),
                ));
                frames
            }
        }
    }
}

/// 去掉 reasoning 事件 id 的 `:summary_index` 后缀，还原 item_id。
fn strip_summary_suffix(id: &str) -> &str {
    id.split_once(':').map(|(base, _)| base).unwrap_or(id)
}

// ---- 错误编码 ----

/// 编码为 OpenAI 错误格式 `{"error":{...}}`（Responses 与 Chat Completions 共用）。
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

/// 编码为 OpenAI `GET /v1/models` 列表（Responses 与 Chat Completions 共用 Models API）。
/// `created` 未知时为 0。
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
    use crate::core::ir::FinishReasonUnified;
    use crate::core::stream::StreamAccumulator;
    use similar_asserts::assert_eq;

    /// wire 形状错误指明出错字段的 JSON 路径，而非笼统的「不是合法 JSON 对象」。
    #[test]
    fn invalid_wire_shape_reports_field_path() {
        let wire = json!({
            "model": "gpt-5",
            "input": "画一张图",
            "temperature": "hot"
        });
        match decode_request(&wire) {
            Err(DecodeError::InvalidShape { detail }) => {
                assert!(detail.contains("temperature"), "报错应含字段路径: {detail}");
            }
            other => panic!("应报 InvalidShape: {other:?}"),
        }
    }

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

    /// 多模态黄金样例请求 decode → encode 往返还原 wire，文本与媒体混排顺序不丢。
    ///
    /// 覆盖 input_image（base64 data URL + detail / 远程 URL）与 input_file
    /// （file_data data URL + filename）两种载体，6 part 混排顺序。
    #[test]
    fn multimodal_fixture_roundtrip() {
        let raw = include_str!("__fixtures__/request_multimodal.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let ir = decode_request(&wire).expect("fixture 应可解码为 IR");
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert_eq!(reencoded, wire, "往返应还原 wire 请求（含混排顺序）");
        assert!(warnings.is_empty(), "同协议图片/文件往返不应产出 warning");

        // 混排顺序：text → 图片(data URL) → text → 文件(data URL) → text → 图片(URL)。
        let parts = &ir.messages[0].content;
        assert_eq!(parts.len(), 6, "应保留 6 个 part");
        assert!(matches!(parts[0], ContentPart::Text { .. }));
        assert!(matches!(
            &parts[1],
            ContentPart::Media {
                media_type,
                data: crate::core::ir::MediaSource::Data { base64 },
                provider_options,
            } if media_type == "image/png" && base64 == "iVBORw0KGgoAAAANSUhEUg=="
                && provider_options.get("openai").and_then(|o| o.get(IMAGE_DETAIL))
                    == Some(&Value::String("low".to_string()))
        ));
        assert!(matches!(parts[2], ContentPart::Text { .. }));
        assert!(matches!(
            &parts[3],
            ContentPart::Media {
                media_type,
                data: crate::core::ir::MediaSource::Data { base64 },
                provider_options,
            } if media_type == "application/pdf" && base64 == "JVBERi0xLjQK"
                && provider_options.get("openai").and_then(|o| o.get(FILE_NAME))
                    == Some(&Value::String("doc.pdf".to_string()))
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

    /// 跨协议：音频媒体（audio/mp3）在 Responses 出站编码为 `input_file`
    ///（Responses 无 input_audio 输入 part，音频媒体映射为文件）。
    #[test]
    fn audio_media_encodes_to_input_file() {
        use crate::core::ir::Message;
        let request = ChatRequest {
            model: "gpt-4.1".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentPart::Media {
                    media_type: "audio/mp3".to_string(),
                    data: crate::core::ir::MediaSource::Data {
                        base64: "UklGRg==".to_string(),
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
        };
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        let part = &encoded["input"][0]["content"][0];
        assert_eq!(part["type"], "input_file", "音频媒体应映射为 input_file");
        assert_eq!(
            part["file_data"], "data:audio/mp3;base64,UklGRg==",
            "文件载荷应为 data URL"
        );
        assert!(warnings.is_empty(), "音频映射为 input_file 不应记 warning");
    }

    /// `input_audio` 入站解码：音频载荷解码为 `audio/<format>` 媒体 part，出站
    /// 编码为 `input_file`（Responses 无一等音频 part）；音频不被静默丢弃。
    #[test]
    fn input_audio_decodes_and_encodes_to_input_file() {
        let wire = json!({
            "model": "gpt-4.1",
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_audio",
                                      "input_audio": { "data": "UklGRg==", "format": "mp3" } }] }]
        });
        let ir = decode_request(&wire).expect("input_audio 应可解码");
        assert!(matches!(
            &ir.messages[0].content[0],
            ContentPart::Media {
                media_type,
                data: crate::core::ir::MediaSource::Data { base64 },
                ..
            } if media_type == "audio/mp3" && base64 == "UklGRg=="
        ));
        let mut warnings = Vec::new();
        let encoded = encode_request(&ir, &mut warnings);
        let part = &encoded["input"][0]["content"][0];
        assert_eq!(part["type"], "input_file", "音频应编码为 input_file");
        assert_eq!(part["file_data"], "data:audio/mp3;base64,UklGRg==");
        assert!(warnings.is_empty(), "音频映射不应记 warning");
    }

    /// `file_id` provider 托管引用经逃生舱往返：入站存逃生舱，出站回传。
    #[test]
    fn file_id_reference_roundtrips_via_provider_options() {
        let wire = json!({
            "model": "gpt-4.1",
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_image", "file_id": "file-abc123" }] }]
        });
        let ir = decode_request(&wire).expect("带 file_id 的 input_image 应可解码");
        assert!(matches!(
            &ir.messages[0].content[0],
            ContentPart::Media { provider_options, .. }
                if provider_options.get("openai").and_then(|o| o.get("file_id"))
                    == Some(&Value::String("file-abc123".to_string()))
        ));
        let mut warnings = Vec::new();
        let reencoded = encode_request(&ir, &mut warnings);
        assert_eq!(reencoded, wire, "往返应还原 file_id");
        assert!(warnings.is_empty());
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

    /// 同协议族 reasoning（encrypted_content）经逃生舱无损回传。
    #[test]
    fn reasoning_response_roundtrip_preserves_encrypted_content() {
        let raw = include_str!("__fixtures__/response_reasoning.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let ir = decode_response(&wire).expect("fixture 应可解码为 IR");
        // reasoning 的 encrypted_content 挂入 Reasoning part 逃生舱。
        let reasoning = ir
            .content
            .iter()
            .find(|p| matches!(p, ContentPart::Reasoning { .. }))
            .expect("应含 reasoning part");
        let ContentPart::Reasoning {
            provider_options, ..
        } = reasoning
        else {
            unreachable!()
        };
        assert_eq!(
            provider_options["openai"][REASONING_ENCRYPTED], "enc_abc",
            "encrypted_content 应进入逃生舱"
        );
        let reencoded = encode_response(&ir);
        assert_eq!(reencoded, wire, "同协议族 reasoning 应无损往返");
    }

    /// 黄金样例流式往返：解码流式事件 → 累积，与非流式 `response.json` 解码结果同构。
    #[test]
    fn stream_fixture_accumulates_to_response() {
        let mut decoder = StreamDecoder::default();
        let mut accumulator = StreamAccumulator::new();

        let frames = [
            include_str!("__fixtures__/stream_created.json"),
            include_str!("__fixtures__/stream_message_added.json"),
            include_str!("__fixtures__/stream_text_delta_1.json"),
            include_str!("__fixtures__/stream_text_delta_2.json"),
            include_str!("__fixtures__/stream_message_done.json"),
            include_str!("__fixtures__/stream_tool_added.json"),
            include_str!("__fixtures__/stream_tool_args.json"),
            include_str!("__fixtures__/stream_tool_done.json"),
            include_str!("__fixtures__/stream_completed.json"),
        ];
        for raw in frames {
            let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
            for event in decoder.process(&wire).events {
                accumulator.push(event);
            }
        }
        let streamed = accumulator.finish();

        // 非流式黄金样例：response.json（同一文本 + 一个 function_call + usage）。
        let raw = include_str!("__fixtures__/response.json");
        let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
        let non_stream = decode_response(&wire).expect("fixture 应可解码");

        // 同构：流式累积结果与非流式解码完全一致（text + tool-call + usage + finish_reason）。
        assert_eq!(streamed, non_stream);
    }

    /// 流式 reasoning 解码：encrypted_content 进入 Reasoning part 逃生舱。
    #[test]
    fn stream_reasoning_preserves_encrypted_content() {
        let mut decoder = StreamDecoder::default();
        let mut accumulator = StreamAccumulator::new();
        for raw in [
            include_str!("__fixtures__/stream_reasoning_added.json"),
            include_str!("__fixtures__/stream_reasoning_delta.json"),
            include_str!("__fixtures__/stream_reasoning_done.json"),
        ] {
            let wire: Value = serde_json::from_str(raw).expect("fixture 应可解析");
            for event in decoder.process(&wire).events {
                accumulator.push(event);
            }
        }
        let response = accumulator.finish();
        let reasoning = response
            .content
            .iter()
            .find(|p| matches!(p, ContentPart::Reasoning { .. }))
            .expect("应含 reasoning part");
        let ContentPart::Reasoning {
            text,
            provider_options,
        } = reasoning
        else {
            unreachable!()
        };
        assert_eq!(text, "先算 925 ÷ 5");
        assert_eq!(
            provider_options["openai"][ITEM_ID], "reason_1",
            "流式 reasoning 应携带 item_id"
        );
        assert_eq!(
            provider_options["openai"][REASONING_ENCRYPTED], "enc_stream",
            "流式 reasoning 应携带 encrypted_content"
        );
    }

    /// 直通快路径 usage 嗅探：非流式顶层与流式 response.completed 帧。
    #[test]
    fn sniff_usage_extracts_four_components() {
        // 非流式响应顶层 usage（带缓存细节）。
        let resp = json!({
            "usage": {
                "input_tokens": 1250,
                "input_tokens_details": { "cached_tokens": 200, "cache_write_tokens": 50 },
                "output_tokens": 100,
                "total_tokens": 1350,
            }
        });
        let usage = sniff_usage(&resp).expect("应提取 usage");
        assert_eq!(
            usage.input_tokens, 1000,
            "input = total - cached - cache_write"
        );
        assert_eq!(usage.output_tokens, 100);
        assert_eq!(usage.cache_read_tokens, 200);
        assert_eq!(usage.cache_write_tokens, 50);

        // 流式 response.completed 帧的 response.usage。
        let frame = json!({
            "type": "response.completed",
            "response": {
                "usage": { "input_tokens": 10, "output_tokens": 2, "total_tokens": 12 }
            }
        });
        let usage = sniff_usage(&frame).expect("流式帧应提取 usage");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 2);

        // 无 usage 字段的帧返回 None。
        assert!(sniff_usage(&json!({ "type": "response.output_text.delta" })).is_none());
    }

    /// 请求有状态特性（store/previous_response_id）出站时显式 warning 并丢弃。
    #[test]
    fn stateful_features_warn_and_drop() {
        let wire = json!({
            "model": "gpt-4o",
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_text", "text": "hi" }] }],
            "store": true,
            "previous_response_id": "resp_prev",
        });
        let ir = decode_request(&wire).expect("应可解码");
        let mut warnings = Vec::new();
        let encoded = encode_request(&ir, &mut warnings);
        assert!(encoded.get("store").is_none(), "store 应被丢弃");
        assert!(
            encoded.get("previous_response_id").is_none(),
            "previous_response_id 应被丢弃"
        );
        assert_eq!(
            warnings
                .iter()
                .filter(|w| matches!(
                    w,
                    Warning::Unsupported { feature, .. } if feature == "store"
                ))
                .count(),
            1,
            "store 丢弃应记 warning"
        );
        assert_eq!(
            warnings
                .iter()
                .filter(|w| matches!(
                    w,
                    Warning::Unsupported { feature, .. } if feature == "previous_response_id"
                ))
                .count(),
            1,
            "previous_response_id 丢弃应记 warning"
        );
    }

    /// 直通快路径 usage 嗅探：非流式响应顶层 usage 与流式 response.completed 帧。
    #[test]
    fn finish_reason_maps_from_status_and_incomplete() {
        // 无 incomplete_details 且无工具调用 → stop。
        assert_eq!(
            decode_response(&json!({
                "id": "r", "object": "response", "status": "completed", "model": "m",
                "output": [], "usage": { "input_tokens": 1, "output_tokens": 1 }
            }))
            .expect("应可解码")
            .finish_reason
            .unified,
            FinishReasonUnified::Stop
        );
        // 含 function_call → tool_calls。
        let ir = decode_response(&json!({
            "id": "r", "object": "response", "status": "completed", "model": "m",
            "output": [{
                "type": "function_call", "call_id": "c1", "name": "f",
                "arguments": "{}"
            }],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        }))
        .expect("应可解码");
        assert_eq!(ir.finish_reason.unified, FinishReasonUnified::ToolCalls);
        // incomplete_details.max_output_tokens → length。
        assert_eq!(
            decode_response(&json!({
                "id": "r", "object": "response", "status": "incomplete", "model": "m",
                "output": [], "incomplete_details": { "reason": "max_output_tokens" },
                "usage": { "input_tokens": 1, "output_tokens": 1 }
            }))
            .expect("应可解码")
            .finish_reason
            .unified,
            FinishReasonUnified::Length
        );
    }

    /// 出站编码时 IR 的 top_k 等采样参数无法表达：丢弃并记 warning。
    #[test]
    fn unsupported_ir_features_produce_warnings() {
        let request = ChatRequest {
            model: "gpt-4o".to_string(),
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
            top_k: Some(40),
            max_tokens: None,
            n: Some(2),
            stop: vec!["<|end|>".to_string()],
            presence_penalty: Some(0.5),
            frequency_penalty: None,
            seed: Some(42),
            response_format: Some(json!({ "type": "json_object" })),
            tools: Vec::new(),
            tool_choice: None,
            reasoning: None,
            provider_options: HashMap::new(),
        };
        let mut warnings = Vec::new();
        let encoded = encode_request(&request, &mut warnings);
        assert!(encoded.get("top_k").is_none(), "Responses 无 top_k 字段");
        assert!(encoded.get("seed").is_none(), "Responses 无 seed 字段");
        assert!(encoded.get("stop").is_none(), "Responses 无 stop 字段");
        for feature in [
            "top_k",
            "n",
            "seed",
            "stop",
            "presence_penalty",
            "response_format",
        ] {
            assert!(
                warnings.iter().any(|w| matches!(
                    w,
                    Warning::Unsupported { feature: f, .. } if f == feature
                )),
                "{feature} 丢弃应记 warning"
            );
        }
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
