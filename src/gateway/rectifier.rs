//! 请求整流器：上游 400 中可预测、可自动修正的一类，按错误消息模式匹配后
//! 做最小修正并重试一次。
//!
//! 输入是上游 400 的错误消息与原请求 IR，输出是修正后的请求 IR（附动作
//! 明细与回传下游的 warnings）或放弃。修正只在确有可修正内容时发生——
//! 模式命中但 IR 无对应改写余地时返回 `None`，避免对同一 400 空转重试。
//! 直通快路径无 IR，不参与整流。

use crate::core::ir::{
    ChatRequest, ContentPart, Message, Role, ToolChoice, Warning, warning_feature,
};
use crate::core::schema::normalize_object_root;

/// 一次整流的产物。
pub(super) struct Rectification {
    /// 修正后的请求 IR；动作说明已附在 warnings，随响应面回传下游。
    pub request: ChatRequest,
    /// 整流规则：审计与日志按此归因。
    pub rule: Rule,
    /// 实际发生的修正动作明细（人读）。
    pub actions: Vec<String>,
}

/// 整流规则：错误消息模式所属的可修正类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Rule {
    /// thinking signature 失效类：剥离 reasoning 内容重试。
    Signature,
    /// tool schema / tool_choice 不合规类：归一化与降级重试。
    ToolShape,
}

impl Rule {
    /// 审计事件里的规则名。
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Signature => "signature",
            Self::ToolShape => "tool_shape",
        }
    }
}

/// 按上游 400 错误消息匹配整流规则并对请求 IR 做最小修正。
///
/// 无规则命中或命中后无改写余地时返回 `None`（放弃整流）。
pub(super) fn rectify(message: &str, request: &ChatRequest) -> Option<Rectification> {
    let lowered = message.to_lowercase();
    if matches_signature_failure(&lowered) {
        return rectify_signature(request);
    }
    if matches_tool_shape(&lowered) {
        return rectify_tool_shape(&lowered, request);
    }
    None
}

/// thinking signature 失效类错误消息模式（不区分大小写）。
///
/// 覆盖 signature 无效/缺失/多余、thinking 块序列不合法与块被改写四族现实
/// 错误；错误体常见嵌套 JSON 字符串形态，子串匹配天然穿透。
fn matches_signature_failure(lowered: &str) -> bool {
    let thinking = lowered.contains("thinking") || lowered.contains("redacted_thinking");
    let signature = lowered.contains("signature");
    // signature 在 thinking 块中无效。
    (signature && thinking && lowered.contains("invalid") && lowered.contains("block"))
        // 第三方渠道的 thought signature 无效。
        || (lowered.contains("thought signature")
            && (lowered.contains("not valid") || lowered.contains("invalid")))
        // assistant 消息必须以 thinking 块开头（工具调用链路缺前置）。
        || lowered.contains("must start with a thinking block")
        // 期望 thinking 块却遇到 tool_use（顺序非法）。
        || (lowered.contains("expected")
            && thinking
            && lowered.contains("found")
            && lowered.contains("tool_use"))
        // signature 字段缺失。
        || (signature && lowered.contains("field required"))
        // signature 字段不被上游接受。
        || (signature && lowered.contains("extra inputs are not permitted"))
        // thinking 块被改写后 signature 校验失败。
        || (thinking && lowered.contains("cannot be modified"))
}

/// tool schema / tool_choice 不合规类错误消息模式（不区分大小写）。
fn matches_tool_shape(lowered: &str) -> bool {
    let schema_reject = lowered.contains("schema")
        && [
            "invalid",
            "must",
            "should",
            "expected",
            "not of type",
            "unable to",
        ]
        .iter()
        .any(|marker| lowered.contains(marker));
    let tool_choice_reject = lowered.contains("tool_choice")
        && [
            "invalid",
            "unknown",
            "not supported",
            "unsupported",
            "must",
            "expected",
            "not found",
        ]
        .iter()
        .any(|marker| lowered.contains(marker));
    schema_reject || tool_choice_reject
}

