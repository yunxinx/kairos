//! 管理 API：独立管理监听 + 管理会话认证 + 资源 CRUD + 嵌入式 Web UI。
//!
//! 管理面与协议面物理隔离：配置文件中可选的管理监听地址（`admin_listen`）配置了
//! 才启动，未配置即管理面整体关闭，协议监听不注册任何管理路由。资源 API **只**
//! 接受登录签发的 HttpOnly Cookie 会话。配置里的 `admin_password` 是 root 的
//! Web UI 登录口令种子，哈希后进库；把它原样放进 `Authorization` 不会通过认证。
//! `webui/dist` 静态资源与 SPA 回退挂在 fallback 上、免认证。产物缺失时管理面
//! 退化为纯 API。
//!
//! 资源 CRUD（令牌/渠道/价格/模型组/统一模型）：写库（事务）→ 原子替换内存快照 → 返回变更后
//! 资源；写失败则库与快照都不动。非法输入返回结构化错误，写操作返回变更后资源。
//! 另承载设置读写、幂等余额命令、
//! 请求日志分页查询（`/logs`）、只读聚合（`/stats`、`/stats/lifetime`）、渠道连通性探测
//! （`/channels/{id}/test`）与按渠道草稿拉取上游模型列表（`/channels/models`）。

mod auth;
mod billing;
mod catalog;
mod channels;
mod logs;
mod models;
mod my_models;
mod plans;
mod probes;
mod settings;
mod stats;
mod token_balance;
mod tokens;
mod user_balance;
mod users;

use axum::{
    Json, Router,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};

use crate::{
    runtime,
    store::StoreError,
    store::users::{ManagementRole, UserRecord},
};

use self::auth::{AdminAuth, ManagementIdentity};
use super::logging::RequestLogWriter;
use super::network::OutboundClients;
use super::throttle::AuthThrottle;

const MAX_BULK_TARGETS: usize = 500;

/// 集合级批量删除契约；各资源统一使用 `targets`，避免前端按资源猜字段名。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BulkDeleteBody<T> {
    targets: Vec<T>,
}

#[derive(Debug, Serialize)]
struct BulkDeleteResult<T> {
    deleted: Vec<T>,
}

/// 幂等余额命令的原始执行结果；`None` 表示令牌处于无限额模式。
#[derive(Debug, Serialize)]
pub(super) struct BalanceAdjustmentResult {
    pub(super) operation_id: String,
    pub(super) before_balance_usd_micros: Option<i64>,
    pub(super) after_balance_usd_micros: Option<i64>,
}

