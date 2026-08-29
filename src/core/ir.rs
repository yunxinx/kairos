//! 规范表示（IR）：网关内部唯一的消息规范模型。
//!
//! 严格的 role + content
//! parts 核心，配 `provider_options`（入）/`provider_metadata`（出）逃生舱。
//! 所有协议适配器都在此中枢与各自 wire 类型之间双向编解码，wire 类型不出
//! 适配器边界。

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hasher};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 逃生舱：`provider_options`（入站解析留存）与 `provider_metadata`（出站/响应侧）。
///
/// 外层按 provider 名，内层为
/// provider 特有字段。Anthropic thinking signature、Responses encrypted reasoning
/// 均经此往返。
pub type ProviderOptions = HashMap<String, Value>;

/// 转换过程中无法表达的内容或设置，随响应显式回传给下游。
///
/// 跨协议族转换是有损的：丢失的
/// reasoning、目标协议不支持的媒体类型或采样参数等一律记 warning，不静默吞掉。
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

/// warning `feature` 词汇表：跨协议转换中触发信息损失或兼容整形的主题名。
///
/// 各适配器统一引用本表构造 warning；下游与日志侧按值聚合统计，改名即
/// 破坏契约，须同步调整消费方。新增触发条件在此登记，不再手写字面量。
pub mod warning_feature {
    /// 媒体 part（image/document/audio 等）在目标协议无承载形态，已丢弃。
    pub const MEDIA: &str = "media";
    /// 未知或目标协议不支持的自定义内容块，已丢弃。
    pub const CUSTOM: &str = "custom";
    /// top-k 采样在目标协议无对应字段，或被 thinking 采样约束剥离。
    pub const TOP_K: &str = "top_k";
    /// top-p 采样在目标协议无对应字段，或被 thinking 采样约束下限整形。
    pub const TOP_P: &str = "top_p";
    /// temperature 在目标协议无对应字段，或被 thinking 采样约束整形为 1。
    pub const TEMPERATURE: &str = "temperature";
    /// presence penalty 在目标协议无对应字段，已丢弃。
    pub const PRESENCE_PENALTY: &str = "presence_penalty";
    /// frequency penalty 在目标协议无对应字段，已丢弃。
    pub const FREQUENCY_PENALTY: &str = "frequency_penalty";
    /// seed 在目标协议无对应字段，已丢弃。
    pub const SEED: &str = "seed";
    /// stop 序列在目标协议无对应字段，已丢弃。
    pub const STOP: &str = "stop";
    /// 单请求多候选（n）在目标协议无对应字段，已丢弃。
    pub const N: &str = "n";
    /// reasoning / thinking 内容在目标协议无承载通道，已丢弃（含渠道级
    /// 兼容输出开关关闭时的历史回放丢弃）。
    pub const REASONING: &str = "reasoning";
    /// Anthropic thinking 配置因上游硬约束被整体剥离（tool_choice 强制时）。
    pub const THINKING: &str = "thinking";
    /// JSON 输出设置（response_format）在目标协议无对应表达。
    pub const RESPONSE_FORMAT: &str = "response_format";
    /// 请求级 provider 逃生舱设置在目标协议无法表达，已丢弃。
    pub const PROVIDER_OPTIONS: &str = "provider_options";
    /// tool 消息携带非 tool_result 的内容 part，无法表达已丢弃。
    pub const TOOL_RESULT: &str = "tool_result";
    /// tool call 的 arguments 非合法 JSON 对象，兜底为空对象。
    pub const TOOL_ARGUMENTS: &str = "tool_arguments";
    /// Anthropic 出站 tool 的 input_schema 已归一化改写（union 摊平、
    /// 非 object 根兜底等）。
    pub const INPUT_SCHEMA: &str = "input_schema";
    /// 请求级白名单外的顶层未知字段在目标协议无法表达，已丢弃。
    pub const UNKNOWN_FIELDS: &str = "unknown_fields";
}

/// 未知字段逃生舱在 provider 逃生舱内的键：`provider_options[<provider>]["extra"]`。
///
/// 各适配器入站解码把本协议白名单外的顶层字段收进该键，同族出站原样回写，
/// 跨族出站丢弃并记 [`warning_feature::UNKNOWN_FIELDS`] warning。
pub const PROVIDER_EXTRA_KEY: &str = "extra";

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
/// 网关只承载原始字节与 URL 两种载体，`reference` 等 provider 托管形态经 `provider_options`
/// 逃生舱表达。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum MediaSource {
    /// 原始字节，base64 编码。
    Data { base64: String },
    /// 指向媒体资源的 URL。
    Url { url: String },
}

