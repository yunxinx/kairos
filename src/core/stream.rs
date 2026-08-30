//! 流式累积器：把 IR 流事件无损归约为非流式响应（流式与非流式同构）。
//!
//! 流式与非流式同构：一条流的 `start/delta/end` 与生命周期事件经
//! [`StreamAccumulator`] 累积后得到与直接解码非流式响应一致的 [`ChatResponse`]。
//! 同构由 `chat_response_to_stream_events` 与累积器互为逆运算锁定（测试见本模块）。
//!
//! [`SseFrame`] 是各适配器编码流式输出的共同载体：Chat Completions 只用 `data:`
//! 行，Anthropic Messages 额外要求 `event:` 事件名，两者由同一类型表达。

use std::collections::HashMap;

use serde_json::Value;

use crate::core::ir::{
    ChatResponse, ContentPart, FinishReason, FinishReasonUnified, StreamEvent, Usage,
};

/// 一个待下发的 SSE 帧：可选事件名 + `data:` 载荷。
///
/// Anthropic Messages 的下游 SDK 按 `event:` 名分派（`message_start`、
/// `content_block_delta` 等），OpenAI 协议只看 `data:`。适配器统一产出该类型，
/// 由网关按 axum SSE 发送。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// SSE `event:` 字段；`None` 表示不写事件名（OpenAI 协议）。
    pub event: Option<String>,
    /// SSE `data:` 字段的完整载荷。
    pub data: String,
}

impl SseFrame {
    /// 构造只有 `data:` 的帧（OpenAI 协议）。
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            event: None,
            data: data.into(),
        }
    }

    /// 构造带 `event:` 名的帧（Anthropic Messages 协议）。
    pub fn named(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            event: Some(event.into()),
            data: data.into(),
        }
    }
}

/// 把 IR 流事件累积为非流式响应。
///
/// 维护进行中的 text/reasoning 块索引与未完成的 tool-input 参数缓冲，
/// 事件按序消费；`finish()` 取出完整响应。
#[derive(Debug)]
pub struct StreamAccumulator {
    response: ChatResponse,
    open_text: Option<usize>,
    open_reasoning: Option<usize>,
    pending_tools: HashMap<String, PendingTool>,
}

/// 进行中的工具输入：参数以字符串片段累积，`tool-input-end` 时解析为 JSON。
#[derive(Debug)]
struct PendingTool {
    tool_call_id: String,
    tool_name: String,
    arguments: String,
    provider_options: crate::core::ir::ProviderOptions,
}

/// 把流事件携带的 `provider_options` 并入已有逃生舱（后到的键覆盖先到的）。
///
/// Anthropic thinking 的 signature 在 `signature_delta` 才到达（内容增量之后），
/// 因此逃生舱必须逐事件累加而非只取首个事件的值，否则 signature 丢失、多轮
/// thinking 被上游拒绝。适配器的流式路径（解码器累积、编码器下发）共用同一
/// 合并语义。
pub(crate) fn merge_provider_options(
    target: &mut crate::core::ir::ProviderOptions,
    incoming: crate::core::ir::ProviderOptions,
) {
    for (provider, value) in incoming {
        match (target.get_mut(&provider), value) {
            // 同一 provider 的内层字段合并，不整体替换。
            (Some(Value::Object(existing)), Value::Object(new)) => {
                existing.extend(new);
            }
            (_, value) => {
                target.insert(provider, value);
            }
        }
    }
}

impl Default for StreamAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamAccumulator {
    /// 创建空累积器。
    pub fn new() -> Self {
        Self {
            response: ChatResponse {
                id: String::new(),
                model: String::new(),
                content: Vec::new(),
                finish_reason: FinishReason {
                    unified: FinishReasonUnified::Other,
                    raw: None,
                },
                usage: Usage::default(),
                provider_metadata: HashMap::new(),
                warnings: Vec::new(),
            },
            open_text: None,
            open_reasoning: None,
            pending_tools: HashMap::new(),
        }
    }

