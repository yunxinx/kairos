//! 跨协议 round-trip 语义矩阵：协议兼容性工作的测试基建。
//!
//! 断言助手把「IR → 协议A wire → IR → 协议B wire → IR → 协议A wire → IR」
//! 的往返归约为对 IR **语义投影**的比较：投影只保留跨协议可表达的语义面
//! （文本、工具调用、finish unified、usage 四分量），wire 形状差异与逃生舱
//! 细节不参与比较。三个协议的全部有向对逐一成格，每个语义面一组用例；
//! 同族逃生舱（anthropic thinking signature / responses encrypted_content）
//! 单独立格锁定。
//!
//! 两条已知的投影排除项，均为 wire 协议事实而非缺陷：
//! - 空文本 part 无语义（chat 编码会把空内容还原为单个空 text）；
//! - `ToolResult.tool_name` 是 IR 侧反规范化字段，三种 wire 均不承载，
//!   配对身份由 `tool_call_id` 承载。
//!
//! 基线只覆盖当前已无损的语义面；跨族有损面（reasoning 丢弃、`Other` finish
//! 等）的 warning 行为由各适配器测试锁定。后续票（tool_choice、reasoning
//! 旋钮、缓存断点）在本基建上扩格。

use std::collections::HashMap;

use serde_json::{Value, json};
use similar_asserts::assert_eq;

use super::{anthropic_messages, openai_chat, openai_responses};
use crate::config::Protocol;
use crate::core::ir::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, FinishReasonUnified, Message, Role, Tool,
    Usage, Warning,
};

/// 全部有向协议对（a 自往返后经 b 中转再回 a）。
fn directed_pairs() -> Vec<(Protocol, Protocol)> {
    let all = [
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
    ];
    let mut pairs = Vec::new();
    for a in all {
        for b in all {
            if a != b {
                pairs.push((a, b));
            }
        }
    }
    pairs
}

fn encode_response_wire(protocol: Protocol, ir: &ChatResponse) -> Value {
    match protocol {
        Protocol::OpenAiChat => openai_chat::encode_response(ir),
        Protocol::OpenAiResponses => openai_responses::encode_response(ir),
        Protocol::AnthropicMessages => anthropic_messages::encode_response(ir),
    }
}

fn decode_response_wire(protocol: Protocol, wire: &Value) -> ChatResponse {
    match protocol {
        Protocol::OpenAiChat => openai_chat::decode_response(wire)
            .unwrap_or_else(|err| panic!("OpenAiChat 响应解码应成功: {err}")),
        Protocol::OpenAiResponses => openai_responses::decode_response(wire)
            .unwrap_or_else(|err| panic!("OpenAiResponses 响应解码应成功: {err}")),
        Protocol::AnthropicMessages => anthropic_messages::decode_response(wire)
            .unwrap_or_else(|err| panic!("AnthropicMessages 响应解码应成功: {err}")),
    }
}

/// 编码 IR 请求为 wire；返回 wire 与转换 warnings，供基线断言「无损面零告警」。
fn encode_request_wire(protocol: Protocol, request: &ChatRequest) -> (Value, Vec<Warning>) {
    let mut warnings = Vec::new();
    let wire = match protocol {
        Protocol::OpenAiChat => openai_chat::encode_request(request, &mut warnings),
        Protocol::OpenAiResponses => openai_responses::encode_request(request, &mut warnings),
        Protocol::AnthropicMessages => anthropic_messages::encode_request(request, &mut warnings),
    };
    (wire, warnings)
}

fn decode_request_wire(protocol: Protocol, wire: &Value) -> ChatRequest {
    match protocol {
        Protocol::OpenAiChat => openai_chat::decode_request(wire)
            .unwrap_or_else(|err| panic!("OpenAiChat 请求解码应成功: {err}")),
        Protocol::OpenAiResponses => openai_responses::decode_request(wire)
            .unwrap_or_else(|err| panic!("OpenAiResponses 请求解码应成功: {err}")),
        Protocol::AnthropicMessages => anthropic_messages::decode_request(wire)
            .unwrap_or_else(|err| panic!("AnthropicMessages 请求解码应成功: {err}")),
    }
}

/// 断言 `ir` 经协议 `a` 自往返、再经协议 `b` 中转往返后语义投影不变。
fn response_survives(a: Protocol, b: Protocol, ir: &ChatResponse) {
    let via_a = decode_response_wire(a, &encode_response_wire(a, ir));
    assert_eq!(
        project_response(&via_a),
        project_response(ir),
        "{a:?} 自往返"
    );
    let via_b = decode_response_wire(b, &encode_response_wire(b, &via_a));
    assert_eq!(
        project_response(&via_b),
        project_response(ir),
        "经 {b:?} 中转"
    );
    let back = decode_response_wire(a, &encode_response_wire(a, &via_b));
    assert_eq!(project_response(&back), project_response(ir), "回到 {a:?}");
}

