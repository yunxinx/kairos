//! 渠道密钥模型与选择策略。
//!
//! 管理 API 复用 [`ChannelKey`] 作为 wire DTO，两个方向语义不同：入站（创建）
//! 携带明文，更新时空串或掩码串表示「保留原值」；出站一律掩码形态，明文不
//! 回显。数据库加载后的运行时密钥使用 [`StoredChannelKey`]，其中明文被
//! `secrecy` 包裹，仅出站认证与更新保留原值时经显式方法暴露。选择策略集中在
//! 本模块，探测与会话粘性路由共享同一套候选过滤和加权规则。

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

/// 渠道上的一把上游密钥，仅用于管理 API 输入输出。
///
/// 读取（响应体）时 `api_key` 为掩码形态；写入（创建）要求明文，更新时空串
/// 或含 `*` 哨兵的掩码串表示按 `name` 保留库中原值。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelKey {
    pub name: String,
    pub api_key: String,
    #[serde(default = "default_channel_key_weight")]
    pub weight: i64,
    #[serde(default = "default_channel_key_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub models: Option<Vec<String>>,
    #[serde(default)]
    pub blocked_models: Option<Vec<String>>,
}

impl std::fmt::Debug for ChannelKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelKey")
            .field("name", &self.name)
            .field("api_key", &"[REDACTED]")
            .field("weight", &self.weight)
            .field("enabled", &self.enabled)
            .field("models", &self.models)
            .field("blocked_models", &self.blocked_models)
            .finish()
    }
}

/// 已持久化的渠道密钥：明文仅可通过显式方法暴露给出站认证或管理 wire 边界。
#[derive(Debug, Clone)]
pub struct StoredChannelKey {
    pub id: i64,
    pub channel_id: i64,
    pub name: String,
    api_key: SecretString,
    pub weight: i64,
    pub enabled: bool,
    pub models: Option<Vec<String>>,
    pub blocked_models: Option<Vec<String>>,
    pub created_at: i64,
}

impl StoredChannelKey {
    /// 从数据库或管理草稿接收密钥所有权，随后由 `secrecy` 负责析构清零。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: i64,
        channel_id: i64,
        name: String,
        api_key: String,
        weight: i64,
        enabled: bool,
        models: Option<Vec<String>>,
        blocked_models: Option<Vec<String>>,
        created_at: i64,
    ) -> Self {
        Self {
            id,
            channel_id,
            name,
            api_key: api_key.into(),
            weight,
            enabled,
            models,
            blocked_models,
            created_at,
        }
    }

    /// 仅供构造出站认证头与更新渠道时保留原值使用；wire 读取面走掩码。
    pub fn expose_api_key(&self) -> &str {
        self.api_key.expose_secret()
    }

    /// 在管理 API wire 边界显式生成可序列化 DTO。
    ///
    /// `api_key` 输出掩码形态，明文不回显；掩码串含 `*` 哨兵，更新方以它表达
    /// 「保留原值」。
    pub fn to_wire(&self) -> ChannelKey {
        ChannelKey {
            name: self.name.clone(),
            api_key: mask_api_key(self.expose_api_key()),
            weight: self.weight,
            enabled: self.enabled,
            models: self.models.clone(),
            blocked_models: self.blocked_models.clone(),
        }
    }
}

/// 管理 API 读取面的密钥掩码：长密钥保留前后 8 个字符，短密钥完全掩码。
///
/// 掩码串必含 `*`——`*` 不可能是合法密钥成分，因此它是「该值为掩码而非
/// 明文」的唯一可靠判定依据，写入面据此决定是否保留原值。
pub fn mask_api_key(api_key: &str) -> String {
    const EDGE: usize = 8;
    let chars: Vec<char> = api_key.chars().collect();
    if chars.len() <= EDGE * 2 {
        "******".to_string()
    } else {
        let prefix: String = chars[..EDGE].iter().collect();
        let suffix: String = chars[chars.len() - EDGE..].iter().collect();
        format!("{prefix}******{suffix}")
    }
}

/// 密钥条目是否表达「保留原值」：空串或含 `*` 哨兵的掩码串。
///
/// `*` 不可能是合法密钥成分，含它即认定调用方原样回传了读取面的掩码形态。
pub fn api_key_requests_preservation(api_key: &str) -> bool {
    api_key.is_empty() || api_key.contains('*')
}

impl PartialEq for StoredChannelKey {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.channel_id == other.channel_id
            && self.name == other.name
            && self.expose_api_key() == other.expose_api_key()
            && self.weight == other.weight
            && self.enabled == other.enabled
            && self.models == other.models
            && self.blocked_models == other.blocked_models
            && self.created_at == other.created_at
    }
}

impl Eq for StoredChannelKey {}

fn default_channel_key_weight() -> i64 {
    1
}

fn default_channel_key_enabled() -> bool {
    true
}

/// 判断密钥是否允许该模型。
pub fn channel_key_supports_model(key: &StoredChannelKey, model: &str) -> bool {
    let allowed = key
        .models
        .as_ref()
        .is_none_or(|models| models.iter().any(|item| item == model));
    let blocked = key
        .blocked_models
        .as_ref()
        .is_some_and(|models| models.iter().any(|item| item == model));
    allowed && !blocked
}

fn is_eligible_channel_key(key: &StoredChannelKey, model: &str) -> bool {
    key.enabled && channel_key_supports_model(key, model)
}