/// signature 失效的最小修正：剥离全部 reasoning part；剥离后末条 assistant
/// 含工具调用且 thinking 请求配置为显式 enabled 时，连带剥离 thinking 配置
/// （否则重试仍会因「工具调用链路缺 thinking 前缀」被拒）。
fn rectify_signature(request: &ChatRequest) -> Option<Rectification> {
    let mut actions = Vec::new();
    let mut corrected = request.clone();
    let mut stripped = 0usize;
    for message in &mut corrected.messages {
        let before = message.content.len();
        message
            .content
            .retain(|part| !matches!(part, ContentPart::Reasoning { .. }));
        stripped += before - message.content.len();
    }
    if stripped > 0 {
        actions.push(format!("剥离 {stripped} 处 reasoning part"));
        corrected.warnings.push(Warning::unsupported(
            warning_feature::REASONING,
            format!("整流重试剥离 {stripped} 处 reasoning part"),
        ));
    }
    if thinking_requires_prefix(&corrected) && last_assistant_has_tool_call(&corrected.messages) {
        if let Some(anthropic) = corrected.provider_options.get_mut("anthropic")
            && let Some(options) = anthropic.as_object_mut()
        {
            options.remove("thinking");
        }
        corrected.reasoning = None;
        actions.push("剥离 thinking 请求配置".to_string());
        corrected.warnings.push(Warning::compatibility(
            warning_feature::THINKING,
            "整流重试剥离 thinking 请求配置（工具调用链路缺 thinking 前缀）",
        ));
    }
    (stripped > 0).then_some(Rectification {
        request: corrected,
        rule: Rule::Signature,
        actions,
    })
}

/// thinking 请求面是否会让重试仍要求「assistant 消息以 thinking 块开头」：
/// 本族逃生舱为显式 `enabled`（legacy budget 形态），或 thinking 仅由类型化
/// effort 旋钮承载（出站形态取决于模型代次，整流器不感知模型，保守剥离）。
/// 逃生舱为 adaptive/auto 时上游自动补齐 thinking，无需剥离。
fn thinking_requires_prefix(request: &ChatRequest) -> bool {
    let hatch = request.provider_options.get("anthropic");
    let hatch_thinking = hatch.and_then(|options| options.get("thinking"));
    let hatch_enabled = hatch_thinking
        .and_then(|thinking| thinking.get("type"))
        .and_then(|kind| kind.as_str())
        .is_some_and(|kind| kind == "enabled");
    let knob_only = hatch_thinking.is_none() && request.reasoning.is_some();
    hatch_enabled || knob_only
}

fn last_assistant_has_tool_call(messages: &[Message]) -> bool {
    messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .is_some_and(|message| {
            message
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::ToolCall { .. }))
        })
}

/// tool 形状不合规的最小修正：tool 输入模式归一化（object 根兜底与根级
/// union 摊平），具名 tool_choice 被上游拒绝时降级 `auto`。
fn rectify_tool_shape(lowered: &str, request: &ChatRequest) -> Option<Rectification> {
    let mut actions = Vec::new();
    let mut corrected = request.clone();
    for tool in &mut corrected.tools {
        let Some(parameters) = &tool.parameters else {
            continue;
        };
        let (normalized, action) = normalize_object_root(Some(parameters));
        let Some(action) = action else {
            continue;
        };
        actions.push(format!("tool {} 的 input_schema {}", tool.name, action));
        corrected.warnings.push(Warning::compatibility(
            warning_feature::INPUT_SCHEMA,
            format!(
                "整流重试归一化 tool {} 的 input_schema：{action}",
                tool.name
            ),
        ));
        tool.parameters = Some(normalized);
    }
    if lowered.contains("tool_choice")
        && let Some(ToolChoice::Tool { name }) = corrected.tool_choice.clone()
    {
        actions.push(format!("tool_choice 指定工具 {name} 被上游拒绝，降级 auto"));
        corrected.warnings.push(Warning::compatibility(
            warning_feature::TOOL_CHOICE,
            format!("整流重试将 tool_choice 从工具 {name} 降级为 auto"),
        ));
        corrected.tool_choice = Some(ToolChoice::Auto);
    }
    (!actions.is_empty()).then_some(Rectification {
        request: corrected,
        rule: Rule::ToolShape,
        actions,
    })
}