/// 从 data URL 提取 `(media_type, base64 载荷)`；非 data URL 返回 `None`。
///
/// data URL 形如 `data:<media_type>;base64,<base64 字节>`。`;base64` 标记缺失时
/// 按明文载荷容忍处理（缺省 media_type 空串），进出拼装对称故往返不受影响。
/// 三种协议出站把 `MediaSource::Data` 拼回 data URL 时共用同一拆分，保证
/// 拆分/拼装互为逆运算。
pub fn split_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, base64) = rest.split_once(',')?;
    let media_type = meta.strip_suffix(";base64").unwrap_or_default().to_string();
    Some((media_type, base64.to_string()))
}

/// 媒体类型顶层段（`image/png` → `image`）；无 `/` 时原样返回。
///
/// 目标协议按顶层段判定媒体类别（Anthropic
/// 据此分派 `image`/`document`，Responses 据此分派 `input_image`/`input_file`）。
pub fn top_level_media_type(media_type: &str) -> &str {
    media_type.split('/').next().unwrap_or(media_type)
}

/// 消息内容 part 枚举。`type` 为 serde tag，序列化为 `snake_case`。
///
/// 跨协议族转换有损时记 warning 而非静默吞掉。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// 文本内容。
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 推理内容。同协议族经逃生舱往返；跨协议族转换有损时记 warning。
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 媒体内容（多模态）。`media_type` 为 IANA 媒体类型（如 `image/png`），
    /// 携带数据源（base64 字节或 URL）+ provider_options 逃生舱；wire 类型不出
    /// 适配器边界。
    Media {
        media_type: String,
        data: MediaSource,
        #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
        provider_options: ProviderOptions,
    },
    /// 工具调用。`input` 统一为 `Value`。
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
    /// 合并出最终值。
    pub fn union_max(&mut self, other: Usage) {
        self.input_tokens = self.input_tokens.max(other.input_tokens);
        self.output_tokens = self.output_tokens.max(other.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.max(other.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.max(other.cache_write_tokens);
        if self.raw.is_none() {
            self.raw = other.raw;
        }
    }

    /// 四分量是否全为零（上游未回报 usage 时的嗅探/解码缺省值）。
    pub fn is_zero(&self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.cache_read_tokens == 0
            && self.cache_write_tokens == 0
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

/// 工具选择：跨协议类型化枚举。
///
/// 三协议的 wire 形状差异（Anthropic 的 `any`、Chat 的嵌套 `function` 对象、
/// Responses 的扁平 `function` 对象）由各适配器双向承担；Anthropic 附加语义
/// （如 `disable_parallel_tool_use`）经请求级逃生舱
/// `provider_options["anthropic"]["tool_choice_extra"]` 保留，只在
/// Anthropic 出站写回。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Tool { name: String },
}

/// reasoning 请求旋钮的档位。
///
/// OpenAI 官方档位为 none 到 max（realtime 封顶 xhigh，支持度随模型）；
/// `ultra` 是 Codex 客户端的扩展档位（官方 API 参考未列出），IR 收入以
/// 保真，跨族映射时钳到 [`ReasoningEffort::Max`]。Anthropic 侧
/// 原生档位为 `output_config.effort` 的 low/medium/high/xhigh/max（无
/// none/minimal），legacy 模型走 budget_tokens 阶梯
/// （[`ReasoningEffort::budget_tokens`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Ultra,
}

impl ReasoningEffort {
    /// 解析 wire 侧 effort 字符串；未知值返回 `None`，由调用方决定拒绝方式。
    pub fn parse_effort(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            "ultra" => Some(Self::Ultra),
            _ => None,
        }
    }

    /// wire 侧 effort 字符串（chat 与 responses 面板共用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
        }
    }

    /// Anthropic budget 阶梯：effort → `budget_tokens`（legacy budget 路径）；
    /// `None` 档对应 `thinking: disabled`，无预算。阶梯为网关规范换算表
    /// （512/1024/8192/24576/32768/128000）；`Ultra` 钳到 `Max` 同档，
    /// 避免选最深思考时映射落空。
    pub fn budget_tokens(self) -> Option<u32> {
        match self {
            Self::None => None,
            Self::Minimal => Some(512),
            Self::Low => Some(1024),
            Self::Medium => Some(8192),
            Self::High => Some(24576),
            Self::XHigh => Some(32768),
            Self::Max | Self::Ultra => Some(128_000),
        }
    }

    /// Anthropic 原生 effort 档位（`output_config.effort` 面）：官方取值为
    /// low/medium/high/xhigh/max，无 none/minimal/ultra。`Minimal` 归 `low`，
    /// `Ultra` 钳到 `max`；`None` 档无 effort 语义（对应 `thinking: disabled`），
    /// 返回 `None`。与 [`ReasoningEffort::budget_tokens`] 分别服务
    /// adaptive/native-effort 与 legacy budget 两条模型形态。
    pub fn native_effort(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Minimal | Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
            Self::Max | Self::Ultra => Some("max"),
        }
    }

    /// Anthropic budget → effort 的有损反向映射，阈值与正向阶梯一致；
    /// 超过 32768 归 `Max`（budget 路径上见不到 `Ultra`）。`disabled`/`adaptive`
    /// 与缺 budget 由调用方另行处理，不经本函数。
    pub fn from_budget(tokens: u32) -> Self {
        match tokens {
            0..=512 => Self::Minimal,
            513..=1024 => Self::Low,
            1025..=8192 => Self::Medium,
            8193..=24576 => Self::High,
            24577..=32768 => Self::XHigh,
            _ => Self::Max,
        }
    }
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
    pub tool_choice: Option<ToolChoice>,
    /// 是否允许模型一次发起多个工具调用。chat/responses 原生承载；Anthropic
    /// 无请求级字段，以 `tool_choice.disable_parallel_tool_use` 反语义表达，
    /// 映射时取反（`false` → `disable: true`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// reasoning 请求旋钮（effort 档位）。Anthropic 原始 `thinking` 配置经
    /// `provider_options["anthropic"]["thinking"]` 逃生舱无损往返，本字段
    /// 只承载可枚举的 effort 语义；本族逃生舱缺席时按协议形状兜底出站。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningEffort>,
    /// 请求级逃生舱：入站解析留存的 provider 特有请求设置。
    ///
    /// Anthropic 的 `thinking`（budget_tokens/display）是请求级而非消息级配置：
    /// 同协议族经 IR 路径出站（命中别名时）必须原样回传，否则多轮 thinking 的
    /// 预算设置丢失。跨协议族出站时丢弃并记 warning。
    #[serde(default, skip_serializing_if = "ProviderOptions::is_empty")]
    pub provider_options: ProviderOptions,
    /// 入站解码侧的兼容动作记录（拒绝改兜底的有损面），随响应面回传下游。
    ///
    /// 出站编码的转换损失由各适配器 `encode_request` 另行积累，两者在网关
    /// 响应面合流；适配器编码不消费本字段。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
}