    /// 消费一个 IR 流事件，更新累积状态。
    pub fn push(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::StreamStart { warnings } => {
                // 流侧 warnings 与非流式响应的 warnings 同一语义；累积后可归约。
                self.response.warnings.extend(warnings);
            }
            StreamEvent::ResponseMetadata { id, model } => {
                self.response.id = id;
                self.response.model = model;
            }
            StreamEvent::TextStart {
                provider_options, ..
            } => {
                self.response.content.push(ContentPart::Text {
                    text: String::new(),
                    provider_options,
                });
                self.open_text = Some(self.response.content.len() - 1);
            }
            StreamEvent::TextDelta {
                delta,
                provider_options,
                ..
            } => {
                let index = self.open_text.unwrap_or_else(|| {
                    self.response.content.push(ContentPart::Text {
                        text: String::new(),
                        provider_options: HashMap::new(),
                    });
                    self.response.content.len() - 1
                });
                self.open_text = Some(index);
                if let ContentPart::Text {
                    text,
                    provider_options: existing,
                } = &mut self.response.content[index]
                {
                    text.push_str(&delta);
                    merge_provider_options(existing, provider_options);
                }
            }
            StreamEvent::TextEnd {
                provider_options, ..
            } => {
                if let Some(index) = self.open_text
                    && let ContentPart::Text {
                        provider_options: existing,
                        ..
                    } = &mut self.response.content[index]
                {
                    merge_provider_options(existing, provider_options);
                }
                self.open_text = None;
            }
            StreamEvent::ReasoningStart {
                provider_options, ..
            } => {
                self.response.content.push(ContentPart::Reasoning {
                    text: String::new(),
                    provider_options,
                });
                self.open_reasoning = Some(self.response.content.len() - 1);
            }
            StreamEvent::ReasoningDelta {
                delta,
                provider_options,
                ..
            } => {
                let index = self.open_reasoning.unwrap_or_else(|| {
                    self.response.content.push(ContentPart::Reasoning {
                        text: String::new(),
                        provider_options: HashMap::new(),
                    });
                    self.response.content.len() - 1
                });
                self.open_reasoning = Some(index);
                if let ContentPart::Reasoning {
                    text,
                    provider_options: existing,
                } = &mut self.response.content[index]
                {
                    text.push_str(&delta);
                    // signature 经 `signature_delta` 随增量到达，逐事件并入。
                    merge_provider_options(existing, provider_options);
                }
            }
            StreamEvent::ReasoningEnd {
                provider_options, ..
            } => {
                if let Some(index) = self.open_reasoning
                    && let ContentPart::Reasoning {
                        provider_options: existing,
                        ..
                    } = &mut self.response.content[index]
                {
                    merge_provider_options(existing, provider_options);
                }
                self.open_reasoning = None;
            }
            StreamEvent::ToolInputStart {
                id,
                tool_name,
                provider_options,
            } => {
                self.pending_tools.insert(
                    id.clone(),
                    PendingTool {
                        tool_call_id: id,
                        tool_name,
                        arguments: String::new(),
                        provider_options,
                    },
                );
            }
            StreamEvent::ToolInputDelta {
                id,
                delta,
                provider_options,
            } => {
                if let Some(tool) = self.pending_tools.get_mut(&id) {
                    tool.arguments.push_str(&delta);
                    merge_provider_options(&mut tool.provider_options, provider_options);
                }
            }
            StreamEvent::ToolInputEnd {
                id,
                provider_options,
            } => {
                if let Some(mut tool) = self.pending_tools.remove(&id) {
                    merge_provider_options(&mut tool.provider_options, provider_options);
                    self.push_tool_call(tool);
                }
            }
            StreamEvent::ToolCall {
                tool_call_id,
                tool_name,
                input,
                provider_options,
            } => {
                self.response.content.push(ContentPart::ToolCall {
                    tool_call_id,
                    tool_name,
                    input,
                    provider_options,
                });
            }
            StreamEvent::Finish {
                finish_reason,
                usage,
                provider_metadata,
            } => {
                self.response.finish_reason = finish_reason;
                self.response.usage = usage;
                self.response.provider_metadata = provider_metadata;
            }
            // 错误不贡献内容：网关消费到即终止流，已累积的 usage 照常结算。
            StreamEvent::Error { .. } => {}
        }
    }

    fn push_tool_call(&mut self, tool: PendingTool) {
        // 空/非法累积参数收尾为 `{}`（Anthropic 要求 tool_use 必有对象 input），避免 `null` 被上游拒绝。
        let input = serde_json::from_str(&tool.arguments).unwrap_or_else(|_| serde_json::json!({}));
        self.response.content.push(ContentPart::ToolCall {
            tool_call_id: tool.tool_call_id,
            tool_name: tool.tool_name,
            input,
            provider_options: tool.provider_options,
        });
    }

    /// 取出累积的完整响应；未收到 `tool-input-end` 的进行中工具调用在此收尾。
    ///
    /// 流 flush：未完成的工具
    /// 调用在流结束时以已累积的参数收尾为 `tool-call`。
    pub fn finish(mut self) -> ChatResponse {
        let pending: Vec<PendingTool> = self.pending_tools.drain().map(|(_, t)| t).collect();
        for tool in pending {
            self.push_tool_call(tool);
        }
        self.response
    }
}

