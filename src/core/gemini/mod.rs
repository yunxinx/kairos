//! Gemini generateContent 协议适配器：wire ↔ IR 双向编解码（非流式 + 流式）。
//!
//! wire 结构体全部私有，透过 `decode_*`/`encode_*` 公共函数暴露 IR 边界，
//! wire 类型不出本模块边界。
//!
//! 映射要点：
//! - 请求侧：`contents[]` 承载消息序列（role 取 `user`/`model`），system 消息
//!   提升为 `systemInstruction`；part 变体 `text`/`thought`/`functionCall`/
//!   `functionResponse`/`inlineData`/`fileData` 双向映射；思考签名经 part 逃生舱
//!   `provider_options["google"]["thought_signature"]` 无损往返——签名与上游
//!   绑定，丢了下一轮就会被拒；出站模型为 Gemini 3 代且回放的 functionCall
//!   缺签名时注入官方 `skip_thought_signature_validator` 哨兵规避 400（同
//!   消息内已见带签名调用的并行形状豁免）。
//! - 工具：定义走 `tools[].functionDeclarations`，选择走
//!   `toolConfig.functionCallingConfig`（`AUTO/NONE/ANY` +
//!   `allowedFunctionNames`）。wire 传统上不带调用 id：入站按 `sha256(名字|入参)`
//!   生成稳定 id（跨轮重放同一调用得到同一 id），工具结果按名字与前文调用配对；
//!   上游显式给了 id 时经 `provider_options["google"]["function_call_id"]` 往返；
//!   functionResponse 紧随含 functionCall 的 model 轮（结果居中时先落独立
//!   user content，保住顺序）。
//! - 响应侧：`candidates[0].content.parts` + `finishReason` 双轨映射（含
//!   functionCall part 时 finish 归 `ToolCalls`）；usage 输入侧为
//!   「`promptTokenCount` 含缓存」的减法约定（与 OpenAI 系同口径），
//!   `thoughtsTokenCount` 由 Gemini 单独回报，但仍属于输出侧计费总量。
//! - 流式：`alt=sse` 的逐 chunk 是完整响应的 part 级片段，流以服务器关闭收尾、
//!   无哨兵行；末 chunk 携带 `finishReason` 与 `usageMetadata`，usage 逐 chunk
//!   累计（取最近一次出现的为终值）。
//! - 模型名不在请求体内（承载在 URL 路径的 `:generateContent` 端点上），
//!   由网关在调用本模块前注入；出站编码同样不写 `model` 字段。

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::core::ir::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, FinishReasonUnified, MediaSource,
    Message, PROVIDER_EXTRA_KEY, ReasoningEffort, Role, StreamEvent, Tool, ToolChoice, Usage,
    Warning, apply_provider_extra, capture_unknown_fields, part_provider_options,
    warn_dropped_provider_options, warning_feature,
};
use crate::core::stream::{
    ErrorMessageShape, SseFrame, decode_failed_frame, merge_provider_options,
};

/// 本适配器的 provider 逃生舱键。
const PROVIDER_KEY: &str = "google";
/// part 级逃生舱键：思考签名。
const THOUGHT_SIGNATURE_KEY: &str = "thought_signature";
/// part 级逃生舱键：上游显式给出的 functionCall id。
const FUNCTION_CALL_ID_KEY: &str = "function_call_id";
/// 请求级逃生舱键：思考配置（`thinkingConfig` 原始形状）。
const THINKING_CONFIG_KEY: &str = "thinking_config";
/// 请求级逃生舱键：安全档位（`safetySettings` 原始形状）。
const SAFETY_SETTINGS_KEY: &str = "safety_settings";
/// 请求级逃生舱键：`generationConfig` 未建模子字段的保真通道（原始形状）。
///
/// 官方持续给 `generationConfig` 增补子字段（`responseLogprobs` 等），未及
/// 类型化的子字段收进本键，同族出站合回 `generationConfig`（不覆盖类型化
/// 字段），不告警——与其它逃生舱字段的同族保真语义一致。
const GENERATION_CONFIG_EXTRA_KEY: &str = "generation_config_extra";
/// Gemini 3 代对回放 functionCall 的 thoughtSignature 校验哨兵：签名不可得
/// 时的官方占位值，缺失签名的回放会被 400 拒绝。
const SKIP_THOUGHT_SIGNATURE_VALIDATOR: &str = "skip_thought_signature_validator";

// ---- 错误 ----

/// wire 解码错误，网关映射为 400。
#[derive(Debug, Error)]
pub enum DecodeError {
    /// wire 形状不符：携带 serde 的具体原因与出错字段的 JSON 路径。
    #[error("wire 形状不符: {detail}")]
    InvalidShape { detail: String },
    #[error("contents[{index}] 的角色 {role} 未知")]
    UnknownRole { index: usize, role: String },
    #[error("contents[{index}] 的 part 无法识别")]
    UnknownContentPart { index: usize },
    #[error("contents[{index}] 的 functionCall 缺少 name")]
    MissingFunctionName { index: usize },
    #[error("toolConfig.functionCallingConfig 的 mode {mode} 未知")]
    UnknownFunctionCallingMode { mode: String },
}

// ---- wire 请求类型 ----

/// 本协议已知顶层请求字段白名单；白名单外的顶层字段由入站解码收进
/// 未知字段逃生舱（`provider_options["google"]["extra"]`）。
///
/// 请求与响应侧同一批键都接受 snake_case 别名：部分官方 SDK 以 proto JSON
/// 命名（snake_case）发送。
///
/// `model` 在列但不回写：模型名承载在 URL 路径上，网关解码前注入 IR，
/// 出站请求体不带该字段。
const KNOWN_REQUEST_FIELDS: &[&str] = &[
    "model",
    "contents",
    "systemInstruction",
    "system_instruction",
    "tools",
    "toolConfig",
    "tool_config",
    "generationConfig",
    "generation_config",
    "safetySettings",
    "safety_settings",
];

/// generateContent 请求体（wire）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireRequest {
    /// 模型名不属请求体（在 URL 路径上）；网关解码前注入，缺席时留空。
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    contents: Vec<WireContent>,
    #[serde(default, alias = "system_instruction")]
    system_instruction: Option<Value>,
    #[serde(default)]
    tools: Vec<WireTool>,
    #[serde(default, alias = "tool_config")]
    tool_config: Option<WireToolConfig>,
    #[serde(default, alias = "generation_config")]
    generation_config: Option<WireGenerationConfig>,
    #[serde(default, alias = "safety_settings")]
    safety_settings: Option<Value>,
}

/// 一条 content：role + 有序 parts。
#[derive(Debug, Clone, Deserialize)]
struct WireContent {
    /// 缺席时按官方缺省 `user` 处理。
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    parts: Vec<WirePart>,
}

