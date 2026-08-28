//! 调用者自己能用的模型：按套餐模型组分段的只读投影。
//!
//! 存在的理由是**不能**把 `/model-groups` 放开给普通用户：组的原始形状内嵌
//! `GroupModel::Source { channel_id, model }`，即渠道拓扑。这里只回可调用名与
//! 折后单价，不回渠道 id、渠道名、出站地址与密钥。
//!
//! 名单口径与下游 `GET /v1/models` 共用 `resources::visible_model_ids`，单价候选
//! 镜像请求路径的两条准入（`hop_for_callable` / `hop_for_unified_member`）：这一页
//! 要回答「网关现在会放我调什么、按什么价收」，答不对就成了误导。

use axum::{Extension, Json, Router, extract::State, routing::get};
use serde::Serialize;

use crate::core::billing;
use crate::runtime::{PlanBinding, RuntimeSnapshot};
use crate::store::channel_keys::channel_has_eligible_key;
use crate::store::resources::{self, ChannelRecord};
use crate::store::users::ManagementRole;

use super::auth::ManagementIdentity;
use super::{AdminDeps, AdminError};

pub(super) fn routes() -> Router<AdminDeps> {
    Router::new().route("/me/models", get(list_my_models))
}

/// 单价区间（micro-USD / 1M tokens，已折后）。
///
/// 价格按渠道定（ADR-0007），同一个可调用名挂在多条渠道上就可能有多个单价；
/// 请求实际落哪条由路由决定，所以这里给区间而不是假装只有一个数。
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) struct PriceRange {
    min_micros: i64,
    max_micros: i64,
}

/// 一个可调用名。刻意不带任何渠道字段。
#[derive(Debug, Serialize)]
pub(super) struct MyModelView {
    /// 请求 body 的 `model` 直接填它。
    id: String,
    /// 统一模型（内部按序 failover），但不暴露成员渠道。
    unified: bool,
    /// 当前是否真能调用：有启用且已定价的渠道。
    callable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<PriceRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<PriceRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_read: Option<PriceRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_write: Option<PriceRange>,
}

/// 一个模型组一段。同一个名字可以出现在多段里（组是允许名单，不是分区）。
#[derive(Debug, Serialize)]
pub(super) struct MyGroupView {
    name: String,
    models: Vec<MyModelView>,
}

#[derive(Debug, Serialize)]
pub(super) struct MyModelsView {
    /// 折扣率（万分比，10000 = 原价）；单价已折过，这里只为界面标注。
    discount_bp: i64,
    groups: Vec<MyGroupView>,
}

async fn list_my_models(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
) -> Result<Json<MyModelsView>, AdminError> {
    let snapshot = deps.snapshot.read().await;
    // 管理员的查看范围与 root 一致；套餐名单只约束管理员创建令牌时的可选组。
    // 两者若混用，root 新增的组会在管理端被错误显示为「不存在」。
    let visibility = if identity.role().at_least(ManagementRole::Admin) {
        PlanBinding::Unrestricted
    } else {
        identity
            .plan_id()
            .map_or(PlanBinding::Unrestricted, PlanBinding::Plan)
    };
    let discount_bp = identity
        .plan_id()
        .and_then(|plan_id| snapshot.plans.get(&plan_id))
        .map_or(billing::DEFAULT_DISCOUNT_BP, |plan| plan.discount_bp);
    Ok(Json(build_view(&snapshot, visibility, discount_bp)))
}

/// 该绑定下可用的模型组名（排序；`default` 置顶，与前端分段顺序一致）。
fn group_names(snapshot: &RuntimeSnapshot, binding: PlanBinding) -> Vec<String> {
    let mut names: Vec<String> = match binding {
        // root 不挂档，等价于运行时的全部组可用。
        PlanBinding::Unrestricted => snapshot.model_groups.keys().cloned().collect(),
        PlanBinding::Plan(plan_id) => snapshot
            .plans
            .get(&plan_id)
            .map(|plan| {
                plan.groups
                    .iter()
                    // 组可能已被删除而套餐名单里还留着名字；只留快照仍有定义的。
                    .filter(|name| snapshot.model_groups.contains_key(*name))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
    };
    names.sort_by(|left, right| {
        if left == resources::DEFAULT_MODEL_GROUP {
            return std::cmp::Ordering::Less;
        }
        if right == resources::DEFAULT_MODEL_GROUP {
            return std::cmp::Ordering::Greater;
        }
        left.cmp(right)
    });
    names
}

fn build_view(snapshot: &RuntimeSnapshot, binding: PlanBinding, discount_bp: i64) -> MyModelsView {
    let groups = group_names(snapshot, binding)
        .into_iter()
        .map(|name| {
            let ids = resources::visible_model_ids(
                &snapshot.model_groups,
                &snapshot.unified_models,
                snapshot.channels.iter().map(|record| &record.channel),
                &name,
            );
            let models = ids
                .into_iter()
                .map(|id| model_view(snapshot, &name, &id, discount_bp))
                .collect();
            MyGroupView { name, models }
        })
        .collect();
    MyModelsView {
        discount_bp,
        groups,
    }
}

/// 请求路径真正会考虑的渠道上，该名字的四档单价。
///
/// 空 `Vec` 表示当前不可调用：没有启用且已定价的渠道，请求会被准入挡下。
fn price_candidates<'a>(
    snapshot: &'a RuntimeSnapshot,
    group_name: &str,
    id: &str,
) -> Vec<&'a resources::Price> {
    match snapshot.unified_models.get(id) {
        // 统一模型：镜像 `hop_for_unified_member`。成员按序尝试，任一条可用即可调用，
        // 所以区间覆盖全部可用成员。
        Some(unified) => unified
            .models
            .iter()
            .filter_map(|member| {
                let record = channel_record(snapshot, member.channel_id)?;
                serves(record, &member.model).then_some(())?;
                snapshot.price_for_channel(record.id, &member.model)
            })
            .collect(),
        // 普通可调用名：镜像 `hop_for_callable`。自定义组若把该名钉在若干渠道上，
        // 候选只留这些渠道——否则会报出请求根本不会走到的渠道的价格。
        None => {
            let pinned = snapshot
                .model_groups
                .get(group_name)
                .and_then(|group| resources::pinned_channel_ids(group, id));
            snapshot
                .channels
                .iter()
                .filter(|record| pinned.as_ref().is_none_or(|ids| ids.contains(&record.id)))
                .filter(|record| serves(record, id))
                .filter_map(|record| snapshot.price_for_channel(record.id, id))
                .collect()
        }
    }
}

