//! 管理面的请求日志与系统日志查询。
//!
//! 日志读取只负责把查询参数转换成存储层过滤条件，并把结果映射成管理面契约。
//! 归属范围由认证身份注入，不能由客户端参数覆盖。

use axum::{Extension, Json, extract::Path, extract::Query, extract::State};
use base64::prelude::{BASE64_STANDARD, Engine as _};
use serde::{Deserialize, Serialize};

use crate::store;

use super::admin::{AdminDeps, AdminError, mask_token_key, parse_comma_list};
use super::admin_auth::ManagementIdentity;

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