/// content part：各变体以可选字段并置，按出现与否判定类别。
///
/// `thought: true` 的文本 part 是思维链；签名可与任意 part 并存。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thought: Option<bool>,
    #[serde(default, alias = "thought_signature")]
    thought_signature: Option<String>,
    #[serde(default, alias = "inline_data")]
    inline_data: Option<WireInlineMedia>,
    #[serde(default, alias = "file_data")]
    file_data: Option<WireFileReference>,
    #[serde(default, alias = "function_call")]
    function_call: Option<WireFunctionCall>,
    #[serde(default, alias = "function_response")]
    function_response: Option<WireFunctionResponse>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireInlineMedia {
    #[serde(default, alias = "mime_type")]
    mime_type: Option<String>,
    #[serde(default)]
    data: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireFileReference {
    #[serde(default, alias = "mime_type")]
    mime_type: Option<String>,
    #[serde(default, alias = "file_uri")]
    file_uri: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireFunctionCall {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    args: Option<Value>,
    /// 部分模型系列给 functionCall 带 id；缺席时按名字与入参生成稳定 id。
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireFunctionResponse {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    response: Option<Value>,
    #[serde(default)]
    id: Option<String>,
}

/// 工具声明容器：一份 `tools[]` 元素可含多条 `functionDeclarations`。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTool {
    #[serde(default, alias = "function_declarations")]
    function_declarations: Vec<WireFunctionDeclaration>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireFunctionDeclaration {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// schema 键有两个官方别名，优先 `parameters`。
    #[serde(default)]
    parameters: Option<Value>,
    #[serde(default)]
    parameters_json_schema: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireToolConfig {
    #[serde(default, alias = "function_calling_config")]
    function_calling_config: Option<WireFunctionCallingConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireFunctionCallingConfig {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default, alias = "allowed_function_names")]
    allowed_function_names: Option<Vec<String>>,
}

/// 生成参数面板；类型化字段逐个提升进 IR 旋钮，未建模子字段经 flatten 收集
/// 进 `unknown`（同族经 `generation_config_extra` 逃生舱回写）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireGenerationConfig {
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    top_p: Option<f64>,
    #[serde(default)]
    top_k: Option<u32>,
    #[serde(default, alias = "max_output_tokens")]
    max_output_tokens: Option<u32>,
    #[serde(default, alias = "stop_sequences")]
    stop_sequences: Option<Vec<String>>,
    #[serde(default, alias = "candidate_count")]
    candidate_count: Option<u32>,
    /// 确定性采样种子，官方 `generationConfig.seed` 字段。
    #[serde(default)]
    seed: Option<u64>,
    #[serde(default, alias = "response_mime_type")]
    response_mime_type: Option<String>,
    #[serde(default, alias = "response_schema")]
    response_schema: Option<Value>,
    #[serde(default, alias = "thinking_config")]
    thinking_config: Option<Value>,
    /// 未建模子字段（含 snake_case 别名形态的原始键）：flatten 保留原键名，
    /// 同族往返在 Value 相等语义下无损（重编码键序为字典序，不承诺字节形状）。
    #[serde(flatten)]
    unknown: Map<String, Value>,
}

// ---- 请求解码：wire → IR ----

/// 解码 generateContent 请求体为 IR。
///
/// 模型名不属请求体（承载在 URL 路径上），由网关在调用前注入；流式与否同样
/// 由端点（`:streamGenerateContent`）决定，本模块只处理非流式请求体。
pub fn decode_request(value: &Value) -> Result<ChatRequest, DecodeError> {
    let wire: WireRequest = serde_path_to_error::deserialize(value.clone()).map_err(|err| {
        DecodeError::InvalidShape {
            detail: err.to_string(),
        }
    })?;

    let mut messages = Vec::new();
    // systemInstruction 的非文本 part（媒体等）在本协议无承载：解码即丢弃，
    // 与响应侧媒体丢弃同规显式告警，不静默。
    let mut warnings = Vec::new();
    if let Some(system) = &wire.system_instruction {
        warn_non_text_system_parts(system, &mut warnings);
        let text = system_instruction_text(system);
        if !text.is_empty() {
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

    // 已出现但尚未被结果配对的调用（name, id），按出现顺序；结果按名字对号。
    let mut pending_calls: Vec<(String, String)> = Vec::new();
    for (index, content) in wire.contents.iter().enumerate() {
        let role = match content.role.as_deref() {
            None | Some("user") => Role::User,
            Some("model") => Role::Assistant,
            Some(other) => {
                return Err(DecodeError::UnknownRole {
                    index,
                    role: other.to_string(),
                });
            }
        };

        let mut parts = Vec::new();
        for part in &content.parts {
            if let Some(result) = part.function_response.as_ref() {
                // 同一条 content 内混排时先落地已累积的角色消息，保持 wire 顺序
                // （工具结果另起一条 Tool 消息，不能插到本条之前）。
                if !parts.is_empty() {
                    messages.push(Message {
                        role,
                        content: std::mem::take(&mut parts),
                        provider_options: HashMap::new(),
                    });
                }
                let name = result.name.clone().unwrap_or_default();
                let call_id = match result.id.clone() {
                    Some(id) => Some(id),
                    None => pop_pending_call(&mut pending_calls, &name),
                };
                // 历史被截断时结果可能没有配对的调用（本请求体里没有那条
                // functionCall）：按名字生成稳定 id 让配对仍然可用，不打断请求。
                let tool_call_id =
                    call_id.unwrap_or_else(|| stable_tool_call_id(&name, &Value::Null));
                messages.push(Message {
                    role: Role::Tool,
                    content: vec![ContentPart::ToolResult {
                        tool_call_id,
                        tool_name: name,
                        output: result.response.clone().unwrap_or(Value::Null),
                        provider_options: part_options(
                            part.thought_signature.as_ref(),
                            result.id.as_ref(),
                        ),
                    }],
                    provider_options: HashMap::new(),
                });
                continue;
            }
            parts.push(decode_part(part, index, &mut pending_calls)?);
        }

        if !parts.is_empty() {
            messages.push(Message {
                role,
                content: parts,
                provider_options: HashMap::new(),
            });
        }
    }

    let mut provider_options: HashMap<String, Value> = HashMap::new();
    let mut google_options = Map::new();
    if let Some(safety) = &wire.safety_settings {
        google_options.insert(SAFETY_SETTINGS_KEY.into(), safety.clone());
    }

    let mut temperature = None;
    let mut top_p = None;
    let mut top_k = None;
    let mut max_tokens = None;
    let mut stop = Vec::new();
    let mut n = None;
    let mut response_format = None;
    let mut reasoning = None;
    let mut seed = None;

    if let Some(config) = &wire.generation_config {
        temperature = config.temperature;
        top_p = config.top_p;
        top_k = config.top_k;
        max_tokens = config.max_output_tokens;
        stop = config.stop_sequences.clone().unwrap_or_default();
        n = config.candidate_count;
        seed = config.seed;
        // 多候选入站即记录有损：IR 保留原值（同族出站原样回写），但上游实际
        // 只返回一个候选，跨族出站时另行告警。
        if let Some(count) = config.candidate_count
            && count > 1
        {
            warnings.push(Warning::unsupported(
                warning_feature::N,
                format!("Gemini 单次请求只返回一个候选，candidateCount={count} 无法表达"),
            ));
        }
        response_format = response_format_from_generation(config);
        // 思考配置：原始形状进逃生舱（同族无损回传），可枚举档位提升进类型化旋钮。
        if let Some(thinking) = &config.thinking_config {
            google_options.insert(THINKING_CONFIG_KEY.into(), thinking.clone());
            reasoning = reasoning_from_thinking_config(thinking);
        }
        // 未建模子字段进逃生舱：同族合回 generationConfig（保真往返、零告警），
        // 跨族随 provider_options 整体告警丢弃。
        if !config.unknown.is_empty() {
            google_options.insert(
                GENERATION_CONFIG_EXTRA_KEY.into(),
                Value::Object(config.unknown.clone()),
            );
        }
    }

    let tools = wire
        .tools
        .iter()
        .flat_map(|tool| tool.function_declarations.iter())
        .filter_map(|declaration| {
            let name = declaration.name.clone()?;
            Some(Tool {
                name,
                description: declaration.description.clone(),
                parameters: declaration
                    .parameters
                    .clone()
                    .or_else(|| declaration.parameters_json_schema.clone()),
                provider_options: HashMap::new(),
            })
        })
        .collect();

    let tool_choice = match &wire.tool_config {
        Some(config) => decode_tool_config(config)?,
        None => None,
    };

    let extra = capture_unknown_fields(value, KNOWN_REQUEST_FIELDS);
    if !extra.is_empty() {
        google_options.insert(PROVIDER_EXTRA_KEY.into(), Value::Object(extra));
    }
    if !google_options.is_empty() {
        provider_options.insert(PROVIDER_KEY.to_string(), Value::Object(google_options));
    }

    Ok(ChatRequest {
        model: wire.model.clone().unwrap_or_default(),
        messages,
        stream: false,
        temperature,
        top_p,
        top_k,
        max_tokens,
        n,
        stop,
        presence_penalty: None,
        frequency_penalty: None,
        seed,
        response_format,
        tools,
        tool_choice,
        parallel_tool_calls: None,
        reasoning,
        provider_options,
        warnings,
    })
}

/// 生成参数面板 → IR `response_format`（chat wire 形状）。
///
/// `application/json` 对应 `json_object`；带 `responseSchema` 时按
/// `json_schema` 提升（schema 收进 `json_schema.schema` 子键）。
fn response_format_from_generation(config: &WireGenerationConfig) -> Option<Value> {
    if config.response_mime_type.as_deref() != Some("application/json") {
        return None;
    }
    match config.response_schema.clone() {
        Some(schema) => Some(json!({
            "type": "json_schema",
            "json_schema": { "schema": schema },
        })),
        None => Some(json!({ "type": "json_object" })),
    }
}

/// 思考配置 → 类型化 effort 档位；两档官方表达都认（`thinkingLevel` 与
/// `thinkingBudget`，各含 snake_case 别名）。
fn reasoning_from_thinking_config(thinking: &Value) -> Option<ReasoningEffort> {
    let level = thinking
        .get("thinkingLevel")
        .or_else(|| thinking.get("thinking_level"))
        .and_then(Value::as_str);
    if let Some(level) = level {
        return ReasoningEffort::parse_effort(level);
    }
    let budget = thinking
        .get("thinkingBudget")
        .or_else(|| thinking.get("thinking_budget"))
        .and_then(Value::as_u64)?;
    let budget = u32::try_from(budget).unwrap_or(u32::MAX);
    Some(ReasoningEffort::from_budget(budget))
}

/// `systemInstruction` 提取为纯文本：字符串或 `{parts:[{text}]}` 两种官方形状。
///
/// 出站统一回写为块数组（单块）——两种形状语义等价，归一后同族往返不再
/// 逐字节相同，属刻意归一化而非信息损失。
fn system_instruction_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(object) => object
            .get("parts")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<String>()
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// systemInstruction 非文本 part 的丢弃告警。
///
/// 媒体 part（inlineData/fileData）与响应侧媒体丢弃同词汇（MEDIA）；其余
/// 既无文本也非媒体的形状（functionCall 等或未知键）按 CUSTOM 告警。携带
/// 文本与媒体并存的 part 只告警媒体侧（文本已保留）。
fn warn_non_text_system_parts(system: &Value, warnings: &mut Vec<Warning>) {
    let Some(parts) = system
        .as_object()
        .and_then(|object| object.get("parts"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for part in parts {
        let media = ["inlineData", "inline_data", "fileData", "file_data"]
            .iter()
            .find_map(|key| part.get(*key));
        if let Some(media) = media {
            let media_type = media
                .get("mimeType")
                .or_else(|| media.get("mime_type"))
                .and_then(Value::as_str)
                .unwrap_or("未知类型");
            warnings.push(Warning::unsupported(
                warning_feature::MEDIA,
                format!("Gemini 系统指令不支持媒体内容（{media_type}），已丢弃"),
            ));
        } else if part.get("text").is_none() {
            let keys = part
                .as_object()
                .map(|object| {
                    object
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join("、")
                })
                .unwrap_or_else(|| "未知形状".to_string());
            warnings.push(Warning::unsupported(
                warning_feature::CUSTOM,
                format!("Gemini 系统指令仅支持文本 part，非文本 part（{keys}）已丢弃"),
            ));
        }
    }
}

/// 从待配对调用里按名字取一个（同名多次调用按出现顺序）。
fn pop_pending_call(pending: &mut Vec<(String, String)>, name: &str) -> Option<String> {
    let position = pending
        .iter()
        .position(|(call_name, _)| call_name == name)?;
    Some(pending.remove(position).1)
}

/// wire 不承载调用 id 时的稳定 id：同一（名字, 入参）跨轮重放得到同一 id，
/// 工具结果按名字配对因此稳定。矩阵夹具同用此形状（跨本协议中转时 id 由
/// 名字与入参重生成，见 `core::roundtrip` 模块注释）。
pub(crate) fn stable_tool_call_id(name: &str, input: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    hasher.update(b"|");
    hasher.update(input.to_string().as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("call_{hex}")
}

/// part 级逃生舱：思考签名 + 上游显式 id。
fn part_options(signature: Option<&String>, call_id: Option<&String>) -> HashMap<String, Value> {
    let mut google = Map::new();
    if let Some(signature) = signature {
        google.insert(THOUGHT_SIGNATURE_KEY.into(), json!(signature));
    }
    if let Some(id) = call_id {
        google.insert(FUNCTION_CALL_ID_KEY.into(), json!(id));
    }
    if google.is_empty() {
        return HashMap::new();
    }
    [(PROVIDER_KEY.to_string(), Value::Object(google))]
        .into_iter()
        .collect()
}

/// 解码单个 part 为 IR content part；functionCall 登记进待配对集合。
fn decode_part(
    part: &WirePart,
    index: usize,
    pending_calls: &mut Vec<(String, String)>,
) -> Result<ContentPart, DecodeError> {
    let signature = part.thought_signature.as_ref();

    if let Some(call) = &part.function_call {
        let name = call
            .name
            .clone()
            .ok_or(DecodeError::MissingFunctionName { index })?;
        let input = call.args.clone().unwrap_or_else(|| json!({}));
        let tool_call_id = call
            .id
            .clone()
            .unwrap_or_else(|| stable_tool_call_id(&name, &input));
        pending_calls.push((name.clone(), tool_call_id.clone()));
        return Ok(ContentPart::ToolCall {
            tool_call_id,
            tool_name: name,
            input,
            provider_options: part_options(signature, call.id.as_ref()),
        });
    }

    if let Some(inline) = &part.inline_data {
        return Ok(ContentPart::Media {
            media_type: inline.mime_type.clone().unwrap_or_default(),
            data: crate::core::ir::MediaSource::Data {
                base64: inline.data.clone().unwrap_or_default(),
            },
            provider_options: part_options(signature, None),
        });
    }

    if let Some(file) = &part.file_data {
        return Ok(ContentPart::Media {
            media_type: file.mime_type.clone().unwrap_or_default(),
            data: crate::core::ir::MediaSource::Url {
                url: file.file_uri.clone().unwrap_or_default(),
            },
            provider_options: part_options(signature, None),
        });
    }

    let text = part.text.clone().unwrap_or_default();
    if part.thought == Some(true) {
        return Ok(ContentPart::Reasoning {
            text,
            provider_options: part_options(signature, None),
        });
    }
    if part.text.is_some() {
        return Ok(ContentPart::Text {
            text,
            provider_options: part_options(signature, None),
        });
    }

    Err(DecodeError::UnknownContentPart { index })
}

/// `toolConfig.functionCallingConfig` → IR 类型化 tool_choice。
fn decode_tool_config(config: &WireToolConfig) -> Result<Option<ToolChoice>, DecodeError> {
    let Some(calling) = config.function_calling_config.as_ref() else {
        return Ok(None);
    };
    let mode = calling.mode.clone().unwrap_or_default();
    let allowed = calling.allowed_function_names.clone().unwrap_or_default();
    match mode.as_str() {
        "AUTO" => Ok(Some(ToolChoice::Auto)),
        "NONE" => Ok(Some(ToolChoice::None)),
        // 单个允许名单即指名工具；多名或空名单为「必须调用」，名单本身无 IR 承载。
        "ANY" => Ok(Some(match allowed.as_slice() {
            [name] => ToolChoice::Tool { name: name.clone() },
            _ => ToolChoice::Required,
        })),
        "" => Ok(None),
        other => Err(DecodeError::UnknownFunctionCallingMode {
            mode: other.to_string(),
        }),
    }
}

// ---- 请求编码：IR → wire ----

/// 编码 IR 请求为 generateContent 请求体。
///
/// 模型名不写进请求体（承载在 URL 路径上）。
pub fn encode_request(request: &ChatRequest, warnings: &mut Vec<Warning>) -> Value {
    encode_request_for_model(request, &request.model, warnings)
}

/// 按最终出站模型编码 Gemini 请求。
///
/// 请求模型可能经过渠道别名或统一模型成员解析；thinking budget 的上限必须
/// 依据实际 URL 使用的模型判断，否则别名路径会把较小模型的上限编码得过大。
pub fn encode_request_for_model(
    request: &ChatRequest,
    outbound_model: &str,
    warnings: &mut Vec<Warning>,
) -> Value {
    let (system_instruction, contents) =
        encode_messages(&request.messages, outbound_model, warnings);

    let mut obj = Map::new();
    if let Some(system_instruction) = system_instruction {
        obj.insert("systemInstruction".into(), system_instruction);
    }
    obj.insert("contents".into(), Value::Array(contents));

    let mut generation = Map::new();
    if let Some(temperature) = request.temperature {
        generation.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = request.top_p {
        generation.insert("topP".into(), json!(top_p));
    }
    if let Some(top_k) = request.top_k {
        generation.insert("topK".into(), json!(top_k));
    }
    if let Some(max_tokens) = request.max_tokens {
        generation.insert("maxOutputTokens".into(), json!(max_tokens));
    }
    if !request.stop.is_empty() {
        generation.insert("stopSequences".into(), json!(request.stop));
    }
    // Gemini 一次只返回一个候选：多候选请求无法表达，丢弃并告警。
    if let Some(n) = request.n {
        if n > 1 {
            warnings.push(Warning::unsupported(
                warning_feature::N,
                format!("Gemini 单次请求只返回一个候选，n={n} 已丢弃"),
            ));
        } else {
            generation.insert("candidateCount".into(), json!(n));
        }
    }
    if let Some(seed) = request.seed {
        // 官方 `generationConfig.seed` 字段承载确定性采样，IR seed 双向映射。
        generation.insert("seed".into(), json!(seed));
    }
    if let Some((mime_type, schema)) = generation_output_format(request, warnings) {
        generation.insert("responseMimeType".into(), json!(mime_type));
        if let Some(schema) = schema {
            generation.insert("responseSchema".into(), schema);
        }
    }
    let google_options = request.provider_options.get(PROVIDER_KEY);
    let hatch_thinking = google_options.and_then(|options| options.get(THINKING_CONFIG_KEY));
    if let Some(thinking) = hatch_thinking {
        generation.insert("thinkingConfig".into(), thinking.clone());
    } else if let Some(config) = typed_thinking_config(outbound_model, request.reasoning, warnings)
    {
        generation.insert("thinkingConfig".into(), config);
    }

    if !request.tools.is_empty() {
        let declarations: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                // 工具级逃生舱（如缓存断点）仅本族键有承载形态，跨族丢弃告警。
                warn_dropped_provider_options(
                    &tool.provider_options,
                    PROVIDER_KEY,
                    "工具级",
                    warnings,
                );
                let mut declaration = Map::new();
                declaration.insert("name".into(), json!(tool.name));
                if let Some(description) = &tool.description {
                    declaration.insert("description".into(), json!(description));
                }
                if let Some(parameters) = &tool.parameters {
                    declaration.insert("parameters".into(), parameters.clone());
                }
                Value::Object(declaration)
            })
            .collect();
        obj.insert(
            "tools".into(),
            json!([{ "functionDeclarations": declarations }]),
        );
    }
    if let Some(choice) = &request.tool_choice {
        obj.insert("toolConfig".into(), encode_tool_config(choice));
    }
    // 无请求级并行开关：允许并行是缺省语义（`true` 丢弃即无损失），
    // 显式禁并行无处表达，只能告警。
    if request.parallel_tool_calls == Some(false) {
        warnings.push(Warning::unsupported(
            warning_feature::PARALLEL_TOOL_CALLS,
            "Gemini 无请求级并行工具调用开关，禁止并行的设置已丢弃",
        ));
    }
    for (feature, present) in [
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
                format!("Gemini 无 {feature} 承载，已丢弃"),
            ));
        }
    }
    if let Some(safety) = google_options.and_then(|options| options.get(SAFETY_SETTINGS_KEY)) {
        obj.insert("safetySettings".into(), safety.clone());
    }

    // generationConfig 未建模子字段（同族来源经逃生舱保真）：合回面板，
    // 不覆盖类型化字段已写的键。合并在空判定之前——仅有未建模子字段的请求
    // 也要还原整个面板。
    if let Some(extra) = google_options.and_then(|options| options.get(GENERATION_CONFIG_EXTRA_KEY))
        && let Value::Object(extra) = extra
    {
        for (key, field) in extra {
            generation.entry(key.clone()).or_insert(field.clone());
        }
    }
    if !generation.is_empty() {
        obj.insert("generationConfig".into(), Value::Object(generation));
    }
    apply_provider_extra(
        &mut obj,
        request,
        PROVIDER_KEY,
        crate::core::ir::ExtraWriteback::WholeFamily,
        warnings,
    );
    Value::Object(obj)
}

/// IR `response_format` → `responseMimeType` + `responseSchema`。
fn generation_output_format(
    request: &ChatRequest,
    warnings: &mut Vec<Warning>,
) -> Option<(&'static str, Option<Value>)> {
    let format = request.response_format.as_ref()?;
    match format.get("type").and_then(Value::as_str) {
        Some("json_object") => Some(("application/json", None)),
        Some("json_schema") => {
            let schema = format
                .get("json_schema")
                .and_then(|inner| inner.get("schema").cloned())
                .or_else(|| format.get("json_schema").cloned());
            match schema {
                Some(schema) => Some(("application/json", Some(schema))),
                None => {
                    warnings.push(Warning::unsupported(
                        warning_feature::RESPONSE_FORMAT,
                        format!("response_format 的 json_schema 缺少 schema，已丢弃: {format}"),
                    ));
                    None
                }
            }
        }
        Some("text") => None,
        _ => {
            warnings.push(Warning::unsupported(
                warning_feature::RESPONSE_FORMAT,
                format!("Gemini 无法表达该 response_format 形状，已丢弃: {format}"),
            ));
            None
        }
    }
}

/// 类型化 effort 档位在本族逃生舱缺席时展开为 `thinkingConfig`
/// （budget 阶梯，与跨族映射同一张换算表）。
fn typed_thinking_config(
    model: &str,
    reasoning: Option<ReasoningEffort>,
    warnings: &mut Vec<Warning>,
) -> Option<Value> {
    let effort = reasoning?;
    let budget = match effort {
        // 2.5 模型默认启用思考；显式 none 必须写 0 才能表达关闭。
        ReasoningEffort::None => 0,
        effort => effort.budget_tokens()?,
    };
    let cap = crate::core::model_family::gemini_2_5_budget_cap(model).unwrap_or(u32::MAX);
    let budget = budget.min(cap);
    if budget != effort.budget_tokens().unwrap_or(0) {
        warnings.push(Warning::compatibility(
            warning_feature::THINKING,
            format!("模型 {model} 的 thinkingBudget 已限制为 {budget}"),
        ));
    }
    Some(json!({ "thinkingBudget": budget }))
}

/// IR tool_choice → `toolConfig.functionCallingConfig`。
fn encode_tool_config(choice: &ToolChoice) -> Value {
    let (mode, allowed): (&str, Option<Vec<String>>) = match choice {
        ToolChoice::Auto => ("AUTO", None),
        ToolChoice::None => ("NONE", None),
        ToolChoice::Required => ("ANY", None),
        ToolChoice::Tool { name } => ("ANY", Some(vec![name.clone()])),
    };
    let mut calling = Map::new();
    calling.insert("mode".into(), json!(mode));
    if let Some(allowed) = allowed {
        calling.insert("allowedFunctionNames".into(), json!(allowed));
    }
    json!({ "functionCallingConfig": Value::Object(calling) })
}

/// IR 消息序列 → `systemInstruction` + `contents[]`。
///
/// system 消息按官方单指令形状合并为一条 `systemInstruction`；工具结果以
/// `functionResponse` part 承载——下一条 user 消息在场时并入其 content 开头，
/// 遇到 model 轮或序列结尾时先落为独立 user content（Gemini 要求
/// functionResponse 紧随含 functionCall 的 model 轮，后续 model 轮插进
/// 中间会被上游拒绝）。出站模型为 Gemini 3 代时，assistant 回放的无签名
/// functionCall 注入 [`SKIP_THOUGHT_SIGNATURE_VALIDATOR`] 哨兵。
fn encode_messages(
    messages: &[Message],
    outbound_model: &str,
    warnings: &mut Vec<Warning>,
) -> (Option<Value>, Vec<Value>) {
    // 消息级与 part 级逃生舱统一在此检查：本族键（思考签名、functionCall id）
    // 由 encode_part 按形状读写；非本族条目（如跨族缓存断点）无法表达，逐处告警。
    for message in messages {
        warn_dropped_provider_options(&message.provider_options, PROVIDER_KEY, "消息级", warnings);
        for provider_options in part_provider_options(&message.content) {
            warn_dropped_provider_options(provider_options, PROVIDER_KEY, "内容级", warnings);
        }
    }

    let system_text: String = messages
        .iter()
        .filter(|message| message.role == Role::System)
        .filter_map(|message| {
            let text = text_parts(&message.content);
            for part in &message.content {
                if let ContentPart::Media { media_type, .. } = part {
                    warnings.push(Warning::unsupported(
                        warning_feature::MEDIA,
                        format!("Gemini 系统指令不支持媒体内容（{media_type}），已丢弃"),
                    ));
                }
            }
            (!text.is_empty()).then_some(text)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let system_instruction =
        (!system_text.is_empty()).then(|| json!({ "parts": [{ "text": system_text }] }));

    let mut contents: Vec<Value> = Vec::new();
    // 待并入下一条 user content 的 functionResponse parts。
    let mut pending_results: Vec<Value> = Vec::new();
    // 已见调用 id → 函数名：Gemini 的工具结果按名字配对，跨族来源（如 chat
    // 的 tool 消息）不携带函数名，编码时从上文调用回填空名字。
    let mut call_names: HashMap<String, String> = HashMap::new();
    let gemini3 = is_gemini3_model(outbound_model);

    for message in messages {
        for part in &message.content {
            if let ContentPart::ToolCall {
                tool_call_id,
                tool_name,
                ..
            } = part
            {
                call_names.insert(tool_call_id.clone(), tool_name.clone());
            }
        }
        match message.role {
            Role::System => continue,
            Role::Tool => {
                for part in &message.content {
                    match part {
                        ContentPart::ToolResult { .. } => {
                            let resolved = resolve_result_name(part, &call_names);
                            if let Some(block) = encode_part(&resolved, warnings) {
                                pending_results.push(block);
                            }
                        }
                        _ => warnings.push(Warning::unsupported(
                            warning_feature::TOOL_RESULT,
                            "工具消息的非 functionResponse part 无法表达，已丢弃",
                        )),
                    }
                }
                continue;
            }
            Role::User | Role::Assistant => {}
        }

        let mut parts = Vec::new();
        if !pending_results.is_empty() {
            if message.role == Role::User {
                parts.append(&mut pending_results);
            } else {
                // Gemini 要求 functionResponse 紧随含 functionCall 的 model 轮：
                // 后续 model 轮之前必须先把 pending 结果落为独立 user content，
                // 否则结果被推迟到模型回答之后，顺序破坏会被上游拒绝。
                contents.push(json!({
                    "role": "user",
                    "parts": std::mem::take(&mut pending_results),
                }));
            }
        }
        // Gemini 3 签名哨兵状态（消息内作用域）：同一条 assistant 消息里已见
        // 带签名调用时，后续无签名调用是并行调用的合法形状，跳过注入。
        let mut saw_signed_call = false;
        let mut sentinel_tools: Vec<&str> = Vec::new();
        for part in &message.content {
            let mut block = encode_part(part, warnings);
            if let (
                Some(block),
                ContentPart::ToolCall {
                    tool_name,
                    provider_options,
                    ..
                },
            ) = (&mut block, part)
            {
                let signed = provider_options
                    .get(PROVIDER_KEY)
                    .and_then(|options| options.get(THOUGHT_SIGNATURE_KEY))
                    .is_some();
                if signed {
                    saw_signed_call = true;
                } else if gemini3 && !saw_signed_call {
                    block["thoughtSignature"] = json!(SKIP_THOUGHT_SIGNATURE_VALIDATOR);
                    sentinel_tools.push(tool_name);
                }
            }
            if let Some(block) = block {
                parts.push(block);
            }
        }
        if !sentinel_tools.is_empty() {
            warnings.push(Warning::compatibility(
                warning_feature::THOUGHT_SIGNATURE,
                format!(
                    "Gemini 3 校验回放的 functionCall 缺 thoughtSignature，已为工具 {} 注入 \
                     skip_thought_signature_validator 哨兵规避 400；签名丢失常见于应用侧未随消息持久化签名",
                    sentinel_tools.join("、")
                ),
            ));
        }
        if parts.is_empty() {
            continue;
        }
        contents.push(json!({
            "role": if message.role == Role::Assistant { "model" } else { "user" },
            "parts": parts,
        }));
    }

    if !pending_results.is_empty() {
        contents.push(json!({ "role": "user", "parts": pending_results }));
    }
    (system_instruction, contents)
}

/// 判定出站模型是否按 Gemini 3 代行为处理（thoughtSignature 校验等）。
///
/// 判定法：`gemini-` 段前缀（字符串首或 `/` 之后，覆盖 `models/` 前缀与
/// 路径接入形态）且不属于已知旧代际——`gemini-1*`、`gemini-2*`（版本号后
/// 须紧跟 `.`/`-` 或结尾，`gemini-2.5` 即旧代际）、`gemini-pro`/`gemini-pro-
/// vision` 旧名（须为字符串结尾）与 `gemini-robotics-er-1.5`。Google 模型
/// ID 开放命名，未识别的新 ID 继承最新行为；家族别名（如 nano-banana）无
/// `gemini-` 前缀，不在此列。
fn is_gemini3_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    let mut is_gemini = false;
    let mut is_known_older = false;
    let mut rest = model.as_str();
    while let Some(offset) = rest.find("gemini-") {
        let tail = &rest[offset..];
        if offset == 0 || rest[..offset].ends_with('/') {
            is_gemini = true;
            let after = &tail["gemini-".len()..];
            // 版本号后紧跟 `.`/`-` 或结尾才算已知代际（`gemini-2.5` 命中，
            // `gemini-25` 这类不存在的前缀不误判）。
            let known_generation = |digit: char| {
                let mut chars = after.chars();
                chars.next() == Some(digit) && matches!(chars.next(), None | Some('.') | Some('-'))
            };
            let robotics = "robotics-er-1.5";
            if known_generation('1')
                || known_generation('2')
                || after == "pro"
                || after == "pro-vision"
                || (after.starts_with(robotics)
                    && after[robotics.len()..]
                        .chars()
                        .next()
                        .is_none_or(|next| next == '.' || next == '-'))
            {
                is_known_older = true;
            }
        }
        rest = &rest[offset + "gemini-".len()..];
    }
    is_gemini && !is_known_older
}

/// 工具结果名字为空时从上文调用回填（Gemini 按名字配对，名字是配对身份）；
/// 名字已有值或上文无对应调用时原样返回。
fn resolve_result_name(part: &ContentPart, call_names: &HashMap<String, String>) -> ContentPart {
    let ContentPart::ToolResult {
        tool_call_id,
        tool_name,
        output,
        provider_options,
    } = part
    else {
        return part.clone();
    };
    if !tool_name.is_empty() {
        return part.clone();
    }
    let Some(name) = call_names.get(tool_call_id) else {
        return part.clone();
    };
    ContentPart::ToolResult {
        tool_call_id: tool_call_id.clone(),
        tool_name: name.clone(),
        output: output.clone(),
        provider_options: provider_options.clone(),
    }
}

/// 消息内文本 part 拼接（system 合并用）。
fn text_parts(parts: &[ContentPart]) -> String {
    crate::core::ir::text_content(parts).unwrap_or_default()
}

/// 编码单个 IR part 为 wire part；无法表达的类别返回 `None`（并告警）。
fn encode_part(part: &ContentPart, warnings: &mut Vec<Warning>) -> Option<Value> {
    let (mut block, provider_options) = match part {
        ContentPart::Text {
            text,
            provider_options,
        } => (json!({ "text": text }), provider_options),
        ContentPart::Reasoning {
            text,
            provider_options,
        } => (json!({ "text": text, "thought": true }), provider_options),
        ContentPart::ToolCall {
            tool_name,
            input,
            provider_options,
            ..
        } => (
            json!({ "functionCall": { "name": tool_name, "args": input } }),
            provider_options,
        ),
        ContentPart::ToolResult {
            tool_name,
            output,
            provider_options,
            ..
        } => {
            // `response` 必须是对象：非对象载荷按 `result` 子键包裹。
            let response = match output {
                Value::Object(_) => output.clone(),
                other => json!({ "result": other }),
            };
            (
                json!({ "functionResponse": { "name": tool_name, "response": response } }),
                provider_options,
            )
        }
        ContentPart::Media {
            media_type,
            data,
            provider_options,
        } => {
            // IR 的 media_type 允许仅顶层段的类别标记（如 chat 远程 image_url
            // 的 `image`），标记值不是合法 IANA 类型、不得写上 wire。fileData
            // 的 mimeType 可省（上游按 fileUri 推断）；inlineData 无法省略
            // （缺类型字节无从解释），完整类型未知时丢弃并告警。
            let block = if !crate::core::ir::is_full_media_type(media_type) {
                match data {
                    MediaSource::Url { url } => json!({ "fileData": { "fileUri": url } }),
                    MediaSource::Data { .. } => {
                        warnings.push(Warning::unsupported(
                            warning_feature::MEDIA,
                            format!(
                                "Gemini inlineData 需要完整媒体类型，{media_type:?} 无法承载，已丢弃"
                            ),
                        ));
                        return None;
                    }
                }
            } else {
                match data {
                    MediaSource::Data { base64 } => {
                        json!({ "inlineData": { "mimeType": media_type, "data": base64 } })
                    }
                    MediaSource::Url { url } => {
                        json!({ "fileData": { "mimeType": media_type, "fileUri": url } })
                    }
                }
            };
            (block, provider_options)
        }
        ContentPart::Custom { kind, .. } => {
            warnings.push(Warning::unsupported(
                warning_feature::CUSTOM,
                format!("Gemini 无法表达自定义 part（{kind}），已丢弃"),
            ));
            return None;
        }
    };

    let google = provider_options.get(PROVIDER_KEY);
    if let Some(signature) = google.and_then(|options| options.get(THOUGHT_SIGNATURE_KEY)) {
        block["thoughtSignature"] = signature.clone();
    }
    // 调用 id 只在「上游给过」时才回写，且回写 IR 侧的当前 id：自生成的稳定
    // id 不回写（否则请求体凭空多出客户端没带的字段），跨族改写过 id 时以
    // 当前值为准（逃生舱里的旧值已失效）。
    if google
        .and_then(|options| options.get(FUNCTION_CALL_ID_KEY))
        .is_some()
    {
        match part {
            ContentPart::ToolCall { tool_call_id, .. } => {
                block["functionCall"]["id"] = json!(tool_call_id);
            }
            ContentPart::ToolResult { tool_call_id, .. } => {
                block["functionResponse"]["id"] = json!(tool_call_id);
            }
            _ => {}
        }
    }
    Some(block)
}

// ---- wire 响应类型 ----

/// generateContent 响应体（wire）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireResponse {
    #[serde(default)]
    candidates: Vec<WireCandidate>,
    #[serde(default, alias = "usage_metadata")]
    usage_metadata: Option<Value>,
    #[serde(default, alias = "model_version")]
    model_version: Option<String>,
    #[serde(default, alias = "response_id")]
    response_id: Option<String>,
    /// 安全拦截反馈：请求级内容审查拒绝生成时 candidates 为空、本对象携带
    /// `blockReason`（SAFETY 等枚举值）。
    #[serde(default, alias = "prompt_feedback")]
    prompt_feedback: Option<WirePromptFeedback>,
    /// 流内错误帧（顶层 `{"error": {...}}`，google.rpc Status 形状）。
    #[serde(default)]
    error: Option<WireStreamError>,
}

/// `promptFeedback` 对象：`blockReason` 为拦截原因枚举。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePromptFeedback {
    #[serde(default, alias = "block_reason")]
    block_reason: Option<String>,
}

/// 流内错误帧的 `error` 对象：`message` 承载失败原因；code/status 不参与
/// IR 映射（未知字段容忍）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireStreamError {
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireCandidate {
    #[serde(default)]
    content: Option<WireContent>,
    #[serde(default, alias = "finish_reason")]
    finish_reason: Option<String>,
}

// ---- 响应解码：wire → IR ----

/// 解码 generateContent 响应为 IR。
pub fn decode_response(value: &Value) -> Result<ChatResponse, DecodeError> {
    let wire: WireResponse = serde_path_to_error::deserialize(value.clone()).map_err(|err| {
        DecodeError::InvalidShape {
            detail: err.to_string(),
        }
    })?;

    let candidate = wire.candidates.first();
    let mut content = Vec::new();
    let mut has_function_call = false;
    let mut warnings = Vec::new();
    if wire.candidates.len() > 1 {
        // 与请求侧 candidateCount 告警同词汇：网关只读取首个候选，其余丢弃
        // 属有损面，显式可观测。
        warnings.push(Warning::compatibility(
            warning_feature::N,
            format!(
                "Gemini 响应携带 {} 个候选，仅读取首个，其余已丢弃",
                wire.candidates.len()
            ),
        ));
    }
    if let Some(candidate) = candidate
        && let Some(content_value) = &candidate.content
    {
        for (index, part) in content_value.parts.iter().enumerate() {
            let decoded = decode_part(part, index, &mut Vec::new())?;
            has_function_call |= matches!(decoded, ContentPart::ToolCall { .. });
            content.push(decoded);
        }
    }
    // 安全拦截：candidates 为空 + promptFeedback.blockReason 表示请求级审查
    // 拒绝生成（非流式零内容、流式零事件都会让下游无从感知原因），映射为
    // ContentFilter 结束并携带告警，拦截原因随 warning details 下发。
    let blocked_reason = wire
        .prompt_feedback
        .as_ref()
        .and_then(|feedback| feedback.block_reason.clone())
        .filter(|_| wire.candidates.is_empty());
    let raw_finish = candidate
        .and_then(|c| c.finish_reason.clone())
        .or_else(|| blocked_reason.clone());
    let unified = if blocked_reason.is_some() {
        FinishReasonUnified::ContentFilter
    } else {
        match raw_finish.as_deref() {
            Some("STOP") if has_function_call => FinishReasonUnified::ToolCalls,
            _ => map_finish_reason(raw_finish.as_deref()),
        }
    };
    if let Some(block) = &blocked_reason {
        warnings.push(Warning::compatibility(
            warning_feature::SAFETY,
            format!("Gemini 安全拦截已拒绝生成（blockReason={block}），内容为空"),
        ));
    }

    Ok(ChatResponse {
        id: wire.response_id.unwrap_or_default(),
        model: wire.model_version.unwrap_or_default(),
        content,
        finish_reason: FinishReason {
            unified,
            raw: raw_finish,
        },
        usage: wire
            .usage_metadata
            .as_ref()
            .map(convert_usage)
            .unwrap_or_default(),
        provider_metadata: HashMap::new(),
        warnings,
    })
}

/// unified finish reason 映射（Gemini finishReason 值）。
///
/// 安全与合规类拦截（SAFETY/RECITATION/BLOCKLIST/PROHIBITED_CONTENT/
/// IMAGE_SAFETY/SPII/LANGUAGE）统一归 ContentFilter；MALFORMED_FUNCTION_CALL
/// 是生成侧错误语义归 Error；未识别值归 Other 并保留原值。
fn map_finish_reason(raw: Option<&str>) -> FinishReasonUnified {
    match raw {
        Some("STOP") => FinishReasonUnified::Stop,
        Some("MAX_TOKENS") => FinishReasonUnified::Length,
        Some("SAFETY")
        | Some("RECITATION")
        | Some("BLOCKLIST")
        | Some("PROHIBITED_CONTENT")
        | Some("IMAGE_SAFETY")
        | Some("SPII")
        | Some("LANGUAGE") => FinishReasonUnified::ContentFilter,
        Some("MALFORMED_FUNCTION_CALL") => FinishReasonUnified::Error,
        _ => FinishReasonUnified::Other,
    }
}

/// usage 四分量折算：`promptTokenCount` 含缓存（减法约定）。
///
/// Gemini 把思考 token 与可见候选 token 分开报告；两者共同构成上游实际
/// 生成量，因此必须相加后写入 IR 的 `output_tokens`。`thoughtsTokenCount`
/// 不是 `candidatesTokenCount` 的子集，省略它会使思考强度越高的请求越少计费。
fn convert_usage(usage: &Value) -> Usage {
    let prompt = usage_count(usage, "promptTokenCount", "prompt_token_count");
    let candidates = usage_count(usage, "candidatesTokenCount", "candidates_token_count");
    let thoughts = usage_count(usage, "thoughtsTokenCount", "thoughts_token_count");
    let cached = usage_count(
        usage,
        "cachedContentTokenCount",
        "cached_content_token_count",
    );
    Usage {
        input_tokens: prompt.saturating_sub(cached),
        output_tokens: candidates.saturating_add(thoughts),
        cache_read_tokens: cached,
        cache_write_tokens: 0,
        cache_write_1h_tokens: 0,
        raw: Some(usage.clone()),
    }
}

/// 读取 usage 计数字段：camelCase 与 proto JSON 的 snake_case 命名都认。
fn usage_count(usage: &Value, camel: &str, snake: &str) -> u64 {
    usage
        .get(camel)
        .or_else(|| usage.get(snake))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// 直通快路径的 usage 嗅探：从任意 JSON 值顶层取 `usageMetadata` 折算四分量。
pub fn sniff_usage(value: &Value) -> Option<Usage> {
    let usage = value
        .get("usageMetadata")
        .or_else(|| value.get("usage_metadata"))?;
    let usage_object = usage.as_object()?;
    let has_metric = [
        "promptTokenCount",
        "prompt_token_count",
        "candidatesTokenCount",
        "candidates_token_count",
        "thoughtsTokenCount",
        "thoughts_token_count",
        "cachedContentTokenCount",
        "cached_content_token_count",
    ]
    .iter()
    .any(|key| usage_object.contains_key(*key));
    if !has_metric {
        return None;
    }
    for key in [
        "promptTokenCount",
        "prompt_token_count",
        "candidatesTokenCount",
        "candidates_token_count",
        "thoughtsTokenCount",
        "thoughts_token_count",
        "cachedContentTokenCount",
        "cached_content_token_count",
    ] {
        if let Some(value) = usage_object.get(key)
            && value.as_u64().is_none()
        {
            return None;
        }
    }
    Some(convert_usage(usage))
}

/// [`sniff_usage`] 的逐帧入口：只物化 `usageMetadata` 子树，不整树建 DOM。
///
/// Gemini 每个流式 chunk 都携带累计 `usageMetadata`（中断流按已观察帧结算
/// 的语义要求逐帧嗅探），整树解析会把 candidates 与 parts 增量全部物化成
/// `Value`。探测结构只声明嗅探读取的键（camelCase 主名 + proto JSON 的
/// snake_case 别名，与 [`sniff_usage`] 的双名读取同口径），serde 流式跳过
/// 其余字段；`usageMetadata` 缺失在此短路返回，不建迷你视图。物化子树装回
/// 同形状迷你视图后复用 [`sniff_usage`] 单一逻辑，口径与整树解析严格一致。
pub fn sniff_usage_str(frame: &str) -> Option<Usage> {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(default, rename = "usageMetadata", alias = "usage_metadata")]
        usage_metadata: Option<Value>,
    }
    let probe: Probe = serde_json::from_str(frame).ok()?;
    let usage_metadata = probe.usage_metadata?;
    sniff_usage(&json!({ "usageMetadata": usage_metadata }))
}

// ---- 流式：上游 chunk → IR 流事件 ----

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

/// 流式解码器：把上游 `alt=sse` 逐 chunk 的 `GenerateContentResponse` 解码为
/// IR 流事件。
///
/// 每个 chunk 是完整响应的 part 级片段：思维链与正文增量各成一帧，块切换时
/// 先收前一块再开新块；`functionCall` 整 part 到达（无参数增量），展开为
/// tool-input 的 start/delta/end 三事件。流以服务器关闭收尾、无哨兵行。
#[derive(Debug, Default)]
pub struct StreamDecoder {
    /// `ResponseMetadata` 一次响应只发一次：chunk 重复携带的 id/model 不重复产出。
    metadata_emitted: bool,
    reasoning_open: bool,
    text_open: bool,
    /// 流内出现过 functionCall part：`STOP` 在场时 finish 归 `ToolCalls`（与
    /// 非流式同一规则，调用可能先于 finishReason 若干 chunk 到达）。
    saw_function_call: bool,
    /// Finish 只产出一次；此后仅当 usage 终值补达时再发一次。
    finish_emitted: bool,
    /// finishReason 收过后的记忆，供补达的 usage 下发终值 Finish。
    last_finish_reason: Option<FinishReason>,
    /// 最近一次出现的 usage 折算值：`usageMetadata` 逐 chunk 累计，末 chunk
    /// 的值即终值。
    last_usage: Option<Usage>,
    /// 流式解码中累积的 warnings（如无法下发的媒体 part）：经 IR 流式
    /// warnings 通道（StreamStart）上抛，不在解码路径静默丢弃。
    warnings: Vec<Warning>,
    /// 多候选告警只发一次：候选数逐 chunk 恒定，重复告警只会淹没真信号。
    candidates_warned: bool,
}

impl StreamDecoder {
    /// 解码单个上游 chunk 为若干 IR 流事件。
    ///
    /// 形状不符的 chunk 留痕后跳过（错误语义可安全提取时映射 IR Error）：
    /// 流式面对的是已建立连接的上游，单个坏块不值得整条流报废，异常流由
    /// 网关的流完整性校验归类。流内错误帧产出 IR Error，交由网关终止流。
    pub fn process(&mut self, chunk: &str) -> DecodeStreamChunk {
        let wire = match serde_json::from_str::<WireResponse>(chunk) {
            Ok(wire) => wire,
            Err(err) => {
                return DecodeStreamChunk::delivery(decode_failed_frame(
                    &err,
                    chunk,
                    ErrorMessageShape::NestedOnly,
                ));
            }
        };

        // 错误帧只产出 IR Error：网关消费到即向下游下发错误帧并终止流，
        // 其余字段（id/model 等）随流终止失去意义。
        if let Some(error) = &wire.error {
            return DecodeStreamChunk::delivery(vec![StreamEvent::Error {
                message: error.message.clone().unwrap_or_default(),
            }]);
        }

        let mut events = Vec::new();
        let mut is_output = false;

        if !self.metadata_emitted && (wire.response_id.is_some() || wire.model_version.is_some()) {
            self.metadata_emitted = true;
            events.push(StreamEvent::ResponseMetadata {
                id: wire.response_id.clone().unwrap_or_default(),
                model: wire.model_version.clone().unwrap_or_default(),
            });
        }

        if let Some(usage) = &wire.usage_metadata {
            self.last_usage = Some(convert_usage(usage));
        }

        let candidate = wire.candidates.first();
        if wire.candidates.len() > 1 && !self.candidates_warned {
            // 与请求侧 candidateCount 告警同词汇：只读取首个候选，其余丢弃
            // 属有损面，经流式 warnings 通道可观测。候选数在整条流内恒定，
            // 只告警首个多候选 chunk。
            self.candidates_warned = true;
            self.warnings.push(Warning::compatibility(
                warning_feature::N,
                format!(
                    "Gemini 响应携带 {} 个候选，仅读取首个，其余已丢弃",
                    wire.candidates.len()
                ),
            ));
            self.flush_warnings(&mut events);
        }
        if let Some(content) = candidate.and_then(|c| c.content.as_ref()) {
            for part in &content.parts {
                // 无法识别的 part 跳过：与坏块跳过同理，不因个别未知形状中断流。
                let Ok(decoded) = decode_part(part, 0, &mut Vec::new()) else {
                    continue;
                };
                match decoded {
                    ContentPart::Reasoning {
                        text,
                        provider_options,
                    } => {
                        self.close_text(&mut events);
                        if !self.reasoning_open {
                            self.reasoning_open = true;
                            events.push(StreamEvent::ReasoningStart {
                                id: "0".to_string(),
                                provider_options: HashMap::new(),
                            });
                        }
                        is_output = true;
                        // 签名可与零长增量并存：逃生舱在场时即使无文本也要下发。
                        if !text.is_empty() || !provider_options.is_empty() {
                            events.push(StreamEvent::ReasoningDelta {
                                id: "0".to_string(),
                                delta: text,
                                provider_options,
                            });
                        }
                    }
                    ContentPart::Text {
                        text,
                        provider_options,
                    } => {
                        self.close_reasoning(&mut events);
                        if !self.text_open {
                            self.text_open = true;
                            events.push(StreamEvent::TextStart {
                                id: "0".to_string(),
                                provider_options: HashMap::new(),
                            });
                        }
                        is_output = true;
                        if !text.is_empty() || !provider_options.is_empty() {
                            events.push(StreamEvent::TextDelta {
                                id: "0".to_string(),
                                delta: text,
                                provider_options,
                            });
                        }
                    }
                    ContentPart::ToolCall {
                        tool_call_id,
                        tool_name,
                        input,
                        provider_options,
                    } => {
                        self.close_reasoning(&mut events);
                        self.close_text(&mut events);
                        is_output = true;
                        self.saw_function_call = true;
                        events.push(StreamEvent::ToolInputStart {
                            id: tool_call_id.clone(),
                            tool_name,
                            provider_options,
                        });
                        events.push(StreamEvent::ToolInputDelta {
                            id: tool_call_id.clone(),
                            delta: input.to_string(),
                            provider_options: HashMap::new(),
                        });
                        events.push(StreamEvent::ToolInputEnd {
                            id: tool_call_id,
                            provider_options: HashMap::new(),
                        });
                    }
                    // 响应流不承载媒体与工具结果 part：媒体丢弃时经流式 warnings
                    // 通道显式告警，不再静默；工具结果为空载荷保持静默。
                    ContentPart::Media { media_type, .. } => {
                        self.warnings.push(Warning::unsupported(
                            warning_feature::MEDIA,
                            format!("响应流不承载媒体内容（{media_type}），已丢弃"),
                        ));
                        self.flush_warnings(&mut events);
                    }
                    _ => {}
                }
            }
        }

        // 安全拦截：blockReason 在场且无候选时对流上是终态——产出 warning +
        // Finish(ContentFilter) 让流非空且拦截原因可感知；否则零事件会被
        // 网关 peek 误判为流中断而空转切换渠道。拦截 chunk 即终末帧，处理完
        // 提前返回（同 chunk 的 usageMetadata 已并入，不再触发补达 Finish）。
        let block_reason = wire
            .prompt_feedback
            .as_ref()
            .and_then(|feedback| feedback.block_reason.clone())
            .filter(|_| wire.candidates.is_empty());
        if let Some(block) = block_reason {
            self.warnings.push(Warning::compatibility(
                warning_feature::SAFETY,
                format!("Gemini 安全拦截已拒绝生成（blockReason={block}），内容为空"),
            ));
            self.flush_warnings(&mut events);
            self.close_reasoning(&mut events);
            self.close_text(&mut events);
            self.last_finish_reason = Some(FinishReason {
                unified: FinishReasonUnified::ContentFilter,
                raw: Some(block),
            });
            if !self.finish_emitted {
                self.finish_emitted = true;
                events.push(StreamEvent::Finish {
                    finish_reason: self.last_finish_reason.clone().unwrap_or(FinishReason {
                        unified: FinishReasonUnified::Other,
                        raw: None,
                    }),
                    usage: self.last_usage.clone().unwrap_or_default(),
                    provider_metadata: HashMap::new(),
                });
            }
            return DecodeStreamChunk { events, is_output };
        }

        // Finish 由 finishReason 触发：usage 逐 chunk 累计，中途出现不构成流
        // 终止信号。finishReason 先到而 usage 终值后补时，以第二次 Finish 下发
        // 终值（流以服务器关闭收尾，此后无更多内容）。
        let has_finish_reason = candidate.is_some_and(|c| c.finish_reason.is_some());
        if has_finish_reason {
            self.close_reasoning(&mut events);
            self.close_text(&mut events);
            let raw = candidate.and_then(|c| c.finish_reason.clone());
            let unified = match raw.as_deref() {
                Some("STOP") if self.saw_function_call => FinishReasonUnified::ToolCalls,
                _ => map_finish_reason(raw.as_deref()),
            };
            self.last_finish_reason = Some(FinishReason { unified, raw });
            if !self.finish_emitted {
                self.finish_emitted = true;
                events.push(StreamEvent::Finish {
                    finish_reason: self.last_finish_reason.clone().unwrap_or(FinishReason {
                        unified: FinishReasonUnified::Other,
                        raw: None,
                    }),
                    usage: self.last_usage.clone().unwrap_or_default(),
                    provider_metadata: HashMap::new(),
                });
            }
        } else if self.finish_emitted && wire.usage_metadata.is_some() {
            events.push(StreamEvent::Finish {
                finish_reason: self.last_finish_reason.clone().unwrap_or(FinishReason {
                    unified: FinishReasonUnified::Other,
                    raw: None,
                }),
                usage: self.last_usage.clone().unwrap_or_default(),
                provider_metadata: HashMap::new(),
            });
        }

        DecodeStreamChunk { events, is_output }
    }

    /// 把累积的流式解码 warnings 以 StreamStart 事件上抛（IR 流式 warnings
    /// 通道）：各协议入站编码器将其转为 warnings 帧下发，流式累积路径并入
    /// 响应 warnings。流中 warnings 发现晚于首帧时允许 StreamStart 再次出现。
    fn flush_warnings(&mut self, events: &mut Vec<StreamEvent>) {
        if !self.warnings.is_empty() {
            let warnings = std::mem::take(&mut self.warnings);
            events.push(StreamEvent::StreamStart { warnings });
        }
    }

    fn close_reasoning(&mut self, events: &mut Vec<StreamEvent>) {
        if self.reasoning_open {
            self.reasoning_open = false;
            events.push(StreamEvent::ReasoningEnd {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            });
        }
    }

    fn close_text(&mut self, events: &mut Vec<StreamEvent>) {
        if self.text_open {
            self.text_open = false;
            events.push(StreamEvent::TextEnd {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            });
        }
    }
}

// ---- 流式：IR 流事件 → 入站 SSE 帧 ----

/// 把 IR 流事件编码为入站 Gemini 流 chunk（`data:` 行、无事件名、无哨兵行）。
///
/// Gemini 的 chunk 是无状态片段：文本/思维链增量各成一帧 part；工具调用无
/// 参数增量，参数缓冲到 `ToolInputEnd` 整块成 part 下发；Finish 以末 chunk 的
/// `finishReason` + `usageMetadata` 收尾，流随后由服务器关闭。
#[derive(Debug, Default)]
pub struct StreamEncoder {
    /// 从 ResponseMetadata 记录的响应 id 与 model，逐 chunk 携带。
    id: String,
    model: String,
    /// 入站模型名覆盖：别名命中时，出站响应模型名须重写回入站短名。
    inbound_model: Option<String>,
    /// 进行中的工具调用（wire 无参数增量，缓冲到收尾一次性成块）。
    open_tools: Vec<OpenToolCall>,
    /// 思维链开始事件携带的逃生舱（如签名）：随首个增量 part 下发。
    reasoning_start_options: HashMap<String, Value>,
}

/// 入站侧进行中的工具调用。
#[derive(Debug)]
struct OpenToolCall {
    id: String,
    tool_name: String,
    arguments: String,
    provider_options: HashMap<String, Value>,
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
            // warnings 以独立首帧下发，让下游在收到任何内容前就感知信息损失。
            StreamEvent::StreamStart { warnings } => {
                match crate::core::openai_chat::encode_warnings(warnings) {
                    Some(gateway) => vec![SseFrame::data(
                        json!({ "candidates": [], "gateway": gateway }).to_string(),
                    )],
                    None => Vec::new(),
                }
            }
            StreamEvent::ResponseMetadata { id, model } => {
                self.id = id.clone();
                self.model = model.clone();
                Vec::new()
            }
            StreamEvent::TextStart { .. } | StreamEvent::TextEnd { .. } => Vec::new(),
            StreamEvent::TextDelta {
                delta,
                provider_options,
                ..
            } => {
                if delta.is_empty() && provider_options.is_empty() {
                    return Vec::new();
                }
                self.part_frame(ContentPart::Text {
                    text: delta.clone(),
                    provider_options: provider_options.clone(),
                })
            }
            StreamEvent::ReasoningStart {
                provider_options, ..
            } => {
                // 签名可能在开始事件先行到达（非流式响应的流式回放形状）：
                // 缓存到首个增量 part 上下发。
                self.reasoning_start_options = provider_options.clone();
                Vec::new()
            }
            StreamEvent::ReasoningDelta {
                delta,
                provider_options,
                ..
            } => {
                let mut options = std::mem::take(&mut self.reasoning_start_options);
                merge_provider_options(&mut options, provider_options.clone());
                if delta.is_empty() && options.is_empty() {
                    return Vec::new();
                }
                self.part_frame(ContentPart::Reasoning {
                    text: delta.clone(),
                    provider_options: options,
                })
            }
            StreamEvent::ReasoningEnd { .. } => Vec::new(),
            StreamEvent::ToolInputStart {
                id,
                tool_name,
                provider_options,
            } => {
                self.open_tools.push(OpenToolCall {
                    id: id.clone(),
                    tool_name: tool_name.clone(),
                    arguments: String::new(),
                    provider_options: provider_options.clone(),
                });
                Vec::new()
            }
            StreamEvent::ToolInputDelta { id, delta, .. } => {
                if let Some(tool) = self.open_tools.iter_mut().find(|tool| tool.id == *id) {
                    tool.arguments.push_str(delta);
                }
                Vec::new()
            }
            StreamEvent::ToolInputEnd { id, .. } => {
                let Some(index) = self.open_tools.iter().position(|tool| tool.id == *id) else {
                    return Vec::new();
                };
                let tool = self.open_tools.remove(index);
                // functionCall.args 必须是对象：无增量（空参数调用）合法，
                // 残缺或非对象参数兜底为 {} 并先下发告警帧（与 chat 请求侧
                // 同词汇），兼容处理可观测。
                let (input, malformed) = if tool.arguments.trim().is_empty() {
                    (json!({}), false)
                } else {
                    match serde_json::from_str::<Value>(&tool.arguments) {
                        Ok(input @ Value::Object(_)) => (input, false),
                        _ => (json!({}), true),
                    }
                };
                let mut frames = Vec::new();
                if malformed {
                    let warning = Warning::compatibility(
                        warning_feature::TOOL_ARGUMENTS,
                        format!(
                            "tool call {} 的流式参数非合法 JSON 对象，已兜底为空对象",
                            tool.tool_name
                        ),
                    );
                    if let Some(gateway) = crate::core::openai_chat::encode_warnings(&[warning]) {
                        frames.push(SseFrame::data(
                            json!({ "candidates": [], "gateway": gateway }).to_string(),
                        ));
                    }
                }
                frames.extend(self.part_frame(ContentPart::ToolCall {
                    tool_call_id: tool.id,
                    tool_name: tool.tool_name,
                    input,
                    provider_options: tool.provider_options,
                }));
                frames
            }
            StreamEvent::ToolCall {
                tool_call_id,
                tool_name,
                input,
                provider_options,
            } => self.part_frame(ContentPart::ToolCall {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                input: input.clone(),
                provider_options: provider_options.clone(),
            }),
            StreamEvent::Finish {
                finish_reason,
                usage,
                ..
            } => {
                let mut candidate = Map::new();
                candidate.insert(
                    "finishReason".into(),
                    json!(encode_finish_reason(finish_reason)),
                );
                candidate.insert("index".into(), json!(0));
                let mut obj = Map::new();
                obj.insert(
                    "candidates".into(),
                    Value::Array(vec![Value::Object(candidate)]),
                );
                obj.insert("usageMetadata".into(), encode_usage_metadata(usage));
                self.attach_metadata(&mut obj);
                vec![SseFrame::data(Value::Object(obj).to_string())]
            }
            // 流内错误以独立 `data:` 帧下发错误 JSON（与网关兜底错误帧同形状），
            // 由调用方感知并终止流。
            StreamEvent::Error { message } => vec![stream_error_frame(message)],
        }
    }

    /// 把单个 IR part 编码为一条 chunk 帧；无法表达的 part（Custom）不产帧。
    fn part_frame(&self, part: ContentPart) -> Vec<SseFrame> {
        let Some(block) = encode_part(&part, &mut Vec::new()) else {
            return Vec::new();
        };
        let mut obj = Map::new();
        obj.insert(
            "candidates".into(),
            json!([{
                "content": { "role": "model", "parts": [block] },
                "index": 0,
            }]),
        );
        self.attach_metadata(&mut obj);
        vec![SseFrame::data(Value::Object(obj).to_string())]
    }

    /// 为 chunk 补响应元数据：`modelVersion`（别名覆盖）与 `responseId` 在
    /// 已知时逐 chunk 携带，与官方流形状一致。
    fn attach_metadata(&self, obj: &mut Map<String, Value>) {
        let model = self.inbound_model.as_deref().unwrap_or(&self.model);
        if !model.is_empty() {
            obj.insert("modelVersion".into(), json!(model));
        }
        if !self.id.is_empty() {
            obj.insert("responseId".into(), json!(self.id));
        }
    }
}