fn channel_record(snapshot: &RuntimeSnapshot, channel_id: i64) -> Option<&ChannelRecord> {
    snapshot
        .channels
        .iter()
        .find(|record| record.id == channel_id)
}

/// 渠道当前是否会为该名字出站：启用、已登记且至少有一把合格密钥。
fn serves(record: &ChannelRecord, model: &str) -> bool {
    record.channel.enabled
        && resources::channel_lists_callable(&record.channel, model)
        && channel_has_eligible_key(&record.keys, model)
}

/// 把候选里某一档的单价折成实收区间；该档全部不计价时为 `None`。
///
/// 用 `billing::discounted_cost_micros` 而不自己写乘除：单价按 1M tokens 报，而
/// `component_cost` 在 1M 处恰好等于单价，两边口径一致。
fn range(
    candidates: &[&resources::Price],
    tier: fn(&resources::Price) -> Option<i64>,
    discount_bp: i64,
) -> Option<PriceRange> {
    let mut min: Option<i64> = None;
    let mut max: Option<i64> = None;
    for price in candidates {
        let Some(unit) = tier(price) else { continue };
        // 单价为负是脏数据；折算会拒绝，跳过而不是把整页打成 500。
        let Ok(discounted) = billing::discounted_cost_micros(unit, discount_bp) else {
            continue;
        };
        min = Some(min.map_or(discounted, |current: i64| current.min(discounted)));
        max = Some(max.map_or(discounted, |current: i64| current.max(discounted)));
    }
    Some(PriceRange {
        min_micros: min?,
        max_micros: max?,
    })
}

fn model_view(
    snapshot: &RuntimeSnapshot,
    group_name: &str,
    id: &str,
    discount_bp: i64,
) -> MyModelView {
    let candidates = price_candidates(snapshot, group_name, id);
    MyModelView {
        id: id.to_string(),
        unified: snapshot.unified_models.contains_key(id),
        callable: !candidates.is_empty(),
        input: range(&candidates, |price| Some(price.input_micros), discount_bp),
        output: range(&candidates, |price| Some(price.output_micros), discount_bp),
        cache_read: range(&candidates, |price| price.cache_read_micros, discount_bp),
        cache_write: range(&candidates, |price| price.cache_write_micros, discount_bp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 区间在同名跨渠道单价不同时给出 min/max；全不计价的档为 None。
    #[test]
    fn range_spans_candidates_and_skips_unpriced_tier() {
        let cheap = resources::Price {
            channel_id: 1,
            model: "m".to_string(),
            input_micros: 2_000_000,
            output_micros: 8_000_000,
            cache_read_micros: None,
            cache_write_micros: None,
        };
        let pricey = resources::Price {
            channel_id: 2,
            model: "m".to_string(),
            input_micros: 3_000_000,
            output_micros: 8_000_000,
            cache_read_micros: Some(500_000),
            cache_write_micros: None,
        };
        let candidates = vec![&cheap, &pricey];
        assert_eq!(
            range(&candidates, |price| Some(price.input_micros), 10_000),
            Some(PriceRange {
                min_micros: 2_000_000,
                max_micros: 3_000_000
            })
        );
        assert_eq!(
            range(&candidates, |price| Some(price.output_micros), 10_000),
            Some(PriceRange {
                min_micros: 8_000_000,
                max_micros: 8_000_000
            }),
            "单值区间的两端相等"
        );
        assert_eq!(
            range(&candidates, |price| price.cache_read_micros, 10_000),
            Some(PriceRange {
                min_micros: 500_000,
                max_micros: 500_000
            }),
            "只有一条渠道计该档时不把缺档当 0"
        );
        assert_eq!(
            range(&candidates, |price| price.cache_write_micros, 10_000),
            None
        );
    }

    /// 折扣作用在报出的单价上，八折即 8000 bp。
    #[test]
    fn range_applies_plan_discount() {
        let price = resources::Price {
            channel_id: 1,
            model: "m".to_string(),
            input_micros: 2_500_000,
            output_micros: 10_000_000,
            cache_read_micros: None,
            cache_write_micros: None,
        };
        let candidates = vec![&price];
        assert_eq!(
            range(&candidates, |price| Some(price.input_micros), 8_000),
            Some(PriceRange {
                min_micros: 2_000_000,
                max_micros: 2_000_000
            })
        );
    }

    /// 没有候选渠道时四档全空——界面据此标「当前不可调用」。
    #[test]
    fn range_is_none_without_candidates() {
        assert_eq!(range(&[], |price| Some(price.input_micros), 10_000), None);
    }
}