/// 断言请求 `request` 经协议 `a` 自往返、再经协议 `b` 中转往返后语义投影不变。
///
/// 基线语义面同时要求每次编码零 warning——投影相保有告警即说明语义面选取
/// 有误，应把该字段移出基线或在 dedicated 用例中声明其有损性。
fn request_survives(a: Protocol, b: Protocol, request: &ChatRequest) {
    let (wire_a, warnings_a) = encode_request_wire(a, request);
    assert!(
        warnings_a.is_empty(),
        "{a:?} 基线编码不应有 warning: {warnings_a:?}"
    );
    let via_a = decode_request_wire(a, &wire_a);
    assert_eq!(
        project_request(&via_a),
        project_request(request),
        "{a:?} 自往返"
    );

    let (wire_b, warnings_b) = encode_request_wire(b, &via_a);
    assert!(
        warnings_b.is_empty(),
        "{b:?} 基线编码不应有 warning: {warnings_b:?}"
    );
    let via_b = decode_request_wire(b, &wire_b);
    assert_eq!(
        project_request(&via_b),
        project_request(request),
        "经 {b:?} 中转"
    );

    let (wire_back, warnings_back) = encode_request_wire(a, &via_b);
    assert!(
        warnings_back.is_empty(),
        "{a:?} 基线编码不应有 warning: {warnings_back:?}"
    );
    let back = decode_request_wire(a, &wire_back);
    assert_eq!(
        project_request(&back),
        project_request(request),
        "回到 {a:?}"
    );
}

/// 响应的语义投影：跨协议可表达的语义面，投影相等即往返无损。
fn project_response(ir: &ChatResponse) -> Value {
    json!({
        "id": ir.id,
        "model": ir.model,
        "content": ir.content.iter().filter_map(project_content_part).collect::<Vec<_>>(),
        "finish": ir.finish_reason.unified,
        "usage": project_usage(&ir.usage),
    })
}

/// 请求的语义投影：同上，覆盖消息序列、工具定义与采样参数。
fn project_request(request: &ChatRequest) -> Value {
    json!({
        "model": request.model,
        "messages": request.messages.iter().map(|message| json!({
            "role": message.role,
            "content": message.content.iter().filter_map(project_content_part).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "tools": request.tools.iter().map(|tool| json!({
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        })).collect::<Vec<_>>(),
        "stream": request.stream,
        "max_tokens": request.max_tokens,
        "temperature": request.temperature,
        "top_p": request.top_p,
    })
}

fn project_usage(usage: &Usage) -> Value {
    json!([
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_tokens,
        usage.cache_write_tokens,
    ])
}

fn project_content_part(part: &ContentPart) -> Option<Value> {
    match part {
        ContentPart::Text { text, .. } if text.is_empty() => None,
        ContentPart::Text { text, .. } => Some(json!({ "text": text })),
        ContentPart::Reasoning {
            text,
            provider_options,
        } => Some(json!({
            "reasoning": { "text": text, "options": provider_options },
        })),
        ContentPart::ToolCall {
            tool_call_id,
            tool_name,
            input,
            ..
        } => Some(json!({ "tool_call": [tool_call_id, tool_name, input] })),
        // tool_name 不被任何 wire 承载（见模块注释），投影只保留配对身份与载荷。
        ContentPart::ToolResult {
            tool_call_id,
            output,
            ..
        } => Some(json!({ "tool_result": [tool_call_id, output] })),
        ContentPart::Media {
            media_type, data, ..
        } => Some(json!({ "media": [media_type, data] })),
        ContentPart::Custom { kind, .. } => Some(json!({ "custom": kind })),
    }
}

fn text_part(text: &str) -> ContentPart {
    ContentPart::Text {
        text: text.to_string(),
        provider_options: HashMap::new(),
    }
}

fn options(entries: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect()
}

/// 基线响应：文本 + 工具调用 + ToolCalls finish + 带缓存的 usage。
fn base_response() -> ChatResponse {
    ChatResponse {
        id: "resp-matrix".to_string(),
        model: "matrix-model".to_string(),
        content: vec![
            text_part("晴天，微风。"),
            ContentPart::ToolCall {
                tool_call_id: "call_1".to_string(),
                tool_name: "get_weather".to_string(),
                input: json!({ "city": "上海" }),
                provider_options: HashMap::new(),
            },
        ],
        finish_reason: FinishReason {
            unified: FinishReasonUnified::ToolCalls,
            raw: Some("tool_use".to_string()),
        },
        usage: Usage {
            input_tokens: 12,
            output_tokens: 5,
            cache_read_tokens: 3,
            cache_write_tokens: 2,
            raw: None,
        },
        provider_metadata: HashMap::new(),
        warnings: Vec::new(),
    }
}

/// 基线请求：system + user + assistant(tool_call) + tool(result) 完整工具轮，
/// 含工具定义与采样参数。
fn base_request() -> ChatRequest {
    ChatRequest {
        model: "matrix-model".to_string(),
        messages: vec![
            Message {
                role: Role::System,
                content: vec![text_part("你是天气助手")],
                provider_options: HashMap::new(),
            },
            Message {
                role: Role::User,
                content: vec![text_part("上海天气如何？")],
                provider_options: HashMap::new(),
            },
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
                    output: json!("晴，26 度"),
                    provider_options: HashMap::new(),
                }],
                provider_options: HashMap::new(),
            },
        ],
        stream: false,
        temperature: Some(0.5),
        top_p: Some(0.9),
        top_k: None,
        max_tokens: Some(512),
        n: None,
        stop: Vec::new(),
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        response_format: None,
        tools: vec![Tool {
            name: "get_weather".to_string(),
            description: Some("查询城市天气".to_string()),
            parameters: Some(json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"],
            })),
        }],
        tool_choice: None,
        provider_options: HashMap::new(),
    }
}

