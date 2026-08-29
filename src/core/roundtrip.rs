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
//! 基线只覆盖当前已无损的语义面（消息序列含归并后的 system、工具轮、
//! tool_choice、reasoning 旋钮、采样参数）；跨族有损面（reasoning 丢弃、
//! `Other` finish、tool arguments 兜底等）的 warning 行为由专用用例声明，
//! 不进零告警基线。

use std::collections::HashMap;

use serde_json::{Value, json};
use similar_asserts::assert_eq;

use super::{anthropic_messages, openai_chat, openai_responses};
use crate::config::Protocol;
use crate::core::ir::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, FinishReasonUnified, Message,
    ReasoningEffort, Role, Tool, ToolChoice, Usage, Warning,
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
    decode_request_result(protocol, wire)
        .unwrap_or_else(|err| panic!("{protocol:?} 请求解码应成功: {err}"))
}

/// 解码请求的原始 `Result` 形态：供「非法形状应在入站拒绝」类用例断言错误。
fn decode_request_result(protocol: Protocol, wire: &Value) -> Result<ChatRequest, String> {
    match protocol {
        Protocol::OpenAiChat => openai_chat::decode_request(wire).map_err(|err| err.to_string()),
        Protocol::OpenAiResponses => {
            openai_responses::decode_request(wire).map_err(|err| err.to_string())
        }
        Protocol::AnthropicMessages => {
            anthropic_messages::decode_request(wire).map_err(|err| err.to_string())
        }
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
///
/// System 消息不进消息序列，按三协议出站统一的归并语义投影为合并文本
/// （逐消息拼接、跨消息 `\n\n` 连接、空文本跳过）——散布的多条 System 与
/// 合并后的单条投影相等，归并是协议整形而非语义损失。
fn project_request(request: &ChatRequest) -> Value {
    let system: String = request
        .messages
        .iter()
        .filter(|message| message.role == Role::System)
        .filter_map(|message| {
            let text: String = message
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            (!text.is_empty()).then_some(text)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    json!({
        "model": request.model,
        "system": system,
        "messages": request.messages.iter().filter(|message| message.role != Role::System).map(|message| json!({
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
        "tool_choice": request.tool_choice,
        "reasoning": request.reasoning,
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
        reasoning: None,
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
        parallel_tool_calls: None,
        provider_options: HashMap::new(),
        warnings: Vec::new(),
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

    // 采样参数中性化：thinking 激活时的采样整形属有损面（见专属用例），
    // 本格只锁定逃生舱无损语义。
    let mut request = base_request();
    request.temperature = None;
    request.top_p = None;
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

/// 各协议 tool_choice 的 wire 形状 → IR 类型化期望值表。
fn tool_choice_wire_shapes(protocol: Protocol) -> Vec<(Value, ToolChoice)> {
    match protocol {
        Protocol::OpenAiChat => vec![
            (json!("auto"), ToolChoice::Auto),
            (json!("none"), ToolChoice::None),
            (json!("required"), ToolChoice::Required),
            (
                json!({ "type": "function", "function": { "name": "f" } }),
                ToolChoice::Tool {
                    name: "f".to_string(),
                },
            ),
        ],
        Protocol::OpenAiResponses => vec![
            (json!("auto"), ToolChoice::Auto),
            (json!("none"), ToolChoice::None),
            (json!("required"), ToolChoice::Required),
            (
                json!({ "type": "function", "name": "f" }),
                ToolChoice::Tool {
                    name: "f".to_string(),
                },
            ),
        ],
        Protocol::AnthropicMessages => vec![
            (json!({ "type": "auto" }), ToolChoice::Auto),
            (json!({ "type": "none" }), ToolChoice::None),
            (json!({ "type": "any" }), ToolChoice::Required),
            (
                json!({ "type": "tool", "name": "f" }),
                ToolChoice::Tool {
                    name: "f".to_string(),
                },
            ),
        ],
    }
}

/// 三入站协议的全部 tool_choice wire 形状（含 anthropic `any`）都解码为
/// 正确的 IR 类型化值。
#[test]
fn tool_choice_wire_shapes_decode_to_typed_across_protocols() {
    for protocol in [
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
    ] {
        for (shape, expected) in tool_choice_wire_shapes(protocol) {
            let mut request = base_request();
            request.tool_choice = Some(expected.clone());
            let (mut wire, _) = encode_request_wire(protocol, &request);
            wire["tool_choice"] = shape.clone();
            let back = decode_request_wire(protocol, &wire);
            assert_eq!(
                back.tool_choice,
                Some(expected),
                "{protocol:?} 解码 {shape}"
            );
        }
    }
}

/// 四种 IR tool_choice 变体编码到每个协议时都产出该协议的规范 wire 形状。
#[test]
fn tool_choice_typed_encodes_to_each_protocol_shape() {
    for protocol in [
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
    ] {
        for (canonical, choice) in tool_choice_wire_shapes(protocol) {
            let mut request = base_request();
            request.tool_choice = Some(choice);
            let (wire, warnings) = encode_request_wire(protocol, &request);
            assert!(warnings.is_empty(), "{protocol:?} 编码不应有 warning");
            assert_eq!(wire["tool_choice"], canonical, "{protocol:?} 编码形状");
        }
    }
}

/// 四种 tool_choice 变体经全部有向对往返保持 IR 类型化语义
/// （`any↔required` 等差异由适配器归一，IR 面无感）。
#[test]
fn tool_choice_all_variants_survive_all_six_pairs() {
    let variants = vec![
        ToolChoice::Auto,
        ToolChoice::None,
        ToolChoice::Required,
        ToolChoice::Tool {
            name: "f".to_string(),
        },
    ];
    for variant in variants {
        for (a, b) in directed_pairs() {
            let mut request = base_request();
            request.tool_choice = Some(variant.clone());
            request_survives(a, b, &request);
        }
    }
}

/// anthropic 反语义映射：tool_choice 内的 `disable_parallel_tool_use` 取反
/// 进 IR 类型化 `parallel_tool_calls`（不进 tool_choice_extra 逃生舱）；同族
/// 出站取反写回，与 thinking 共存。tool_choice 上其余未知键仍经
/// tool_choice_extra 逃生舱原样回写。
#[test]
fn anthropic_disable_parallel_tool_use_maps_to_typed_knob() {
    let mut request = base_request();
    request.tool_choice = Some(ToolChoice::Auto);
    let (mut wire, _) = encode_request_wire(Protocol::AnthropicMessages, &request);
    wire["tool_choice"] = json!({ "type": "auto", "disable_parallel_tool_use": true });

    let decoded = decode_request_wire(Protocol::AnthropicMessages, &wire);
    assert_eq!(decoded.tool_choice, Some(ToolChoice::Auto));
    assert_eq!(decoded.parallel_tool_calls, Some(false));
    assert!(
        !decoded.provider_options.contains_key("anthropic"),
        "反语义字段已类型化，不应再进逃生舱"
    );

    let (wire_back, warnings) = encode_request_wire(Protocol::AnthropicMessages, &decoded);
    assert!(warnings.is_empty());
    assert_eq!(
        wire_back["tool_choice"],
        json!({ "type": "auto", "disable_parallel_tool_use": true })
    );

    // 反语义取反回程：disable=false 即允许并行，同族往返逐位还原。
    let (mut wire_open, _) = encode_request_wire(Protocol::AnthropicMessages, &request);
    wire_open["tool_choice"] = json!({ "type": "auto", "disable_parallel_tool_use": false });
    let decoded_open = decode_request_wire(Protocol::AnthropicMessages, &wire_open);
    assert_eq!(decoded_open.parallel_tool_calls, Some(true));
    let (wire_back, _) = encode_request_wire(Protocol::AnthropicMessages, &decoded_open);
    assert_eq!(
        wire_back["tool_choice"],
        json!({ "type": "auto", "disable_parallel_tool_use": false })
    );

    // 与 thinking 共存：同一 anthropic 对象内 thinking 与类型化旋钮互不影响。
    let mut both = base_request();
    both.tool_choice = Some(ToolChoice::Auto);
    both.provider_options = options(&[(
        "anthropic",
        json!({ "thinking": { "type": "enabled", "budget_tokens": 1024 } }),
    )]);
    let (mut wire_both, _) = encode_request_wire(Protocol::AnthropicMessages, &both);
    wire_both["tool_choice"] = json!({ "type": "auto", "disable_parallel_tool_use": true });
    let decoded_both = decode_request_wire(Protocol::AnthropicMessages, &wire_both);
    assert_eq!(
        decoded_both.provider_options["anthropic"]["thinking"]["budget_tokens"],
        json!(1024)
    );
    assert_eq!(decoded_both.parallel_tool_calls, Some(false));

    // tool_choice 上的其余未知键仍经 tool_choice_extra 逃生舱同族回写。
    let (mut wire_extra, _) = encode_request_wire(Protocol::AnthropicMessages, &request);
    wire_extra["tool_choice"] = json!({ "type": "auto", "custom_hint": "keep" });
    let decoded_extra = decode_request_wire(Protocol::AnthropicMessages, &wire_extra);
    assert_eq!(
        decoded_extra.provider_options["anthropic"]["tool_choice_extra"]["custom_hint"],
        json!("keep")
    );
    let (wire_back, _) = encode_request_wire(Protocol::AnthropicMessages, &decoded_extra);
    assert_eq!(wire_back["tool_choice"]["custom_hint"], json!("keep"));
}

/// 未知 tool_choice 形状在入站面直接拒绝，错误信息指明字段。
#[test]
fn unknown_tool_choice_shapes_are_rejected_at_ingress() {
    let cases = vec![
        (Protocol::OpenAiChat, json!("bogus")),
        (Protocol::OpenAiChat, json!({ "type": "allowed_tools" })),
        (Protocol::OpenAiChat, json!(true)),
        (Protocol::OpenAiResponses, json!("bogus")),
        (Protocol::OpenAiResponses, json!(true)),
        (Protocol::AnthropicMessages, json!({ "type": "bogus" })),
        (Protocol::AnthropicMessages, json!("auto")),
        (Protocol::AnthropicMessages, json!({ "type": "tool" })),
    ];
    for (protocol, shape) in cases {
        let mut request = base_request();
        request.tool_choice = Some(ToolChoice::Auto);
        let (mut wire, _) = encode_request_wire(protocol, &request);
        wire["tool_choice"] = shape;
        let err =
            decode_request_result(protocol, &wire).expect_err("未知 tool_choice 形状应被拒绝");
        assert!(
            err.contains("tool_choice"),
            "{protocol:?} 错误应指明字段: {err}"
        );
    }
}

/// 三协议 reasoning 旋钮：入站解码产出类型化档位，同族出站原样回传。
/// anthropic 走原始 thinking 逃生舱（budget 数值无损），chat 直出 effort。
#[test]
fn reasoning_knob_decodes_and_survives_same_family() {
    for value in [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ] {
        let mut request = base_request();
        request.reasoning = Some(ReasoningEffort::parse_effort(value).expect("档位应可解析"));
        let (mut wire, _) = encode_request_wire(Protocol::OpenAiChat, &request);
        wire["reasoning_effort"] = json!(value);
        let back = decode_request_wire(Protocol::OpenAiChat, &wire);
        assert_eq!(back.reasoning, request.reasoning, "chat 解码 {value}");
        let (wire_back, warnings) = encode_request_wire(Protocol::OpenAiChat, &back);
        assert!(warnings.is_empty());
        assert_eq!(wire_back["reasoning_effort"], json!(value));
    }

    // anthropic：enabled + budget → 逃生舱逐位还原，typed 按阶梯区间派生。
    // 采样参数中性化（同上，整形属有损面）。
    let mut request = base_request();
    request.temperature = None;
    request.top_p = None;
    request.provider_options = options(&[(
        "anthropic",
        json!({ "thinking": { "type": "enabled", "budget_tokens": 8000 } }),
    )]);
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert!(warnings.is_empty());
    let back = decode_request_wire(Protocol::AnthropicMessages, &wire);
    assert_eq!(back.reasoning, Some(ReasoningEffort::Medium));
    assert_eq!(
        back.provider_options["anthropic"]["thinking"]["budget_tokens"],
        json!(8000)
    );
    let (wire_back, _) = encode_request_wire(Protocol::AnthropicMessages, &back);
    assert_eq!(
        wire_back["thinking"],
        json!({ "type": "enabled", "budget_tokens": 8000 })
    );

    // disabled → typed None 档；adaptive → typed 缺席，均由逃生舱无损回传。
    for (thinking, expected) in [
        (json!({ "type": "disabled" }), Some(ReasoningEffort::None)),
        (json!({ "type": "adaptive" }), None),
    ] {
        let mut request = base_request();
        request.temperature = None;
        request.top_p = None;
        request.provider_options = options(&[("anthropic", json!({ "thinking": thinking }))]);
        let (wire, _) = encode_request_wire(Protocol::AnthropicMessages, &request);
        let back = decode_request_wire(Protocol::AnthropicMessages, &wire);
        assert_eq!(back.reasoning, expected);
        let (wire_back, _) = encode_request_wire(Protocol::AnthropicMessages, &back);
        assert_eq!(wire_back["thinking"], thinking);
    }
}

/// 类型化旋钮在本族逃生舱缺席时按协议形状兜底出站：anthropic 按模型形态
/// 分流（legacy 阶梯出 budget，adaptive 模型出 adaptive + 原生 effort），
/// chat/responses 出 effort 字符串。采样参数中性化，整形见专属用例。
#[test]
fn reasoning_typed_knob_encodes_when_escape_hatch_absent() {
    let mut request = base_request();
    request.temperature = None;
    request.top_p = None;
    request.reasoning = Some(ReasoningEffort::High);
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert!(warnings.is_empty());
    assert_eq!(
        wire["thinking"],
        json!({ "type": "enabled", "budget_tokens": 24576 })
    );

    let mut request = base_request();
    request.temperature = None;
    request.top_p = None;
    request.reasoning = Some(ReasoningEffort::Ultra);
    let (wire, _) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert_eq!(
        wire["thinking"],
        json!({ "type": "enabled", "budget_tokens": 128_000 })
    );

    let mut request = base_request();
    request.temperature = None;
    request.top_p = None;
    request.reasoning = Some(ReasoningEffort::None);
    let (wire, _) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert_eq!(wire["thinking"], json!({ "type": "disabled" }));

    let mut request = base_request();
    request.reasoning = Some(ReasoningEffort::XHigh);
    let (wire, _) = encode_request_wire(Protocol::OpenAiChat, &request);
    assert_eq!(wire["reasoning_effort"], json!("xhigh"));
    let (wire, _) = encode_request_wire(Protocol::OpenAiResponses, &request);
    assert_eq!(wire["reasoning"], json!({ "effort": "xhigh" }));
}

/// responses 面板逃生舱优先：含 effort 之外字段（如 summary）的原始面板
/// 出站原样回传，类型化字段零双写。
#[test]
fn responses_reasoning_escape_hatch_wins_over_typed_knob() {
    let mut request = base_request();
    request.reasoning = Some(ReasoningEffort::High);
    request.provider_options = options(&[(
        "openai",
        json!({ "reasoning": { "effort": "low", "summary": "auto" } }),
    )]);
    let (wire, _) = encode_request_wire(Protocol::OpenAiResponses, &request);
    assert_eq!(
        wire["reasoning"],
        json!({ "effort": "low", "summary": "auto" })
    );
}

/// 未知 effort 档位在入站面拒绝（chat 与 responses 面板同规）。
#[test]
fn unknown_reasoning_effort_rejected_at_ingress() {
    for (protocol, key, shape) in [
        (Protocol::OpenAiChat, "reasoning_effort", json!("bogus")),
        (
            Protocol::OpenAiResponses,
            "reasoning",
            json!({ "effort": "bogus" }),
        ),
    ] {
        let mut request = base_request();
        request.reasoning = Some(ReasoningEffort::High);
        let (mut wire, _) = encode_request_wire(protocol, &request);
        wire[key] = shape;
        let err = decode_request_result(protocol, &wire).expect_err("未知 effort 档位应被拒绝");
        assert!(err.contains(key), "{protocol:?} 错误应指明字段: {err}");
    }
}

/// chat 入站的 developer 角色按 System 处理：跨协议出站归并语义与 system
/// 一致（anthropic 提升为顶层 system，responses 归 system 项）。
#[test]
fn developer_role_treats_as_system_across_protocols() {
    let wire = json!({
        "model": "gpt-5",
        "messages": [
            { "role": "developer", "content": "输出须为 JSON" },
            { "role": "user", "content": "上海天气如何？" }
        ]
    });
    let ir = decode_request_wire(Protocol::OpenAiChat, &wire);
    assert!(matches!(
        ir.messages.as_slice(),
        [
            Message {
                role: Role::System,
                ..
            },
            Message {
                role: Role::User,
                ..
            }
        ]
    ));

    let (anthropic_wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &ir);
    assert!(warnings.is_empty());
    assert_eq!(anthropic_wire["system"], json!("输出须为 JSON"));

    let (responses_wire, warnings) = encode_request_wire(Protocol::OpenAiResponses, &ir);
    assert!(warnings.is_empty());
    assert_eq!(responses_wire["instructions"], json!("输出须为 JSON"));
}

/// chat 入站 `max_completion_tokens` 归一进 IR 后三方向出站不丢：
/// anthropic 映射 `max_tokens`，responses 映射 `max_output_tokens`，
/// chat 同族按请求原字段回写。
#[test]
fn max_completion_tokens_survives_three_outbound_directions() {
    let wire = json!({
        "model": "o4-mini",
        "messages": [{ "role": "user", "content": "上海天气如何？" }],
        "max_completion_tokens": 2048
    });
    let ir = decode_request_wire(Protocol::OpenAiChat, &wire);

    let (anthropic_wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &ir);
    assert!(warnings.is_empty());
    assert_eq!(anthropic_wire["max_tokens"], json!(2048));

    let (responses_wire, warnings) = encode_request_wire(Protocol::OpenAiResponses, &ir);
    assert!(warnings.is_empty());
    assert_eq!(responses_wire["max_output_tokens"], json!(2048));

    let (chat_wire, warnings) = encode_request_wire(Protocol::OpenAiChat, &ir);
    assert!(warnings.is_empty());
    assert_eq!(chat_wire["max_completion_tokens"], json!(2048));
    assert!(chat_wire.get("max_tokens").is_none(), "不应双写旧字段");
}

/// chat 入站 `reasoning_content` 经 anthropic thinking 块中转后回 chat：
/// 无 signature 的 reasoning 语义保留，全程零告警。
#[test]
fn chat_reasoning_content_survives_via_anthropic_thinking_block() {
    let reasoning_text = "先算 900 ÷ 5 = 180，再算 25 ÷ 5 = 5。";
    let wire = json!({
        "model": "deepseek-reasoner",
        "messages": [
            { "role": "user", "content": "925 ÷ 5 等于多少？" },
            {
                "role": "assistant",
                "content": "925 ÷ 5 = 185",
                "reasoning_content": reasoning_text
            }
        ]
    });
    let ir = decode_request_wire(Protocol::OpenAiChat, &wire);
    assert!(matches!(
        &ir.messages[1].content[0],
        ContentPart::Reasoning { text, .. } if text == reasoning_text
    ));

    // anthropic 出站：无 signature 的 reasoning 预置为无 signature 的 thinking 块。
    let (anthropic_wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &ir);
    assert!(warnings.is_empty());
    let assistant = anthropic_wire["messages"]
        .as_array()
        .expect("应有消息数组")
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("应有 assistant 消息");
    assert_eq!(
        assistant["content"][0],
        json!({ "type": "thinking", "thinking": reasoning_text })
    );

    // anthropic 入站解码回 Reasoning part，回 chat 面语义保留。
    let via_anthropic = decode_request_wire(Protocol::AnthropicMessages, &anthropic_wire);
    let (chat_wire, warnings) = encode_request_wire(Protocol::OpenAiChat, &via_anthropic);
    assert!(warnings.is_empty());
    assert_eq!(
        chat_wire["messages"][1]["reasoning_content"],
        json!(reasoning_text)
    );
}

/// reasoning 旋钮的跨族全链路：chat 入站 → anthropic 中转 → chat 回归，
/// 类型化档位在投影面无损。anthropic 中转会在 IR 上固化 thinking 逃生舱，
/// 回到 chat 面时该逃生舱显式丢弃并告警——属声明过的有损面，告警形状
/// 在此一并锁定。
#[test]
fn reasoning_knob_survives_chat_anthropic_chat_cycle() {
    let mut request = base_request();
    request.temperature = Some(1.0);
    request.top_p = None;
    request.reasoning = Some(ReasoningEffort::High);
    let (wire_a, warnings_a) = encode_request_wire(Protocol::OpenAiChat, &request);
    assert!(warnings_a.is_empty());
    let via_a = decode_request_wire(Protocol::OpenAiChat, &wire_a);

    let (wire_b, warnings_b) = encode_request_wire(Protocol::AnthropicMessages, &via_a);
    assert!(warnings_b.is_empty());
    assert_eq!(
        wire_b["thinking"],
        json!({ "type": "enabled", "budget_tokens": 24576 })
    );
    let via_b = decode_request_wire(Protocol::AnthropicMessages, &wire_b);
    assert_eq!(via_b.reasoning, Some(ReasoningEffort::High));

    let (wire_back, warnings_back) = encode_request_wire(Protocol::OpenAiChat, &via_b);
    assert_eq!(wire_back["reasoning_effort"], json!("high"));
    assert!(matches!(
        warnings_back.as_slice(),
        [Warning::Unsupported { feature, .. }] if feature == "provider_options"
    ));
    let back = decode_request_wire(Protocol::OpenAiChat, &wire_back);
    assert_eq!(project_request(&back), project_request(&request));
}

/// 跨族映射验收：chat 入站 effort 经 IR 到 anthropic 出站按模型形态分流
/// （adaptive 模型 → adaptive + output_config.effort；legacy 模型 → budget
/// 阶梯），effort 面钳制 Minimal→low、Ultra→max，budget 面 Ultra→128000。
#[test]
fn chat_effort_maps_to_anthropic_by_model_form() {
    let user = "上海天气如何？";
    let chat_wire = |model: &str, effort: &str| {
        json!({
            "model": model,
            "messages": [{ "role": "user", "content": user }],
            "reasoning_effort": effort
        })
    };

    let ir = decode_request_wire(Protocol::OpenAiChat, &chat_wire("claude-opus-4-6", "high"));
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &ir);
    assert!(warnings.is_empty());
    assert_eq!(wire["thinking"], json!({ "type": "adaptive" }));
    assert_eq!(wire["output_config"], json!({ "effort": "high" }));

    let ir = decode_request_wire(
        Protocol::OpenAiChat,
        &chat_wire("claude-sonnet-4-5", "high"),
    );
    let (wire, _) = encode_request_wire(Protocol::AnthropicMessages, &ir);
    assert_eq!(
        wire["thinking"],
        json!({ "type": "enabled", "budget_tokens": 24576 })
    );

    for (effort, native) in [("minimal", "low"), ("ultra", "max")] {
        let ir = decode_request_wire(Protocol::OpenAiChat, &chat_wire("claude-opus-4-6", effort));
        let (wire, _) = encode_request_wire(Protocol::AnthropicMessages, &ir);
        assert_eq!(wire["output_config"], json!({ "effort": native }));
    }
    let ir = decode_request_wire(
        Protocol::OpenAiChat,
        &chat_wire("claude-sonnet-4-5", "ultra"),
    );
    let (wire, _) = encode_request_wire(Protocol::AnthropicMessages, &ir);
    assert_eq!(wire["thinking"]["budget_tokens"], json!(128_000));
}

/// anthropic 入站 `output_config.effort` 捕获进类型化旋钮并同族往返；
/// 与 `thinking.budget_tokens` 并存时显式 effort 优先，原始配置双双经
/// 逃生舱无损回传。
#[test]
fn anthropic_output_config_effort_captures_to_typed_and_roundtrips() {
    let anthropic_wire = json!({
        "model": "claude-opus-4-6",
        "max_tokens": 1024,
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "你好" }] }],
        "output_config": { "effort": "xhigh" }
    });
    let ir = decode_request_wire(Protocol::AnthropicMessages, &anthropic_wire);
    assert_eq!(ir.reasoning, Some(ReasoningEffort::XHigh));
    let (wire_back, warnings) = encode_request_wire(Protocol::AnthropicMessages, &ir);
    assert!(warnings.is_empty());
    assert_eq!(wire_back["output_config"], json!({ "effort": "xhigh" }));

    let both = json!({
        "model": "claude-opus-4-6",
        "max_tokens": 1024,
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "你好" }] }],
        "thinking": { "type": "enabled", "budget_tokens": 8000 },
        "output_config": { "effort": "high" }
    });
    let ir = decode_request_wire(Protocol::AnthropicMessages, &both);
    assert_eq!(ir.reasoning, Some(ReasoningEffort::High));
    let (wire_back, warnings) = encode_request_wire(Protocol::AnthropicMessages, &ir);
    assert!(warnings.is_empty());
    assert_eq!(
        wire_back["thinking"],
        json!({ "type": "enabled", "budget_tokens": 8000 })
    );
    assert_eq!(wire_back["output_config"], json!({ "effort": "high" }));
}

/// tool_choice 强制（any/tool）时 thinking 配置整体剥离且 warning 可观测；
/// auto 不剥离。剥离后 thinking 未激活，采样参数不再受约束整形。
#[test]
fn forced_tool_choice_strips_thinking_with_warning() {
    let mut request = base_request();
    request.temperature = Some(0.7);
    request.reasoning = Some(ReasoningEffort::High);
    request.tool_choice = Some(ToolChoice::Required);
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert!(wire.get("thinking").is_none());
    assert!(wire.get("output_config").is_none());
    assert_eq!(wire["temperature"], json!(0.7));
    assert_eq!(
        warnings,
        vec![Warning::compatibility(
            "thinking",
            "tool_choice 强制工具调用时 Anthropic 拒绝 thinking 配置，已剥离（含 output_config.effort）",
        )]
    );

    let mut request = base_request();
    request.provider_options = options(&[(
        "anthropic",
        json!({ "thinking": { "type": "enabled", "budget_tokens": 1024 } }),
    )]);
    request.tool_choice = Some(ToolChoice::Tool {
        name: "f".to_string(),
    });
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert!(wire.get("thinking").is_none());
    assert!(matches!(
        warnings.as_slice(),
        [Warning::Compatibility { feature, .. }] if feature == "thinking"
    ));

    let mut request = base_request();
    request.temperature = None;
    request.top_p = None;
    request.reasoning = Some(ReasoningEffort::High);
    request.tool_choice = Some(ToolChoice::Auto);
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert!(warnings.is_empty());
    assert_eq!(
        wire["thinking"],
        json!({ "type": "enabled", "budget_tokens": 24576 })
    );
}

/// thinking 激活时的采样整形：temperature→1、top_p 下限 0.95、top_k 剥离，
/// 各动作记 compatibility warning；未激活（disabled 或旋钮缺席）时采样
/// 参数原样透传。
#[test]
fn thinking_active_sampling_shaped_with_warning() {
    let thinking = json!({ "type": "enabled", "budget_tokens": 1024 });
    let mut request = base_request();
    request.temperature = Some(0.7);
    request.top_p = Some(0.9);
    request.top_k = Some(40);
    request.provider_options = options(&[("anthropic", json!({ "thinking": thinking }))]);
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert_eq!(wire["temperature"], json!(1.0));
    assert_eq!(wire["top_p"], json!(0.95));
    assert!(wire.get("top_k").is_none());
    assert_eq!(
        warnings,
        vec![
            Warning::compatibility("temperature", "thinking 激活时 temperature 0.7 整形为 1"),
            Warning::compatibility("top_p", "thinking 激活时 top_p 0.9 整形为 0.95"),
            Warning::compatibility("top_k", "thinking 激活时 top_k 已剥离"),
        ]
    );

    // 已在约束内：零整形零告警。
    let mut request = base_request();
    request.temperature = Some(1.0);
    request.top_p = Some(0.95);
    request.provider_options = options(&[(
        "anthropic",
        json!({ "thinking": { "type": "enabled", "budget_tokens": 1024 } }),
    )]);
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert_eq!(wire["temperature"], json!(1.0));
    assert_eq!(wire["top_p"], json!(0.95));
    assert!(warnings.is_empty());

    // thinking disabled：采样参数原样。
    let mut request = base_request();
    request.temperature = Some(0.7);
    request.top_p = Some(0.9);
    request.top_k = Some(40);
    request.provider_options =
        options(&[("anthropic", json!({ "thinking": { "type": "disabled" } }))]);
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert_eq!(wire["temperature"], json!(0.7));
    assert_eq!(wire["top_p"], json!(0.9));
    assert_eq!(wire["top_k"], json!(40));
    assert!(warnings.is_empty());

    // thinking 旋钮整体缺席：采样参数原样。
    let mut request = base_request();
    request.top_k = Some(40);
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert_eq!(wire["temperature"], json!(0.5));
    assert_eq!(wire["top_p"], json!(0.9));
    assert_eq!(wire["top_k"], json!(40));
    assert!(warnings.is_empty());
}

/// chat 入站非法 tool arguments 的兜底是有损面，不进基线矩阵（矩阵要求
/// 零告警的无损面）：解码成功、input 兜底空对象、warning 记录在请求上，
/// 跨族出站全程无编码告警。
#[test]
fn illegal_tool_arguments_fallback_declared_lossy() {
    let wire = json!({
        "model": "matrix-model",
        "messages": [
            {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "get_weather", "arguments": "{oops" }
                }]
            },
            { "role": "tool", "tool_call_id": "call_1", "content": "晴" },
        ],
    });
    let request = decode_request_wire(Protocol::OpenAiChat, &wire);
    assert_eq!(
        request.warnings,
        vec![Warning::compatibility(
            "tool_arguments",
            "tool call get_weather 的 arguments 非合法 JSON 对象，已兜底为空对象",
        )]
    );

    // 兜底后的 tool_call 跨族出站为空 input，编码零告警。
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert_eq!(wire["messages"][0]["content"][0]["type"], "tool_use");
    assert_eq!(wire["messages"][0]["content"][0]["input"], json!({}));
    assert!(warnings.is_empty());
}

