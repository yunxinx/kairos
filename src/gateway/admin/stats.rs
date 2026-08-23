//! 请求统计与生命周期聚合。

use axum::{
    Extension, Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};

use crate::store;

use super::auth::{ManagementCapability, ManagementIdentity};
use super::{AdminDeps, AdminError};

pub(super) fn routes() -> Router<AdminDeps> {
    Router::new()
        .route("/stats", get(get_stats))
        .route("/stats/lifetime", get(get_lifetime_stats))
}

// --- stats 聚合 ---

/// `/stats` 查询参数：`days` 缺省 7，由存储层夹取上限。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatsQueryParams {
    days: Option<u64>,
}

/// 汇总卡片 wire 契约。
#[derive(Debug, Serialize)]
struct StatsSummaryView {
    request_count: u64,
    success_count: u64,
    input_tokens: u64,
    output_tokens: u64,
    /// 实收（折后）合计。
    cost_usd_micros: i64,
    /// 渠道原价合计（成本）。
    base_cost_usd_micros: i64,
    /// 毛利：实收 - 渠道原价。
    gross_profit_usd_micros: i64,
    /// 令牌数：全局视图为全部，归属视图只数本人的。
    token_count: u64,
    /// 出站渠道数；归属视图整键省略（渠道属运营视角，普通用户不可见）。
    #[serde(skip_serializing_if = "Option::is_none")]
    channel_count: Option<u64>,
}

/// 逐日序列点 wire 契约。
#[derive(Debug, Serialize)]
struct DailyPointView {
    date: String,
    request_count: u64,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd_micros: i64,
    base_cost_usd_micros: i64,
    gross_profit_usd_micros: i64,
}

/// 按模型的费用/请求分布。
#[derive(Debug, Serialize)]
struct ModelShareView {
    model: String,
    request_count: u64,
    cost_usd_micros: i64,
    base_cost_usd_micros: i64,
    gross_profit_usd_micros: i64,
}

/// 按渠道的费用/请求分布。
#[derive(Debug, Serialize)]
struct ChannelShareView {
    channel: String,
    request_count: u64,
    cost_usd_micros: i64,
    base_cost_usd_micros: i64,
    gross_profit_usd_micros: i64,
}

/// `/stats` 响应：汇总 + 趋势序列 + 模型/渠道分布。
#[derive(Debug, Serialize)]
struct StatsView {
    summary: StatsSummaryView,
    daily: Vec<DailyPointView>,
    by_model: Vec<ModelShareView>,
    by_channel: Vec<ChannelShareView>,
}

/// 只读聚合：时间窗内请求量/token/费用与分布。非法 `days`（非数字）返回 400。
async fn get_stats(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    query: Result<Query<StatsQueryParams>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<StatsView>, AdminError> {
    identity.require_admin_capability(ManagementCapability::ViewLogsStats)?;
    let params = query
        .map_err(|rejection| AdminError::InvalidBody(format!("查询参数非法: {rejection}")))?
        .0;
    let days = store::clamp_stats_days(params.days);
    let stats = store::query_stats(&deps.pool, days, identity.owner_scope())
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(StatsView {
        summary: StatsSummaryView {
            request_count: stats.summary.request_count,
            success_count: stats.summary.success_count,
            input_tokens: stats.summary.input_tokens,
            output_tokens: stats.summary.output_tokens,
            cost_usd_micros: stats.summary.cost_usd_micros,
            base_cost_usd_micros: stats.summary.base_cost_usd_micros,
            gross_profit_usd_micros: stats.summary.gross_profit_usd_micros,
            token_count: stats.summary.token_count,
            channel_count: stats.summary.channel_count,
        },
        daily: stats
            .daily
            .into_iter()
            .map(|bucket| DailyPointView {
                date: bucket.date,
                request_count: bucket.request_count,
                input_tokens: bucket.input_tokens,
                output_tokens: bucket.output_tokens,
                cost_usd_micros: bucket.cost_usd_micros,
                base_cost_usd_micros: bucket.base_cost_usd_micros,
                gross_profit_usd_micros: bucket.gross_profit_usd_micros,
            })
            .collect(),
        by_model: stats
            .by_model
            .into_iter()
            .map(|share| ModelShareView {
                model: share.name,
                request_count: share.request_count,
                cost_usd_micros: share.cost_usd_micros,
                base_cost_usd_micros: share.base_cost_usd_micros,
                gross_profit_usd_micros: share.gross_profit_usd_micros,
            })
            .collect(),
        by_channel: stats
            .by_channel
            .into_iter()
            .map(|share| ChannelShareView {
                channel: share.name,
                request_count: share.request_count,
                cost_usd_micros: share.cost_usd_micros,
                base_cost_usd_micros: share.base_cost_usd_micros,
                gross_profit_usd_micros: share.gross_profit_usd_micros,
            })
            .collect(),
    }))
}

/// `/stats/lifetime` 响应：全量累计，不受时间窗影响。
///
/// `request_count` 与 `total_tokens` 含未结算行；`cost_usd_micros` 只计 HTTP 2xx
/// 且已结算的费用。两套口径并列时不要把 token 合计当成已入账费用的用量。
#[derive(Debug, Serialize)]
struct LifetimeStatsView {
    request_count: u64,
    /// 已结算的成功请求实收合计（micro-USD）。
    cost_usd_micros: i64,
    /// 已结算的成功请求渠道原价合计（成本）。
    base_cost_usd_micros: i64,
    /// 毛利：实收 - 渠道原价。
    gross_profit_usd_micros: i64,
    /// 全部请求日志的四分量 token 合计（含未结算行）。
    total_tokens: u64,
}

/// 只读全量累计：请求数 / 成功结算费用 / 四分量 token 合计。
async fn get_lifetime_stats(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
) -> Result<Json<LifetimeStatsView>, AdminError> {
    identity.require_admin_capability(ManagementCapability::ViewLogsStats)?;
    let stats = store::query_lifetime_stats(&deps.pool, identity.owner_scope())
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(LifetimeStatsView {
        request_count: stats.request_count,
        cost_usd_micros: stats.cost_usd_micros,
        base_cost_usd_micros: stats.base_cost_usd_micros,
        gross_profit_usd_micros: stats.gross_profit_usd_micros,
        total_tokens: stats.total_tokens,
    }))
}
