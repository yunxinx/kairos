//! Gemini generateContent 协议适配器：wire ↔ IR 双向编解码（非流式）。
//!
//! wire 结构体全部私有，透过 `decode_*`/`encode_*` 公共函数暴露 IR 边界，
//! wire 类型不出本模块边界。
//!
//! 映射要点：
//! - 请求侧：`contents[]` 承载消息序列（role 取 `user`/`model`），system 消息
//!   提升为 `systemInstruction`；part 变体 `text`/`thought`/`functionCall`/
//!   `functionResponse`/`inlineData`/`fileData` 双向映射；思考签名经 part 逃生舱
//!   `provider_options["google"]["thought_signature"]` 无损往返——签名与上游
//!   绑定，丢了下一轮就会被拒。
//! - 工具：定义走 `tools[].functionDeclarations`，选择走
//!   `toolConfig.functionCallingConfig`（`AUTO/NONE/ANY` +
//!   `allowedFunctionNames`）。wire 传统上不带调用 id：入站按 `sha256(名字|入参)`
//!   生成稳定 id（跨轮重放同一调用得到同一 id），工具结果按名字与前文调用配对；
//!   上游显式给了 id 时经 `provider_options["google"]["function_call_id"]` 往返。
//! - 响应侧：`candidates[0].content.parts` + `finishReason` 双轨映射（含
//!   functionCall part 时 finish 归 `ToolCalls`）；usage 输入侧为
//!   「`promptTokenCount` 含缓存」的减法约定（与 OpenAI 系同口径），
//!   `thoughtsTokenCount` 是输出侧子集，不另计价。
//! - 模型名不在请求体内（承载在 URL 路径的 `:generateContent` 端点上），
//!   由网关在调用本模块前注入；出站编码同样不写 `model` 字段。

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::core::ir::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, FinishReasonUnified, MediaSource,
    Message, PROVIDER_EXTRA_KEY, ReasoningEffort, Role, Tool, ToolChoice, Usage, Warning,
    apply_provider_extra, capture_unknown_fields, warning_feature,
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
    inline_data: Option<WireInlineData>,
    #[serde(default, alias = "file_data")]
    file_data: Option<WireFileData>,
    #[serde(default, alias = "function_call")]
    function_call: Option<WireFunctionCall>,
    #[serde(default, alias = "function_response")]
    function_response: Option<WireFunctionResponse>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireInlineData {
    #[serde(default, alias = "mime_type")]
    mime_type: Option<String>,
    #[serde(default)]
    data: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireFileData {
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

/// 生成参数面板；整块原样进逃生舱，逐字段另提升进 IR 类型化旋钮。
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
    #[serde(default, alias = "response_mime_type")]
    response_mime_type: Option<String>,
    #[serde(default, alias = "response_schema")]
    response_schema: Option<Value>,
    #[serde(default, alias = "thinking_config")]
    thinking_config: Option<Value>,
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
    if let Some(system) = &wire.system_instruction {
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
    let mut warnings = Vec::new();

    if let Some(config) = &wire.generation_config {
        temperature = config.temperature;
        top_p = config.top_p;
        top_k = config.top_k;
        max_tokens = config.max_output_tokens;
        stop = config.stop_sequences.clone().unwrap_or_default();
        n = config.candidate_count;
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
        seed: None,
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

/// 从待配对调用里按名字取一个（同名多次调用按出现顺序）。
fn pop_pending_call(pending: &mut Vec<(String, String)>, name: &str) -> Option<String> {
    let position = pending
        .iter()
        .position(|(call_name, _)| call_name == name)?;
    Some(pending.remove(position).1)
}

/// wire 不承载调用 id 时的稳定 id：同一（名字, 入参）跨轮重放得到同一 id，
/// 工具结果按名字配对因此稳定。
fn stable_tool_call_id(name: &str, input: &Value) -> String {
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
    let (system_instruction, contents) = encode_messages(&request.messages, warnings);

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
    } else if let Some(config) = typed_thinking_config(request.reasoning) {
        generation.insert("thinkingConfig".into(), config);
    }

    if !request.tools.is_empty() {
        let declarations: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
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
        (warning_feature::SEED, request.seed.is_some()),
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

    if !generation.is_empty() {
        obj.insert("generationConfig".into(), Value::Object(generation));
    }
    apply_provider_extra(&mut obj, request, PROVIDER_KEY, warnings);
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
fn typed_thinking_config(reasoning: Option<ReasoningEffort>) -> Option<Value> {
    let budget = reasoning?.budget_tokens()?;
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
/// `functionResponse` part 并入下一条 user content（末尾无后续时单独成一条
/// user content）。
fn encode_messages(
    messages: &[Message],
    warnings: &mut Vec<Warning>,
) -> (Option<Value>, Vec<Value>) {
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

    for message in messages {
        match message.role {
            Role::System => continue,
            Role::Tool => {
                for part in &message.content {
                    match part {
                        ContentPart::ToolResult { .. } => {
                            if let Some(block) = encode_part(part, warnings) {
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
        if message.role == Role::User && !pending_results.is_empty() {
            parts.append(&mut pending_results);
        }
        for part in &message.content {
            if let Some(block) = encode_part(part, warnings) {
                parts.push(block);
            }
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

/// 消息内文本 part 拼接（system 合并用）。
fn text_parts(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
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
            let block = match data {
                MediaSource::Data { base64 } => {
                    json!({ "inlineData": { "mimeType": media_type, "data": base64 } })
                }
                MediaSource::Url { url } => {
                    json!({ "fileData": { "mimeType": media_type, "fileUri": url } })
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
    if let Some(candidate) = candidate
        && let Some(content_value) = &candidate.content
    {
        for (index, part) in content_value.parts.iter().enumerate() {
            let decoded = decode_part(part, index, &mut Vec::new())?;
            has_function_call |= matches!(decoded, ContentPart::ToolCall { .. });
            content.push(decoded);
        }
    }
    let raw_finish = candidate.and_then(|c| c.finish_reason.clone());
    let unified = match raw_finish.as_deref() {
        Some("STOP") if has_function_call => FinishReasonUnified::ToolCalls,
        _ => map_finish_reason(raw_finish.as_deref()),
    };

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
        warnings: Vec::new(),
    })
}

/// unified finish reason 映射（Gemini finishReason 值）。
fn map_finish_reason(raw: Option<&str>) -> FinishReasonUnified {
    match raw {
        Some("STOP") => FinishReasonUnified::Stop,
        Some("MAX_TOKENS") => FinishReasonUnified::Length,
        Some("SAFETY") | Some("RECITATION") | Some("BLOCKLIST") | Some("PROHIBITED_CONTENT") => {
            FinishReasonUnified::ContentFilter
        }
        _ => FinishReasonUnified::Other,
    }
}

/// usage 四分量折算：`promptTokenCount` 含缓存（减法约定），
/// `thoughtsTokenCount` 是输出侧子集，不另计。
fn convert_usage(usage: &Value) -> Usage {
    let prompt = usage_count(usage, "promptTokenCount", "prompt_token_count");
    let candidates = usage_count(usage, "candidatesTokenCount", "candidates_token_count");
    let cached = usage_count(
        usage,
        "cachedContentTokenCount",
        "cached_content_token_count",
    );
    Usage {
        input_tokens: prompt.saturating_sub(cached),
        output_tokens: candidates,
        cache_read_tokens: cached,
        cache_write_tokens: 0,
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
    Some(convert_usage(usage))
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

    let mut usage = Map::new();
    let cached = response.usage.cache_read_tokens;
    let prompt = response.usage.input_tokens + cached + response.usage.cache_write_tokens;
    usage.insert("promptTokenCount".into(), json!(prompt));
    usage.insert(
        "candidatesTokenCount".into(),
        json!(response.usage.output_tokens),
    );
    usage.insert(
        "totalTokenCount".into(),
        json!(prompt + response.usage.output_tokens),
    );
    if cached > 0 {
        usage.insert("cachedContentTokenCount".into(), json!(cached));
    }

    let mut obj = Map::new();
    obj.insert("candidates".into(), json!([Value::Object(candidate)]));
    obj.insert("usageMetadata".into(), Value::Object(usage));
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

/// 把 IR unified finish reason 映射为 Gemini finishReason。
fn encode_finish_reason(finish_reason: &FinishReason) -> &'static str {
    match finish_reason.unified {
        FinishReasonUnified::Stop => "STOP",
        FinishReasonUnified::Length => "MAX_TOKENS",
        FinishReasonUnified::ContentFilter => "SAFETY",
        // Gemini 无「已完成工具调用」的独立终止原因：函数请求以 STOP 收尾。
        FinishReasonUnified::ToolCalls => "STOP",
        FinishReasonUnified::Error | FinishReasonUnified::Other => "STOP",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(usage.output_tokens, 20, "思考 token 是输出侧子集，不另计");
        assert_eq!(usage.cache_read_tokens, 30);
        assert_eq!(usage.cache_write_tokens, 0);

        // 嗅探在直通路径上从顶层 usageMetadata 取值，与 IR 路径同口径。
        let sniffed = sniff_usage(
            &json!({ "usageMetadata": { "promptTokenCount": 100, "cachedContentTokenCount": 30 } }),
        )
        .expect("应能嗅探 usage");
        assert_eq!(sniffed.input_tokens, 70);
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
                warning_feature::SEED,
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
        let (_, contents) = encode_messages(&messages, &mut warnings);
        assert_eq!(contents.len(), 2, "工具结果应并入随后的 user content");
        assert_eq!(
            contents[1]["parts"][0]["functionResponse"]["name"],
            json!("get_weather")
        );
        assert_eq!(contents[1]["parts"][1]["text"], json!("明天呢？"));
        assert!(warnings.is_empty());
    }
}