/// tool 的根级 union schema 只在 anthropic 出站面归一化：摊平合并并记
/// warning；chat 出站按原样透传（OpenAI 系接受根级 union），零告警。
#[test]
fn root_union_input_schema_normalizes_only_for_anthropic() {
    let mut request = base_request();
    request.tools[0].parameters = Some(json!({
        "anyOf": [
            { "type": "object", "properties": { "city": { "type": "string" } } },
            { "type": "object", "properties": { "days": { "type": "number" } } },
        ],
    }));

    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert_eq!(
        wire["tools"][0]["input_schema"],
        json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" },
                "days": { "type": "number" },
            },
        })
    );
    assert!(matches!(
        warnings.as_slice(),
        [Warning::Compatibility { feature, .. }] if feature == "input_schema"
    ));

    let (wire, warnings) = encode_request_wire(Protocol::OpenAiChat, &request);
    assert!(
        wire["tools"][0]["function"]["parameters"]
            .get("anyOf")
            .is_some()
    );
    assert!(warnings.is_empty());
}

/// 请求级未知字段逃生舱：白名单外的顶层字段入站收进
/// `provider_options[<本协议>]["extra"]`，同族出站原样回写（往返 byte-shape
/// 一致），与既有逃生舱键（max_completion_tokens / thinking / reasoning 面板）
/// 同一 provider 对象内共存。
#[test]
fn unknown_fields_capture_and_roundtrip_same_family() {
    let chat = json!({
        "model": "gpt-4o",
        "messages": [{ "role": "user", "content": "hi" }],
        "max_completion_tokens": 2048,
        "service_tier": "flex",
        "logprobs": true,
    });
    let ir = decode_request_wire(Protocol::OpenAiChat, &chat);
    assert_eq!(
        ir.provider_options["openai"]["extra"],
        json!({ "service_tier": "flex", "logprobs": true })
    );
    assert_eq!(
        ir.provider_options["openai"]["max_completion_tokens"],
        json!(2048)
    );
    let (wire_back, warnings) = encode_request_wire(Protocol::OpenAiChat, &ir);
    assert!(warnings.is_empty());
    assert_eq!(wire_back, chat, "chat 同族未知字段往返 byte-shape 一致");

    let responses = json!({
        "model": "gpt-4o",
        "input": [{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }],
        "reasoning": { "effort": "low" },
        "service_tier": "flex",
    });
    let ir = decode_request_wire(Protocol::OpenAiResponses, &responses);
    assert_eq!(
        ir.provider_options["openai"]["extra"],
        json!({ "service_tier": "flex" })
    );
    let (wire_back, warnings) = encode_request_wire(Protocol::OpenAiResponses, &ir);
    assert!(warnings.is_empty());
    assert_eq!(
        wire_back, responses,
        "responses 同族未知字段往返 byte-shape 一致"
    );

    let anthropic = json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 1024,
        "messages": [{ "role": "user", "content": "hi" }],
        "thinking": { "type": "disabled" },
        "metadata": { "user_id": "u_1" },
    });
    let ir = decode_request_wire(Protocol::AnthropicMessages, &anthropic);
    assert_eq!(
        ir.provider_options["anthropic"]["extra"],
        json!({ "metadata": { "user_id": "u_1" } })
    );
    assert_eq!(
        ir.provider_options["anthropic"]["thinking"],
        json!({ "type": "disabled" })
    );
    let (wire_back, warnings) = encode_request_wire(Protocol::AnthropicMessages, &ir);
    assert!(warnings.is_empty());
    assert_eq!(
        wire_back, anthropic,
        "anthropic 同族未知字段往返 byte-shape 一致"
    );
}