// ---- 错误编码 ----

/// 编码为 Gemini 错误格式 `{"error":{code,message,status}}`。
///
/// `status` 是 google.rpc.Code 枚举名，按 HTTP 状态码映射；流内错误（固定
/// 500）与网关兜底路径共用本形状。
pub fn encode_error(status: u16, message: &str) -> Value {
    let status_name = match status {
        400 => "INVALID_ARGUMENT",
        401 => "UNAUTHENTICATED",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        409 => "ABORTED",
        429 => "RESOURCE_EXHAUSTED",
        499 => "CANCELLED",
        500 => "INTERNAL",
        501 => "UNIMPLEMENTED",
        503 => "UNAVAILABLE",
        504 => "DEADLINE_EXCEEDED",
        _ => "UNKNOWN",
    };
    json!({
        "error": {
            "code": status,
            "message": message,
            "status": status_name,
        }
    })
}

/// 流内错误的入站 SSE 帧（500 语义）。流式编码器消费 IR Error 事件与网关
/// 兜底路径共用，保证形状一致。
pub fn stream_error_frame(message: &str) -> SseFrame {
    SseFrame::data(encode_error(500, message).to_string())
}

/// 编码为 Gemini `GET /v1beta/models` 列表：`models[].name` 带 `models/`
/// 前缀的官方形状。
pub fn encode_model_list(ids: &[String]) -> Value {
    let models: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({
                "name": format!("models/{id}"),
                "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
            })
        })
        .collect();
    json!({ "models": models })
}