/// 渠道是否至少有一把启用且允许指定模型的密钥。
///
/// 只做确定性判断，不执行加权随机选择，也不改变会话粘性缓存。
pub fn channel_has_eligible_key(keys: &[StoredChannelKey], model: &str) -> bool {
    keys.iter().any(|key| is_eligible_channel_key(key, model))
}

/// 返回启用且允许指定模型的密钥，保留存储顺序。
pub fn eligible_channel_keys<'a>(
    keys: &'a [StoredChannelKey],
    model: &str,
) -> Vec<&'a StoredChannelKey> {
    keys.iter()
        .filter(|key| is_eligible_channel_key(key, model))
        .collect()
}

/// 按权重随机选择候选；权重全为零时退化为等概率。
pub fn select_weighted_channel_key<'a>(
    candidates: &[&'a StoredChannelKey],
) -> Option<&'a StoredChannelKey> {
    match candidates {
        [] => None,
        [only] => Some(*only),
        _ => {
            let total = total_weight(candidates);
            if total == 0 {
                return Some(candidates[rand::random_range(0..candidates.len())]);
            }
            select_weighted_channel_key_at(candidates, rand::random_range(0..total))
        }
    }
}

/// 过滤模型候选并按权重随机选取一把密钥。
pub fn select_channel_key<'a>(
    keys: &'a [StoredChannelKey],
    model: &str,
) -> Option<&'a StoredChannelKey> {
    let candidates = eligible_channel_keys(keys, model);
    select_weighted_channel_key(&candidates)
}

fn total_weight(candidates: &[&StoredChannelKey]) -> u128 {
    candidates.iter().map(|key| key.weight.max(0) as u128).sum()
}

/// `point` 必须位于 `[0, total_weight)`；调用方从同一个总权重生成它。
fn select_weighted_channel_key_at<'a>(
    candidates: &[&'a StoredChannelKey],
    mut point: u128,
) -> Option<&'a StoredChannelKey> {
    for key in candidates {
        let weight = key.weight.max(0) as u128;
        if point < weight {
            return Some(*key);
        }
        point = point.saturating_sub(weight);
    }
    None
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn stored_key(id: i64, weight: i64, enabled: bool) -> StoredChannelKey {
        StoredChannelKey::new(
            id,
            1,
            format!("key-{id}"),
            format!("secret-{id}"),
            weight,
            enabled,
            None,
            None,
            0,
        )
    }

    proptest! {
        /// 任意合法权重和采样点都必须选中一个正权重候选，且不会落到末尾回退。
        #[test]
        fn weighted_partition_covers_every_point(
            weights in prop::collection::vec(0i64..=i64::MAX, 1..32),
        ) {
            let keys: Vec<_> = weights
                .iter()
                .enumerate()
                .map(|(index, weight)| stored_key(index as i64, *weight, true))
                .collect();
            let candidates: Vec<_> = keys.iter().collect();
            let total = total_weight(&candidates);
            if total > 0 {
                for point in [0, total / 2, total - 1] {
                    let selected = select_weighted_channel_key_at(&candidates, point)
                        .expect("合法采样点必须命中候选");
                    prop_assert!(selected.weight > 0);
                    prop_assert!(candidates.iter().any(|candidate| candidate.id == selected.id));
                }
            }
        }
    }

    #[test]
    fn secret_debug_output_is_redacted() {
        let key = stored_key(1, 1, true);
        let debug = format!("{key:?}");
        assert!(!debug.contains("secret-1"));
        assert!(debug.contains("REDACTED"));
    }

    /// 读取面掩码：短密钥完全掩码，长密钥保留前后 8 字符，两种形态都含
    /// `*` 哨兵且不含完整明文。
    #[test]
    fn wire_output_masks_api_key_with_sentinel() {
        let short = stored_key(1, 1, true);
        let short_wire = short.to_wire();
        assert_eq!(short_wire.api_key, "******");
        assert!(!short_wire.api_key.contains("secret-1"));

        let long = StoredChannelKey::new(
            2,
            1,
            "long".to_string(),
            "sk-ant-api03-1234567890abcdef-secret-tail".to_string(),
            1,
            true,
            None,
            None,
            0,
        );
        let long_wire = long.to_wire();
        assert_eq!(long_wire.api_key, "sk-ant-a******ret-tail");
        assert!(
            !long_wire.api_key.contains("1234567890"),
            "长密钥中段不得出现在掩码里"
        );
        assert!(api_key_requests_preservation(&long_wire.api_key));
    }

    /// 「保留原值」判定：空串与任何含 `*` 的值都算；普通明文不算。
    #[test]
    fn preservation_sentinel_matches_empty_and_masked_values() {
        assert!(api_key_requests_preservation(""));
        assert!(api_key_requests_preservation("******"));
        assert!(api_key_requests_preservation("sk-ant-a******ecret-tail"));
        assert!(!api_key_requests_preservation("sk-live-plain"));
    }

    #[test]
    fn eligible_key_presence_matches_candidate_filter() {
        let mut disabled = stored_key(1, 1, false);
        disabled.models = Some(vec!["model-a".to_string()]);
        let mut blocked = stored_key(2, 1, true);
        blocked.blocked_models = Some(vec!["model-a".to_string()]);
        let allowed = stored_key(3, 1, true);

        let keys = [disabled, blocked, allowed];
        assert!(channel_has_eligible_key(&keys, "model-a"));
        assert_eq!(
            channel_has_eligible_key(&keys, "model-a"),
            !eligible_channel_keys(&keys, "model-a").is_empty()
        );
    }
}