/// 未知字段跨族出站：目标协议不表达该字段，丢弃并记 unknown_fields
/// warning（details 携带字段名，可观测）。anthropic 入站来源在 OpenAI 两个
/// 出站面均告警；chat 入站来源在 anthropic 出站面告警。
#[test]
fn unknown_fields_warn_and_drop_on_cross_family_outbound() {
    let chat = json!({
        "model": "gpt-4o",
        "messages": [{ "role": "user", "content": "hi" }],
        "logprobs": true,
    });
    let ir = decode_request_wire(Protocol::OpenAiChat, &chat);
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &ir);
    assert!(wire.get("logprobs").is_none(), "跨族出站不应携带未知字段");
    assert!(matches!(
        warnings.as_slice(),
        [Warning::Unsupported { feature, details }]
            if feature == "unknown_fields" && details.as_deref().is_some_and(|d| d.contains("logprobs"))
    ));

    let anthropic = json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 1024,
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }],
        "metadata": { "user_id": "u_1" },
    });
    let ir = decode_request_wire(Protocol::AnthropicMessages, &anthropic);
    for protocol in [Protocol::OpenAiChat, Protocol::OpenAiResponses] {
        let (wire, warnings) = encode_request_wire(protocol, &ir);
        assert!(
            wire.get("metadata").is_none(),
            "{protocol:?} 跨族出站不应携带未知字段"
        );
        assert!(matches!(
            warnings.as_slice(),
            [Warning::Unsupported { feature, .. }] if feature == "unknown_fields"
        ));
    }
}

