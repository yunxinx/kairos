//! 流式累积器：把 IR 流事件无损归约为非流式响应（ADR-0001 同构）。
//!
//! 流式与非流式同构：一条流的 `start/delta/end` 与生命周期事件经
//! [`StreamAccumulator`] 累积后得到与直接解码非流式响应一致的 [`ChatResponse`]。
//! 同构由 `chat_response_to_stream_events` 与累积器互为逆运算锁定（测试见本模块）。

use std::collections::HashMap;

use crate::core::ir::{
    ChatResponse, ContentPart, FinishReason, FinishReasonUnified, StreamEvent, Usage,
};

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
            },
            open_text: None,
            open_reasoning: None,
            pending_tools: HashMap::new(),
        }
    }

    /// 消费一个 IR 流事件，更新累积状态。
    pub fn push(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::StreamStart => {}
            StreamEvent::ResponseMetadata { id, model } => {
                self.response.id = id;
                self.response.model = model;
            }
            StreamEvent::TextStart { .. } => {
                self.response.content.push(ContentPart::Text {
                    text: String::new(),
                    provider_options: HashMap::new(),
                });
                self.open_text = Some(self.response.content.len() - 1);
            }
            StreamEvent::TextDelta { delta, .. } => {
                let index = self.open_text.unwrap_or_else(|| {
                    self.response.content.push(ContentPart::Text {
                        text: String::new(),
                        provider_options: HashMap::new(),
                    });
                    self.response.content.len() - 1
                });
                self.open_text = Some(index);
                if let ContentPart::Text { text, .. } = &mut self.response.content[index] {
                    text.push_str(&delta);
                }
            }
            StreamEvent::TextEnd { .. } => {
                self.open_text = None;
            }
            StreamEvent::ReasoningStart { .. } => {
                self.response.content.push(ContentPart::Reasoning {
                    text: String::new(),
                    provider_options: HashMap::new(),
                });
                self.open_reasoning = Some(self.response.content.len() - 1);
            }
            StreamEvent::ReasoningDelta { delta, .. } => {
                let index = self.open_reasoning.unwrap_or_else(|| {
                    self.response.content.push(ContentPart::Reasoning {
                        text: String::new(),
                        provider_options: HashMap::new(),
                    });
                    self.response.content.len() - 1
                });
                self.open_reasoning = Some(index);
                if let ContentPart::Reasoning { text, .. } = &mut self.response.content[index] {
                    text.push_str(&delta);
                }
            }
            StreamEvent::ReasoningEnd { .. } => {
                self.open_reasoning = None;
            }
            StreamEvent::ToolInputStart { id, tool_name, .. } => {
                self.pending_tools.insert(
                    id.clone(),
                    PendingTool {
                        tool_call_id: id,
                        tool_name,
                        arguments: String::new(),
                    },
                );
            }
            StreamEvent::ToolInputDelta { id, delta, .. } => {
                if let Some(tool) = self.pending_tools.get_mut(&id) {
                    tool.arguments.push_str(&delta);
                }
            }
            StreamEvent::ToolInputEnd { id, .. } => {
                if let Some(tool) = self.pending_tools.remove(&id) {
                    self.push_tool_call(tool);
                }
            }
            StreamEvent::ToolCall {
                tool_call_id,
                tool_name,
                input,
                ..
            } => {
                self.response.content.push(ContentPart::ToolCall {
                    tool_call_id,
                    tool_name,
                    input,
                    provider_options: HashMap::new(),
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
        }
    }

    fn push_tool_call(&mut self, tool: PendingTool) {
        let input = serde_json::from_str(&tool.arguments).unwrap_or_default();
        self.response.content.push(ContentPart::ToolCall {
            tool_call_id: tool.tool_call_id,
            tool_name: tool.tool_name,
            input,
            provider_options: HashMap::new(),
        });
    }

    /// 取出累积的完整响应；未收到 `tool-input-end` 的进行中工具调用在此收尾。
    ///
    /// 对齐 AI SDK 流 flush 时 `StreamingToolCallTracker::flush()`：未完成的工具
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
    let mut events = vec![StreamEvent::StreamStart];
    events.push(StreamEvent::ResponseMetadata {
        id: response.id.clone(),
        model: response.model.clone(),
    });

    for part in &response.content {
        match part {
            ContentPart::Text { text, .. } => {
                events.push(StreamEvent::TextStart {
                    id: "0".to_string(),
                    provider_options: HashMap::new(),
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
            ContentPart::Reasoning { text, .. } => {
                events.push(StreamEvent::ReasoningStart {
                    id: "reasoning".to_string(),
                    provider_options: HashMap::new(),
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
                ..
            } => {
                events.push(StreamEvent::ToolInputStart {
                    id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    provider_options: HashMap::new(),
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
            | ContentPart::File { .. }
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

    /// 同构：非流式响应 → 流事件 → 累积，得到与非流式解码一致的响应。
    #[test]
    fn stream_accumulation_is_lossless() {
        let response = ChatResponse {
            id: "chatcmpl-123".to_string(),
            model: "gpt-4o".to_string(),
            content: vec![
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
        };

        let events = chat_response_to_stream_events(&response);
        let accumulated: ChatResponse = events.into();
        assert_eq!(accumulated, response, "流事件归约应还原原响应");
    }

    /// 流式 text 以增量片段累积；tool-input 参数跨多帧拼接后解析。
    #[test]
    fn delta_and_tool_input_accumulate() {
        let events = vec![
            StreamEvent::StreamStart,
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
}
