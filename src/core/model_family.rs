//! 模型形态判定：按模型名识别协议家族的请求形状代际。
//!
//! 网关暂无上游模型能力表，跨协议整形只能依据模型名约定。判定逻辑集中本
//! 模块、以「未识别 ID 继承最新行为」的口径演进（对齐 AI SDK 的
//! `GoogleModelCapabilities` 模式）——分散在各适配器的子串硬编码会随模型
//! 演进各自漂移。新的代际形态落地时在此处补一条判定与依据注释即可。

/// Anthropic adaptive thinking 形态：`thinking: {type: "adaptive"}` + 原生
/// effort 档位（fable/mythos 家族与 4.6+/5 代 sonnet/opus），日期后缀与
/// 点分变体（bedrock/vertex/azure 接入形态）一并覆盖。判否时按 legacy
/// budget 阶梯兜底——该形状对所有 budget 模型合法，误判只损失 effort 档位
/// 粒度，不产生非法请求。
pub fn anthropic_adaptive_thinking(model: &str) -> bool {
    let model = model.to_lowercase();
    let opus = model.contains("opus");
    let version_46 = model.contains("4-6") || model.contains("4.6");
    let opus_47_plus = opus
        && (model.contains("4-7")
            || model.contains("4.7")
            || model.contains("4-8")
            || model.contains("4.8")
            || model.contains("opus-5"));
    let sonnet_5_plus = model.contains("sonnet-5");
    let fable_family = model.contains("fable") || model.contains("mythos");
    opus_47_plus
        || sonnet_5_plus
        || fable_family
        || (version_46 && (opus || model.contains("sonnet")))
}

/// Gemini 2.5 家族的 thinkingBudget 上限：2.5-pro 为 32_768，其余 2.5 为
/// 24_576；非 2.5 家族返回 `None`（不设上限）。官方对 2.5 代各模型的
/// thinkingBudget 有硬上限，超限请求会被上游拒绝，钳制并告警是兼容整形。
pub fn gemini_2_5_budget_cap(model: &str) -> Option<u32> {
    let model = model.to_ascii_lowercase();
    if model.contains("2.5-pro") {
        Some(32_768)
    } else if model.contains("2.5") {
        Some(24_576)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{anthropic_adaptive_thinking, gemini_2_5_budget_cap};

    /// Anthropic adaptive 形态判定：代际、家族别名与接入形态变体。
    #[test]
    fn anthropic_adaptive_thinking_matches_model_forms() {
        for model in [
            "claude-opus-4-6",
            "claude-opus-4-6-20260201",
            "claude-opus-4.6",
            "claude-sonnet-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-fable-5",
            "claude-mythos-5",
            "us.anthropic.claude-opus-4-6-v1:0",
        ] {
            assert!(
                anthropic_adaptive_thinking(model),
                "{model} 应判为 adaptive 形态"
            );
        }
        for model in [
            "claude-sonnet-4-5",
            "claude-opus-4-5",
            "claude-opus-4-1",
            "claude-haiku-4-5",
            "claude-3-7-sonnet",
            "gpt-4o",
        ] {
            assert!(
                !anthropic_adaptive_thinking(model),
                "{model} 应判为 legacy 形态"
            );
        }
    }

    /// Gemini 2.5 budget 上限：pro 档与普通档两阶，非 2.5 家族不设限。
    #[test]
    fn gemini_2_5_budget_cap_matches_model_forms() {
        assert_eq!(gemini_2_5_budget_cap("gemini-2.5-pro"), Some(32_768));
        assert_eq!(
            gemini_2_5_budget_cap("gemini-2.5-pro-preview"),
            Some(32_768)
        );
        assert_eq!(gemini_2_5_budget_cap("gemini-2.5-flash"), Some(24_576));
        assert_eq!(gemini_2_5_budget_cap("gemini-2.5"), Some(24_576));
        assert_eq!(gemini_2_5_budget_cap("gemini-3-pro"), None);
        assert_eq!(gemini_2_5_budget_cap("gemini-2.0-flash"), None);
        assert_eq!(gemini_2_5_budget_cap("gpt-4o"), None);
    }
}