/// openai 族内（chat ↔ responses 共用 openai 逃生舱键）未知字段直接回写：
/// 两协议均为 OpenAI 家族，常见共享字段（service_tier、metadata 等）跨协议
/// 不丢不告警。
#[test]
fn unknown_fields_write_back_across_openai_protocols() {
    let chat = json!({
        "model": "gpt-4o",
        "messages": [{ "role": "user", "content": "hi" }],
        "service_tier": "flex",
    });
    let ir = decode_request_wire(Protocol::OpenAiChat, &chat);
    let (responses_wire, warnings) = encode_request_wire(Protocol::OpenAiResponses, &ir);
    assert!(warnings.is_empty());
    assert_eq!(responses_wire["service_tier"], json!("flex"));

    let responses = json!({
        "model": "gpt-4o",
        "input": [{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }],
        "metadata": { "request_tag": "t1" },
    });
    let ir = decode_request_wire(Protocol::OpenAiResponses, &responses);
    let (chat_wire, warnings) = encode_request_wire(Protocol::OpenAiChat, &ir);
    assert!(warnings.is_empty());
    assert_eq!(chat_wire["metadata"], json!({ "request_tag": "t1" }));
}

/// 基线语义面：散布的多条 System 消息经全部有向对无损——三协议出站统一
/// 归并（单条置顶 / 顶层 system / instructions），投影把 System 归一为
/// 合并文本后往返相等，全程零告警。
#[test]
fn multiple_system_messages_survive_all_six_pairs() {
    let mut request = base_request();
    // 散布：中段再插一条 System 消息（第二段系统提示）。
    let second = Message {
        role: Role::System,
        content: vec![text_part("输出一律使用 JSON")],
        provider_options: HashMap::new(),
    };
    request.messages.insert(2, second);
    for (a, b) in directed_pairs() {
        request_survives(a, b, &request);
    }
}