impl ChatRequest {
    /// 读取指定 provider 未知字段逃生舱内的字段集合；键缺席或非对象时为 `None`。
    pub(crate) fn provider_extra(&self, provider: &str) -> Option<&serde_json::Map<String, Value>> {
        self.provider_options
            .get(provider)?
            .get(PROVIDER_EXTRA_KEY)?
            .as_object()
    }
}

/// 从入站 wire 请求对象捕获白名单外的顶层字段（未知字段逃生舱的捕获面）。
///
/// 请求对象非 JSON object 时返回空集（形状错误由 wire 解码另行拒绝）。
pub(crate) fn capture_unknown_fields(
    value: &Value,
    known: &[&str],
) -> serde_json::Map<String, Value> {
    let mut extra = serde_json::Map::new();
    if let Some(fields) = value.as_object() {
        for (key, field) in fields {
            if !known.contains(&key.as_str()) {
                extra.insert(key.clone(), field.clone());
            }
        }
    }
    extra
}

/// 出站编码的未知字段逃生舱处理：本族字段回写、跨族字段丢弃并告警。
///
/// `family` 为本适配器的 provider 键：本族 `extra` 内的字段原样写回出站
/// 对象（不覆盖类型化字段已写的键）；其他 provider 的字段丢弃并记
/// [`warning_feature::UNKNOWN_FIELDS`] warning，details 携带字段名。
pub(crate) fn apply_provider_extra(
    obj: &mut serde_json::Map<String, Value>,
    request: &ChatRequest,
    family: &str,
    warnings: &mut Vec<Warning>,
) {
    if let Some(extra) = request.provider_extra(family) {
        for (key, field) in extra {
            obj.entry(key.clone()).or_insert(field.clone());
        }
    }
    for provider in request.provider_options.keys() {
        if provider == family {
            continue;
        }
        let Some(extra) = request.provider_extra(provider) else {
            continue;
        };
        if !extra.is_empty() {
            warnings.push(Warning::unsupported(
                warning_feature::UNKNOWN_FIELDS,
                format!(
                    "{provider} 的未知字段 {} 无法在目标协议表达，已丢弃",
                    extra.keys().cloned().collect::<Vec<_>>().join("、")
                ),
            ));
        }
    }
}

