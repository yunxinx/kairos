//! 渠道路由：按同名渠道顺序表确定 failover 尝试顺序。
//!
//! 纯算法模块，无 HTTP 依赖，便于单元测试分布与顺序。模型 → 候选渠道匹配
//! 规则：渠道 `models` 列表含该模型，或 `model_aliases` 的 key（下游稳定短名）
//! 匹配。上游真实模型名（alias 的 value）不参与匹配。

use std::collections::HashMap;

use crate::store::resources::Channel;
#[cfg(test)]
use crate::store::resources::ChannelModelOrder;
#[cfg(test)]
use crate::store::resources::ChannelRecord;

/// 一次路由的结果：按 failover 顺序排列的运行时渠道下标。
///
/// 出站模型名不在此共享：轮到某渠道时用 [`outbound_model`] 查该渠道自己的
/// 别名表，禁止全体套用第一个候选的出站名。
#[derive(Debug, Clone)]
pub struct Route {
    /// 按尝试顺序排列的渠道下标，指向本次请求持有的运行时快照。
    pub channel_indices: Vec<usize>,
    /// 已在准入阶段为每个候选渠道选定的密钥；同渠道重试复用该结果。
    pub selected_key_ids: HashMap<i64, i64>,
}

impl Route {
    /// 取该渠道本次请求已选定的密钥 id。
    pub fn selected_key_id(&self, channel_id: i64) -> Option<i64> {
        self.selected_key_ids.get(&channel_id).copied()
    }
}

/// 该渠道对入站模型名应发给上游的出站名：命中本渠道别名表则用 value，否则原样。
pub fn outbound_model<'a>(channel: &'a Channel, inbound: &'a str) -> &'a str {
    channel
        .model_aliases
        .get(inbound)
        .map(String::as_str)
        .unwrap_or(inbound)
}

/// 启用渠道之间同一别名 key 指向不同真名时的冲突。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasConflict {
    /// 冲突的下游别名 key。
    pub alias: String,
    /// 已登记的真名。
    pub existing: String,
    /// 与已登记真名不一致的真名。
    pub conflicting: String,
}

/// 在启用渠道集合中查找别名冲突：同一 key 指向不同 value 则返回第一处。
///
/// 禁用渠道不参与。同一 key 指向同一 value 允许（同模型多渠道 failover）。
pub fn find_alias_conflict(channels: &[&Channel]) -> Option<AliasConflict> {
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for channel in channels {
        if !channel.enabled {
            continue;
        }
        let mut aliases: Vec<(&str, &str)> = channel
            .model_aliases
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        aliases.sort_unstable();
        for (key, value) in aliases {
            match seen.get(key).copied() {
                Some(existing) if existing != value => {
                    return Some(AliasConflict {
                        alias: key.to_string(),
                        existing: existing.to_string(),
                        conflicting: value.to_string(),
                    });
                }
                Some(_) => {}
                None => {
                    seen.insert(key, value);
                }
            }
        }
    }
    None
}