/// parallel_tool_calls 类型化旋钮的三协议映射：chat/responses 原生承载同族
/// 原样往返；anthropic 无请求级字段，以 `tool_choice.disable_parallel_tool_use`
/// 反语义承载（取反）——禁并行时无显式 tool_choice 按 auto 兜底合成，允许
/// 并行为缺省语义不合成、经 anthropic 中转后旋钮落空（等价默认，非信息损失）。
#[test]
fn parallel_tool_calls_knob_maps_across_protocols() {
    for protocol in [Protocol::OpenAiChat, Protocol::OpenAiResponses] {
        for parallel in [true, false] {
            let mut request = base_request();
            request.parallel_tool_calls = Some(parallel);
            let (wire, warnings) = encode_request_wire(protocol, &request);
            assert!(warnings.is_empty());
            assert_eq!(wire["parallel_tool_calls"], json!(parallel), "{protocol:?}");
            let back = decode_request_wire(protocol, &wire);
            assert_eq!(back.parallel_tool_calls, Some(parallel), "{protocol:?}");
        }
    }

    // chat → anthropic：false 取反为 disable=true（无 tool_choice 按自动合成）。
    let mut request = base_request();
    request.parallel_tool_calls = Some(false);
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert!(warnings.is_empty());
    assert_eq!(
        wire["tool_choice"],
        json!({ "type": "auto", "disable_parallel_tool_use": true })
    );
    let back = decode_request_wire(Protocol::AnthropicMessages, &wire);
    assert_eq!(back.parallel_tool_calls, Some(false));

    // 允许并行为 anthropic 缺省语义：不合成 tool_choice，中转后旋钮落空。
    let mut request = base_request();
    request.parallel_tool_calls = Some(true);
    let (wire, warnings) = encode_request_wire(Protocol::AnthropicMessages, &request);
    assert!(warnings.is_empty());
    assert!(
        wire.get("tool_choice").is_none(),
        "允许并行不应合成 tool_choice"
    );
    let back = decode_request_wire(Protocol::AnthropicMessages, &wire);
    assert_eq!(back.parallel_tool_calls, None);

    // anthropic 入站（any + disable）→ chat 出站：取反还原 parallel 字段。
    let anthropic = json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 1024,
        "messages": [{ "role": "user", "content": "hi" }],
        "tools": [{ "name": "get_weather", "input_schema": { "type": "object" } }],
        "tool_choice": { "type": "any", "disable_parallel_tool_use": true },
    });
    let ir = decode_request_wire(Protocol::AnthropicMessages, &anthropic);
    assert_eq!(ir.tool_choice, Some(ToolChoice::Required));
    assert_eq!(ir.parallel_tool_calls, Some(false));
    let (chat_wire, warnings) = encode_request_wire(Protocol::OpenAiChat, &ir);
    assert!(warnings.is_empty());
    assert_eq!(chat_wire["parallel_tool_calls"], json!(false));
    assert_eq!(chat_wire["tool_choice"], json!("required"));

    // chat → anthropic → chat 全链路：false 原样还原；tool_choice None→auto
    // 是 anthropic 承载面的协议整形（承载禁并行必须挂 tool_choice）。
    let mut request = base_request();
    request.parallel_tool_calls = Some(false);
    let (anth_wire, _) = encode_request_wire(Protocol::AnthropicMessages, &request);
    let via_anthropic = decode_request_wire(Protocol::AnthropicMessages, &anth_wire);
    let (chat_wire, warnings) = encode_request_wire(Protocol::OpenAiChat, &via_anthropic);
    assert!(warnings.is_empty());
    assert_eq!(chat_wire["parallel_tool_calls"], json!(false));
    assert_eq!(chat_wire["tool_choice"], json!("auto"));
}