/// 基线语义面：文本、工具调用与 usage 四分量（含缓存）经全部有向对无损。
#[test]
fn response_text_tool_call_and_usage_survive_all_six_pairs() {
    for (a, b) in directed_pairs() {
        response_survives(a, b, &base_response());
    }
}

/// finish 的跨协议无损变体（Stop/Length/ContentFilter）经全部有向对保持
/// unified 语义。`ToolCalls` 由基线响应覆盖；`Error`/`Other` 在 chat 面映射
/// 为 stop，属有损面，由适配器测试锁定。
#[test]
fn finish_lossless_variants_survive_all_six_pairs() {
    for unified in [
        FinishReasonUnified::Stop,
        FinishReasonUnified::Length,
        FinishReasonUnified::ContentFilter,
    ] {
        for (a, b) in directed_pairs() {
            let ir = ChatResponse {
                id: "resp-finish".to_string(),
                model: "matrix-model".to_string(),
                content: Vec::new(),
                finish_reason: FinishReason { unified, raw: None },
                usage: Usage::default(),
                provider_metadata: HashMap::new(),
                warnings: Vec::new(),
            };
            response_survives(a, b, &ir);
        }
    }
}

/// 基线语义面：完整工具轮的消息序列（system/user/assistant/tool）、工具定义
/// 与采样参数经全部有向对无损。
#[test]
fn request_tool_sequence_and_sampling_survive_all_six_pairs() {
    for (a, b) in directed_pairs() {
        request_survives(a, b, &base_request());
    }
}

/// anthropic 同族逃生舱：响应侧 thinking signature / redacted_thinking、
/// 请求级 thinking 配置均经往返原样还原（多轮 thinking 的硬约束）。
#[test]
fn anthropic_reasoning_escape_hatches_survive_same_family() {
    let response = ChatResponse {
        id: "resp-thinking".to_string(),
        model: "matrix-model".to_string(),
        content: vec![
            ContentPart::Reasoning {
                text: "先算 925 ÷ 5".to_string(),
                provider_options: options(&[("anthropic", json!({ "signature": "sig_1" }))]),
            },
            ContentPart::Reasoning {
                text: String::new(),
                provider_options: options(&[(
                    "anthropic",
                    json!({ "redacted_data": "encrypted" }),
                )]),
            },
        ],
        finish_reason: FinishReason {
            unified: FinishReasonUnified::Stop,
            raw: None,
        },
        usage: Usage::default(),
        provider_metadata: HashMap::new(),
        warnings: Vec::new(),
    };
    let wire = encode_response_wire(Protocol::AnthropicMessages, &response);
    let back = decode_response_wire(Protocol::AnthropicMessages, &wire);
    assert_eq!(project_response(&back), project_response(&response));

    let mut request = base_request();
    request.provider_options = options(&[(
        "anthropic",
        json!({ "thinking": { "type": "enabled", "budget_tokens": 1024 } }),
    )]);
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert!(warnings.is_empty());
    let back = decode_request_wire(Protocol::AnthropicMessages, &wire);
    assert_eq!(
        back.provider_options["anthropic"]["thinking"]["budget_tokens"],
        json!(1024)
    );
}

/// responses 同族逃生舱：reasoning 项的 item id 与 encrypted_content 经响应
/// 往返原样还原（store:false 场景多轮 reasoning 的硬约束）。
#[test]
fn responses_reasoning_escape_hatch_survives_same_family() {
    let response = ChatResponse {
        id: "resp-encrypted".to_string(),
        model: "matrix-model".to_string(),
        content: vec![ContentPart::Reasoning {
            text: "推理摘要".to_string(),
            provider_options: options(&[(
                "openai",
                json!({ "item_id": "rs_1", "reasoning_encrypted_content": "enc" }),
            )]),
        }],
        finish_reason: FinishReason {
            unified: FinishReasonUnified::Stop,
            raw: None,
        },
        usage: Usage::default(),
        provider_metadata: HashMap::new(),
        warnings: Vec::new(),
    };
    let wire = encode_response_wire(Protocol::OpenAiResponses, &response);
    let back = decode_response_wire(Protocol::OpenAiResponses, &wire);
    assert_eq!(project_response(&back), project_response(&response));
}