impl From<Vec<StreamEvent>> for ChatResponse {
    fn from(events: Vec<StreamEvent>) -> Self {
        let mut accumulator = StreamAccumulator::new();
        for event in events {
            accumulator.push(event);
        }
        accumulator.finish()
    }
}

/// 把非流式响应展开为等价 IR 流事件序列（累积的逆运算）。
///
/// 供同构测试与「以流形式回放完整响应」场景使用；`ToolResult` 等请求侧 part
/// 无流事件对应，这里不产出（响应 content 不含它们）。
pub fn chat_response_to_stream_events(response: &ChatResponse) -> Vec<StreamEvent> {
    let mut events = vec![StreamEvent::StreamStart {
        warnings: response.warnings.clone(),
    }];
    events.push(StreamEvent::ResponseMetadata {
        id: response.id.clone(),
        model: response.model.clone(),
    });

    for part in &response.content {
        match part {
            ContentPart::Text {
                text,
                provider_options,
            } => {
                events.push(StreamEvent::TextStart {
                    id: "0".to_string(),
                    provider_options: provider_options.clone(),
                });
                events.push(StreamEvent::TextDelta {
                    id: "0".to_string(),
                    delta: text.clone(),
                    provider_options: HashMap::new(),
                });
                events.push(StreamEvent::TextEnd {
                    id: "0".to_string(),
                    provider_options: HashMap::new(),
                });
            }
            ContentPart::Reasoning {
                text,
                provider_options,
            } => {
                events.push(StreamEvent::ReasoningStart {
                    id: "reasoning".to_string(),
                    // signature 等逃生舱字段随 start 事件带出，累积后无损还原。
                    provider_options: provider_options.clone(),
                });
                events.push(StreamEvent::ReasoningDelta {
                    id: "reasoning".to_string(),
                    delta: text.clone(),
                    provider_options: HashMap::new(),
                });
                events.push(StreamEvent::ReasoningEnd {
                    id: "reasoning".to_string(),
                    provider_options: HashMap::new(),
                });
            }
            ContentPart::ToolCall {
                tool_call_id,
                tool_name,
                input,
                provider_options,
            } => {
                events.push(StreamEvent::ToolInputStart {
                    id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    provider_options: provider_options.clone(),
                });
                events.push(StreamEvent::ToolInputDelta {
                    id: tool_call_id.clone(),
                    delta: input.to_string(),
                    provider_options: HashMap::new(),
                });
                events.push(StreamEvent::ToolInputEnd {
                    id: tool_call_id.clone(),
                    provider_options: HashMap::new(),
                });
            }
            // 响应侧不携带请求侧 part；此处不产出流事件。
            ContentPart::ToolResult { .. }
            | ContentPart::Media { .. }
            | ContentPart::Custom { .. } => {}
        }
    }

    events.push(StreamEvent::Finish {
        finish_reason: response.finish_reason.clone(),
        usage: response.usage.clone(),
        provider_metadata: response.provider_metadata.clone(),
    });
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ir::{FinishReason, FinishReasonUnified, Usage};
    use serde_json::json;
    use similar_asserts::assert_eq;

    /// 同构：非流式响应 → 流事件 → 累积，得到与非流式解码一致的响应。
    ///
    /// 含 reasoning 的 signature 逃生舱：signature 挂在 part 的 `provider_options`
    /// 上，展开为流事件再累积必须原样还原，否则多轮 thinking 会被上游拒绝。
    #[test]
    fn stream_accumulation_is_lossless() {
        let response = ChatResponse {
            id: "chatcmpl-123".to_string(),
            model: "gpt-4o".to_string(),
            content: vec![
                ContentPart::Reasoning {
                    text: "先算 925 ÷ 5".to_string(),
                    provider_options: [(
                        "anthropic".to_string(),
                        json!({ "signature": "ErUBCkYIBBg" }),
                    )]
                    .into_iter()
                    .collect(),
                },
                ContentPart::Text {
                    text: "The weather is sunny.".to_string(),
                    provider_options: HashMap::new(),
                },
                ContentPart::ToolCall {
                    tool_call_id: "call_1".to_string(),
                    tool_name: "get_weather".to_string(),
                    input: json!({ "city": "SF" }),
                    provider_options: HashMap::new(),
                },
            ],
            finish_reason: FinishReason {
                unified: FinishReasonUnified::ToolCalls,
                raw: Some("tool_calls".to_string()),
            },
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 2,
                cache_write_tokens: 1,
                raw: None,
            },
            provider_metadata: HashMap::new(),
            warnings: vec![crate::core::ir::Warning::unsupported("top_k", "已丢弃")],
        };

        let events = chat_response_to_stream_events(&response);
        let accumulated: ChatResponse = events.into();
        assert_eq!(accumulated, response, "流事件归约应还原原响应");
    }

    /// 流式 text 以增量片段累积；tool-input 参数跨多帧拼接后解析。
    #[test]
    fn delta_and_tool_input_accumulate() {
        let events = vec![
            StreamEvent::StreamStart {
                warnings: Vec::new(),
            },
            StreamEvent::ResponseMetadata {
                id: "chatcmpl-9".to_string(),
                model: "gpt-4o".to_string(),
            },
            StreamEvent::TextStart {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::TextDelta {
                id: "0".to_string(),
                delta: "Hel".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::TextDelta {
                id: "0".to_string(),
                delta: "lo".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::TextEnd {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::ToolInputStart {
                id: "call_1".to_string(),
                tool_name: "get_weather".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::ToolInputDelta {
                id: "call_1".to_string(),
                delta: r#"{"city":"San "#.to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::ToolInputDelta {
                id: "call_1".to_string(),
                delta: r#"Francisco"}"#.to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::ToolInputEnd {
                id: "call_1".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::Finish {
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
            },
        ];

        let response: ChatResponse = events.into();
        assert_eq!(response.id, "chatcmpl-9");
        assert_eq!(response.model, "gpt-4o");
        assert_eq!(
            response.content,
            vec![
                ContentPart::Text {
                    text: "Hello".to_string(),
                    provider_options: HashMap::new(),
                },
                ContentPart::ToolCall {
                    tool_call_id: "call_1".to_string(),
                    tool_name: "get_weather".to_string(),
                    input: json!({ "city": "San Francisco" }),
                    provider_options: HashMap::new(),
                },
            ]
        );
        assert_eq!(response.finish_reason.unified, FinishReasonUnified::Stop);
        assert_eq!(response.usage.output_tokens, 2);
    }

    /// reasoning 的逃生舱逐事件累加：Anthropic 的 signature 在 `signature_delta`
    /// 才到达（内容增量之后），累积器必须并入而非丢弃，否则多轮 thinking 无
    /// signature 会被上游拒绝。
    #[test]
    fn reasoning_provider_options_accumulate_across_deltas() {
        let events = vec![
            StreamEvent::ReasoningStart {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::ReasoningDelta {
                id: "0".to_string(),
                delta: "先算 ".to_string(),
                provider_options: HashMap::new(),
            },
            StreamEvent::ReasoningDelta {
                id: "0".to_string(),
                delta: "925 ÷ 5".to_string(),
                provider_options: HashMap::new(),
            },
            // signature 随末尾的零长增量到达（Anthropic `signature_delta` 帧型）。
            StreamEvent::ReasoningDelta {
                id: "0".to_string(),
                delta: String::new(),
                provider_options: [("anthropic".to_string(), json!({ "signature": "ErUBCkY" }))]
                    .into_iter()
                    .collect(),
            },
            StreamEvent::ReasoningEnd {
                id: "0".to_string(),
                provider_options: HashMap::new(),
            },
        ];

        let response: ChatResponse = events.into();
        assert_eq!(
            response.content,
            vec![ContentPart::Reasoning {
                text: "先算 925 ÷ 5".to_string(),
                provider_options: [("anthropic".to_string(), json!({ "signature": "ErUBCkY" }))]
                    .into_iter()
                    .collect(),
            }],
            "signature 应并入累积后的 reasoning part"
        );
    }

    /// 同一 provider 的逃生舱字段合并而非整体覆盖：`redacted_data` 先到、
    /// `signature` 后到时两者共存。
    #[test]
    fn same_provider_escape_hatch_fields_merge() {
        let mut target: crate::core::ir::ProviderOptions =
            [("anthropic".to_string(), json!({ "redacted_data": "abc" }))]
                .into_iter()
                .collect();
        merge_provider_options(
            &mut target,
            [("anthropic".to_string(), json!({ "signature": "sig" }))]
                .into_iter()
                .collect(),
        );
        assert_eq!(target["anthropic"]["redacted_data"], "abc");
        assert_eq!(target["anthropic"]["signature"], "sig");
    }
}