// ---- 响应编码：IR → wire ----

/// 编码 IR 响应为 generateContent 响应体。
///
/// 转换 warnings 以顶层 `gateway.warnings` 暴露（Gemini 无标准 warnings
/// 字段，属无害扩展，未知字段会被 SDK 忽略）。
pub fn encode_response(response: &ChatResponse) -> Value {
    let parts: Vec<Value> = response
        .content
        .iter()
        .filter_map(|part| encode_part(part, &mut Vec::new()))
        .collect();

    let mut candidate = Map::new();
    candidate.insert("content".into(), json!({ "role": "model", "parts": parts }));
    candidate.insert(
        "finishReason".into(),
        json!(encode_finish_reason(&response.finish_reason)),
    );
    candidate.insert("index".into(), json!(0));

    let mut obj = Map::new();
    obj.insert("candidates".into(), json!([Value::Object(candidate)]));
    obj.insert(
        "usageMetadata".into(),
        encode_usage_metadata(&response.usage),
    );
    if !response.model.is_empty() {
        obj.insert("modelVersion".into(), json!(response.model));
    }
    if !response.id.is_empty() {
        obj.insert("responseId".into(), json!(response.id));
    }
    if let Some(gateway) = crate::core::openai_chat::encode_warnings(&response.warnings) {
        obj.insert("gateway".into(), gateway);
    }
    Value::Object(obj)
}

