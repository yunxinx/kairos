//! 管理面的请求日志与系统日志查询。
//!
//! 日志读取只负责把查询参数转换成存储层过滤条件，并把结果映射成管理面契约。
//! 归属范围由认证身份注入，不能由客户端参数覆盖。

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use base64::prelude::{BASE64_STANDARD, Engine as _};
use serde::{Deserialize, Serialize};

use crate::store;

use super::auth::ManagementIdentity;
use super::tokens::mask_token_key;
use super::{AdminDeps, AdminError, parse_comma_list};

pub(super) fn signed_in_routes() -> Router<AdminDeps> {
    Router::new()
        .route("/logs", get(query_logs))
        .route("/logs/{id}", get(get_log))
}

pub(super) fn admin_routes() -> Router<AdminDeps> {
    Router::new().route("/system-logs", get(query_system_logs))
}

/// 日志维护端点：只读体积统计与按时间窗清理，均要求 root（挂 `root_only` 层）。
pub(super) fn root_routes() -> Router<AdminDeps> {
    Router::new()
        .route("/logs/size", get(log_size))
        .route("/logs/cleanup", post(cleanup_logs))
}

/// 请求日志条目 wire 契约：完整 body 以 base64 编码（二进制安全）。
#[derive(Debug, Serialize)]
pub(super) struct LogEntry {
    id: i64,
    created_at: i64,
    token_name: String,
    token_key: String,
    inbound_protocol: String,
    model: String,
    outbound_model: Option<String>,
    channel: String,
    status_code: i64,
    latency_ms: i64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    input_price_usd_micros: i64,
    output_price_usd_micros: i64,
    cache_read_price_usd_micros: i64,
    cache_write_price_usd_micros: i64,
    /// 渠道原价（折扣前）。
    base_cost_usd_micros: i64,
    /// 万分比折扣率（10000 = 原价）。
    discount_bp: i64,
    /// 实收（折后），补扣/豁免按此列处理。
    cost_usd_micros: i64,
    /// 费用是否已完成所属用户钱包结算。
    settled: bool,
    request_body: Option<String>,
    response_body: Option<String>,
}