#[cfg(test)]
mod tests {
    use super::{Rule, rectify};
    use crate::core::ir::{ChatRequest, ContentPart, Tool, ToolChoice, Warning, warning_feature};
    use serde_json::{Value, json};

    fn request(content: Value) -> ChatRequest {
        serde_json::from_value(json!({
            "model": "m",
            "messages": content,
        }))
        .expect("测试请求应能解码")
    }

    fn assistant_with_reasoning_and_tool_call() -> ChatRequest {
        request(json!([
            {
                "role": "assistant",
                "content": [
                    { "type": "reasoning", "text": "thinking", "provider_options": { "anthropic": { "signature": "sig" } } },
                    { "type": "tool_call", "tool_call_id": "t1", "tool_name": "f", "input": {} },
                ],
            },
            {
                "role": "user",
                "content": [{ "type": "text", "text": "continue" }],
            },
        ]))
    }

    #[test]
    fn signature_patterns_match_real_error_shapes() {
        let cases = [
            "messages.1.content.0: Invalid `signature` in `thinking` block",
            r#"{"error":{"message":"{\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"Invalid `signature` in `thinking` block\"}}"}}"#,
            "Unable to submit request because Thought signature is not valid..",
            "a final `assistant` message must start with a thinking block",
            "messages.69.content.0.type: Expected `thinking` or `redacted_thinking`, but found `tool_use`.",
            "messages.0.signature: Field required",
            "xxx.signature: Extra inputs are not permitted",
            "thinking or redacted_thinking blocks in the response cannot be modified",
        ];
        for message in cases {
            assert!(
                super::matches_signature_failure(&message.to_lowercase()),
                "应命中 signature 失效模式：{message}"
            );
        }
        for message in [
            "Request timeout",
            "Input tag 'adaptive' found using 'type' does not match expected tags",
            "messages.69.content.0.type: Expected `thinking` or `redacted_thinking`, but found `text`.",
        ] {
            assert!(
                !super::matches_signature_failure(&message.to_lowercase()),
                "不应命中 signature 失效模式：{message}"
            );
        }
    }

    #[test]
    fn signature_rectification_strips_reasoning_parts() {
        let original = assistant_with_reasoning_and_tool_call();
        let rectified =
            rectify("Invalid `signature` in `thinking` block", &original).expect("应产生整流");
        assert_eq!(rectified.rule, Rule::Signature);
        let assistant = &rectified.request.messages[0];
        assert_eq!(assistant.content.len(), 1);
        assert!(matches!(assistant.content[0], ContentPart::ToolCall { .. }));
        // 动作随 warnings 回传下游。
        assert!(rectified.request.warnings.iter().any(|warning| matches!(
            warning,
            Warning::Unsupported { feature, .. } if feature == warning_feature::REASONING
        )));
        // 原 IR 不被改动。
        assert_eq!(original.messages[0].content.len(), 2);
    }

    #[test]
    fn signature_rectification_drops_enabled_thinking_for_tool_use_tail() {
        let mut original = assistant_with_reasoning_and_tool_call();
        original.provider_options.insert(
            "anthropic".to_string(),
            json!({ "thinking": { "type": "enabled", "budget_tokens": 1024 } }),
        );
        original.reasoning = Some(crate::core::ir::ReasoningEffort::High);
        let rectified = rectify(
            "a final `assistant` message must start with a thinking block",
            &original,
        )
        .expect("应产生整流");
        assert!(
            rectified
                .actions
                .contains(&"剥离 thinking 请求配置".to_string())
        );
        assert!(
            rectified.request.provider_options["anthropic"]
                .get("thinking")
                .is_none()
        );
        assert_eq!(rectified.request.reasoning, None);
        assert!(rectified.request.warnings.iter().any(|warning| matches!(
            warning,
            Warning::Compatibility { feature, .. } if feature == warning_feature::THINKING
        )));
    }

    #[test]
    fn signature_rectification_keeps_adaptive_thinking() {
        let mut original = assistant_with_reasoning_and_tool_call();
        original.provider_options.insert(
            "anthropic".to_string(),
            json!({ "thinking": { "type": "adaptive" } }),
        );
        let rectified =
            rectify("Invalid `signature` in `thinking` block", &original).expect("应产生整流");
        // adaptive 形态不要求 thinking 前缀，thinking 配置保留。
        assert!(
            rectified.request.provider_options["anthropic"]
                .get("thinking")
                .is_some()
        );
        assert_eq!(rectified.actions.len(), 1);
    }

