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

/// 转换过程中无法表达的内容或设置，随响应显式回传给下游。
///
/// 形状对齐 AI SDK `SharedV4Warning`。跨协议族转换是有损的（ADR-0001）：丢失的
/// reasoning、目标协议不支持的采样参数等一律记 warning，不静默吞掉。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Warning {
    /// 目标协议不支持该特性，已丢弃。
    Unsupported {
        feature: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<String>,
    },
    /// 以兼容方式处理，结果可能次优（如补默认值）。
    Compatibility {
        feature: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<String>,
    },
    /// 其他情况。
    Other { message: String },
}

impl Warning {
    /// 构造 `Unsupported` warning。
    pub fn unsupported(feature: impl Into<String>, details: impl Into<String>) -> Self {
        Self::Unsupported {
            feature: feature.into(),
            details: Some(details.into()),
        }
    }

    /// 构造 `Compatibility` warning。
    pub fn compatibility(feature: impl Into<String>, details: impl Into<String>) -> Self {
        Self::Compatibility {
            feature: feature.into(),
            details: Some(details.into()),
        }
    }
}

/// 消息角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 媒体 part 的数据源：原始字节（base64）或 URL 二选一。
///
/// 形状对齐 AI SDK `FilePart` 的 `data` 判别联合（`{type:'data'}`/`{type:'url'}`）；
/// 网关只承载两种载体，`reference` 等 provider 托管形态经 `provider_options`
/// 逃生舱表达。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum MediaSource {
    /// 原始字节，base64 编码。
    Data { base64: String },
    /// 指向媒体资源的 URL。
    Url { url: String },
}

/// 消息内容 part 枚举。`type` 为 serde tag，序列化为 `snake_case`。
///
/// `media` 由 ADR-0001 预留的占位演进为携带真实载荷的媒体 part（形状演进
/// 见 ADR-0003）；跨协议族转换有损时记 warning 而非静默吞掉。
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
    /// 媒体内容（多模态）。`media_type` 为 IANA 媒体类型（如 `image/png`），
    /// 携带数据源（base64 字节或 URL）+ provider_options 逃生舱。v1 的 `File`
    /// 占位演进为此形状，wire 类型不出适配器边界。
    Media {
        media_type: String,
        data: MediaSource,
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

impl Usage {
    /// 逐分量取更大的合并：直通快路径跨帧嗅探 usage 时使用。
    ///
    /// Anthropic 的 usage 分散在各事件（`message_start` 有输入侧 input/cache 早期值，
    /// `message_delta` 有最终 output），任一帧都不完整；逐分量取 max 可无顺序依赖地
    /// 合并出最终值（bifrost passthrough 同款机制）。
    pub fn union_max(&mut self, other: Usage) {
        self.input_tokens = self.input_tokens.max(other.input_tokens);
        self.output_tokens = self.output_tokens.max(other.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.max(other.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.max(other.cache_write_tokens);
        if self.raw.is_none() {
            self.raw = other.raw;
        }
    }
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
    /// top-k 采样。Anthropic Messages 原生支持；OpenAI 两个协议不支持，出站时
    /// 丢弃并记 warning。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
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
    /// 请求级逃生舱：入站解析留存的 provider 特有请求设置。
    ///
    /// Anthropic 的 `thinking`（budget_tokens/display）是请求级而非消息级配置：
    /// 同协议族经 IR 路径出站（命中别名时）必须原样回传，否则多轮 thinking 的
    /// 预算设置丢失。跨协议族出站时丢弃并记 warning。
    #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
    pub provider_options: ProviderOptions,
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
    /// 本次转换中无法表达的内容（跨协议族丢弃的 reasoning、目标协议不支持的
    /// 设置）。适配器编码入站响应时把它暴露给下游。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
}

/// 流式事件（IR 侧）：start/delta/end 成对事件 + 生命周期事件。
///
/// 形状遵循 ADR-0001 与 AI SDK `LanguageModelV4StreamPart`：text/reasoning/
/// tool-input 三类 content 各以 start/delta/end 成对出现，tool-call 在 input
/// 汇聚完成后单发，生命周期事件含 stream-start/response-metadata/finish。
/// 流式与非流式同构——`StreamAccumulator` 可将流无损归约为 `ChatResponse`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// 文本块开始。
    TextStart {
        id: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 文本增量。
    TextDelta {
        id: String,
        delta: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 文本块结束。
    TextEnd {
        id: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 推理块开始。
    ReasoningStart {
        id: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 推理增量。
    ReasoningDelta {
        id: String,
        delta: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 推理块结束。
    ReasoningEnd {
        id: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 工具输入开始，携带工具名。
    ToolInputStart {
        id: String,
        tool_name: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 工具输入增量（arguments 字符串片段）。
    ToolInputDelta {
        id: String,
        delta: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 工具输入结束。
    ToolInputEnd {
        id: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 工具调用完成时单发，`input` 为已解析的 JSON 值。
    ToolCall {
        tool_call_id: String,
        tool_name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 生命周期：流开始，携带本次转换的 warnings（对齐 AI SDK `stream-start`）。
    StreamStart {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<Warning>,
    },
    /// 生命周期：响应元数据（id/model）。
    ResponseMetadata { id: String, model: String },
    /// 生命周期：流结束，携带 finish_reason 与 usage。
    Finish {
        finish_reason: FinishReason,
        usage: Usage,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        provider_metadata: ProviderOptions,
    },
}