impl LogEntry {
    /// 从存储行构造 wire 条目；完整 body 字节以 base64 编码，令牌 key 按管理面规则脱敏。
    pub(super) fn from_store_log(log: store::RequestLog) -> Self {
        Self {
            id: log.id,
            created_at: log.created_at,
            token_name: log.token_name,
            token_key: mask_token_key(&log.token_key),
            inbound_protocol: log.inbound_protocol,
            model: log.model,
            outbound_model: log.outbound_model,
            channel: log.channel,
            status_code: log.status_code,
            latency_ms: log.latency_ms,
            input_tokens: log.input_tokens,
            output_tokens: log.output_tokens,
            cache_read_tokens: log.cache_read_tokens,
            cache_write_tokens: log.cache_write_tokens,
            input_price_usd_micros: log.price.input_micros,
            output_price_usd_micros: log.price.output_micros,
            cache_read_price_usd_micros: log.price.cache_read_micros,
            cache_write_price_usd_micros: log.price.cache_write_micros,
            base_cost_usd_micros: log.base_cost_usd_micros,
            discount_bp: log.discount_bp,
            cost_usd_micros: log.cost_usd_micros,
            settled: log.settled,
            request_body: log.request_body.map(|bytes| BASE64_STANDARD.encode(bytes)),
            response_body: log.response_body.map(|bytes| BASE64_STANDARD.encode(bytes)),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LogQueryParams {
    token_key: Option<String>,
    token_name: Option<String>,
    model: Option<String>,
    channel: Option<String>,
    keyword: Option<String>,
    from_created_at: Option<i64>,
    to_created_at: Option<i64>,
    settled: Option<bool>,
    inbound_protocol: Option<String>,
    sort_by: Option<store::RequestLogSortBy>,
    sort_dir: Option<store::SortDir>,
    page: Option<u64>,
    page_size: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(super) struct LogPage {
    items: Vec<LogEntry>,
    page: u64,
    page_size: u64,
    total: u64,
    unsettled_total: u64,
}

/// 分页查询请求日志；普通用户的归属范围由会话身份注入。
pub(super) async fn query_logs(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    query: Result<Query<LogQueryParams>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<LogPage>, AdminError> {
    let params = query
        .map_err(|rejection| AdminError::InvalidBody(format!("查询参数非法: {rejection}")))?
        .0;
    let mut filter =
        store::RequestLogQuery::new(params.page.unwrap_or(1), params.page_size.unwrap_or(20));
    filter.user_id = identity.owner_scope();
    filter.token_key = params.token_key;
    filter.token_name = params.token_name;
    filter.model = params.model;
    filter.channel = params.channel;
    filter.keyword = params.keyword.filter(|keyword| !keyword.trim().is_empty());
    filter.from_created_at = params.from_created_at;
    filter.to_created_at = params.to_created_at;
    filter.settled = params.settled;
    filter.inbound_protocols = parse_comma_list(params.inbound_protocol.as_deref());
    filter.sort_by = params.sort_by.unwrap_or_default();
    filter.sort_dir = params.sort_dir.unwrap_or_default();

    let (rows, total, unsettled_total) = store::query_request_log_page(&deps.pool, &filter)
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(LogPage {
        items: rows.into_iter().map(LogEntry::from_store_log).collect(),
        page: filter.page,
        page_size: filter.page_size,
        total,
        unsettled_total,
    }))
}

/// 按 id 读取一条请求日志；不属于当前身份的行按不存在处理。
pub(super) async fn get_log(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(raw): Path<String>,
) -> Result<Json<LogEntry>, AdminError> {
    let id = parse_log_id(&raw)?;
    let log = store::get_request_log(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .filter(|log| {
            identity
                .owner_scope()
                .is_none_or(|owner| owner == log.user_id)
        })
        .ok_or_else(|| AdminError::NotFound(format!("日志 {id} 不存在")))?;
    Ok(Json(LogEntry::from_store_log(log)))
}

/// 解析路径中的日志 id；非整数视为不存在。
pub(super) fn parse_log_id(raw: &str) -> Result<i64, AdminError> {
    raw.parse()
        .map_err(|_| AdminError::NotFound(format!("日志 {raw} 不存在")))
}

/// 清理窗口上限（天）：超过即视为误操作（如把年份敲成毫秒）。
const MAX_CLEANUP_DAYS: u64 = 3_650;
const MS_PER_DAY: i64 = 86_400_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CleanupBody {
    older_than_days: u64,
}

#[derive(Debug, Serialize)]
struct CleanupResultView {
    removed_request_logs: u64,
    removed_system_logs: u64,
}

#[derive(Debug, Serialize)]
struct LogSizeView {
    /// 主库文件字节数（含空闲页，删除不回缩、后续写入复用）。
    db_size_bytes: u64,
    /// WAL 边车字节数；清理收尾的 checkpoint 成功时会把它截断为零。
    wal_size_bytes: u64,
    request_log_rows: u64,
    system_log_rows: u64,
}

/// 日志占用快照：主库与 WAL 的文件系统实际大小 + 两张日志表的行数。
async fn log_size(
    State(deps): State<AdminDeps>,
    Extension(_identity): Extension<ManagementIdentity>,
) -> Result<Json<LogSizeView>, AdminError> {
    let stats = store::log_store_stats(&deps.pool, &deps.db_path)
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(LogSizeView {
        db_size_bytes: stats.db_size_bytes,
        wal_size_bytes: stats.wal_size_bytes,
        request_log_rows: stats.request_log_rows,
        system_log_rows: stats.system_log_rows,
    }))
}

/// 按时间窗手动清理：删除早于 N 天的已结算请求日志与系统日志。
///
/// 破坏性操作只交给 root 手动触发，不设自动周期——删多删少由人按占用决定。
/// 口径提示：已删除明细不再计入 `/stats` 与用户用量统计（请求数/token 会缩水），
/// 但钱包余额与累计结算金额独立于日志表，不受影响。删除后尽力把 WAL
/// checkpoint 进主库并截断（多批提交期间 WAL 会涨）；主库文件不回缩，空闲页
/// 由后续写入复用。
async fn cleanup_logs(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<CleanupBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<CleanupResultView>, AdminError> {
    let days = body.map_err(AdminError::bad_body)?.0.older_than_days;
    if days == 0 || days > MAX_CLEANUP_DAYS {
        return Err(AdminError::InvalidBody(format!(
            "older_than_days 须在 1..={MAX_CLEANUP_DAYS} 天之间"
        )));
    }
    let now = crate::gateway::logging::unix_millis();
    let cutoff = now.saturating_sub((days as i64).saturating_mul(MS_PER_DAY));
    let removed_request_logs = store::purge_settled_request_logs_before(&deps.pool, cutoff)
        .await
        .map_err(AdminError::Store)?;
    let removed_system_logs = store::purge_system_logs_before(&deps.pool, cutoff)
        .await
        .map_err(AdminError::Store)?;
    store::record_audit_detached(
        &deps.pool,
        Some(identity.actor()),
        "info",
        "logs",
        &format!(
            "清理日志：删除 {removed_request_logs} 条已结算请求日志与 \
             {removed_system_logs} 条系统日志（早于 {days} 天）"
        ),
    )
    .await;
    // 正常路径以 checkpoint 收尾：审计行先落库，WAL 才能在最后截断为零。
    // 行已删掉、审计已留痕，截断失败只记系统日志，不让清理整体报错；该告警
    // 本身会产生少量 WAL，但避免把未完成的收尾伪报为成功。
    if let Err(err) = store::checkpoint_wal_truncate(&deps.pool).await {
        store::record_system_warn(&deps.pool, "logs", &format!("清理后 WAL 收尾失败: {err}")).await;
    }
    Ok(Json(CleanupResultView {
        removed_request_logs,
        removed_system_logs,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SystemLogQueryParams {
    keyword: Option<String>,
    from_created_at: Option<i64>,
    to_created_at: Option<i64>,
    level: Option<String>,
    target: Option<String>,
    actor_user_id: Option<i64>,
    sort_by: Option<store::SystemLogSortBy>,
    sort_dir: Option<store::SortDir>,
    page: Option<u64>,
    page_size: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SystemLogEntry {
    id: i64,
    created_at: i64,
    level: String,
    target: String,
    message: String,
    actor_user_id: Option<i64>,
    actor_email: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct SystemLogPage {
    items: Vec<SystemLogEntry>,
    page: u64,
    page_size: u64,
    total: u64,
    targets: Vec<String>,
}

/// 分页查询系统日志；该端点已经由路由层限制为 admin+。
pub(super) async fn query_system_logs(
    State(deps): State<AdminDeps>,
    query: Result<Query<SystemLogQueryParams>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<SystemLogPage>, AdminError> {
    let params = query
        .map_err(|rejection| AdminError::InvalidBody(format!("查询参数非法: {rejection}")))?
        .0;
    let mut filter =
        store::SystemLogQuery::new(params.page.unwrap_or(1), params.page_size.unwrap_or(20));
    filter.keyword = params.keyword.filter(|keyword| !keyword.trim().is_empty());
    filter.from_created_at = params.from_created_at;
    filter.to_created_at = params.to_created_at;
    filter.levels = parse_comma_list(params.level.as_deref());
    filter.targets = parse_comma_list(params.target.as_deref());
    filter.actor_user_id = params.actor_user_id;
    filter.sort_by = params.sort_by.unwrap_or_default();
    filter.sort_dir = params.sort_dir.unwrap_or_default();
    let page = store::query_system_log_page(&deps.pool, &filter)
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(SystemLogPage {
        items: page
            .items
            .into_iter()
            .map(|log| SystemLogEntry {
                id: log.id,
                created_at: log.created_at,
                level: log.level,
                target: log.target,
                message: log.message,
                actor_user_id: log.actor_user_id,
                actor_email: log.actor_email,
            })
            .collect(),
        page: filter.page,
        page_size: filter.page_size,
        total: page.total,
        targets: page.targets,
    }))
}