/// 为没有显式会话头的请求计算前缀亲和标识。
///
/// 只纳入 system 消息全文与前两条消息的角色/文本；这对应上游 prompt cache
/// 的稳定前缀，同时避免把每轮新增内容纳入会话标识。`DefaultHasher` 只用于
/// 进程内粘性分桶，不提供密码学性质。
pub(crate) fn prefix_hash(request: &ChatRequest) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(b"kairos-prefix-v1\0");
    for message in request
        .messages
        .iter()
        .filter(|message| message.role == Role::System)
    {
        write_message_prefix(&mut hasher, message);
    }
    for message in request.messages.iter().take(2) {
        write_message_prefix(&mut hasher, message);
    }
    hasher.finish()
}

fn write_message_prefix(hasher: &mut DefaultHasher, message: &Message) {
    hasher.write_u8(match message.role {
        Role::System => 0,
        Role::User => 1,
        Role::Assistant => 2,
        Role::Tool => 3,
    });
    for part in &message.content {
        let text = match part {
            ContentPart::Text { text, .. } | ContentPart::Reasoning { text, .. } => text,
            _ => continue,
        };
        hasher.write_u64(text.len() as u64);
        hasher.write(text.as_bytes());
    }
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
/// text/reasoning/
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
    /// 生命周期：流开始，携带本次转换的 warnings。
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
    /// 生命周期：上游在 200 之后于流内报错（如 Anthropic `event: error` 的
    /// overloaded_error）。可多次出现；网关消费到即向下游下发入站协议错误帧
    /// 并按已累积 usage 结算后终止流。
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::{prefix_hash, warning_feature};
    use serde_json::json;

    /// 词汇表常量与 wire 值逐一定值：`gateway.warnings[].feature` 是下游与
    /// 日志侧的聚合键，改名属破坏性变更，须显式改本表并同步消费方。
    #[test]
    fn warning_feature_values_are_pinned() {
        let expected = [
            (warning_feature::MEDIA, "media"),
            (warning_feature::CUSTOM, "custom"),
            (warning_feature::TOP_K, "top_k"),
            (warning_feature::TOP_P, "top_p"),
            (warning_feature::TEMPERATURE, "temperature"),
            (warning_feature::PRESENCE_PENALTY, "presence_penalty"),
            (warning_feature::FREQUENCY_PENALTY, "frequency_penalty"),
            (warning_feature::SEED, "seed"),
            (warning_feature::STOP, "stop"),
            (warning_feature::N, "n"),
            (warning_feature::REASONING, "reasoning"),
            (warning_feature::THINKING, "thinking"),
            (warning_feature::RESPONSE_FORMAT, "response_format"),
            (warning_feature::PROVIDER_OPTIONS, "provider_options"),
            (warning_feature::TOOL_RESULT, "tool_result"),
            (warning_feature::TOOL_ARGUMENTS, "tool_arguments"),
            (warning_feature::INPUT_SCHEMA, "input_schema"),
            (warning_feature::UNKNOWN_FIELDS, "unknown_fields"),
        ];
        for (constant, value) in expected {
            assert_eq!(constant, value);
        }
    }

    fn request(messages: serde_json::Value) -> super::ChatRequest {
        serde_json::from_value(json!({
            "model": "gpt-4o",
            "messages": messages,
        }))
        .expect("测试请求应能解码")
    }

    #[test]
    fn prefix_hash_ignores_messages_after_the_first_two() {
        let first = request(json!([
            {"role": "system", "content": [{"type": "text", "text": "be precise"}]},
            {"role": "user", "content": [{"type": "text", "text": "hello"}]},
            {"role": "user", "content": [{"type": "text", "text": "turn one"}]}
        ]));
        let second = request(json!([
            {"role": "system", "content": [{"type": "text", "text": "be precise"}]},
            {"role": "user", "content": [{"type": "text", "text": "hello"}]},
            {"role": "user", "content": [{"type": "text", "text": "turn two"}]}
        ]));
        assert_eq!(prefix_hash(&first), prefix_hash(&second));
    }

    #[test]
    fn prefix_hash_includes_system_prompt_and_message_role_text() {
        let base = request(json!([
            {"role": "system", "content": [{"type": "text", "text": "be precise"}]},
            {"role": "user", "content": [{"type": "text", "text": "hello"}]}
        ]));
        let changed_system = request(json!([
            {"role": "system", "content": [{"type": "text", "text": "be creative"}]},
            {"role": "user", "content": [{"type": "text", "text": "hello"}]}
        ]));
        let changed_role = request(json!([
            {"role": "system", "content": [{"type": "text", "text": "be precise"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "hello"}]}
        ]));
        assert_ne!(prefix_hash(&base), prefix_hash(&changed_system));
        assert_ne!(prefix_hash(&base), prefix_hash(&changed_role));
    }
}