/// IR 四分量 usage → `usageMetadata`（加法约定回写：`promptTokenCount` 含缓存）。
fn encode_usage_metadata(usage: &Usage) -> Value {
    let cached = usage.cache_read_tokens;
    let prompt = usage.input_tokens + cached + usage.cache_write_tokens;
    let mut obj = Map::new();
    obj.insert("promptTokenCount".into(), json!(prompt));
    obj.insert("candidatesTokenCount".into(), json!(usage.output_tokens));
    obj.insert(
        "totalTokenCount".into(),
        json!(prompt + usage.output_tokens),
    );
    if cached > 0 {
        obj.insert("cachedContentTokenCount".into(), json!(cached));
    }
    Value::Object(obj)
}

/// 把 IR unified finish reason 映射为 Gemini finishReason。
///
/// `Error`/`Other` 不折为 STOP：STOP 语义是「模型自然完成」，把错误或未知
/// 终态谎报为自然完成会让下游误判。两者都映射为官方枚举的 `OTHER`（Gemini
/// 合法值，表示「其他原因终止」）——不用 `MALFORMED_FUNCTION_CALL`，它指称
/// 「模型发起的工具调用畸形」这一特定成因，IR `Error` 是泛化错误语义（如
/// 上游中途失败），乱用会让下游误判为工具调用问题。
fn encode_finish_reason(finish_reason: &FinishReason) -> &'static str {
    match finish_reason.unified {
        FinishReasonUnified::Stop => "STOP",
        FinishReasonUnified::Length => "MAX_TOKENS",
        FinishReasonUnified::ContentFilter => "SAFETY",
        // Gemini 无「已完成工具调用」的独立终止原因：函数请求以 STOP 收尾。
        FinishReasonUnified::ToolCalls => "STOP",
        FinishReasonUnified::Error | FinishReasonUnified::Other => "OTHER",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::frames_to_snapshot;
    use similar_asserts::assert_eq;

    /// 带思维链、媒体、工具轮与工具声明的完整请求（fixture 黄金样例）。
    fn fixture_request() -> Value {
        serde_json::from_str(include_str!("__fixtures__/request_generate_content.json"))
            .expect("fixture 应为合法 JSON")
    }

    /// 带文本、思维链与工具调用的响应（fixture 黄金样例）。
    fn fixture_response() -> Value {
        serde_json::from_str(include_str!("__fixtures__/response_generate_content.json"))
            .expect("fixture 应为合法 JSON")
    }

    /// fixture 同族往返恒等：解码 → 编码 → 与原 wire 逐字段相等，且零告警。
    #[test]
    fn request_fixture_survives_same_family_roundtrip() {
        let wire = fixture_request();
        let request = decode_request(&wire).expect("fixture 应能解码");
        let mut warnings = Vec::new();
        let reencoded = encode_request(&request, &mut warnings);
        assert!(warnings.is_empty(), "同族往返不应有 warning: {warnings:?}");
        assert_eq!(reencoded, wire, "同族往返应逐字段还原");
    }

    /// fixture 响应同族往返恒等。
    #[test]
    fn response_fixture_survives_same_family_roundtrip() {
        let wire = fixture_response();
        let response = decode_response(&wire).expect("fixture 应能解码");
        let reencoded = encode_response(&response);
        assert_eq!(reencoded, wire, "同族往返应逐字段还原");
    }

    /// 模型名不属请求体：由网关从 URL 路径注入 IR，出站不再写回 body
    /// （写回会与路径上的真实模型名冲突）。
    #[test]
    fn model_lives_in_path_not_in_request_body() {
        let mut wire = fixture_request();
        wire["model"] = json!("gemini-2.5-pro");
        let request = decode_request(&wire).expect("注入模型名后应能解码");
        assert_eq!(request.model, "gemini-2.5-pro");

        let mut warnings = Vec::new();
        let reencoded = encode_request(&request, &mut warnings);
        assert!(
            reencoded.get("model").is_none(),
            "出站请求体不应携带模型名: {reencoded}"
        );
        assert!(warnings.is_empty());
    }

    /// proto JSON 命名（snake_case）的请求与 usage 字段同样可解码并同族还原：
    /// 官方 SDK 的 REST 传输用 proto JSON 命名发送同一批字段。
    #[test]
    fn snake_case_field_aliases_decode_and_roundtrip() {
        let wire = json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "system_instruction": { "parts": [{ "text": "你是助手" }] },
            "generation_config": { "max_output_tokens": 64, "thinking_config": { "thinking_budget": 1024 } },
            "tool_config": {
                "function_calling_config": {
                    "mode": "ANY",
                    "allowed_function_names": ["get_weather"]
                }
            }
        });
        let request = decode_request(&wire).expect("snake_case 形状应能解码");
        assert_eq!(request.max_tokens, Some(64));
        assert_eq!(request.reasoning, Some(ReasoningEffort::Low));
        assert_eq!(text_parts(&request.messages[0].content), "你是助手");
        assert_eq!(
            request.tool_choice,
            Some(ToolChoice::Tool {
                name: "get_weather".to_string()
            })
        );

        let snake_usage = convert_usage(&json!({
            "prompt_token_count": 100,
            "candidates_token_count": 20,
            "cached_content_token_count": 30,
        }));
        assert_eq!(snake_usage.input_tokens, 70);
        assert_eq!(snake_usage.cache_read_tokens, 30);
    }

    /// 显式调用 id 的往返：上游给了 id 时原样回写；没给（id 由本模块按
    /// 名字与入参生成）时不回写，避免请求体凭空多出客户端没带的字段。
    #[test]
    fn explicit_tool_ids_roundtrip_and_generated_ids_stay_internal() {
        let with_id = json!({
            "contents": [
                { "role": "model", "parts": [
                    { "functionCall": { "id": "native-1", "name": "get_weather", "args": {} } }
                ] },
                { "role": "user", "parts": [
                    { "functionResponse": { "id": "native-1", "name": "get_weather", "response": { "result": "晴" } } }
                ] }
            ]
        });
        let request = decode_request(&with_id).expect("显式 id 应能解码");
        let mut warnings = Vec::new();
        let wire = encode_request(&request, &mut warnings);
        assert_eq!(
            wire["contents"][0]["parts"][0]["functionCall"]["id"],
            json!("native-1")
        );
        assert_eq!(
            wire["contents"][1]["parts"][0]["functionResponse"]["id"],
            json!("native-1")
        );
        assert!(warnings.is_empty());

        let without_id = json!({
            "contents": [{ "role": "model", "parts": [
                { "functionCall": { "name": "get_weather", "args": { "city": "上海" } } }
            ] }]
        });
        let request = decode_request(&without_id).expect("应能解码");
        let wire = encode_request(&request, &mut warnings);
        assert!(
            wire["contents"][0]["parts"][0]["functionCall"]
                .get("id")
                .is_none(),
            "自生成的 id 不应回写: {wire}"
        );
        assert!(warnings.is_empty());
    }

    /// fixture 解码后的 IR 语义面：system 提升、消息角色、思维链签名、
    /// 工具调用入参与工具声明。
    #[test]
    fn fixture_decodes_to_expected_ir() {
        let mut wire = fixture_request();
        wire["model"] = json!("gemini-2.5-pro");
        let request = decode_request(&wire).expect("fixture 应能解码");
        assert_eq!(request.model, "gemini-2.5-pro");

        let system = &request.messages[0];
        assert_eq!(system.role, Role::System);
        assert_eq!(text_parts(&system.content), "你是天气助手。");

        let user = &request.messages[1];
        assert_eq!(user.role, Role::User);
        assert!(matches!(
            &user.content[0],
            ContentPart::Text { text, .. } if text == "上海天气？"
        ));

        // 思维链 part 与签名逃生舱同在。
        let assistant = &request.messages[2];
        assert_eq!(assistant.role, Role::Assistant);
        match &assistant.content[0] {
            ContentPart::Reasoning {
                text,
                provider_options,
            } => {
                assert_eq!(text, "先查天气预报接口。");
                assert_eq!(
                    provider_options[PROVIDER_KEY][THOUGHT_SIGNATURE_KEY],
                    json!("sig_thought")
                );
            }
            other => panic!("应为思维链 part，实际 {other:?}"),
        }
        // 工具调用：入参是对象（非字符串），id 为稳定生成值。
        match &assistant.content[1] {
            ContentPart::ToolCall {
                tool_call_id,
                tool_name,
                input,
                ..
            } => {
                assert_eq!(tool_name, "get_weather");
                assert_eq!(input, &json!({ "city": "上海" }));
                assert_eq!(tool_call_id, &stable_tool_call_id("get_weather", input));
            }
            other => panic!("应为工具调用 part，实际 {other:?}"),
        }

        // 工具结果按名字与前文调用配对，output 为整个 response 对象。
        let tool = &request.messages[3];
        assert_eq!(tool.role, Role::Tool);
        match &tool.content[0] {
            ContentPart::ToolResult {
                tool_call_id,
                tool_name,
                output,
                ..
            } => {
                assert_eq!(tool_name, "get_weather");
                assert_eq!(output, &json!({ "result": "晴，26 度" }));
                assert_eq!(
                    tool_call_id,
                    &stable_tool_call_id("get_weather", &json!({ "city": "上海" }))
                );
            }
            other => panic!("应为工具结果 part，实际 {other:?}"),
        }

        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, "get_weather");
        assert_eq!(
            request.tools[0].description.as_deref(),
            Some("查询城市天气")
        );
        assert_eq!(
            request.tools[0].parameters,
            Some(json!({ "type": "object", "properties": { "city": { "type": "string" } } }))
        );
        assert_eq!(request.tool_choice, Some(ToolChoice::Auto));
        assert_eq!(request.max_tokens, Some(512));
        assert_eq!(request.temperature, Some(0.5));
        assert_eq!(request.top_k, Some(40));
        assert_eq!(request.stop, vec!["END".to_string()]);
        assert_eq!(request.reasoning, Some(ReasoningEffort::Medium));
        assert_eq!(
            request.provider_options[PROVIDER_KEY][SAFETY_SETTINGS_KEY],
            json!([{ "category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "BLOCK_ONLY_HIGH" }])
        );
    }

    /// 工具调用 id 的稳定性：同一（名字, 入参）在任何轮次都得到同一 id，
    /// 工具结果才能按名字对号入座。
    #[test]
    fn tool_call_id_is_stable_across_replays() {
        let input = json!({ "city": "上海" });
        let first = stable_tool_call_id("get_weather", &input);
        let second = stable_tool_call_id("get_weather", &input);
        assert_eq!(first, second);
        assert_ne!(first, stable_tool_call_id("get_weather", &json!({})));
        assert_ne!(first, stable_tool_call_id("get_time", &input));
    }

    /// 多个同名调用按出现顺序与结果配对。
    #[test]
    fn same_name_tool_calls_pair_with_results_in_order() {
        let wire = json!({
            "contents": [
                { "role": "model", "parts": [
                    { "functionCall": { "name": "get_weather", "args": { "city": "上海" } } },
                    { "functionCall": { "name": "get_weather", "args": { "city": "北京" } } }
                ] },
                { "role": "user", "parts": [
                    { "functionResponse": { "name": "get_weather", "response": { "result": "晴" } } },
                    { "functionResponse": { "name": "get_weather", "response": { "result": "霾" } } }
                ] }
            ]
        });
        let request = decode_request(&wire).expect("应能解码");
        let shanghai = stable_tool_call_id("get_weather", &json!({ "city": "上海" }));
        let beijing = stable_tool_call_id("get_weather", &json!({ "city": "北京" }));
        let results: Vec<(&str, &str)> = request.messages[1..]
            .iter()
            .filter_map(|message| match &message.content[0] {
                ContentPart::ToolResult {
                    tool_call_id,
                    output,
                    ..
                } => Some((tool_call_id.as_str(), output["result"].as_str()?)),
                _ => None,
            })
            .collect();
        assert_eq!(
            results,
            vec![(shanghai.as_str(), "晴"), (beijing.as_str(), "霾")],
            "同名调用应按出现顺序与结果配对"
        );
    }

    /// usage 减法约定：`promptTokenCount` 含缓存，输入侧扣除后计费。
    #[test]
    fn usage_subtracts_cached_content_from_prompt() {
        let usage = convert_usage(&json!({
            "promptTokenCount": 100,
            "candidatesTokenCount": 20,
            "cachedContentTokenCount": 30,
            "thoughtsTokenCount": 8,
        }));
        assert_eq!(usage.input_tokens, 70);
        assert_eq!(
            usage.output_tokens, 28,
            "候选与思考 token 都属于输出侧计费量"
        );
        assert_eq!(usage.cache_read_tokens, 30);
        assert_eq!(usage.cache_write_tokens, 0);

        // 嗅探在直通路径上从顶层 usageMetadata 取值，与 IR 路径同口径。
        let sniffed = sniff_usage(
            &json!({ "usageMetadata": { "promptTokenCount": 100, "cachedContentTokenCount": 30 } }),
        )
        .expect("应能嗅探 usage");
        assert_eq!(sniffed.input_tokens, 70);
        assert!(
            sniff_usage(&json!({ "usageMetadata": { "promptTokenCount": "invalid" } })).is_none()
        );
        assert!(sniff_usage(&json!({ "usageMetadata": { "unrelated": 1 } })).is_none());
    }

    /// 逐帧嗅探入口与 Value 版同口径：chunk 携带 parts 增量与累计
    /// usageMetadata 时只提取计费分量；snake_case 别名同样生效；坏 JSON
    /// 返回 None。
    #[test]
    fn sniff_usage_str_matches_value_sniff() {
        let chunk = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"hi"}]}}],"usageMetadata":{"promptTokenCount":130,"candidatesTokenCount":40,"thoughtsTokenCount":10,"cachedContentTokenCount":30}}"#;
        let usage = sniff_usage_str(chunk).expect("chunk 应提取 usage");
        assert_eq!(usage.input_tokens, 100, "input = prompt - cached");
        assert_eq!(usage.output_tokens, 50, "输出 = 候选 + 思考");

        // proto JSON 的 snake_case 别名。
        let snake = r#"{"usage_metadata":{"prompt_token_count":100,"candidates_token_count":2}}"#;
        let usage = sniff_usage_str(snake).expect("snake_case 别名应提取");
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 2);

        assert!(sniff_usage_str(r#"{"candidates":[]}"#).is_none());
        assert!(sniff_usage_str("not json").is_none());
    }

    /// finish 双轨：functionCall part 在场时 STOP 归 ToolCalls；
    /// 其余按官方枚举映射，未知值归 Other 且保留原值。
    #[test]
    fn finish_reason_maps_with_function_call_rule() {
        let with_call = decode_response(&json!({
            "candidates": [{
                "content": { "role": "model", "parts": [
                    { "functionCall": { "name": "get_weather", "args": {} } }
                ] },
                "finishReason": "STOP"
            }]
        }))
        .expect("应能解码");
        assert_eq!(
            with_call.finish_reason,
            FinishReason {
                unified: FinishReasonUnified::ToolCalls,
                raw: Some("STOP".to_string()),
            }
        );

        for (raw, unified) in [
            ("STOP", FinishReasonUnified::Stop),
            ("MAX_TOKENS", FinishReasonUnified::Length),
            ("SAFETY", FinishReasonUnified::ContentFilter),
            ("RECITATION", FinishReasonUnified::ContentFilter),
            ("SOMETHING_NEW", FinishReasonUnified::Other),
        ] {
            let response = decode_response(&json!({
                "candidates": [{ "content": { "role": "model", "parts": [{ "text": "ok" }] }, "finishReason": raw }]
            }))
            .expect("应能解码");
            assert_eq!(
                response.finish_reason.unified, unified,
                "finishReason {raw}"
            );
            assert_eq!(response.finish_reason.raw.as_deref(), Some(raw));
        }
    }

    /// 未知角色与未知 mode 在入站面拒绝，错误信息指明位置。
    #[test]
    fn unknown_shapes_are_rejected_at_ingress() {
        let unknown_role = json!({
            "contents": [{ "role": "system", "parts": [{ "text": "hi" }] }]
        });
        let err = decode_request(&unknown_role).expect_err("未知角色应被拒绝");
        assert!(
            err.to_string().contains("contents[0]"),
            "错误应指明位置: {err}"
        );

        let unknown_part = json!({ "contents": [{ "role": "user", "parts": [{ "foo": 1 }] }] });
        assert!(
            decode_request(&unknown_part).is_err(),
            "无法识别的 part 应被拒绝"
        );

        let unknown_mode = json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "toolConfig": { "functionCallingConfig": { "mode": "FORCED" } }
        });
        let err = decode_request(&unknown_mode).expect_err("未知 mode 应被拒绝");
        assert!(err.to_string().contains("mode"), "错误应指明字段: {err}");
    }

    /// 无承载的旋钮与未知字段：前者告警丢弃，后者进出逃生舱往返。
    #[test]
    fn unsupported_knobs_warn_and_unknown_fields_roundtrip() {
        let mut request = ChatRequest {
            model: "gemini-2.5-pro".to_string(),
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
            n: Some(4),
            stop: Vec::new(),
            presence_penalty: Some(0.5),
            frequency_penalty: Some(0.25),
            seed: Some(7),
            response_format: None,
            tools: Vec::new(),
            tool_choice: Some(ToolChoice::Required),
            parallel_tool_calls: Some(false),
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
        };
        let mut warnings = Vec::new();
        let wire = encode_request(&request, &mut warnings);
        assert_eq!(
            wire["toolConfig"],
            json!({ "functionCallingConfig": { "mode": "ANY" } })
        );
        // seed 走官方 generationConfig.seed 承载，不再是丢弃项。
        assert_eq!(wire["generationConfig"]["seed"], json!(7));
        let features: Vec<&str> = warnings
            .iter()
            .map(|warning| match warning {
                Warning::Unsupported { feature, .. } => feature.as_str(),
                other => panic!("应全部为 unsupported: {other:?}"),
            })
            .collect();
        assert_eq!(
            features,
            vec![
                warning_feature::N,
                warning_feature::PARALLEL_TOOL_CALLS,
                warning_feature::PRESENCE_PENALTY,
                warning_feature::FREQUENCY_PENALTY,
            ]
        );

        // 未知顶层字段：入站收进逃生舱，同族出站原样回写。
        let inbound = json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "labels": { "env": "test" }
        });
        let decoded = decode_request(&inbound).expect("应能解码");
        assert_eq!(
            decoded.provider_options[PROVIDER_KEY][PROVIDER_EXTRA_KEY],
            json!({ "labels": { "env": "test" } })
        );
        request.provider_options = decoded.provider_options;
        let mut warnings = Vec::new();
        let reencoded = encode_request(&request, &mut warnings);
        assert!(warnings.iter().all(|warning| !matches!(
            warning,
            Warning::Unsupported { feature, .. } if feature == warning_feature::UNKNOWN_FIELDS
        )));
        assert_eq!(reencoded["labels"], json!({ "env": "test" }));
    }

    /// 类型化 effort 档位在逃生舱缺席时展开为 thinkingConfig budget。
    #[test]
    fn typed_effort_expands_to_thinking_budget() {
        let mut request = ChatRequest {
            model: "gemini-2.5-pro".to_string(),
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
            reasoning: Some(ReasoningEffort::High),
            provider_options: HashMap::new(),
            warnings: Vec::new(),
        };
        let mut warnings = Vec::new();
        let wire = encode_request(&request, &mut warnings);
        assert_eq!(
            wire["generationConfig"]["thinkingConfig"],
            json!({ "thinkingBudget": 24576 })
        );

        // 逃生舱在场时以原始配置为准，类型化档位不双写。
        request.provider_options = [(
            PROVIDER_KEY.to_string(),
            json!({ THINKING_CONFIG_KEY: { "thinkingLevel": "low" } }),
        )]
        .into_iter()
        .collect();
        let wire = encode_request(&request, &mut warnings);
        assert_eq!(
            wire["generationConfig"]["thinkingConfig"],
            json!({ "thinkingLevel": "low" })
        );
    }

    /// 媒体 part 双向：`inlineData` 为 base64 字节，`fileData` 为 URL 引用，
    /// 两种载体各自往返还原。
    #[test]
    fn media_parts_roundtrip_between_inline_and_file_sources() {
        let wire = json!({
            "contents": [{ "role": "user", "parts": [
                { "inlineData": { "mimeType": "image/png", "data": "aGVsbG8=" } },
                { "fileData": { "mimeType": "application/pdf", "fileUri": "https://example.com/a.pdf" } }
            ] }]
        });
        let request = decode_request(&wire).expect("应能解码");
        assert_eq!(
            request.messages[0].content[0],
            ContentPart::Media {
                media_type: "image/png".to_string(),
                data: MediaSource::Data {
                    base64: "aGVsbG8=".to_string()
                },
                provider_options: HashMap::new(),
            }
        );
        assert_eq!(
            request.messages[0].content[1],
            ContentPart::Media {
                media_type: "application/pdf".to_string(),
                data: MediaSource::Url {
                    url: "https://example.com/a.pdf".to_string()
                },
                provider_options: HashMap::new(),
            }
        );
        let mut warnings = Vec::new();
        assert_eq!(encode_request(&request, &mut warnings), wire);
        assert!(warnings.is_empty());
    }

    /// `responseMimeType`/`responseSchema` 与 IR `response_format`（chat 形状）
    /// 双向互认：`application/json` ↔ `json_object`，带 schema 时 ↔ `json_schema`。
    #[test]
    fn json_output_format_maps_both_directions() {
        let plain = json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "generationConfig": { "responseMimeType": "application/json" }
        });
        let request = decode_request(&plain).expect("应能解码");
        assert_eq!(
            request.response_format,
            Some(json!({ "type": "json_object" }))
        );
        let mut warnings = Vec::new();
        assert_eq!(
            encode_request(&request, &mut warnings)["generationConfig"]["responseMimeType"],
            json!("application/json")
        );

        let schema = json!({ "type": "object", "properties": { "city": { "type": "string" } } });
        let with_schema = json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseSchema": schema
            }
        });
        let request = decode_request(&with_schema).expect("应能解码");
        assert_eq!(
            request.response_format,
            Some(json!({ "type": "json_schema", "json_schema": { "schema": schema } }))
        );
        let wire = encode_request(&request, &mut warnings);
        assert_eq!(wire["generationConfig"]["responseSchema"], schema);
        assert!(warnings.is_empty());
    }

    /// 工具结果并入下一条 user content；末尾无后续时单独成一条 user content。
    #[test]
    fn tool_results_merge_into_following_user_content() {
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentPart::Text {
                    text: "上海天气？".to_string(),
                    provider_options: HashMap::new(),
                }],
                provider_options: HashMap::new(),
            },
            Message {
                role: Role::Tool,
                content: vec![ContentPart::ToolResult {
                    tool_call_id: "call_1".to_string(),
                    tool_name: "get_weather".to_string(),
                    output: json!({ "result": "晴" }),
                    provider_options: HashMap::new(),
                }],
                provider_options: HashMap::new(),
            },
            Message {
                role: Role::User,
                content: vec![ContentPart::Text {
                    text: "明天呢？".to_string(),
                    provider_options: HashMap::new(),
                }],
                provider_options: HashMap::new(),
            },
        ];
        let mut warnings = Vec::new();
        let (_, contents) = encode_messages(&messages, "gemini-2.5-pro", &mut warnings);
        assert_eq!(contents.len(), 2, "工具结果应并入随后的 user content");
        assert_eq!(
            contents[1]["parts"][0]["functionResponse"]["name"],
            json!("get_weather")
        );
        assert_eq!(contents[1]["parts"][1]["text"], json!("明天呢？"));
        assert!(warnings.is_empty());
    }

    /// 流内错误帧解码为 IR Error（message 取自 error 对象），不产出其他事件。
    ///
    /// 流首错误帧即此形状：网关 peek 窗口对 IR Error 的归类（可 failover）
    /// 由此打通，无需网关侧改动。
    #[test]
    fn stream_error_frame_decodes_to_ir_error() {
        let error_chunk = json!({
            "error": {
                "code": 429,
                "message": "Resource has been exhausted",
                "status": "RESOURCE_EXHAUSTED",
            }
        });
        let decoded = StreamDecoder::default().process(&error_chunk.to_string());
        assert!(!decoded.is_output);
        assert_eq!(
            decoded.events,
            vec![StreamEvent::Error {
                message: "Resource has been exhausted".to_string(),
            }]
        );
    }

    /// 形状畸形的错误帧（其余字段类型不符导致整帧解析失败）仍提取错误语义：
    /// 留痕后以顶层 error.message 映射 IR Error，不静默为空。
    #[test]
    fn malformed_error_frame_still_surfaces_error_semantics() {
        let chunk = json!({
            "error": { "code": 500, "message": "boom", "status": "INTERNAL" },
            "candidates": "not-an-array",
        });
        let decoded = StreamDecoder::default().process(&chunk.to_string());
        assert_eq!(
            decoded.events,
            vec![StreamEvent::Error {
                message: "boom".to_string(),
            }]
        );
    }

    /// 解析失败且无错误语义的 chunk：留痕后跳过（空事件），不中断流。
    #[test]
    fn malformed_frame_without_error_semantics_is_skipped() {
        let chunk = json!({ "candidates": "not-an-array" });
        let decoded = StreamDecoder::default().process(&chunk.to_string());
        assert_eq!(decoded.events, Vec::new());
    }

    /// 流式 image part（inlineData）无法在响应流下发：解码 warning 沿流式
    /// 路径以 StreamStart 事件上抛（IR 流式 warnings 通道），不再静默丢弃。
    #[test]
    fn stream_image_part_surfaces_media_warning() {
        let chunk = json!({
            "candidates": [{ "content": { "role": "model", "parts": [
                { "inlineData": { "mimeType": "image/png", "data": "aGVsbG8=" } }
            ] }, "index": 0 }],
        });
        let decoded = StreamDecoder::default().process(&chunk.to_string());
        assert!(!decoded.is_output);
        assert_eq!(
            decoded.events,
            vec![StreamEvent::StreamStart {
                warnings: vec![Warning::unsupported(
                    warning_feature::MEDIA,
                    "响应流不承载媒体内容（image/png），已丢弃",
                )],
            }]
        );
    }

    /// 流式 chunk 序列解码：元数据一次、思维链与正文块切换先收后开、
    /// functionCall 展开为 tool-input 三事件、末 chunk 的 finishReason 触发
    /// Finish 并携带累计 usage。
    #[test]
    fn stream_chunk_sequence_decodes_to_ir_events() {
        let chunks = [
            json!({
                "candidates": [{ "content": { "role": "model", "parts": [
                    { "text": "先想", "thought": true, "thoughtSignature": "sig_thought" }
                ] }, "index": 0 }],
                "responseId": "resp-1",
                "modelVersion": "gemini-2.5-pro",
                "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 2 }
            }),
            json!({
                "candidates": [{ "content": { "role": "model", "parts": [{ "text": "答案" }] }, "index": 0 }],
                "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 5 }
            }),
            json!({
                "candidates": [{ "content": { "role": "model", "parts": [
                    { "functionCall": { "name": "get_weather", "args": { "city": "上海" } } }
                ] }, "index": 0 }],
                "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 9 }
            }),
            json!({
                "candidates": [{ "finishReason": "STOP", "index": 0 }],
                "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 12 }
            }),
        ];
        let mut decoder = StreamDecoder::default();
        let events: Vec<StreamEvent> = chunks
            .iter()
            .flat_map(|chunk| decoder.process(&chunk.to_string()).events)
            .collect();

        assert_eq!(
            events,
            vec![
                StreamEvent::ResponseMetadata {
                    id: "resp-1".to_string(),
                    model: "gemini-2.5-pro".to_string(),
                },
                StreamEvent::ReasoningStart {
                    id: "0".to_string(),
                    provider_options: HashMap::new(),
                },
                StreamEvent::ReasoningDelta {
                    id: "0".to_string(),
                    delta: "先想".to_string(),
                    provider_options: [(
                        PROVIDER_KEY.to_string(),
                        json!({ THOUGHT_SIGNATURE_KEY: "sig_thought" }),
                    )]
                    .into_iter()
                    .collect(),
                },
                StreamEvent::ReasoningEnd {
                    id: "0".to_string(),
                    provider_options: HashMap::new(),
                },
                StreamEvent::TextStart {
                    id: "0".to_string(),
                    provider_options: HashMap::new(),
                },
                StreamEvent::TextDelta {
                    id: "0".to_string(),
                    delta: "答案".to_string(),
                    provider_options: HashMap::new(),
                },
                StreamEvent::TextEnd {
                    id: "0".to_string(),
                    provider_options: HashMap::new(),
                },
                StreamEvent::ToolInputStart {
                    id: stable_tool_call_id("get_weather", &json!({ "city": "上海" })),
                    tool_name: "get_weather".to_string(),
                    provider_options: HashMap::new(),
                },
                StreamEvent::ToolInputDelta {
                    id: stable_tool_call_id("get_weather", &json!({ "city": "上海" })),
                    delta: json!({ "city": "上海" }).to_string(),
                    provider_options: HashMap::new(),
                },
                StreamEvent::ToolInputEnd {
                    id: stable_tool_call_id("get_weather", &json!({ "city": "上海" })),
                    provider_options: HashMap::new(),
                },
                StreamEvent::Finish {
                    finish_reason: FinishReason {
                        unified: FinishReasonUnified::ToolCalls,
                        raw: Some("STOP".to_string()),
                    },
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 12,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                        cache_write_1h_tokens: 0,
                        raw: Some(json!({
                            "promptTokenCount": 10,
                            "candidatesTokenCount": 12,
                        })),
                    },
                    provider_metadata: HashMap::new(),
                },
            ]
        );
    }

    /// usage 逐 chunk 累计：中途出现不触发 Finish（不是流终止信号），
    /// finishReason 触发 Finish，其后补达的 usage 终值再发一次 Finish。
    #[test]
    fn cumulative_usage_finishes_only_on_finish_reason() {
        let mut decoder = StreamDecoder::default();
        let mid_stream = decoder.process(
            &json!({
                "candidates": [{ "content": { "role": "model", "parts": [{ "text": "a" }] } }],
                "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 1 }
            })
            .to_string(),
        );
        assert!(
            !mid_stream
                .events
                .iter()
                .any(|event| matches!(event, StreamEvent::Finish { .. })),
            "中途 usage 不应触发 Finish"
        );

        let finish = decoder.process(
            &json!({
                "candidates": [{ "finishReason": "STOP" }],
                "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 3 }
            })
            .to_string(),
        );
        let finish_event = finish
            .events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Finish { usage, .. } => Some(usage.clone()),
                _ => None,
            })
            .expect("finishReason chunk 应触发 Finish");
        assert_eq!(finish_event.output_tokens, 3);

        let trailing = decoder.process(
            &json!({
                "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 5 }
            })
            .to_string(),
        );
        let final_usage = trailing
            .events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Finish { usage, .. } => Some(usage.clone()),
                _ => None,
            })
            .expect("补达的 usage 终值应再发一次 Finish");
        assert_eq!(final_usage.output_tokens, 5, "第二次 Finish 携带终值");
    }

    /// 流内出现过 functionCall part 时，`STOP` 归 `ToolCalls`（与非流式同一
    /// 规则，调用先于 finishReason 到达也要记住）。
    #[test]
    fn stop_after_function_call_finishes_as_tool_calls() {
        let mut decoder = StreamDecoder::default();
        decoder.process(
            &json!({
                "candidates": [{ "content": { "role": "model", "parts": [
                    { "functionCall": { "name": "get_weather", "args": {} } }
                ] } }]
            })
            .to_string(),
        );
        let events = decoder
            .process(&json!({ "candidates": [{ "finishReason": "STOP" }] }).to_string())
            .events;
        assert!(matches!(
            events.as_slice(),
            [StreamEvent::Finish { finish_reason, .. }]
                if finish_reason.unified == FinishReasonUnified::ToolCalls
                    && finish_reason.raw.as_deref() == Some("STOP")
        ));
    }

    /// IR 流事件编码为入站 chunk 帧。
    ///
    /// 快照锁住整条帧序列：开始/结束事件不产帧、工具参数缓冲到收尾整块成
    /// part、思维链签名随 part 下发、别名覆盖 modelVersion、Finish chunk 携带
    /// finishReason 与 usageMetadata，全程无事件名（Gemini 流无哨兵行）。
    #[test]
    fn stream_events_encode_to_gemini_frames() {
        let mut encoder = StreamEncoder::new(Some("gemini-flash".to_string()));
        let events = vec![
            StreamEvent::StreamStart {
                warnings: vec![Warning::unsupported("top_k", "已丢弃")],
            },
            StreamEvent::ResponseMetadata {
                id: "resp-1".to_string(),
                model: "gemini-2.5-flash".to_string(),
            },
            StreamEvent::ReasoningStart {
                id: "0".to_string(),
                provider_options: [(
                    PROVIDER_KEY.to_string(),
                    json!({ THOUGHT_SIGNATURE_KEY: "sig_thought" }),
                )]
                .into_iter()
                .collect(),
            },
            StreamEvent::ReasoningDelta {
                id: "0".to_string(),
                delta: "先想".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::ReasoningEnd {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::TextStart {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::TextDelta {
                id: "0".to_string(),
                delta: "答案".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::TextEnd {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::ToolInputStart {
                id: "call_1".to_string(),
                tool_name: "get_weather".to_string(),
                provider_options: [(
                    PROVIDER_KEY.to_string(),
                    json!({ FUNCTION_CALL_ID_KEY: "call_1" }),
                )]
                .into_iter()
                .collect(),
            },
            StreamEvent::ToolInputDelta {
                id: "call_1".to_string(),
                delta: json!({ "city": "上海" }).to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::ToolInputEnd {
                id: "call_1".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::Finish {
                finish_reason: FinishReason {
                    unified: FinishReasonUnified::Stop,
                    raw: Some("STOP".to_string()),
                },
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 12,
                    cache_read_tokens: 4,
                    cache_write_tokens: 0,
                    cache_write_1h_tokens: 0,
                    raw: None,
                },
                provider_metadata: HashMap::new(),
            },
        ];
        let mut frames = Vec::new();
        for event in &events {
            frames.extend(encoder.encode(event));
        }
        insta::assert_json_snapshot!(frames_to_snapshot(&frames));
    }

    /// 错误编码：`code` 为 HTTP 状态码，`status` 按 google.rpc.Code 枚举映射。
    #[test]
    fn encode_error_maps_status_to_google_rpc_code() {
        assert_eq!(
            encode_error(429, "配额耗尽"),
            json!({ "error": { "code": 429, "message": "配额耗尽", "status": "RESOURCE_EXHAUSTED" } })
        );
        assert_eq!(
            encode_error(400, "参数错误")["error"]["status"],
            json!("INVALID_ARGUMENT")
        );
        assert_eq!(
            encode_error(404, "不存在")["error"]["status"],
            json!("NOT_FOUND")
        );
        assert_eq!(
            encode_error(503, "过载")["error"]["status"],
            json!("UNAVAILABLE")
        );
        assert_eq!(
            encode_error(599, "未知")["error"]["status"],
            json!("UNKNOWN")
        );
    }

    /// 模型列表编码：`models[].name` 带 `models/` 前缀的官方形状。
    #[test]
    fn encode_model_list_prefixes_names() {
        assert_eq!(
            encode_model_list(&["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()]),
            json!({
                "models": [
                    {
                        "name": "models/gemini-2.5-pro",
                        "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
                    },
                    {
                        "name": "models/gemini-2.5-flash",
                        "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
                    },
                ]
            })
        );
    }

    /// 流内错误编码：以独立 `data:` 帧下发 Gemini 错误形状（500 语义）。
    #[test]
    fn stream_error_event_encodes_to_error_frame() {
        let mut encoder = StreamEncoder::default();
        let frames = encoder.encode(&StreamEvent::Error {
            message: "Overloaded".to_string(),
        });
        assert_eq!(frames, vec![stream_error_frame("Overloaded")]);
        assert!(frames[0].event.is_none());
        let body: Value = serde_json::from_str(&frames[0].data).expect("错误帧载荷应为 JSON");
        assert_eq!(
            body,
            json!({ "error": { "code": 500, "message": "Overloaded", "status": "INTERNAL" } })
        );
    }

    /// 同族流式往返：IR 事件 → chunk 帧 → 解码，得到等价的事件序列（工具
    /// 调用 id 经逃生舱保留、签名随 part 往返、usage 加法/减法约定互逆）。
    #[test]
    fn stream_frames_decode_back_to_equivalent_events() {
        let events = vec![
            StreamEvent::ResponseMetadata {
                id: "resp-1".to_string(),
                model: "gemini-2.5-flash".to_string(),
            },
            StreamEvent::ReasoningStart {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::ReasoningDelta {
                id: "0".to_string(),
                delta: "先想".to_string(),
                provider_options: [(
                    PROVIDER_KEY.to_string(),
                    json!({ THOUGHT_SIGNATURE_KEY: "sig_thought" }),
                )]
                .into_iter()
                .collect(),
            },
            StreamEvent::ReasoningEnd {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::TextStart {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::TextDelta {
                id: "0".to_string(),
                delta: "答案".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::TextEnd {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::ToolInputStart {
                id: "call_1".to_string(),
                tool_name: "get_weather".to_string(),
                provider_options: [(
                    PROVIDER_KEY.to_string(),
                    json!({ FUNCTION_CALL_ID_KEY: "call_1" }),
                )]
                .into_iter()
                .collect(),
            },
            StreamEvent::ToolInputDelta {
                id: "call_1".to_string(),
                delta: json!({ "city": "上海" }).to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::ToolInputEnd {
                id: "call_1".to_string(),
                provider_options: HashMap::new(),
            },
            // 流内含 functionCall：`STOP` 按双轨规则归 `ToolCalls`（与非流式
            // 解码同一规则），同族往返后两侧一致。
            StreamEvent::Finish {
                finish_reason: FinishReason {
                    unified: FinishReasonUnified::ToolCalls,
                    raw: Some("STOP".to_string()),
                },
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 12,
                    cache_read_tokens: 4,
                    cache_write_tokens: 0,
                    cache_write_1h_tokens: 0,
                    raw: None,
                },
                provider_metadata: HashMap::new(),
            },
        ];
        let mut encoder = StreamEncoder::default();
        let mut decoder = StreamDecoder::default();
        let mut decoded = Vec::new();
        for event in &events {
            for frame in encoder.encode(event) {
                let payload: Value =
                    serde_json::from_str(&frame.data).expect("帧载荷应为合法 JSON");
                decoded.extend(decoder.process(&payload.to_string()).events);
            }
        }
        // usage.raw 保留解码侧的原 wire 值（加法回写后的 usageMetadata），
        // 不参与语义比较。
        for event in &mut decoded {
            if let StreamEvent::Finish { usage, .. } = event {
                usage.raw = None;
            }
        }
        assert_eq!(decoded, events, "同族往返应还原等价事件序列");
    }

    // ---- 以下为本批协议兼容性修复的用例 ----

    fn text_message(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentPart::Text {
                text: text.to_string(),
                provider_options: HashMap::new(),
            }],
            provider_options: HashMap::new(),
        }
    }

    /// 工具结果居中的序列（tool → assistant → user）：functionResponse 必须先
    /// 于后续 model 轮落为独立 user content，保住「紧随 functionCall」的顺序。
    #[test]
    fn mid_sequence_tool_result_lands_before_following_model_turn() {
        let messages = vec![
            text_message(Role::User, "上海天气？"),
            Message {
                role: Role::Assistant,
                content: vec![ContentPart::ToolCall {
                    tool_call_id: "call_1".to_string(),
                    tool_name: "get_weather".to_string(),
                    input: json!({ "city": "上海" }),
                    provider_options: HashMap::new(),
                }],
                provider_options: HashMap::new(),
            },
            Message {
                role: Role::Tool,
                content: vec![ContentPart::ToolResult {
                    tool_call_id: "call_1".to_string(),
                    tool_name: "get_weather".to_string(),
                    output: json!({ "result": "晴" }),
                    provider_options: HashMap::new(),
                }],
                provider_options: HashMap::new(),
            },
            text_message(Role::Assistant, "上海晴，26 度。"),
            text_message(Role::User, "明天呢？"),
        ];
        let mut warnings = Vec::new();
        let (_, contents) = encode_messages(&messages, "gemini-2.5-pro", &mut warnings);
        assert!(warnings.is_empty());
        // user(问) → model(functionCall) → user(functionResponse) → model(答) → user(新问)。
        assert_eq!(contents.len(), 5, "顺序: {contents:?}");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(
            contents[1]["parts"][0]["functionCall"]["name"],
            "get_weather"
        );
        assert_eq!(
            contents[2]["role"], "user",
            "functionResponse 应为独立 user content"
        );
        assert_eq!(
            contents[2]["parts"][0]["functionResponse"]["name"],
            json!("get_weather")
        );
        assert_eq!(contents[3]["role"], "model");
        assert_eq!(contents[3]["parts"][0]["text"], json!("上海晴，26 度。"));
        assert_eq!(contents[4]["parts"][0]["text"], json!("明天呢？"));
    }

    /// promptFeedback.blockReason（candidates 为空）：映射 ContentFilter 结束，
    /// 拦截原因随 warning 可观测，不再解码成零内容零告警。
    #[test]
    fn prompt_block_reason_maps_to_content_filter_with_warning() {
        let response = decode_response(&json!({
            "promptFeedback": { "blockReason": "SAFETY" },
            "usageMetadata": { "promptTokenCount": 10 }
        }))
        .expect("拦截响应应可解码");
        assert_eq!(
            response.finish_reason.unified,
            FinishReasonUnified::ContentFilter
        );
        assert_eq!(response.finish_reason.raw.as_deref(), Some("SAFETY"));
        assert!(response.content.is_empty());
        assert!(matches!(
            response.warnings.as_slice(),
            [Warning::Compatibility { feature, details }]
                if feature == warning_feature::SAFETY && details.as_deref().is_some_and(|d| d.contains("SAFETY"))
        ));
        // 有候选内容时 blockReason 不参与终态判定（内容面优先）。
        let with_candidates = decode_response(&json!({
            "candidates": [{ "content": { "role": "model", "parts": [{ "text": "ok" }] }, "finishReason": "STOP" }],
            "promptFeedback": { "blockReason": "SAFETY" }
        }))
        .expect("应可解码");
        assert_eq!(
            with_candidates.finish_reason.unified,
            FinishReasonUnified::Stop
        );
        assert!(with_candidates.warnings.is_empty());
    }

    /// 流式安全拦截：blockReason chunk 产出 warning + Finish(ContentFilter)，
    /// 流非空且原因可感知（不再零事件被网关 peek 误判流中断）。
    #[test]
    fn stream_prompt_block_reason_produces_warning_and_finish() {
        let chunk = json!({
            "promptFeedback": { "blockReason": "SAFETY" },
            "usageMetadata": { "promptTokenCount": 5 }
        });
        let decoded = StreamDecoder::default().process(&chunk.to_string());
        assert!(!decoded.is_output);
        match decoded.events.as_slice() {
            [
                StreamEvent::StreamStart { warnings },
                StreamEvent::Finish {
                    finish_reason,
                    usage,
                    ..
                },
            ] => {
                assert!(matches!(
                    warnings.as_slice(),
                    [Warning::Compatibility { feature, details }]
                        if feature == warning_feature::SAFETY && details.as_deref().is_some_and(|d| d.contains("SAFETY"))
                ));
                assert_eq!(finish_reason.unified, FinishReasonUnified::ContentFilter);
                assert_eq!(finish_reason.raw.as_deref(), Some("SAFETY"));
                assert_eq!(usage.input_tokens, 5);
            }
            other => panic!("应产出 warning + Finish，实际 {other:?}"),
        }
    }

    /// generationConfig 未建模子字段经逃生舱同族往返保真；seed 走官方字段
    /// 双向映射（不再是「Gemini 无 seed 承载」的丢弃项）。
    #[test]
    fn generation_config_unknown_subfields_roundtrip_and_seed_maps() {
        let wire = json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "generationConfig": {
                "temperature": 0.7,
                "seed": 42,
                "presencePenalty": 0.1,
                "responseLogprobs": true
            }
        });
        let request = decode_request(&wire).expect("应可解码");
        assert_eq!(request.seed, Some(42));
        assert_eq!(request.temperature, Some(0.7));
        // penalty 未类型化：不进 IR 旋钮，原始子字段由逃生舱承载。
        assert_eq!(request.presence_penalty, None);
        let mut warnings = Vec::new();
        let reencoded = encode_request(&request, &mut warnings);
        assert!(warnings.is_empty(), "同族往返零告警: {warnings:?}");
        assert_eq!(reencoded, wire, "未建模子字段应逐字段还原");

        // 跨族 seed：chat 入站的 seed 经 IR 映射为 generationConfig.seed。
        let chat = json!({
            "model": "gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }],
            "seed": 7
        });
        let ir = crate::core::openai_chat::decode_request(&chat).expect("chat 应可解码");
        let mut warnings = Vec::new();
        let wire = crate::core::gemini::encode_request(&ir, &mut warnings);
        assert_eq!(wire["generationConfig"]["seed"], json!(7));
        assert!(warnings.is_empty(), "seed 有承载不应告警: {warnings:?}");
    }

    /// finishReason 映射对齐 AI SDK 权威表：安全族归 ContentFilter、
    /// MALFORMED_FUNCTION_CALL 归 Error。
    #[test]
    fn finish_reason_maps_safety_family_and_malformed_function_call() {
        for (raw, unified) in [
            ("IMAGE_SAFETY", FinishReasonUnified::ContentFilter),
            ("SPII", FinishReasonUnified::ContentFilter),
            ("LANGUAGE", FinishReasonUnified::ContentFilter),
            ("MALFORMED_FUNCTION_CALL", FinishReasonUnified::Error),
            ("OTHER", FinishReasonUnified::Other),
        ] {
            let response = decode_response(&json!({
                "candidates": [{ "content": { "role": "model", "parts": [{ "text": "ok" }] }, "finishReason": raw }]
            }))
            .expect("应可解码");
            assert_eq!(
                response.finish_reason.unified, unified,
                "finishReason {raw}"
            );
            assert_eq!(response.finish_reason.raw.as_deref(), Some(raw));
        }
    }

    /// IR Error/Other 出站不再折为 STOP：映射为官方合法值 OTHER（诚实终态，
    /// 不谎报自然完成）；同族往返 OTHER 还原为 Other。
    #[test]
    fn error_and_other_finish_encode_to_gemini_other() {
        for unified in [FinishReasonUnified::Error, FinishReasonUnified::Other] {
            let response = ChatResponse {
                id: "resp-err".to_string(),
                model: "gemini-2.5-pro".to_string(),
                content: Vec::new(),
                finish_reason: FinishReason { unified, raw: None },
                usage: Usage::default(),
                provider_metadata: HashMap::new(),
                warnings: Vec::new(),
            };
            let encoded = encode_response(&response);
            assert_eq!(
                encoded["candidates"][0]["finishReason"],
                json!("OTHER"),
                "{unified:?} 应编码为 OTHER 而非 STOP"
            );
        }
        // 同族往返：OTHER 解码回 Other（Error 编码折为 OTHER 后语义为 Other，
        // 属双值单向折叠，诚实性优先于往返恒等）。
        let back = decode_response(&json!({
            "candidates": [{ "content": { "role": "model", "parts": [{ "text": "" }] }, "finishReason": "OTHER" }]
        }))
        .expect("应可解码");
        assert_eq!(back.finish_reason.unified, FinishReasonUnified::Other);
        assert_eq!(back.finish_reason.raw.as_deref(), Some("OTHER"));
    }

    /// 流式残缺工具参数：兜底 `{}` 前先下发 gateway warnings 帧（TOOL_ARGUMENTS
    /// 词汇与 chat 请求侧一致）；空参数（无增量）是合法形状，不告警。
    #[test]
    fn stream_malformed_tool_arguments_warn_and_fall_back() {
        let drive = |deltas: &[&str]| -> Vec<SseFrame> {
            let mut encoder = StreamEncoder::default();
            encoder.encode(&StreamEvent::ToolInputStart {
                id: "call_1".to_string(),
                tool_name: "get_weather".to_string(),
                provider_options: HashMap::new(),
            });
            for delta in deltas {
                encoder.encode(&StreamEvent::ToolInputDelta {
                    id: "call_1".to_string(),
                    delta: delta.to_string(),
                    provider_options: HashMap::new(),
                });
            }
            encoder.encode(&StreamEvent::ToolInputEnd {
                id: "call_1".to_string(),
                provider_options: HashMap::new(),
            })
        };

        // 残缺参数：告警帧 + 兜底空对象的 functionCall 帧。
        let frames = drive(&["{oo", "ps"]);
        assert_eq!(frames.len(), 2, "告警帧 + functionCall 帧: {frames:?}");
        let warning: Value = serde_json::from_str(&frames[0].data).expect("告警帧载荷应为 JSON");
        assert_eq!(
            warning["gateway"]["warnings"][0]["feature"],
            json!(warning_feature::TOOL_ARGUMENTS)
        );
        let part: Value = serde_json::from_str(&frames[1].data).expect("part 帧应为 JSON");
        assert_eq!(
            part["candidates"][0]["content"]["parts"][0]["functionCall"]["args"],
            json!({}),
            "残缺参数应兜底为空对象"
        );

        // 合法对象参数：零告警帧。
        let frames = drive(&[r#"{"city""#, r#":"上海"}"#]);
        assert_eq!(frames.len(), 1, "合法参数不产告警帧: {frames:?}");

        // 空参数（无任何增量）是合法调用：零告警帧。
        let frames = drive(&[]);
        assert_eq!(frames.len(), 1, "空参数不产告警帧: {frames:?}");
    }

    /// fileData 的 mimeType 未知时省略字段（顶层段标记不是合法 IANA 类型）；
    /// inlineData 需要完整类型，未知时丢弃并告警。
    #[test]
    fn file_data_omits_mime_type_when_not_full_and_inline_data_drops() {
        let request = ChatRequest {
            model: "gemini-2.5-pro".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![
                    // chat 远程 image_url 解码出的类别标记 `image`（无子类型）。
                    ContentPart::Media {
                        media_type: "image".to_string(),
                        data: MediaSource::Url {
                            url: "https://example.com/a.png".to_string(),
                        },
                        provider_options: HashMap::new(),
                    },
                    ContentPart::Media {
                        media_type: "image".to_string(),
                        data: MediaSource::Data {
                            base64: "aGVsbG8=".to_string(),
                        },
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
            parallel_tool_calls: None,
            reasoning: None,
            provider_options: HashMap::new(),
            warnings: Vec::new(),
        };
        let mut warnings = Vec::new();
        let wire = encode_request(&request, &mut warnings);
        let file_data = &wire["contents"][0]["parts"][0]["fileData"];
        assert_eq!(file_data["fileUri"], json!("https://example.com/a.png"));
        assert!(
            file_data.get("mimeType").is_none(),
            "未知 mimeType 应省略字段: {file_data}"
        );
        // inlineData 无法省略 mimeType：标记类型丢弃并告警。
        assert!(wire["contents"][0]["parts"].as_array().map(Vec::len) == Some(1));
        assert!(matches!(
            warnings.as_slice(),
            [Warning::Unsupported { feature, .. }] if feature == warning_feature::MEDIA
        ));
    }

    /// 响应携带多个候选时告警（与请求侧 candidateCount 同词汇）：网关只读取
    /// 首个候选，其余丢弃可观测。非流式与流式同规。
    #[test]
    fn multiple_response_candidates_warn_first_only() {
        let response = decode_response(&json!({
            "candidates": [
                { "content": { "role": "model", "parts": [{ "text": "a" }] }, "finishReason": "STOP" },
                { "content": { "role": "model", "parts": [{ "text": "b" }] }, "index": 1 }
            ]
        }))
        .expect("应可解码");
        assert!(matches!(
            response.warnings.as_slice(),
            [Warning::Compatibility { feature, details }]
                if feature == warning_feature::N && details.as_deref().is_some_and(|d| d.contains("2"))
        ));
        assert!(matches!(
            response.content.as_slice(),
            [ContentPart::Text { text, .. }] if text == "a"
        ));

        let mut decoder = StreamDecoder::default();
        let multi = json!({
            "candidates": [
                { "content": { "role": "model", "parts": [{ "text": "a" }] } },
                { "content": { "role": "model", "parts": [{ "text": "b" }] }, "index": 1 }
            ]
        });
        let first = decoder.process(&multi.to_string());
        assert!(first.events.iter().any(|event| matches!(
            event,
            StreamEvent::StreamStart { warnings }
                if matches!(warnings.as_slice(), [Warning::Compatibility { feature, .. }]
                    if feature == warning_feature::N)
        )));
        // 候选数在整条流内恒定：第二个多候选 chunk 不重复告警。
        let second = decoder.process(&multi.to_string());
        assert!(
            second.events.iter().all(|event| !matches!(
                event,
                StreamEvent::StreamStart { warnings }
                    if warnings
                        .iter()
                        .any(|w| matches!(w, Warning::Compatibility { feature, .. }
                            if feature == warning_feature::N))
            )),
            "多候选告警只应发一次: {:?}",
            second.events
        );
    }

    /// systemInstruction 携带非文本 part：媒体按 MEDIA 告警（与响应侧丢弃同
    /// 词汇）、其余非文本形状按 CUSTOM 告警，不再静默丢弃。
    #[test]
    fn system_instruction_non_text_parts_warn() {
        let wire = json!({
            "systemInstruction": { "parts": [
                { "text": "你是天气助手" },
                { "inlineData": { "mimeType": "image/png", "data": "aGVsbG8=" } },
                { "functionCall": { "name": "f", "args": {} } }
            ] },
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }]
        });
        let request = decode_request(&wire).expect("应可解码");
        let features: Vec<(&str, String)> = request
            .warnings
            .iter()
            .map(|warning| match warning {
                Warning::Unsupported { feature, details } => {
                    (feature.as_str(), details.clone().unwrap_or_default())
                }
                other => panic!("应为 unsupported: {other:?}"),
            })
            .collect();
        assert_eq!(features.len(), 2, "{features:?}");
        assert!(features[0].0 == warning_feature::MEDIA && features[0].1.contains("image/png"));
        assert!(features[1].0 == warning_feature::CUSTOM && features[1].1.contains("functionCall"));
        // 文本正常提取，媒体/未知 part 丢弃。
        assert_eq!(text_parts(&request.messages[0].content), "你是天气助手");
    }

    /// Gemini 3 代判定：`gemini-` 段前缀且非已知 1/2 代/旧名；未识别的新 ID
    /// 继承最新行为；家族别名与异族模型判否。
    #[test]
    fn gemini3_model_detection_follows_generation_patterns() {
        for model in [
            "gemini-3-pro",
            "gemini-3-flash-thinking",
            "models/gemini-3-pro",
            "gemini-flash-latest",
            "gemini-4-flash",
        ] {
            assert!(is_gemini3_model(model), "{model} 应判为 3 代行为");
        }
        for model in [
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.0-flash",
            "gemini-1.5-pro",
            "gemini-pro",
            "gemini-pro-vision",
            "gemini-robotics-er-1.5",
            "models/gemini-2.5-pro",
            "nano-banana-pro",
            "gpt-4o",
            "claude-sonnet-4-5",
        ] {
            assert!(!is_gemini3_model(model), "{model} 应判为非 3 代行为");
        }
    }

    /// Gemini 3 回放无签名 functionCall：注入官方哨兵规避 400 并告警（列出
    /// 工具名）；已带签名与 reasoning part 不注入；非 3 代模型不注入。
    #[test]
    fn gemini3_sentinel_injection_for_unsigned_function_calls() {
        let assistant = |provider_options: Option<Value>| Message {
            role: Role::Assistant,
            content: vec![ContentPart::ToolCall {
                tool_call_id: "call_1".to_string(),
                tool_name: "get_weather".to_string(),
                input: json!({ "city": "上海" }),
                provider_options: provider_options
                    .map(|signature| {
                        [(
                            PROVIDER_KEY.to_string(),
                            json!({ THOUGHT_SIGNATURE_KEY: signature }),
                        )]
                        .into_iter()
                        .collect()
                    })
                    .unwrap_or_default(),
            }],
            provider_options: HashMap::new(),
        };
        let request = |assistant: Message| ChatRequest {
            model: "gemini-3-pro".to_string(),
            messages: vec![text_message(Role::User, "hi"), assistant],
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

        // 3 代 + 无签名 → 哨兵 + 告警。
        let mut warnings = Vec::new();
        let wire = encode_request(&request(assistant(None)), &mut warnings);
        assert_eq!(
            wire["contents"][1]["parts"][0]["thoughtSignature"],
            json!(SKIP_THOUGHT_SIGNATURE_VALIDATOR)
        );
        assert!(matches!(
            warnings.as_slice(),
            [Warning::Compatibility { feature, details }]
                if feature == warning_feature::THOUGHT_SIGNATURE && details.as_deref().is_some_and(|d| d.contains("get_weather"))
        ));

        // 已带签名 → 原样回写，零注入零告警。
        let mut warnings = Vec::new();
        let wire = encode_request(&request(assistant(Some(json!("sig_1")))), &mut warnings);
        assert_eq!(
            wire["contents"][1]["parts"][0]["thoughtSignature"],
            json!("sig_1")
        );
        assert!(warnings.is_empty(), "已带签名不应告警: {warnings:?}");

        // 非 3 代模型（2.5）→ 不注入不告警。
        let mut request = request(assistant(None));
        request.model = "gemini-2.5-pro".to_string();
        let mut warnings = Vec::new();
        let wire = encode_request(&request, &mut warnings);
        assert!(
            wire["contents"][1]["parts"][0]
                .get("thoughtSignature")
                .is_none(),
            "非 3 代不应注入哨兵"
        );
        assert!(warnings.is_empty());
    }

    /// 并行豁免：同一条 assistant 消息内已见带签名调用时，后续无签名调用是
    /// Gemini 3 并行调用的合法形状，跳过哨兵注入；reasoning part 不参与注入。
    #[test]
    fn gemini3_sentinel_skips_after_signed_call_in_same_message() {
        let signed_options: HashMap<String, Value> = [(
            PROVIDER_KEY.to_string(),
            json!({ THOUGHT_SIGNATURE_KEY: "sig_1" }),
        )]
        .into_iter()
        .collect();
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentPart::Reasoning {
                    text: "先想".to_string(),
                    provider_options: HashMap::new(),
                },
                ContentPart::ToolCall {
                    tool_call_id: "call_1".to_string(),
                    tool_name: "signed_tool".to_string(),
                    input: json!({}),
                    provider_options: signed_options,
                },
                ContentPart::ToolCall {
                    tool_call_id: "call_2".to_string(),
                    tool_name: "unsigned_tool".to_string(),
                    input: json!({}),
                    provider_options: HashMap::new(),
                },
            ],
            provider_options: HashMap::new(),
        };
        let request = ChatRequest {
            model: "gemini-3-pro".to_string(),
            messages: vec![text_message(Role::User, "hi"), message],
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
        let wire = encode_request(&request, &mut warnings);
        let parts = &wire["contents"][1]["parts"];
        // reasoning part 无签名不注入；带签名调用原样；后续无签名调用豁免。
        assert!(
            parts[0].get("thoughtSignature").is_none(),
            "reasoning 不注入"
        );
        assert_eq!(parts[1]["thoughtSignature"], json!("sig_1"));
        assert!(
            parts[2].get("thoughtSignature").is_none(),
            "并行豁免：同消息已见签名调用，后续无签名调用跳过注入"
        );
        assert!(warnings.is_empty(), "豁免路径零告警: {warnings:?}");
    }
}