    #[test]
    fn signature_pattern_without_reasoning_parts_gives_up() {
        let original = request(json!([
            { "role": "user", "content": [{ "type": "text", "text": "hi" }] },
        ]));
        let rectified = rectify("Invalid `signature` in `thinking` block", &original);
        assert!(rectified.is_none(), "无 reasoning part 可剥离时应放弃整流");
    }

    #[test]
    fn tool_shape_rectification_normalizes_schemas_and_downgrades_choice() {
        let mut original = request(json!([
            { "role": "user", "content": [{ "type": "text", "text": "hi" }] },
        ]));
        original.tools = vec![
            Tool {
                name: "a".to_string(),
                description: None,
                parameters: Some(json!({ "anyOf": [
                    { "type": "object", "properties": { "x": { "type": "string" } } },
                ] })),
                provider_options: Default::default(),
            },
            Tool {
                name: "b".to_string(),
                description: None,
                parameters: Some(json!({ "type": "object", "properties": {} })),
                provider_options: Default::default(),
            },
        ];
        original.tool_choice = Some(ToolChoice::Tool {
            name: "missing".to_string(),
        });
        let rectified = rectify(
            "Invalid schema for tools['a']: root must be object; Invalid 'tool_choice' specified",
            &original,
        )
        .expect("应产生整流");
        assert_eq!(rectified.rule, Rule::ToolShape);
        assert_eq!(
            rectified.request.tools[0].parameters,
            Some(json!({ "type": "object", "properties": { "x": { "type": "string" } } })),
        );
        // 合法 object schema 原样保留。
        assert_eq!(
            rectified.request.tools[1].parameters,
            Some(json!({ "type": "object", "properties": {} })),
        );
        assert_eq!(rectified.request.tool_choice, Some(ToolChoice::Auto));
        assert!(rectified.request.warnings.iter().any(|warning| matches!(
            warning,
            Warning::Compatibility { feature, .. } if feature == warning_feature::INPUT_SCHEMA
        )));
        assert!(rectified.request.warnings.iter().any(|warning| matches!(
            warning,
            Warning::Compatibility { feature, .. } if feature == warning_feature::TOOL_CHOICE
        )));
    }

    #[test]
    fn tool_choice_error_downgrades_named_choice_even_when_tool_exists() {
        let mut original = request(json!([
            { "role": "user", "content": [{ "type": "text", "text": "hi" }] },
        ]));
        original.tools = vec![Tool {
            name: "present".to_string(),
            description: None,
            parameters: Some(json!({ "type": "object", "properties": {} })),
            provider_options: Default::default(),
        }];
        original.tool_choice = Some(ToolChoice::Tool {
            name: "present".to_string(),
        });
        let rectified = rectify("Invalid 'tool_choice' specified", &original).expect("应产生整流");
        assert_eq!(rectified.request.tool_choice, Some(ToolChoice::Auto));
        assert!(rectified.actions[0].contains("present"));
    }

    #[test]
    fn signature_rectification_drops_typed_reasoning_knob_for_tool_use_tail() {
        let mut original = assistant_with_reasoning_and_tool_call();
        original.reasoning = Some(crate::core::ir::ReasoningEffort::High);
        let rectified = rectify(
            "a final `assistant` message must start with a thinking block",
            &original,
        )
        .expect("应产生整流");
        // thinking 仅由 effort 旋钮承载时同样剥离（出站形态取决于模型代次，
        // 整流器不感知模型，保守剥离）。
        assert_eq!(rectified.request.reasoning, None);
        assert!(
            rectified
                .actions
                .contains(&"剥离 thinking 请求配置".to_string())
        );
    }

    #[test]
    fn unrelated_errors_give_up() {
        let original = assistant_with_reasoning_and_tool_call();
        for message in ["Request timeout", "overloaded", "rate limit exceeded"] {
            assert!(
                rectify(message, &original).is_none(),
                "{message} 不应触发整流"
            );
        }
    }
}