/// 客户端操作 id 只接受短 ASCII 标识，避免不可见字符和无界幂等键进入持久层。
pub(super) fn validate_operation_id(operation_id: &str) -> Result<(), AdminError> {
    if operation_id.is_empty()
        || operation_id.len() > 128
        || !operation_id
            .bytes()
            .all(|byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'))
    {
        return Err(AdminError::InvalidBody(
            "operation_id 必须是 1 到 128 位 ASCII 字母、数字、连字符或下划线".to_string(),
        ));
    }
    Ok(())
}

impl<T> BulkDeleteResult<T> {
    fn new(deleted: Vec<T>) -> Self {
        Self { deleted }
    }
}

/// 批量目标必须非空、数量有界且不重复；重复通常意味着前端选择状态已损坏。
fn validate_bulk_targets<T>(targets: Vec<T>) -> Result<Vec<T>, AdminError>
where
    T: Eq + std::hash::Hash,
{
    if targets.is_empty() {
        return Err(AdminError::InvalidBody("批量删除目标不能为空".to_string()));
    }
    if targets.len() > MAX_BULK_TARGETS {
        return Err(AdminError::InvalidBody(format!(
            "单次最多删除 {MAX_BULK_TARGETS} 项"
        )));
    }
    let mut seen = std::collections::HashSet::with_capacity(targets.len());
    for target in &targets {
        if !seen.insert(target) {
            return Err(AdminError::InvalidBody("批量删除目标不能重复".to_string()));
        }
    }
    Ok(targets)
}

/// 管理面依赖：存储连接池 + 运行时快照句柄（写后原子替换）+ 出站 HTTP 客户端。
#[derive(Clone)]
pub(super) struct AdminDeps {
    pub(super) pool: SqlitePool,
    pub(super) snapshot: crate::runtime::SnapshotHandle,
    pub(super) client: reqwest::Client,
    pub(super) outbound_clients: OutboundClients,
    pub(super) throttle: AuthThrottle,
    /// Argon2id 是刻意昂贵的阻塞工作；独立预算限制同时运行的校验数量，避免登录
    /// 洪峰把 tokio blocking 池和内存同时占满。permit 只覆盖一次口令校验。
    pub(super) password_verifiers: Arc<Semaphore>,
    /// 数据库文件路径：日志维护的磁盘占用统计需要读主库与 WAL 边车的实际大小，
    /// SQL 层拿不到 WAL 文件尺寸，只能走文件系统。
    pub(super) db_path: std::path::PathBuf,
    /// 串行化「库提交后重载快照」，避免慢重载用旧库状态回退覆盖新快照。
    pub(super) reload_lock: Arc<Mutex<()>>,
    /// 结算队列写入器；管理面重放成功后通过同一通知通道立即唤醒后台消费。
    pub(super) request_log_writer: RequestLogWriter,
}

/// 开启 SQLite 写事务并立即取得写保留锁。
///
/// 管理写路径必须在同一事务内读取授权依据并修改目标；`BEGIN IMMEDIATE` 防止读取
/// 目标角色/归属后，另一个写者在实际修改前改变授权事实。
pub(super) async fn begin_write(
    deps: &AdminDeps,
) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, AdminError> {
    deps.pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(db_err)
}

/// 组装管理面路由：资源 CRUD 挂在认证中间件之后；`/login` 与静态 UI 免认证。
///
/// 路由以领域词直出（`/tokens`、`/channels`、`/prices`），集合端点 GET 列出、
/// POST 新建；单资源端点 PUT 整体替换、DELETE 删除。UI 静态资源与未匹配的 GET
/// 深链不经认证中间件。
pub fn router(
    pool: SqlitePool,
    snapshot: crate::runtime::SnapshotHandle,
    db_path: std::path::PathBuf,
) -> Router {
    router_with_writer(
        pool.clone(),
        snapshot,
        db_path,
        RequestLogWriter::start(pool),
    )
}

/// 组装管理面路由并注入共享的请求日志写入器。
pub fn router_with_writer(
    pool: SqlitePool,
    snapshot: crate::runtime::SnapshotHandle,
    db_path: std::path::PathBuf,
    request_log_writer: RequestLogWriter,
) -> Router {
    // 未配置自定义 TLS/DNS 时，rustls 后端下 `ClientBuilder::build` 只在
    // builder 事先记下错误时失败；本路径未设置会失败的选项。
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("未配置会失败的 ClientBuilder 选项，rustls 客户端应能构建");
    let throttle = AuthThrottle::new();
    let deps = AdminDeps {
        pool: pool.clone(),
        snapshot: snapshot.clone(),
        client,
        outbound_clients: OutboundClients::new(),
        throttle: throttle.clone(),
        password_verifiers: Arc::new(Semaphore::new(4)),
        db_path,
        reload_lock: Arc::new(Mutex::new(())),
        request_log_writer,
    };
    let root_only = Router::new()
        .merge(channels::routes())
        .merge(channels::model_routes())
        .merge(models::order_routes())
        .merge(probes::routes())
        .merge(settings::routes())
        .merge(plans::root_routes())
        .merge(users::root_routes())
        .merge(logs::root_routes())
        .route_layer(middleware::from_fn(auth::require_root));
    let admin_plus = Router::new()
        .merge(models::routes())
        .merge(channels::summary_routes())
        .merge(catalog::routes())
        .merge(users::admin_routes())
        .merge(billing::routes())
        .merge(plans::admin_routes())
        .route_layer(middleware::from_fn(auth::require_admin));
    // 此层等于「所有登录用户可见」。在这里新增端点前必须先回答：它是否要按归属
    // 收窄？需要收窄的（日志、统计）由处理器用 `owner_scope` 注入 user_id；
    // 与归属无关的运营端点应放进 `admin_plus` / `root_only`，而不是留在这里。
    let signed_in = Router::new()
        .merge(tokens::routes())
        .merge(logs::signed_in_routes())
        .merge(stats::routes())
        .merge(users::signed_in_routes())
        // 完全由调用者自己的套餐推导，天然按归属收窄：不接受任何「看别人」的参数。
        .merge(my_models::routes());
    let protected = Router::new()
        .merge(root_only)
        .merge(admin_plus)
        .merge(signed_in)
        .route_layer(middleware::from_fn(auth::same_origin_guard))
        .route_layer(middleware::from_fn_with_state(
            AdminAuth { pool },
            auth::admin_auth,
        ));
    // 管理 API 整体挂在 `/api` 下，SPA 独占根命名空间。
    //
    // 管理 API 与 SPA 曾共用扁平命名空间：SPA 路由只能起别名躲开同名 API，
    // 深链刷新也无法返回页面。API 统一收进 `/api` 前缀后，SPA 独占根命名空间，
    // 深链刷新才能正常命中页面路由。
    let api = Router::new()
        .merge(protected)
        .merge(users::public_routes())
        // `/api` 子路由必须有自己的 fallback：否则未匹配的 `/api/typo` 会落到顶层
        // fallback 上，把 index.html 当成 API 响应回给调用方。
        .fallback(api_not_found);
    Router::new()
        .nest("/api", api)
        // fallback 不走 route_layer：静态资源与 SPA 回退免认证；API 路由仍受中间件保护。
        .fallback(super::webui::serve)
        .with_state(deps)
}

/// `/api` 下未匹配的路径：返回结构化 404，而不是 SPA 的 index.html。
async fn api_not_found() -> Response {
    AdminError::NotFound("接口不存在".to_string()).into_response()
}

/// micro-USD 整数 → 两位小数的美元串，供审计文案使用。
///
/// 符号由金额本身决定；使用无符号绝对值以覆盖 `i64::MIN`。
pub(super) fn format_usd_micros(micros: i64) -> String {
    let negative = micros < 0;
    let abs = micros.unsigned_abs();
    let dollars = abs / 1_000_000;
    let cents = (abs % 1_000_000) / 10_000;
    format!("{}{dollars}.{cents:02}", if negative { "-" } else { "" })
}

/// admin 不能管理 admin/root；user 不能管理任何人；改角色到更高档需 root。
///
/// root 全局唯一（内置 id=1）：任何角色都不能把别人升成 root，
/// 也不能经创建接口造出第二个 root。「最后一个 root」保护仍是兜底——它另经
/// 直连数据库等旁路守住禁用/删除。
pub(in crate::gateway::admin) fn reject_user_management(
    actor: &ManagementIdentity,
    target: &UserRecord,
    new_role: Option<ManagementRole>,
) -> Result<(), AdminError> {
    if new_role == Some(ManagementRole::Root) {
        return Err(AdminError::Forbidden);
    }
    match actor.role() {
        ManagementRole::User => {
            if actor.user.id != target.id || new_role.is_some() {
                return Err(AdminError::Forbidden);
            }
        }
        ManagementRole::Admin => {
            if target.role != ManagementRole::User {
                return Err(AdminError::Forbidden);
            }
            if new_role.is_some_and(|role| role != ManagementRole::User) {
                return Err(AdminError::Forbidden);
            }
        }
        ManagementRole::Root => {}
    }
    Ok(())
}

pub(super) fn map_user_store_err(err: StoreError) -> AdminError {
    match err {
        StoreError::LastRootProtected => AdminError::LastRootProtected,
        StoreError::EmailTaken => AdminError::Conflict("邮箱已被使用".to_string()),
        StoreError::UserNotFound(id) => AdminError::NotFound(format!("用户 {id} 不存在")),
        StoreError::InvalidResource(message) => AdminError::InvalidBody(message),
        other => AdminError::Store(other),
    }
}

/// 把逗号分隔的查询参数拆成精确匹配列表；空段丢弃。
pub(super) fn parse_comma_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

/// 一次写操作的事务：开启 → 执行 → 提交。写失败（事务内报错）则回滚，库不动。
///
/// 各 CRUD 处理器内联调用（事务生命周期与处理器局部资源绑定，不宜用闭包抽象），
/// 此处只提供事务开启/提交的 sqlx 错误到 `AdminError` 的映射。
pub(super) fn db_err(err: sqlx::Error) -> AdminError {
    AdminError::Store(StoreError::Query(err))
}

/// 提交后全量重载快照并原子替换，使新资源即时生效且与库一致。
///
/// 只有写事务提交成功才会走到这里；重载失败返回 500，此时库已提交而快照未换，
/// 属极端存储错误，交由运营重试。
pub(super) async fn reload_and_swap(deps: &AdminDeps) -> Result<(), AdminError> {
    let _guard = deps.reload_lock.lock().await;
    let new_snapshot = runtime::load_snapshot(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    runtime::swap_snapshot(&deps.snapshot, new_snapshot).await;
    Ok(())
}

/// 管理面错误：全部以统一结构化 JSON 返回给调用方。
pub(super) enum AdminError {
    /// 请求体非法（400）：JSON 解析失败或字段校验失败。
    InvalidBody(String),
    /// 未认证（401）。
    Unauthorized,
    /// 已认证但角色不够（403）。
    Forbidden,
    /// 认证尝试过于频繁（429）。
    RateLimited,
    /// 资源不存在（404）。
    NotFound(String),
    /// 资源冲突（409）：同名已存在、渠道内别名占用已登记主模型，或启用渠道间别名指向不同真名。
    Conflict(String),
    /// 最后一个 root 不能删除或降级（409）。
    LastRootProtected,
    /// 上游访问失败（502）：模型列表请求不可达、非 2xx 或响应形态非法。
    Upstream(String),
    /// 存储层错误（500）。
    Store(StoreError),
}

impl AdminError {
    /// 把 JSON 提取器拒绝（畸形 body / 缺 content-type）映射为结构化 400。
    pub(super) fn bad_body(rejection: axum::extract::rejection::JsonRejection) -> Self {
        AdminError::InvalidBody(format!("请求体非法: {rejection}"))
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            AdminError::InvalidBody(msg) => (StatusCode::BAD_REQUEST, "invalid_body", msg),
            AdminError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "无效或缺失的管理凭证".to_string(),
            ),
            AdminError::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "权限不足".to_string()),
            AdminError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "认证尝试过于频繁，请稍后再试".to_string(),
            ),
            AdminError::NotFound(msg) => (StatusCode::NOT_FOUND, "not_found", msg),
            AdminError::Conflict(msg) => (StatusCode::CONFLICT, "conflict", msg),
            AdminError::LastRootProtected => (
                StatusCode::CONFLICT,
                "last_root_protected",
                "不能删除或降级最后一个 root".to_string(),
            ),
            AdminError::Upstream(msg) => (StatusCode::BAD_GATEWAY, "upstream_error", msg),
            AdminError::Store(err) => {
                tracing::error!(error = ?err, "管理 API 存储操作失败");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store_error",
                    "内部存储错误".to_string(),
                )
            }
        };
        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_usd_micros_handles_i64_min_without_overflow() {
        assert_eq!(format_usd_micros(i64::MIN), "-9223372036854.77");
    }
}