/// 为 `model` 在 `channels` 中选出候选并按 failover 顺序排列。
///
/// 候选须处于启用状态（禁用的渠道不参与路由与失败切换）。同名的显式顺序行按
/// `position` 升序；没有显式行的候选全部排在其后，再按渠道 id 升序。顺序表只
/// 决定尝试顺序，不会滤掉候选；无任何候选时返回 `None`。
#[cfg(test)]
pub fn route(
    channels: &[ChannelRecord],
    channel_model_order: &[ChannelModelOrder],
    model: &str,
) -> Option<Route> {
    let mut candidates: Vec<(usize, &ChannelRecord, Option<i64>)> = channels
        .iter()
        .enumerate()
        .filter(|record| {
            let c = &record.1.channel;
            c.enabled
                && (c.models.iter().any(|m| m == model) || c.model_aliases.contains_key(model))
        })
        .map(|(index, record)| {
            let position = channel_model_order
                .iter()
                .find(|entry| entry.model == model && entry.channel_id == record.id)
                .map(|entry| entry.position);
            (index, record, position)
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }

    candidates.sort_unstable_by_key(|(_, record, position)| match position {
        Some(position) => (0, *position, record.id),
        None => (1, 0, record.id),
    });

    Some(Route {
        channel_indices: candidates.into_iter().map(|(index, _, _)| index).collect(),
        selected_key_ids: HashMap::new(),
    })
}

/// 从运行时预排索引构造路由。
pub fn indexed_route(candidates: &HashMap<String, Vec<usize>>, model: &str) -> Option<Route> {
    Some(Route {
        channel_indices: candidates.get(model)?.clone(),
        selected_key_ids: HashMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(name: &str, models: &[&str]) -> Channel {
        Channel {
            name: name.to_string(),
            protocol: crate::config::Protocol::OpenAiChat,
            base_url: "http://localhost".to_string(),
            keys: vec![crate::store::resources::ChannelKey {
                name: "default".to_string(),
                api_key: "k".to_string(),
                weight: 1,
                enabled: true,
                models: None,
                blocked_models: None,
            }],
            models: models.iter().map(|s| s.to_string()).collect(),
            model_aliases: Default::default(),
            timeout_ms: 1000,
            max_retries: 0,
            enabled: true,
            model_group: crate::store::resources::DEFAULT_MODEL_GROUP.to_string(),
            reasoning_output: Default::default(),
            session_cache_key: Default::default(),
            injects_cache_breakpoints: false,
            abort_on_disconnect: true,
        }
    }

    /// 测试用记录包装：id 仅作身份占位，路由不依赖其取值。
    fn record(id: i64, channel: Channel) -> ChannelRecord {
        ChannelRecord {
            id,
            channel,
            keys: Vec::new(),
        }
    }

    /// 显式顺序在前，未写顺序的候选随后按渠道 id 升序兜底。
    #[test]
    fn orders_by_explicit_position_then_default_channel_id() {
        let channels = vec![
            record(4, channel("default-late", &["gpt-4o"])),
            record(1, channel("explicit-first", &["gpt-4o"])),
            record(3, channel("explicit-second", &["gpt-4o"])),
            record(2, channel("default-first", &["gpt-4o"])),
        ];
        let order = vec![
            ChannelModelOrder {
                model: "gpt-4o".to_string(),
                channel_id: 3,
                position: 9,
            },
            ChannelModelOrder {
                model: "gpt-4o".to_string(),
                channel_id: 1,
                position: 2,
            },
        ];
        let route = route(&channels, &order, "gpt-4o").expect("应有候选");
        assert_eq!(
            route
                .channel_indices
                .iter()
                .map(|index| channels[*index].id)
                .collect::<Vec<_>>(),
            vec![1, 3, 2, 4]
        );
    }

    /// 没有任何顺序行时，渠道创建先后（id 升序）就是确定的默认顺序。
    #[test]
    fn defaults_to_channel_id_order_without_explicit_position() {
        let channels = vec![
            record(8, channel("last", &["gpt-4o"])),
            record(2, channel("first", &["gpt-4o"])),
            record(5, channel("middle", &["gpt-4o"])),
        ];
        let route = route(&channels, &[], "gpt-4o").expect("应有候选");
        assert_eq!(
            route
                .channel_indices
                .iter()
                .map(|index| channels[*index].id)
                .collect::<Vec<_>>(),
            vec![2, 5, 8]
        );
    }

    /// 无任何候选渠道 → 返回 None。
    #[test]
    fn no_candidate_is_none() {
        let channels = vec![record(1, channel("a", &["other-model"]))];
        assert!(route(&channels, &[], "gpt-4o").is_none());
    }

    /// 禁用的渠道不参与路由：仅剩启用渠道入选。
    #[test]
    fn disabled_channel_is_excluded() {
        let mut disabled = channel("off", &["gpt-4o"]);
        disabled.enabled = false;
        let channels = vec![record(1, disabled), record(2, channel("on", &["gpt-4o"]))];
        let route = route(&channels, &[], "gpt-4o").expect("启用渠道应可命中");
        assert_eq!(route.channel_indices.len(), 1, "禁用渠道不应进入候选");
        assert_eq!(channels[route.channel_indices[0]].channel.name, "on");
    }

    /// 全部候选都被禁用 → 与无候选同等处理，返回 None。
    #[test]
    fn all_disabled_is_none() {
        let mut only = channel("off", &["gpt-4o"]);
        only.enabled = false;
        assert!(route(&[record(1, only)], &[], "gpt-4o").is_none());
    }

    /// 别名 key 命中候选，出站模型名重写为 alias value。
    #[test]
    fn alias_key_matches_and_outbound_model_rewritten() {
        let mut c = channel("c", &["gpt-4o"]);
        c.model_aliases
            .insert("fast".to_string(), "gpt-4o-mini".to_string());
        let channels = [record(1, c)];
        let route = route(&channels, &[], "fast").expect("别名短名应命中");
        assert_eq!(
            outbound_model(&channels[route.channel_indices[0]].channel, "fast"),
            "gpt-4o-mini"
        );
    }

    /// 未命中别名时出站模型名原样。
    #[test]
    fn outbound_model_unchanged_without_alias() {
        let channels = vec![record(1, channel("c", &["gpt-4o"]))];
        let route = route(&channels, &[], "gpt-4o").expect("应有候选");
        assert_eq!(
            outbound_model(&channels[route.channel_indices[0]].channel, "gpt-4o"),
            "gpt-4o"
        );
    }

    /// 各候选渠道用自己的别名表得到出站名，不共用第一个候选。
    #[test]
    fn each_channel_rewrites_outbound_from_its_own_alias_table() {
        let mut aliased = channel("aliased", &["gpt-4o"]);
        aliased
            .model_aliases
            .insert("fast".to_string(), "gpt-4o-mini".to_string());
        let plain = channel("plain", &["fast"]);
        let order = [ChannelModelOrder {
            model: "fast".to_string(),
            channel_id: 2,
            position: 0,
        }];
        let channels = [record(1, aliased), record(2, plain)];
        let route = route(&channels, &order, "fast").expect("应有候选");
        let first = &channels[route.channel_indices[0]].channel;
        let second = &channels[route.channel_indices[1]].channel;
        assert_eq!(first.name, "plain");
        assert_eq!(outbound_model(first, "fast"), "fast");
        assert_eq!(second.name, "aliased");
        assert_eq!(outbound_model(second, "fast"), "gpt-4o-mini");
    }

    /// 两条启用渠道同一别名指向不同真名 → 冲突。
    #[test]
    fn enabled_channels_conflict_when_alias_values_differ() {
        let mut a = channel("a", &["gpt-4o"]);
        a.model_aliases
            .insert("fast".to_string(), "gpt-4o-mini".to_string());
        let mut b = channel("b", &["gpt-4o"]);
        b.model_aliases
            .insert("fast".to_string(), "gpt-4o".to_string());
        let conflict = find_alias_conflict(&[&a, &b]).expect("应检出冲突");
        assert_eq!(conflict.alias, "fast");
        assert!(
            (conflict.existing == "gpt-4o-mini" && conflict.conflicting == "gpt-4o")
                || (conflict.existing == "gpt-4o" && conflict.conflicting == "gpt-4o-mini")
        );
    }

    /// 同一别名指向同一真名：多渠道 failover 允许。
    #[test]
    fn same_alias_same_value_is_not_conflict() {
        let mut a = channel("a", &["gpt-4o"]);
        a.model_aliases
            .insert("fast".to_string(), "gpt-4o-mini".to_string());
        let mut b = channel("b", &["gpt-4o"]);
        b.model_aliases
            .insert("fast".to_string(), "gpt-4o-mini".to_string());
        assert!(find_alias_conflict(&[&a, &b]).is_none());
    }

    /// 禁用渠道不参与别名冲突：与启用渠道指向不同真名仍允许。
    #[test]
    fn disabled_channel_does_not_participate_in_alias_conflict() {
        let mut enabled = channel("on", &["gpt-4o"]);
        enabled
            .model_aliases
            .insert("fast".to_string(), "gpt-4o-mini".to_string());
        let mut disabled = channel("off", &["gpt-4o"]);
        disabled.enabled = false;
        disabled
            .model_aliases
            .insert("fast".to_string(), "gpt-4o".to_string());
        assert!(find_alias_conflict(&[&enabled, &disabled]).is_none());
    }
}
