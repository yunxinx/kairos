//! 规范表示（IR）：网关内部唯一的消息规范模型。
//!
//! 形状遵循 ADR-0001 与 Vercel AI SDK `LanguageModelV4`：严格的 role + content
//! parts 核心，配 `provider_options`（入）/`provider_metadata`（出）逃生舱。
//! 所有协议适配器都在此中枢与各自 wire 类型之间双向编解码，wire 类型不出
//! 适配器边界。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 逃生舱：`provider_options`（入站解析留存）与 `provider_metadata`（出站/响应侧）。
///
/// 形状对齐 AI SDK `Record<string, JSONObject>`：外层按 provider 名，内层为
/// provider 特有字段。Anthropic thinking signature、Responses encrypted reasoning
/// 均经此往返。
pub type ProviderOptions = HashMap<String, Value>;

/// 消息角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 消息内容 part 枚举。`type` 为 serde tag，序列化为 `snake_case`。
///
/// `file`/`custom`/`reasoning` 为 ADR-0001 预留的 part 类型：v1 仅声明不实现
/// 多模态，跨协议族转换有损时记 warning 而非静默吞掉。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// 文本内容。
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 推理内容。v1 的 openai_chat 非流式路径不产出，同协议族经逃生舱往返。
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 文件内容。v1 仅声明，不实现多模态。
    File {
        media_type: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 工具调用。`input` 统一为 `Value`（AI SDK prompt 侧对象/流侧字符串的
    /// 不一致不照搬）。
    ToolCall {
        tool_call_id: String,
        tool_name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 工具调用结果。`output` 为任意 JSON 值。
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        output: Value,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// provider 特有内容 part。v1 仅声明。
    Custom {
        kind: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
}

/// 一条消息：角色 + 有序 content parts + 逃生舱。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
    pub provider_options: ProviderOptions,
}

/// 统一 finish reason：跨 provider 一致 + 原始值双轨。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReasonUnified {
    Stop,
    Length,
    ContentFilter,
    ToolCalls,
    Error,
    Other,
}

/// finish reason 双轨：unified 用于跨 provider 一致语义，raw 保留原始值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishReason {
    pub unified: FinishReasonUnified,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

/// usage 四分量 + raw 兜底。四分量对齐价格表四档（input/output/cache_read/cache_write），
/// `raw` 保留上游原始 usage 形状，供后续计费与对账。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

/// 工具定义（出站请求侧）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

/// 非流式聊天请求的 IR 中枢。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
}

/// 非流式聊天响应的 IR 中枢。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub model: String,
    pub content: Vec<ContentPart>,
    pub finish_reason: FinishReason,
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub provider_metadata: ProviderOptions,
}
