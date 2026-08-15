//! 渠道路由：按 priority 升序、同级 weight 加权随机选择，产出 failover 顺序。
//!
//! 纯算法模块，无 HTTP 依赖，便于单元测试分布与顺序。模型 → 候选渠道匹配
//! 规则：渠道 `models` 列表含该模型，或 `model_aliases` 的 key（下游稳定短名）
//! 匹配。上游真实模型名（alias 的 value）不参与匹配。

use std::collections::HashMap;

use rand::RngExt;

use crate::store::resources::{Channel, ChannelRecord};

/// 一次路由的结果：按 failover 顺序排列的候选渠道 + 出站模型名。
///
/// 出站模型名：命中别名时取 alias 指向的真实模型名，否则原样。所有候选
/// 渠道共享同一出站模型名（同一模型的不同渠道）。
#[derive(Debug, Clone)]
pub struct Route {
    /// 按尝试顺序排列的候选渠道（克隆自配置记录）。
    pub channels: Vec<Channel>,
    /// 出站请求应使用的模型名（别名重写后）。
    pub outbound_model: String,
}

/// 为 `model` 在 `channels` 中选出候选并按 failover 顺序排列。
///
/// 候选须处于启用状态（禁用的渠道不参与路由与失败切换）。
/// 优先级按数值升序（数值越小越先尝试）；同一优先级内按 weight 加权随机排序，
/// 作为本次请求的失败切换顺序。全部候选都在结果里（低优先级渠道是兜底），
/// 无任何候选时返回 `None`。
pub fn route(channels: &[ChannelRecord], model: &str) -> Option<Route> {
    let candidates: Vec<&ChannelRecord> = channels
        .iter()
        .filter(|record| {
            let c = &record.channel;
            c.enabled
                && (c.models.iter().any(|m| m == model) || c.model_aliases.contains_key(model))
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }

    let outbound_model = candidates[0]
        .channel
        .model_aliases
        .get(model)
        .cloned()
        .unwrap_or_else(|| model.to_string());

    // 按 priority 升序分组：数值越小优先级越高，先被尝试；组内按 weight 加权
    // 随机排序，作为 failover 顺序。全部候选都在结果里（低优先级渠道是兜底）。
    let mut priority_groups: HashMap<u32, Vec<&ChannelRecord>> = HashMap::new();
    for &record in &candidates {
        priority_groups
            .entry(record.channel.priority)
            .or_default()
            .push(record);
    }
    let mut priorities: Vec<u32> = priority_groups.keys().copied().collect();
    priorities.sort_unstable();

    let mut rng = rand::rng();
    let mut channels: Vec<Channel> = Vec::with_capacity(candidates.len());
    for priority in priorities {
        let group = &priority_groups[&priority];
        // 组内按 weight 加权随机排列：A-ExpJ 指数抽样，每个渠道的 key =
        // ln(U)/weight，按 key 升序即为「weight 越大越靠前」的加权随机顺序
        // （Efraimidis & Spirakis）。
        let mut keys: Vec<(f64, usize)> = group
            .iter()
            .enumerate()
            .map(|(i, record)| (exp_key(record.channel.weight, &mut rng), i))
            .collect();
        keys.sort_by(|a, b| a.0.total_cmp(&b.0));
        channels.extend(keys.into_iter().map(|(_, i)| group[i].channel.clone()));
    }

    Some(Route {
        channels,
        outbound_model,
    })
}

/// A-ExpJ 抽样 key：`-ln(U) / weight`，U 为 (0,1) 均匀随机数。
///
/// weight 越大，key 越小，排序越靠前。weight 必为配置里的正整数，不会为 0。
fn exp_key<G: RngExt>(weight: u32, rng: &mut G) -> f64 {
    // `random::<f64>()` 是 [0,1)，0 会使 ln(0) 为 +inf；用 1 - U 落入 (0,1]，
    // 保证 ln 有限且 weight 越大 key 越小。
    let u: f64 = 1.0 - rng.random::<f64>();
    -u.ln() / weight as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(name: &str, priority: u32, weight: u32, models: &[&str]) -> Channel {
        Channel {
            name: name.to_string(),
            protocol: crate::config::Protocol::OpenAiChat,
            base_url: "http://localhost".to_string(),
            api_key: "k".to_string(),
            models: models.iter().map(|s| s.to_string()).collect(),
            model_aliases: Default::default(),
            priority,
            weight,
            timeout_ms: 1000,
            max_retries: 0,
            enabled: true,
        }
    }

    /// 测试用记录包装：id 仅作身份占位，路由不依赖其取值。
    fn record(id: i64, channel: Channel) -> ChannelRecord {
        ChannelRecord { id, channel }
    }

    /// priority 升序优先：最高优先级（数值最小）的渠道先被尝试，低优先级兜底。
    #[test]
    fn orders_by_priority_ascending() {
        let channels = vec![
            record(1, channel("p2-a", 2, 1, &["gpt-4o"])),
            record(2, channel("p1-a", 1, 1, &["gpt-4o"])),
            record(3, channel("p1-b", 1, 1, &["gpt-4o"])),
        ];
        let route = route(&channels, "gpt-4o").expect("应有候选");
        assert_eq!(route.channels.len(), 3, "全部候选都应参与 failover");
        // 前两个是最高优先级（p1），最后一个兜底（p2）。
        assert_eq!(route.channels[0].priority, 1);
        assert_eq!(route.channels[1].priority, 1);
        assert_eq!(route.channels[2].priority, 2);
    }

    /// 同级 weight 加权随机：高 weight 渠道应更可能被排在前。
    #[test]
    fn higher_weight_tends_to_front() {
        let channels = vec![
            record(1, channel("heavy", 1, 100, &["gpt-4o"])),
            record(2, channel("light", 1, 1, &["gpt-4o"])),
        ];
        // 大量采样，统计 heavy 排第一的频率应显著高于 light。
        let mut heavy_first = 0;
        let trials = 200;
        for _ in 0..trials {
            let route = route(&channels, "gpt-4o").expect("应有候选");
            if route.channels[0].name == "heavy" {
                heavy_first += 1;
            }
        }
        assert!(
            heavy_first > trials * 2 / 3,
            "weight 高的渠道应大概率在前，实际 {heavy_first}/{trials}"
        );
    }

    /// 无任何候选渠道 → 返回 None。
    #[test]
    fn no_candidate_is_none() {
        let channels = vec![record(1, channel("a", 1, 1, &["other-model"]))];
        assert!(route(&channels, "gpt-4o").is_none());
    }

    /// 禁用的渠道不参与路由：仅剩启用渠道入选。
    #[test]
    fn disabled_channel_is_excluded() {
        let mut disabled = channel("off", 1, 1, &["gpt-4o"]);
        disabled.enabled = false;
        let channels = vec![
            record(1, disabled),
            record(2, channel("on", 2, 1, &["gpt-4o"])),
        ];
        let route = route(&channels, "gpt-4o").expect("启用渠道应可命中");
        assert_eq!(route.channels.len(), 1, "禁用渠道不应进入候选");
        assert_eq!(route.channels[0].name, "on");
    }

    /// 全部候选都被禁用 → 与无候选同等处理，返回 None。
    #[test]
    fn all_disabled_is_none() {
        let mut only = channel("off", 1, 1, &["gpt-4o"]);
        only.enabled = false;
        assert!(route(&[record(1, only)], "gpt-4o").is_none());
    }

    /// 别名 key 命中候选，出站模型名重写为 alias value。
    #[test]
    fn alias_key_matches_and_outbound_model_rewritten() {
        let mut c = channel("c", 1, 1, &["gpt-4o"]);
        c.model_aliases
            .insert("fast".to_string(), "gpt-4o-mini".to_string());
        let route = route(&[record(1, c)], "fast").expect("别名短名应命中");
        assert_eq!(route.outbound_model, "gpt-4o-mini");
    }

    /// 未命中别名时出站模型名原样。
    #[test]
    fn outbound_model_unchanged_without_alias() {
        let channels = vec![record(1, channel("c", 1, 1, &["gpt-4o"]))];
        let route = route(&channels, "gpt-4o").expect("应有候选");
        assert_eq!(route.outbound_model, "gpt-4o");
    }
}
