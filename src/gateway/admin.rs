//! 管理 API：独立管理监听 + 管理会话认证 + 资源 CRUD + 嵌入式 Web UI。
//!
//! 管理面与协议面物理隔离：配置文件中可选的管理监听地址（`admin_listen`）配置了
//! 才启动，未配置即管理面整体关闭，协议监听不注册任何管理路由。资源 API **只**
//! 接受登录签发的会话（`ksess_…` Bearer）。配置里的 `admin_password` 是 root 的
//! Web UI 登录口令种子，哈希后进库；把它原样放进 `Authorization` 不会通过认证。
//! `webui/dist` 静态资源与 SPA 回退挂在 fallback 上、免认证。产物缺失时管理面
//! 退化为纯 API。
//!
//! 资源 CRUD（令牌/渠道/价格/模型组/统一模型）：写库（事务）→ 原子替换内存快照 → 返回变更后
//! 资源；写失败则库与快照都不动。非法输入返回结构化错误，写操作返回变更后资源。
//! 另承载设置读写（`/settings`）、令牌余额相对调整（`/tokens/{key}/balance`）、
//! 请求日志分页查询（`/logs`）、只读聚合（`/stats`、`/stats/lifetime`）、渠道连通性探测
//! （`/channels/{id}/test`）与按渠道草稿拉取上游模型列表（`/channels/models`）。

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, Path, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use base64::prelude::{BASE64_STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::{
    catalog,
    config::Protocol,
    core::ir::{ChatRequest, ContentPart, Message, Role},
    runtime, store,
    store::StoreError,
    store::catalog::{CatalogMeta, CatalogModel, CatalogView},
    store::resources::{
        Channel, ChannelRecord, GroupModel, ModelGroup, Price, Settings, Token, TokenRecord,
        UnifiedMember, UnifiedModel, channel_lists_callable,
    },
    store::users::{self, ManagementRole, NewUser, UserRecord},
};

use super::http::{OutboundAuth, extract_bearer};
use super::protocol;
use super::throttle::AuthThrottle;

/// 管理面依赖：存储连接池 + 运行时快照句柄（写后原子替换）+ 出站 HTTP 客户端。
#[derive(Clone)]
struct AdminDeps {
    pool: SqlitePool,
    snapshot: crate::runtime::SnapshotHandle,
    client: reqwest::Client,
    throttle: AuthThrottle,
}

/// 管理认证中间件状态：认证失败限流 + 会话查库。
///
/// 故意不持有配置里的 `admin_password`：持有它会诱使中间件做常数时间比较，
/// 把登录口令重新变成机器 API 密钥。
#[derive(Clone)]
struct AdminAuth {
    throttle: AuthThrottle,
    snapshot: crate::runtime::SnapshotHandle,
    pool: SqlitePool,
}

/// 组装管理面路由：资源 CRUD 挂在认证中间件之后；`/login` 与静态 UI 免认证。
///
/// 路由以领域词直出（`/tokens`、`/channels`、`/prices`），集合端点 GET 列出、
/// POST 新建；单资源端点 PUT 整体替换、DELETE 删除。UI 静态资源与未匹配的 GET
/// 深链不经认证中间件。
pub fn router(pool: SqlitePool, snapshot: crate::runtime::SnapshotHandle) -> Router {
    // 未配置自定义 TLS/DNS 时，rustls 后端下 `ClientBuilder::build` 只在
    // builder 事先记下错误时失败；本路径未设置会失败的选项。
    let client = reqwest::Client::builder()
        .build()
        .expect("未配置会失败的 ClientBuilder 选项，rustls 客户端应能构建");
    let throttle = AuthThrottle::new();
    let deps = AdminDeps {
        pool: pool.clone(),
        snapshot: snapshot.clone(),
        client,
        throttle: throttle.clone(),
    };
    let root_only = Router::new()
        .route("/channels", get(list_channels).post(create_channel))
        .route("/channels/models", post(list_upstream_models))
        .route("/channels/{id}", put(update_channel).delete(delete_channel))
        .route("/channels/{id}/test", post(test_channel))
        .route("/settings", get(get_settings).put(update_settings))
        .route_layer(middleware::from_fn(require_root));
    let admin_plus = Router::new()
        .route("/prices", get(list_prices).post(create_price))
        .route(
            "/prices/{channel_id}/{model}",
            put(update_price).delete(delete_price),
        )
        .route(
            "/model-groups",
            get(list_model_groups).post(create_model_group),
        )
        .route(
            "/model-groups/{name}",
            put(update_model_group).delete(delete_model_group),
        )
        .route(
            "/unified-models",
            get(list_unified_models).post(create_unified_model),
        )
        .route(
            "/unified-models/{id}",
            put(update_unified_model).delete(delete_unified_model),
        )
        .route("/catalog", get(get_catalog).put(put_catalog))
        .route("/catalog/meta", get(get_catalog_meta))
        .route("/catalog/sync", post(sync_catalog))
        .route("/users", get(list_management_users))
        .route("/users/{id}", get(get_management_user))
        .route("/users/{id}/balance", post(recharge_user))
        .route("/users/{id}/tokens", get(list_user_tokens))
        .route(
            "/users/{id}/model-groups",
            get(get_user_model_groups).put(replace_user_model_groups),
        )
        .route("/logs/{id}/settle", post(settle_log))
        .route("/logs/{id}/waive", post(waive_log))
        .route("/system-logs", get(query_system_logs))
        .route_layer(middleware::from_fn(require_admin));
    // 此层等于「所有登录用户可见」。在这里新增端点前必须先回答：它是否要按归属
    // 收窄？需要收窄的（日志、统计）由处理器用 `owner_scope` 注入 user_id；
    // 与归属无关的运营端点应放进 `admin_plus` / `root_only`，而不是留在这里。
    let signed_in = Router::new()
        .route("/tokens", get(list_tokens).post(create_token))
        .route(
            "/tokens/{token_key}",
            put(update_token).delete(delete_token),
        )
        .route("/tokens/{token_key}/balance", post(adjust_token_balance))
        .route("/logs", get(query_logs))
        .route("/logs/{id}", get(get_log))
        .route("/stats", get(get_stats))
        .route("/stats/lifetime", get(get_lifetime_stats))
        .route("/me", get(get_me).put(update_me))
        .route("/logout", post(logout))
        .route("/users", post(create_user))
        .route("/users/{id}", put(update_user).delete(delete_user));
    let protected = Router::new()
        .merge(root_only)
        .merge(admin_plus)
        .merge(signed_in)
        .route_layer(middleware::from_fn_with_state(
            AdminAuth {
                throttle,
                snapshot,
                pool,
            },
            admin_auth,
        ));
    Router::new()
        .merge(protected)
        // POST 是登录 API；GET/HEAD 走 SPA（仅 POST 时 axum 对 GET 返回 405，登录页刷新/深链打不开）。
        .route("/login", post(login).get(super::webui::serve))
        // fallback 不走 route_layer：静态资源与 SPA 回退免认证；API 路由仍受中间件保护。
        .fallback(super::webui::serve)
        .with_state(deps)
}

/// 已认证主体：一条未吊销的管理会话对应用户。
///
/// Bearer 必须是 `POST /login` 签发的 `ksess_…`，与用户登录口令不是同一种东西。
#[derive(Clone)]
struct ManagementIdentity {
    user: UserRecord,
}

impl ManagementIdentity {
    fn role(&self) -> ManagementRole {
        self.user.role
    }

    fn user_id(&self) -> i64 {
        self.user.id
    }

    /// 只读聚合与日志查询的归属范围：普通用户钉自己，admin/root 不限。
    ///
    /// ADR-0009 给 `user` 的可见面只有「自己的令牌、余额与用量」；日志与统计
    /// 都必须过这道收窄，否则登录即可读到全站流量与他人对话 body。
    fn owner_scope(&self) -> Option<i64> {
        if self.role().at_least(ManagementRole::Admin) {
            None
        } else {
            Some(self.user_id())
        }
    }
}

/// 管理认证：Bearer 仅为未吊销的会话。失败 401，窗口内过多则 429。
///
/// 只走 `user_for_session`（查 `ksess_…` 哈希）。不把配置密码、库内 Argon2 哈希
/// 或任何静态密钥拿来和 Bearer 比较——否则登录口令会退化成机器 API 密钥。
async fn admin_auth(
    State(auth): State<AdminAuth>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    mut request: Request,
    next: Next,
) -> Response {
    let ip = addr.ip();
    let snapshot = auth.snapshot.read().await;
    let max_failures = snapshot.auth_throttle_max_failures;
    let window = snapshot.auth_throttle_window();
    drop(snapshot);
    if auth.throttle.is_blocked(ip, max_failures, window) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(
                json!({ "error": { "code": "rate_limited", "message": "认证尝试过于频繁，请稍后再试" } }),
            ),
        )
            .into_response();
    }
    let provided = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(extract_bearer)
        .unwrap_or("");
    let now = super::logging::unix_millis();
    match users::user_for_session(&auth.pool, provided, now).await {
        Ok(Some(user)) => {
            request.extensions_mut().insert(ManagementIdentity { user });
            next.run(request).await
        }
        Ok(None) => {
            auth.throttle.record_failure(ip, max_failures, window);
            unauthorized_response()
        }
        Err(err) => AdminError::Store(err).into_response(),
    }
}

fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": { "code": "unauthorized", "message": "无效或缺失的管理凭证" } })),
    )
        .into_response()
}

fn forbidden_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": { "code": "forbidden", "message": "权限不足" } })),
    )
        .into_response()
}

async fn require_root(request: Request, next: Next) -> Response {
    require_min_role(request, next, ManagementRole::Root).await
}

async fn require_admin(request: Request, next: Next) -> Response {
    require_min_role(request, next, ManagementRole::Admin).await
}

async fn require_min_role(request: Request, next: Next, min: ManagementRole) -> Response {
    let Some(identity) = request.extensions().get::<ManagementIdentity>() else {
        return unauthorized_response();
    };
    if identity.role().at_least(min) {
        next.run(request).await
    } else {
        forbidden_response()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginBody {
    email: String,
    password: String,
}

#[derive(Debug, Serialize)]
struct UserView {
    id: i64,
    email: String,
    display_name: String,
    role: ManagementRole,
    enabled: bool,
    avatar: Option<String>,
    rate_limit_rpm: Option<u64>,
}

impl UserView {
    fn from_record(record: UserRecord) -> Self {
        Self {
            id: record.id,
            email: record.email,
            display_name: record.display_name,
            role: record.role,
            enabled: record.enabled,
            avatar: record.avatar,
            rate_limit_rpm: record.rate_limit_rpm,
        }
    }
}

#[derive(Debug, Serialize)]
struct LoginView {
    token: String,
    expires_at: i64,
    user: UserView,
}

/// 邮箱密码换会话。成功后的 Bearer 是会话令牌，不是登录口令本身。
async fn login(
    State(deps): State<AdminDeps>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: Result<Json<LoginBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<LoginView>, AdminError> {
    let snapshot = deps.snapshot.read().await;
    let max_failures = snapshot.auth_throttle_max_failures;
    let window = snapshot.auth_throttle_window();
    drop(snapshot);
    let ip = addr.ip();
    if deps.throttle.is_blocked(ip, max_failures, window) {
        return Err(AdminError::RateLimited);
    }
    let body = body.map_err(AdminError::bad_body)?.0;
    let Some(user) = users::authenticate_password(&deps.pool, &body.email, &body.password)
        .await
        .map_err(AdminError::Store)?
    else {
        deps.throttle.record_failure(ip, max_failures, window);
        return Err(AdminError::Unauthorized);
    };
    let now = super::logging::unix_millis();
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    let (token, expires_at) = users::issue_session(&mut tx, user.id, now)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    Ok(Json(LoginView {
        token,
        expires_at,
        user: UserView::from_record(user),
    }))
}

/// 吊销当前会话；非 `ksess_` 前缀视为无操作，仍 204。
async fn logout(State(deps): State<AdminDeps>, request: Request) -> Result<StatusCode, AdminError> {
    let provided = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(extract_bearer)
        .unwrap_or("");
    users::revoke_session(&deps.pool, provided)
        .await
        .map_err(AdminError::Store)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_me(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
) -> Result<Json<UserAdminView>, AdminError> {
    let user = users::get_user(&deps.pool, identity.user_id())
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {} 不存在", identity.user_id())))?;
    user_admin_view(&deps.pool, user, None).await.map(Json)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MeUpdate {
    email: Option<String>,
    password: Option<String>,
    /// 改密码时必填；只改邮箱/展示名不必带。防止有人拿已窃会话静默换口令。
    current_password: Option<String>,
    display_name: Option<String>,
    avatar: Option<String>,
}

/// 当前用户改自己的邮箱、展示名或密码。
async fn update_me(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<MeUpdate>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<UserView>, AdminError> {
    let update = body.map_err(AdminError::bad_body)?.0;
    let user_id = identity.user_id();
    if update.password.is_some() {
        let Some(current) = update.current_password.as_deref() else {
            return Err(AdminError::InvalidBody(
                "修改密码需要提供当前密码".to_string(),
            ));
        };
        let matches = users::password_matches(&deps.pool, user_id, current)
            .await
            .map_err(AdminError::Store)?;
        if !matches {
            return Err(AdminError::InvalidBody("当前密码不正确".to_string()));
        }
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    if let Some(email) = update.email {
        users::set_email(&mut tx, user_id, &email)
            .await
            .map_err(map_user_store_err)?;
    }
    if let Some(password) = update.password {
        users::set_password(&mut tx, user_id, &password)
            .await
            .map_err(map_user_store_err)?;
    }
    if let Some(display_name) = update.display_name {
        let name = display_name.trim();
        if name.is_empty() {
            return Err(AdminError::InvalidBody("display_name 不能为空".to_string()));
        }
        sqlx::query("UPDATE users SET display_name = ? WHERE id = ?")
            .bind(name)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
    }
    if let Some(avatar) = update.avatar {
        let avatar_val = if avatar.trim().is_empty() {
            None
        } else {
            Some(avatar.as_str())
        };
        users::set_avatar(&mut tx, user_id, avatar_val)
            .await
            .map_err(map_user_store_err)?;
    }
    tx.commit().await.map_err(db_err)?;
    let user = users::get_user(&deps.pool, user_id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {user_id} 不存在")))?;
    Ok(Json(UserView::from_record(user)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserCreate {
    email: String,
    display_name: String,
    password: String,
    role: ManagementRole,
    #[serde(default)]
    rate_limit_rpm: Option<u64>,
}

async fn create_user(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<UserCreate>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<UserView>), AdminError> {
    let create = body.map_err(AdminError::bad_body)?.0;
    match (identity.role(), create.role) {
        (ManagementRole::Root, _) => {}
        (ManagementRole::Admin, ManagementRole::User) => {}
        _ => return Err(AdminError::Forbidden),
    }
    let now = super::logging::unix_millis();
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    let user = users::insert_user(
        &mut tx,
        NewUser {
            email: &create.email,
            display_name: &create.display_name,
            password: &create.password,
            role: create.role,
            rate_limit_rpm: create.rate_limit_rpm,
        },
        now,
    )
    .await
    .map_err(map_user_store_err)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok((StatusCode::CREATED, Json(UserView::from_record(user))))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserUpdate {
    role: Option<ManagementRole>,
    enabled: Option<bool>,
    password: Option<String>,
    display_name: Option<String>,
    avatar: Option<String>,
    #[serde(default)]
    rate_limit_rpm: Option<Option<u64>>,
}

async fn update_user(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
    body: Result<Json<UserUpdate>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<UserView>, AdminError> {
    let update = body.map_err(AdminError::bad_body)?.0;
    let target = users::get_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    reject_user_management(&identity, &target, update.role)?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    if let Some(role) = update.role {
        users::set_user_role(&mut tx, id, role)
            .await
            .map_err(map_user_store_err)?;
    }
    if let Some(enabled) = update.enabled {
        users::set_user_enabled(&mut tx, id, enabled)
            .await
            .map_err(map_user_store_err)?;
    }
    if let Some(password) = update.password {
        users::set_password(&mut tx, id, &password)
            .await
            .map_err(map_user_store_err)?;
    }
    if let Some(display_name) = update.display_name {
        let name = display_name.trim();
        if name.is_empty() {
            return Err(AdminError::InvalidBody("display_name 不能为空".to_string()));
        }
        sqlx::query("UPDATE users SET display_name = ? WHERE id = ?")
            .bind(name)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
    }
    if let Some(avatar) = update.avatar {
        let avatar_val = if avatar.trim().is_empty() {
            None
        } else {
            Some(avatar.as_str())
        };
        users::set_avatar(&mut tx, id, avatar_val)
            .await
            .map_err(map_user_store_err)?;
    }
    if let Some(rpm_update) = update.rate_limit_rpm {
        users::set_rate_limit_rpm(&mut tx, id, rpm_update)
            .await
            .map_err(map_user_store_err)?;
    }
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let user = users::get_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    Ok(Json(UserView::from_record(user)))
}

async fn delete_user(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AdminError> {
    let target = users::get_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    reject_user_management(&identity, &target, None)?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    users::delete_user(&mut tx, id)
        .await
        .map_err(map_user_store_err)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignedGroupsBody {
    groups: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AssignedGroupsView {
    groups: Vec<String>,
}

async fn get_user_model_groups(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
) -> Result<Json<AssignedGroupsView>, AdminError> {
    let target = users::get_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    reject_user_management(&identity, &target, None)?;
    let groups = users::list_assigned_groups(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(AssignedGroupsView { groups }))
}

async fn replace_user_model_groups(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
    body: Result<Json<AssignedGroupsBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<AssignedGroupsView>, AdminError> {
    let body = body.map_err(AdminError::bad_body)?.0;
    let target = users::get_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    reject_user_management(&identity, &target, None)?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    let groups = users::replace_assigned_groups(&mut tx, id, &body.groups)
        .await
        .map_err(map_user_store_err)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(AssignedGroupsView { groups }))
}

#[derive(Debug, Serialize)]
struct UserAdminView {
    id: i64,
    email: String,
    display_name: String,
    role: ManagementRole,
    enabled: bool,
    avatar: Option<String>,
    rate_limit_rpm: Option<u64>,
    assigned_groups: Vec<String>,
    balance_usd_micros: i64,
    settled_usd_micros: i64,
    request_count: u64,
    input_tokens: u64,
    output_tokens: u64,
    last_used_at: Option<i64>,
}

async fn user_admin_view(
    pool: &SqlitePool,
    record: UserRecord,
    stats: Option<users::UserStatsRecord>,
) -> Result<UserAdminView, AdminError> {
    let groups = users::list_assigned_groups(pool, record.id)
        .await
        .map_err(AdminError::Store)?;
    let (balance_usd_micros, settled_usd_micros) = store::get_user_wallet(pool, record.id)
        .await
        .map_err(AdminError::Store)?;
    let stats = match stats {
        Some(s) => s,
        None => users::get_user_stats(pool, record.id)
            .await
            .map_err(AdminError::Store)?,
    };
    Ok(UserAdminView {
        id: record.id,
        email: record.email,
        display_name: record.display_name,
        role: record.role,
        enabled: record.enabled,
        avatar: record.avatar,
        rate_limit_rpm: record.rate_limit_rpm,
        assigned_groups: groups,
        balance_usd_micros,
        settled_usd_micros,
        request_count: stats.request_count,
        input_tokens: stats.input_tokens,
        output_tokens: stats.output_tokens,
        last_used_at: stats.last_used_at,
    })
}

async fn list_management_users(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
) -> Result<Json<Vec<UserAdminView>>, AdminError> {
    let mut records = users::list_users(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    if identity.role() == ManagementRole::Admin {
        records.retain(|record| record.role == ManagementRole::User);
    }
    let stats_map = users::list_users_stats(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    let mut views = Vec::with_capacity(records.len());
    for record in records {
        let stats = stats_map.get(&record.id).cloned();
        views.push(user_admin_view(&deps.pool, record, stats).await?);
    }
    Ok(Json(views))
}

async fn get_management_user(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
) -> Result<Json<UserAdminView>, AdminError> {
    let target = users::get_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    reject_user_management(&identity, &target, None)?;
    user_admin_view(&deps.pool, target, None).await.map(Json)
}

async fn recharge_user(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
    body: Result<Json<BalanceAdjustment>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<UserAdminView>, AdminError> {
    let delta = body.map_err(AdminError::bad_body)?.0.delta_usd_micros;
    let target = users::get_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    reject_user_management(&identity, &target, None)?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    store::adjust_user_balance(&mut tx, id, delta)
        .await
        .map_err(map_user_store_err)?;
    tx.commit().await.map_err(db_err)?;
    user_admin_view(&deps.pool, target, None).await.map(Json)
}

async fn list_user_tokens(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<TokenView>>, AdminError> {
    let target = users::get_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    reject_user_management(&identity, &target, None)?;
    let mut records = store::resources::list_token_records(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    records.retain(|record| record.token.user_id == id);
    records.sort_by(|a, b| a.token.token_key.cmp(&b.token.token_key));
    let settled = store::list_token_settled(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(
        records
            .into_iter()
            .map(|record| {
                let amount = settled.get(&record.token.token_key).copied().unwrap_or(0);
                TokenView::from_record(record, amount)
            })
            .collect(),
    ))
}

/// 自己的令牌可整体替换或删除。admin/root 对他人令牌仅当所有者为普通用户且只改 `enabled`。
async fn reject_token_mutation(
    deps: &AdminDeps,
    identity: &ManagementIdentity,
    existing: &TokenRecord,
    next: Option<&Token>,
) -> Result<(), AdminError> {
    if existing.token.user_id == identity.user_id() {
        return Ok(());
    }
    if !identity.role().at_least(ManagementRole::Admin) {
        return Err(AdminError::Forbidden);
    }
    let owner = users::get_user(&deps.pool, existing.token.user_id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {} 不存在", existing.token.user_id)))?;
    if owner.role != ManagementRole::User {
        return Err(AdminError::Forbidden);
    }
    let Some(next) = next else {
        return Err(AdminError::Forbidden);
    };
    if !disable_only_token_change(&existing.token, next) {
        return Err(AdminError::Forbidden);
    }
    Ok(())
}

/// 除 `enabled` 外，定义字段必须与现有令牌一致（跨用户禁用不得改名/改绑/改限额）。
fn disable_only_token_change(existing: &Token, next: &Token) -> bool {
    next.token_key == existing.token_key
        && next.name == existing.name
        && next.limit_usd_micros == existing.limit_usd_micros
        && next.rate_limit_rpm == existing.rate_limit_rpm
        && next.model_group == existing.model_group
}

/// admin 不能管理 admin/root；user 不能管理任何人；改角色到更高档需 root。
fn reject_user_management(
    actor: &ManagementIdentity,
    target: &UserRecord,
    new_role: Option<ManagementRole>,
) -> Result<(), AdminError> {
    match actor.role() {
        ManagementRole::User => return Err(AdminError::Forbidden),
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

fn map_user_store_err(err: StoreError) -> AdminError {
    match err {
        StoreError::LastRootProtected => AdminError::LastRootProtected,
        StoreError::EmailTaken => AdminError::Conflict("邮箱已被使用".to_string()),
        StoreError::UserNotFound(id) => AdminError::NotFound(format!("用户 {id} 不存在")),
        StoreError::InvalidResource(message) => AdminError::InvalidBody(message),
        other => AdminError::Store(other),
    }
}

// --- 令牌 ---

/// 令牌读响应 wire 契约：定义字段 + 生命周期元数据 + 该令牌累计结算（写契约仍是 `Token`）。
#[derive(Debug, Serialize)]
struct TokenView {
    token_key: String,
    name: String,
    limit_usd_micros: Option<i64>,
    rate_limit_rpm: Option<u64>,
    enabled: bool,
    model_group: String,
    created_at: i64,
    last_used_at: Option<i64>,
    settled_usd_micros: i64,
}

impl TokenView {
    /// 从存储层记录构造 wire 视图。
    fn from_record(record: store::resources::TokenRecord, settled_usd_micros: i64) -> Self {
        Self {
            token_key: record.token.token_key,
            name: record.token.name,
            limit_usd_micros: record.token.limit_usd_micros,
            rate_limit_rpm: record.token.rate_limit_rpm,
            enabled: record.token.enabled,
            model_group: record.token.model_group,
            created_at: record.created_at,
            last_used_at: record.last_used_at,
            settled_usd_micros,
        }
    }
}

async fn token_view(
    pool: &SqlitePool,
    record: store::resources::TokenRecord,
) -> Result<TokenView, AdminError> {
    let settled = store::get_token_settled(pool, &record.token.token_key)
        .await
        .map_err(AdminError::Store)?;
    Ok(TokenView::from_record(record, settled))
}

/// 列出当前用户的令牌（按 `token_key` 排序，保证确定性）。
///
/// 直接读库而非快照：`last_used_at` 随请求路径刷新、不进快照，列表需要库内最新值。
/// 他人令牌经 `GET /users/{id}/tokens`。
async fn list_tokens(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
) -> Result<Json<Vec<TokenView>>, AdminError> {
    let mut records = store::resources::list_token_records(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    records.retain(|record| record.token.user_id == identity.user_id());
    records.sort_by(|a, b| a.token.token_key.cmp(&b.token.token_key));
    let settled = store::list_token_settled(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(
        records
            .into_iter()
            .map(|record| {
                let amount = settled.get(&record.token.token_key).copied().unwrap_or(0);
                TokenView::from_record(record, amount)
            })
            .collect(),
    ))
}

/// 新建令牌请求契约：不接受指定 key，key 由系统高熵生成。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenCreate {
    name: String,
    limit_usd_micros: Option<i64>,
    #[serde(default)]
    rate_limit_rpm: Option<u64>,
    enabled: bool,
    #[serde(default = "crate::store::resources::default_model_group")]
    model_group: String,
}

/// 系统生成的令牌 key 前缀。
const TOKEN_KEY_PREFIX: &str = "ks-";
/// key 前缀之后的随机字符数。
const TOKEN_KEY_RANDOM_LEN: usize = 64;

/// 生成高熵令牌 key：`ks-` + 64 位大小写字母与数字（约 380 bit 熵，CSPRNG 采样）。
fn generate_token_key() -> String {
    use rand::distr::{Alphanumeric, SampleString};
    let random_part = Alphanumeric.sample_string(&mut rand::rng(), TOKEN_KEY_RANDOM_LEN);
    format!("{TOKEN_KEY_PREFIX}{random_part}")
}

/// 新建令牌：key 由系统生成（快照内查重，碰撞实际不可能发生），写库 + 换快照 + 返回新令牌。
async fn create_token(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<TokenCreate>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<TokenView>), AdminError> {
    let create = body.map_err(AdminError::bad_body)?.0;
    let token = Token {
        token_key: {
            let snapshot = deps.snapshot.read().await;
            loop {
                let candidate = generate_token_key();
                if !snapshot.tokens.contains_key(&candidate) {
                    break candidate;
                }
            }
        },
        name: create.name,
        limit_usd_micros: create.limit_usd_micros,
        rate_limit_rpm: create.rate_limit_rpm,
        enabled: create.enabled,
        model_group: create.model_group.trim().to_string(),
        user_id: identity.user_id(),
    };
    validate_token(&token)?;
    reject_unknown_group(&deps, &token.model_group).await?;
    reject_unassigned_group(&deps, &identity, &token.model_group).await?;
    let now = super::logging::unix_millis();
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_token(&mut tx, &token, now)
        .await
        .map_err(AdminError::Store)?;
    // 令牌定义与累计结算行分离：新建时同步建零额结算行，并把后续充值记入所属用户钱包
    // （`adjust_balance` 的 UPDATE 按令牌找用户，缺结算行仍可读钱包）。
    crate::store::ensure_token_balance(&mut tx, &token.token_key, 0.0, now)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let created = read_token_record(&deps, &token.token_key).await?;
    Ok((
        StatusCode::CREATED,
        Json(token_view(&deps.pool, created).await?),
    ))
}

/// 整体替换令牌（按路径 `token_key`，路径权威）：写库 + 换快照 + 返回新令牌。
///
/// 不存在则 404，不借 PUT 隐式创建（与 POST 只生成系统 key 对齐）。upsert 的冲突
/// 分支不触碰创建时间与最后使用时间，属性编辑不重置生命周期元数据。
async fn update_token(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(token_key): Path<String>,
    body: Result<Json<Token>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<TokenView>, AdminError> {
    let mut token = body.map_err(AdminError::bad_body)?.0;
    token.token_key = token_key;
    token.model_group = token.model_group.trim().to_string();
    reject_token_key_shape(&token)?;
    let existing = read_token_record(&deps, &token.token_key).await?;
    reject_token_mutation(&deps, &identity, &existing, Some(&token)).await?;
    if existing.token.user_id == identity.user_id() {
        validate_token(&token)?;
        reject_unknown_group(&deps, &token.model_group).await?;
        reject_unassigned_group(&deps, &identity, &token.model_group).await?;
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_token(&mut tx, &token, super::logging::unix_millis())
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_token_record(&deps, &token.token_key).await?;
    Ok(Json(token_view(&deps.pool, updated).await?))
}

/// 删除令牌：不存在则 404，否则删除并返回被删令牌。
///
/// 同事务先删余额、后删令牌定义：`token_balance.token_key` 外键指向 `tokens`
/// （ON DELETE CASCADE 兜底）；显式清理余额行，不依赖级联语义。余额行残留会
/// 让同 key 重建的令牌复活旧余额。
async fn delete_token(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(token_key): Path<String>,
) -> Result<Json<TokenView>, AdminError> {
    let deleted = read_token_record(&deps, &token_key).await?;
    reject_token_mutation(&deps, &identity, &deleted, None).await?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::delete_token_balance(&mut tx, &token_key)
        .await
        .map_err(AdminError::Store)?;
    crate::store::resources::delete_token(&mut tx, &token_key)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(token_view(&deps.pool, deleted).await?))
}

// --- 渠道 ---

/// 渠道读视图：库生成的稳定身份 + 定义字段（同级展开序列化）。
///
/// 写契约仍是无 id 的 `Channel`；id 只随读响应返回。
#[derive(Debug, Serialize)]
struct ChannelView {
    id: i64,
    #[serde(flatten)]
    channel: Channel,
}

impl ChannelView {
    fn from_record(record: ChannelRecord) -> Self {
        ChannelView {
            id: record.id,
            channel: record.channel,
        }
    }
}

/// 解析路径中的渠道 id；非整数不标识任何渠道，按不存在处理（404）。
fn parse_channel_id(raw: String) -> Result<i64, AdminError> {
    raw.parse::<i64>()
        .map_err(|_| AdminError::NotFound(format!("渠道 {raw} 不存在")))
}

/// 列出全部渠道（保持快照顺序），返回带 id 的视图。
async fn list_channels(
    State(deps): State<AdminDeps>,
) -> Result<Json<Vec<ChannelView>>, AdminError> {
    let snapshot = deps.snapshot.read().await;
    Ok(Json(
        snapshot
            .channels
            .iter()
            .cloned()
            .map(ChannelView::from_record)
            .collect(),
    ))
}

/// 新建渠道：同名已存在则冲突（409），否则写库 + 换快照 + 返回新渠道视图。
async fn create_channel(
    State(deps): State<AdminDeps>,
    body: Result<Json<Channel>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<ChannelView>), AdminError> {
    let mut channel = body.map_err(AdminError::bad_body)?;
    normalize_channel_group(&mut channel);
    validate_channel(&channel)?;
    reject_unknown_group(&deps, &channel.model_group).await?;
    {
        let snapshot = deps.snapshot.read().await;
        if snapshot
            .channels
            .iter()
            .any(|record| record.channel.name == channel.name)
        {
            return Err(AdminError::Conflict(format!(
                "渠道 {} 已存在",
                channel.name
            )));
        }
        reject_alias_occupancy(&channel)?;
        reject_alias_conflict(&snapshot.channels, &channel, None)?;
        reject_unhidden_unified_collision(
            &snapshot.channels,
            Some(&channel),
            None,
            snapshot.unified_models.values(),
            None,
        )?;
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    let id = crate::store::resources::insert_channel(&mut tx, &channel)
        .await
        .map_err(AdminError::Store)?;
    enroll_channel_models(&mut tx, id, None, &channel).await?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let created = read_channel_record(&deps, id).await?;
    Ok((StatusCode::CREATED, Json(ChannelView::from_record(created))))
}

/// 整体替换渠道（按路径 `id` 定位）：写库 + 换快照 + 返回新渠道视图。
///
/// `name` 变化即改名，id 保持不变；新名已被其它渠道占用返回 409。
async fn update_channel(
    State(deps): State<AdminDeps>,
    Path(raw_id): Path<String>,
    body: Result<Json<Channel>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ChannelView>, AdminError> {
    let id = parse_channel_id(raw_id)?;
    let mut channel = body.map_err(AdminError::bad_body)?;
    normalize_channel_group(&mut channel);
    validate_channel(&channel)?;
    reject_unknown_group(&deps, &channel.model_group).await?;
    let previous = {
        let snapshot = deps.snapshot.read().await;
        let current = snapshot
            .channels
            .iter()
            .find(|record| record.id == id)
            .ok_or_else(|| AdminError::NotFound(format!("渠道 {id} 不存在")))?;
        if channel.name != current.channel.name
            && snapshot
                .channels
                .iter()
                .any(|record| record.channel.name == channel.name)
        {
            return Err(AdminError::Conflict(format!(
                "渠道 {} 已存在",
                channel.name
            )));
        }
        reject_alias_occupancy(&channel)?;
        reject_alias_conflict(&snapshot.channels, &channel, Some(id))?;
        reject_unhidden_unified_collision(
            &snapshot.channels,
            Some(&channel),
            Some(id),
            snapshot.unified_models.values(),
            None,
        )?;
        current.channel.clone()
    };
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::update_channel(&mut tx, id, &channel)
        .await
        .map_err(AdminError::Store)?;
    crate::store::resources::retain_channel_prices(
        &mut tx,
        id,
        &crate::store::resources::channel_callable_names(&channel),
    )
    .await
    .map_err(AdminError::Store)?;
    enroll_channel_models(&mut tx, id, Some(&previous), &channel).await?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_channel_record(&deps, id).await?;
    Ok(Json(ChannelView::from_record(updated)))
}

/// 删除渠道（按路径 `id`）：不存在则 404，否则删除并返回被删渠道视图。
async fn delete_channel(
    State(deps): State<AdminDeps>,
    Path(raw_id): Path<String>,
) -> Result<Json<ChannelView>, AdminError> {
    let id = parse_channel_id(raw_id)?;
    let deleted = read_channel_record(&deps, id).await?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::delete_channel(&mut tx, id)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(ChannelView::from_record(deleted)))
}

// --- 价格 ---

/// 列出全部价格（按渠道 id、模型名排序，保证确定性）。
async fn list_prices(State(deps): State<AdminDeps>) -> Result<Json<Vec<Price>>, AdminError> {
    let snapshot = deps.snapshot.read().await;
    let mut prices: Vec<Price> = snapshot
        .prices
        .values()
        .flat_map(|inner| inner.values())
        .cloned()
        .collect();
    prices.sort_by(|left, right| {
        left.channel_id
            .cmp(&right.channel_id)
            .then(left.model.cmp(&right.model))
    });
    Ok(Json(prices))
}

/// 新建价格：同一渠道同一模型已存在则冲突（409），否则写库 + 换快照 + 返回新价格。
async fn create_price(
    State(deps): State<AdminDeps>,
    body: Result<Json<Price>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<Price>), AdminError> {
    let price = body.map_err(AdminError::bad_body)?;
    validate_price(&price)?;
    {
        let snapshot = deps.snapshot.read().await;
        reject_unknown_price_channel(&snapshot, price.channel_id)?;
        reject_unlisted_price_callable(&snapshot, price.channel_id, &price.model)?;
        if snapshot
            .price_for_channel(price.channel_id, &price.model)
            .is_some()
        {
            return Err(AdminError::Conflict(format!(
                "价格 渠道 {} / {} 已存在",
                price.channel_id, price.model
            )));
        }
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_price(&mut tx, &price)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let created = read_price(&deps, price.channel_id, &price.model).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// 整体替换价格（路径 `channel_id`/`model` 权威）：写库 + 换快照 + 返回新价格。
async fn update_price(
    State(deps): State<AdminDeps>,
    Path((channel_id, model)): Path<(i64, String)>,
    body: Result<Json<Price>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Price>, AdminError> {
    let mut price = body.map_err(AdminError::bad_body)?;
    price.channel_id = channel_id;
    price.model = model;
    validate_price(&price)?;
    {
        let snapshot = deps.snapshot.read().await;
        reject_unknown_price_channel(&snapshot, price.channel_id)?;
        reject_unlisted_price_callable(&snapshot, price.channel_id, &price.model)?;
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_price(&mut tx, &price)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_price(&deps, price.channel_id, &price.model).await?;
    Ok(Json(updated))
}

/// 删除价格：不存在则 404，否则删除并返回被删价格。
async fn delete_price(
    State(deps): State<AdminDeps>,
    Path((channel_id, model)): Path<(i64, String)>,
) -> Result<Json<Price>, AdminError> {
    let deleted = read_price(&deps, channel_id, &model).await?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::delete_price(&mut tx, channel_id, &model)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(deleted))
}

// --- 模型组 ---

/// 列出全部模型组（按 `name` 排序，保证确定性）。
async fn list_model_groups(
    State(deps): State<AdminDeps>,
) -> Result<Json<Vec<ModelGroup>>, AdminError> {
    let snapshot = deps.snapshot.read().await;
    let mut groups: Vec<ModelGroup> = snapshot.model_groups.values().cloned().collect();
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(groups))
}

/// 新建模型组：同名已存在则冲突（409），否则写库 + 换快照 + 返回新组。
async fn create_model_group(
    State(deps): State<AdminDeps>,
    body: Result<Json<ModelGroup>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<ModelGroup>), AdminError> {
    let mut group = body.map_err(AdminError::bad_body)?;
    {
        let snapshot = deps.snapshot.read().await;
        normalize_model_group(&mut group, &snapshot)?;
        if snapshot.model_groups.contains_key(&group.name) {
            return Err(AdminError::Conflict(format!(
                "模型组 {} 已存在",
                group.name
            )));
        }
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_model_group(&mut tx, &group)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let created = read_model_group(&deps, &group.name).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// 整体替换模型组（按路径 `name`，路径权威）：写库 + 换快照 + 返回新组。
async fn update_model_group(
    State(deps): State<AdminDeps>,
    Path(name): Path<String>,
    body: Result<Json<ModelGroup>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ModelGroup>, AdminError> {
    let mut group = body.map_err(AdminError::bad_body)?;
    group.name = name;
    {
        let snapshot = deps.snapshot.read().await;
        normalize_model_group(&mut group, &snapshot)?;
        if !snapshot.model_groups.contains_key(&group.name) {
            return Err(AdminError::NotFound(format!(
                "模型组 {} 不存在",
                group.name
            )));
        }
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_model_group(&mut tx, &group)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_model_group(&deps, &group.name).await?;
    Ok(Json(updated))
}

/// 删除模型组：内置 `default` 拒绝；令牌组由外键置空失效，渠道默认组改回 `default`。
async fn delete_model_group(
    State(deps): State<AdminDeps>,
    Path(name): Path<String>,
) -> Result<Json<ModelGroup>, AdminError> {
    if name == crate::store::resources::DEFAULT_MODEL_GROUP {
        return Err(AdminError::Conflict("内置组 default 不能删除".to_string()));
    }
    let deleted = read_model_group(&deps, &name).await?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::rebind_channels_to_default(&mut tx, &name)
        .await
        .map_err(AdminError::Store)?;
    crate::store::resources::delete_model_group(&mut tx, &name)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(deleted))
}

// --- 统一模型 ---

/// 列出全部统一模型（按 `id` 排序，保证确定性）。
///
/// 读视图带 `available`：渠道已删/停用/不再登记该名时为 false，写契约不含此字段。
async fn list_unified_models(
    State(deps): State<AdminDeps>,
) -> Result<Json<Vec<UnifiedModelView>>, AdminError> {
    let snapshot = deps.snapshot.read().await;
    let mut models: Vec<UnifiedModelView> = snapshot
        .unified_models
        .values()
        .map(|model| unified_model_view(model, &snapshot))
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Json(models))
}

/// 新建统一模型：同 ID 已存在则冲突（409），否则写库 + 换快照 + 返回新资源。
async fn create_unified_model(
    State(deps): State<AdminDeps>,
    body: Result<Json<UnifiedModel>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<UnifiedModel>), AdminError> {
    let mut model = body.map_err(AdminError::bad_body)?;
    {
        let snapshot = deps.snapshot.read().await;
        normalize_unified_model(&mut model, &snapshot)?;
        if snapshot.unified_models.contains_key(&model.id) {
            return Err(AdminError::Conflict(format!(
                "统一模型 {} 已存在",
                model.id
            )));
        }
        reject_unhidden_unified_collision(
            &snapshot.channels,
            None,
            None,
            snapshot.unified_models.values(),
            Some(&model),
        )?;
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_unified_model(&mut tx, &model)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let created = read_unified_model(&deps, &model.id).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// 整体替换统一模型（按路径 `id`，路径权威）：写库 + 换快照 + 返回新资源。
async fn update_unified_model(
    State(deps): State<AdminDeps>,
    Path(id): Path<String>,
    body: Result<Json<UnifiedModel>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<UnifiedModel>, AdminError> {
    let mut model = body.map_err(AdminError::bad_body)?;
    model.id = id;
    {
        let snapshot = deps.snapshot.read().await;
        normalize_unified_model(&mut model, &snapshot)?;
        if !snapshot.unified_models.contains_key(&model.id) {
            return Err(AdminError::NotFound(format!(
                "统一模型 {} 不存在",
                model.id
            )));
        }
        reject_unhidden_unified_collision(
            &snapshot.channels,
            None,
            None,
            snapshot.unified_models.values(),
            Some(&model),
        )?;
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_unified_model(&mut tx, &model)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_unified_model(&deps, &model.id).await?;
    Ok(Json(updated))
}

/// 删除统一模型：不存在则 404，否则删除并返回被删资源。
async fn delete_unified_model(
    State(deps): State<AdminDeps>,
    Path(id): Path<String>,
) -> Result<Json<UnifiedModel>, AdminError> {
    let deleted = read_unified_model(&deps, &id).await?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::delete_unified_model(&mut tx, &id)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(deleted))
}

// --- 设置 ---

/// 读当前运行时设置：从内存快照直接取（与请求路径读同一份真值）。
async fn get_settings(State(deps): State<AdminDeps>) -> Result<Json<Settings>, AdminError> {
    let updated = read_settings(&deps).await?;
    Ok(Json(updated))
}

/// 整体更新运行时设置：写库 → 换快照 → 返回变更后设置。
///
/// 设置变更后经快照原子替换即时生效：入站请求体上限、认证限流、SSE 重装上限
/// 与同渠道退避的变更立刻作用于后续请求。
async fn update_settings(
    State(deps): State<AdminDeps>,
    body: Result<Json<Settings>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Settings>, AdminError> {
    let settings = body.map_err(AdminError::bad_body)?;
    validate_settings(&settings)?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_settings(&mut tx, &settings)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_settings(&deps).await?;
    Ok(Json(updated))
}

/// 校验设置字段：须为正的阈值写成 0 属运营误配；认证失败次数允许 0（关闭限流）。
fn validate_settings(settings: &Settings) -> Result<(), AdminError> {
    if settings.max_request_bytes == 0 {
        return Err(AdminError::InvalidBody(
            "max_request_bytes 必须大于 0".to_string(),
        ));
    }
    if settings.max_response_bytes == 0 {
        return Err(AdminError::InvalidBody(
            "max_response_bytes 必须大于 0".to_string(),
        ));
    }
    if settings.auth_throttle_window_secs == 0 {
        return Err(AdminError::InvalidBody(
            "auth_throttle_window_secs 必须大于 0".to_string(),
        ));
    }
    if settings.sse_reassembly_max_bytes == 0 {
        return Err(AdminError::InvalidBody(
            "sse_reassembly_max_bytes 必须大于 0".to_string(),
        ));
    }
    if settings.retry_backoff_ms == 0 {
        return Err(AdminError::InvalidBody(
            "retry_backoff_ms 必须大于 0".to_string(),
        ));
    }
    if settings.retry_backoff_cap_ms == 0 {
        return Err(AdminError::InvalidBody(
            "retry_backoff_cap_ms 必须大于 0".to_string(),
        ));
    }
    if settings.retry_backoff_cap_ms < settings.retry_backoff_ms {
        return Err(AdminError::InvalidBody(
            "retry_backoff_cap_ms 不能小于 retry_backoff_ms".to_string(),
        ));
    }
    if settings.retry_after_cap_secs == 0 {
        return Err(AdminError::InvalidBody(
            "retry_after_cap_secs 必须大于 0".to_string(),
        ));
    }
    if settings.log_body_max_bytes == 0 {
        return Err(AdminError::InvalidBody(
            "log_body_max_bytes 必须大于 0".to_string(),
        ));
    }
    Ok(())
}

/// 从当前快照读回设置。
async fn read_settings(deps: &AdminDeps) -> Result<Settings, AdminError> {
    let snapshot = deps.snapshot.read().await;
    Ok(snapshot.to_settings())
}

/// 目录写入契约：整表替换缓存行；同步时刻由服务端填写。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogPut {
    models: Vec<CatalogModel>,
}

/// `GET /catalog` 查询参数。两个都缺省（或空串）时返回全表，兼容 PUT 后 roundtrip。
///
/// `provider_id` 为逗号分隔的提供方 id。axum 标准 `Query` 走 `serde_urlencoded`，
/// 同一键重复（`?provider_id=a&provider_id=b`）不会反序列化成 `Vec`（那是
/// `axum_extra::extract::Query`）；故用单个字符串。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogQuery {
    q: Option<String>,
    provider_id: Option<String>,
}

/// 把逗号分隔的查询参数拆成精确匹配列表；空段丢弃。
fn parse_comma_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

/// 读价格目录缓存；可按 `q` / `provider_id` 过滤。
async fn get_catalog(
    State(deps): State<AdminDeps>,
    query: Result<Query<CatalogQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<CatalogView>, AdminError> {
    let params = query
        .map_err(|rejection| AdminError::InvalidBody(format!("查询参数非法: {rejection}")))?
        .0;
    let q = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty());
    let provider_ids = parse_comma_list(params.provider_id.as_deref());
    let view = crate::store::catalog::load_catalog_view(&deps.pool, q, &provider_ids)
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(view))
}

/// 读目录元数据：上次同步时刻与提供方列表，不返回模型行。
async fn get_catalog_meta(State(deps): State<AdminDeps>) -> Result<Json<CatalogMeta>, AdminError> {
    let meta = crate::store::catalog::load_catalog_meta(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(meta))
}

/// 整表替换价格目录缓存（供导入与测试播种）；同步时刻记为现在。
async fn put_catalog(
    State(deps): State<AdminDeps>,
    body: Result<Json<CatalogPut>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<CatalogView>, AdminError> {
    let Json(CatalogPut { models }) = body.map_err(AdminError::bad_body)?;
    let synced_at = super::logging::unix_millis();
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::catalog::replace_catalog_models(&mut tx, &models)
        .await
        .map_err(AdminError::Store)?;
    crate::store::catalog::set_catalog_synced_at(&mut tx, synced_at)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    Ok(Json(CatalogView {
        synced_at: Some(synced_at),
        models,
    }))
}

/// 从 models.dev 拉取并替换价格目录缓存。
async fn sync_catalog(State(deps): State<AdminDeps>) -> Result<Json<CatalogView>, AdminError> {
    let view =
        catalog::fetch_and_replace(&deps.pool, &deps.client, catalog::MODELS_DEV_CATALOG_URL)
            .await
            .map_err(catalog_err)?;
    Ok(Json(view))
}

/// 目录拉取失败视为上游错误；存储失败保持 500。
fn catalog_err(err: catalog::CatalogError) -> AdminError {
    match err {
        catalog::CatalogError::Store(err) => AdminError::Store(err),
        other => AdminError::Upstream(other.to_string()),
    }
}

// --- 令牌余额调整 ---

/// 余额调整请求体：`delta_usd_micros` 为相对量（正数充值、负数扣减）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BalanceAdjustment {
    delta_usd_micros: i64,
}

/// 余额视图 wire 契约：调整后余额与累计结算额。
#[derive(Debug, Serialize)]
struct BalanceView {
    token_key: String,
    balance_usd_micros: i64,
    settled_usd_micros: i64,
}

/// 相对调整令牌所属用户的钱包（充值/扣减），库内原子完成，返回调整后视图。
///
/// 不动令牌定义（修改令牌属性不重置钱包）；剩余存在 `user_balance`，不参与
/// 快照替换。令牌不存在返回 404；结算行缺失先在事务内建零额行再调整。
async fn adjust_token_balance(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(token_key): Path<String>,
    body: Result<Json<BalanceAdjustment>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<BalanceView>, AdminError> {
    if !identity.role().at_least(ManagementRole::Admin) {
        return Err(AdminError::Forbidden);
    }
    let adjustment = body.map_err(AdminError::bad_body)?;
    let existing = read_token_record(&deps, &token_key).await?;
    let owner = users::get_user(&deps.pool, existing.token.user_id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {} 不存在", existing.token.user_id)))?;
    reject_user_management(&identity, &owner, None)?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    // 存在性校验在事务内针对 tokens 表完成（与后续写持同一写锁），避免并发删除后
    // 仍写出一条孤儿余额行、被重建令牌复活。
    if !crate::store::resources::token_exists(&mut tx, &token_key)
        .await
        .map_err(AdminError::Store)?
    {
        return Err(AdminError::NotFound(format!("令牌 {token_key} 不存在")));
    }
    // 确保余额行存在（新建令牌已建零额行，此处防御已删余额行的边界），再原子调整。
    crate::store::ensure_token_balance(&mut tx, &token_key, 0.0, super::logging::unix_millis())
        .await
        .map_err(AdminError::Store)?;
    let balance = crate::store::adjust_balance(&mut tx, &token_key, adjustment.delta_usd_micros)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    Ok(Json(BalanceView {
        token_key,
        balance_usd_micros: balance.balance_usd_micros,
        settled_usd_micros: balance.settled_usd_micros,
    }))
}

// --- 请求日志查询 ---

/// `/logs` 查询参数：全部过滤维度可选，缺省即不限。
///
/// `deny_unknown_fields`：参数拼写错误若被静默忽略，会返回未过滤的结果误导运营，
/// 与 body 契约拒绝未知字段的决策一致。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogQueryParams {
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

/// 请求日志条目 wire 契约：完整 body 以 base64 编码（二进制安全）。
///
/// `GET /logs` 列表不读 BLOB，`request_body` / `response_body` 为 null；
/// `GET /logs/{id}` 才返回落库的 body。
#[derive(Debug, Serialize)]
struct LogEntry {
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
    /// 费用是否已写入 `token_balance`。
    settled: bool,
    request_body: Option<String>,
    response_body: Option<String>,
}

impl LogEntry {
    /// 从存储行构造 wire 条目；完整 body 字节以 base64 编码；令牌 key 按 UI 同款规则脱敏。
    fn from_store_log(log: store::RequestLog) -> Self {
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

/// 分页响应：本页条目 + 实际采用的页码/每页条数 + 满足过滤的总数。
#[derive(Debug, Serialize)]
struct LogPage {
    items: Vec<LogEntry>,
    page: u64,
    page_size: u64,
    total: u64,
    /// 当前过滤条件下 `settled = false` 的条数（忽略 settled 查询维）。
    unsettled_total: u64,
}

/// 分页查询请求日志（缺省时间倒序），按令牌 key/名、模型、渠道、综合关键字、时间范围过滤，只读。
///
/// `page`/`page_size` 缺省 1/20；`page_size` 上限 200（由存储层夹取），响应的
/// `page`/`page_size` 反映实际采用值。非法查询参数（如非数字页码）返回结构化 400。
async fn query_logs(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    query: Result<Query<LogQueryParams>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<LogPage>, AdminError> {
    let params = query
        .map_err(|rejection| AdminError::InvalidBody(format!("查询参数非法: {rejection}")))?
        .0;
    let mut filter =
        store::RequestLogQuery::new(params.page.unwrap_or(1), params.page_size.unwrap_or(20));
    // 归属范围由会话身份决定，不接受查询参数覆盖：普通用户改不了自己的可见面。
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

/// 按 id 读取一条请求日志（含 body）。不存在、id 非法或不属于本人一律 404。
///
/// 越权按「不存在」而非 403 应答：403 会确认该 id 存在，让普通用户能靠遍历 id
/// 摸出全站的流量规模。
async fn get_log(
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
fn parse_log_id(raw: &str) -> Result<i64, AdminError> {
    raw.parse()
        .map_err(|_| AdminError::NotFound(format!("日志 {raw} 不存在")))
}

/// 对未结算日志补扣：按行上费用写入余额（允许透支），再标为已结算。
async fn settle_log(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(raw): Path<String>,
) -> Result<Json<LogEntry>, AdminError> {
    close_unsettled_log(&deps, &identity, &raw, true).await
}

/// 豁免未结算日志：只翻 `settled`，不改余额。
async fn waive_log(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(raw): Path<String>,
) -> Result<Json<LogEntry>, AdminError> {
    close_unsettled_log(&deps, &identity, &raw, false).await
}

/// 未结算闭环：`charge` 为 true 时补扣，否则豁免。
///
/// 路由层已要求 admin+；这里再按日志归属用户过一次 [`reject_user_management`]，
/// 使 admin 只能处理普通用户的行，不能动 root/其他 admin 的账。
async fn close_unsettled_log(
    deps: &AdminDeps,
    identity: &ManagementIdentity,
    raw: &str,
    charge: bool,
) -> Result<Json<LogEntry>, AdminError> {
    let id = parse_log_id(raw)?;
    let log = store::get_request_log(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("日志 {id} 不存在")))?;
    // 存量行的 user_id 为 0（迁移前无归属），此时无从判定越权，只允许 root 处理。
    if log.user_id == 0 {
        if identity.role() != ManagementRole::Root {
            return Err(AdminError::Forbidden);
        }
    } else {
        let owner = users::get_user(&deps.pool, log.user_id)
            .await
            .map_err(AdminError::Store)?
            .ok_or_else(|| AdminError::NotFound(format!("用户 {} 不存在", log.user_id)))?;
        reject_user_management(identity, &owner, None)?;
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    let outcome = if charge {
        store::settle_unsettled_log(&mut tx, id).await
    } else {
        store::waive_unsettled_log(&mut tx, id).await
    }
    .map_err(AdminError::Store)?;
    match outcome {
        store::UnsettledLogAction::NotFound => {
            return Err(AdminError::NotFound(format!("日志 {id} 不存在")));
        }
        store::UnsettledLogAction::AlreadySettled => {
            return Err(AdminError::Conflict(format!("日志 {id} 已结算")));
        }
        store::UnsettledLogAction::Closed => {}
    }
    tx.commit().await.map_err(db_err)?;
    let log = store::get_request_log(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("日志 {id} 不存在")))?;
    Ok(Json(LogEntry::from_store_log(log)))
}

/// `/system-logs` 查询参数：关键字、时间窗、级别与目标可选。
///
/// `level` / `target` 为逗号分隔列表；axum 标准 `Query` 不会把重复键收成 `Vec`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SystemLogQueryParams {
    keyword: Option<String>,
    from_created_at: Option<i64>,
    to_created_at: Option<i64>,
    level: Option<String>,
    target: Option<String>,
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
}

#[derive(Debug, Serialize)]
struct SystemLogPage {
    items: Vec<SystemLogEntry>,
    page: u64,
    page_size: u64,
    total: u64,
    /// 当前关键字/时间/级别下出现过的 target，供分面筛选。
    targets: Vec<String>,
}

/// 分页查询系统日志（缺省时间倒序）。
async fn query_system_logs(
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
            })
            .collect(),
        page: filter.page,
        page_size: filter.page_size,
        total: page.total,
        targets: page.targets,
    }))
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
    cost_usd_micros: i64,
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
}

/// 按模型的费用/请求分布。
#[derive(Debug, Serialize)]
struct ModelShareView {
    model: String,
    request_count: u64,
    cost_usd_micros: i64,
}

/// 按渠道的费用/请求分布。
#[derive(Debug, Serialize)]
struct ChannelShareView {
    channel: String,
    request_count: u64,
    cost_usd_micros: i64,
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
            })
            .collect(),
        by_model: stats
            .by_model
            .into_iter()
            .map(|share| ModelShareView {
                model: share.name,
                request_count: share.request_count,
                cost_usd_micros: share.cost_usd_micros,
            })
            .collect(),
        by_channel: stats
            .by_channel
            .into_iter()
            .map(|share| ChannelShareView {
                channel: share.name,
                request_count: share.request_count,
                cost_usd_micros: share.cost_usd_micros,
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
    /// 已结算的成功请求费用合计（micro-USD）。
    cost_usd_micros: i64,
    /// 全部请求日志的四分量 token 合计（含未结算行）。
    total_tokens: u64,
}

/// 只读全量累计：请求数 / 成功结算费用 / 四分量 token 合计。
async fn get_lifetime_stats(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
) -> Result<Json<LifetimeStatsView>, AdminError> {
    let stats = store::query_lifetime_stats(&deps.pool, identity.owner_scope())
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(LifetimeStatsView {
        request_count: stats.request_count,
        cost_usd_micros: stats.cost_usd_micros,
        total_tokens: stats.total_tokens,
    }))
}

// --- 渠道连通性探测 ---

/// 渠道探测请求：指定要测的模型（清单条目或别名映射的主模型名）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelProbeRequest {
    model: String,
}

/// 渠道探测结果：可达性、超时、状态码、延迟、错误摘要与上游 body 截断。
///
/// 探测不经令牌认证/计费、不落 `request_log`。超时沿用渠道 `timeout_ms`。
#[derive(Debug, Serialize)]
struct ChannelProbeResult {
    reachable: bool,
    timed_out: bool,
    status_code: Option<u16>,
    latency_ms: u64,
    error: Option<String>,
    upstream_body: Option<String>,
}

/// 解析探测出站模型名：清单里的主模型名、清单里的别名、或仅别名生效时的主模型名。
fn resolve_probe_model(channel: &Channel, requested: &str) -> Option<String> {
    if requested.is_empty() {
        return None;
    }
    if let Some(canonical) = channel.model_aliases.get(requested)
        && channel
            .models
            .iter()
            .any(|item| item == requested || item == canonical)
    {
        return Some(canonical.clone());
    }
    if channel.models.iter().any(|item| item == requested) {
        return Some(requested.to_string());
    }
    if channel.models.iter().any(|item| {
        channel
            .model_aliases
            .get(item)
            .is_some_and(|canonical| canonical == requested)
    }) {
        return Some(requested.to_string());
    }
    None
}

/// 向渠道 `base_url` 发一条最小非流式请求，按渠道协议编码，回报可达性。
async fn test_channel(
    State(deps): State<AdminDeps>,
    Path(raw_id): Path<String>,
    body: Result<Json<ChannelProbeRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ChannelProbeResult>, AdminError> {
    let Json(req) = body.map_err(AdminError::bad_body)?;
    let requested = req.model.trim();
    if requested.is_empty() {
        return Err(AdminError::InvalidBody("model 不能为空".to_string()));
    }
    let id = parse_channel_id(raw_id)?;
    let record = read_channel_record(&deps, id).await?;
    let channel = record.channel;
    reject_non_http_url(&channel.base_url)?;
    let model = resolve_probe_model(&channel, requested).ok_or_else(|| {
        AdminError::InvalidBody(format!("模型 {requested} 不在渠道 {id} 的清单中"))
    })?;
    let request = minimal_probe_request(&model);
    let mut warnings = Vec::new();
    let outbound = protocol::encode_request(&request, channel.protocol, &mut warnings);
    let upstream_url = format!(
        "{}{}",
        channel.base_url.trim_end_matches('/'),
        protocol::upstream_path(channel.protocol)
    );

    let started = Instant::now();
    let send = deps
        .client
        .post(&upstream_url)
        .timeout(Duration::from_millis(channel.timeout_ms))
        .apply_outbound_auth(&channel)
        .json(&outbound)
        .send()
        .await;

    let result = match send {
        Ok(resp) => {
            let status_code = resp.status().as_u16();
            let body_text = resp.text().await.unwrap_or_default();
            let error = if (200..300).contains(&status_code) {
                None
            } else {
                Some(probe_error_summary(&body_text, status_code))
            };
            let upstream_body = if body_text.is_empty() {
                None
            } else {
                Some(truncate_error(body_text))
            };
            ChannelProbeResult {
                reachable: true,
                timed_out: false,
                status_code: Some(status_code),
                latency_ms: elapsed_ms(started),
                error,
                upstream_body,
            }
        }
        Err(err) => ChannelProbeResult {
            reachable: false,
            timed_out: err.is_timeout(),
            status_code: None,
            latency_ms: elapsed_ms(started),
            error: Some(truncate_error(upstream_unreachable_message(&err))),
            upstream_body: None,
        },
    };
    Ok(Json(result))
}

/// 探测用最小非流式请求：单条 user 文本 + `max_tokens = 1`。
fn minimal_probe_request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentPart::Text {
                text: "ping".to_string(),
                provider_options: HashMap::new(),
            }],
            provider_options: HashMap::new(),
        }],
        stream: false,
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: Some(1),
        n: None,
        stop: Vec::new(),
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        response_format: None,
        tools: Vec::new(),
        tool_choice: None,
        provider_options: HashMap::new(),
    }
}

/// 从上游错误 body 提取可读摘要；非 JSON 时回退状态码描述。
fn probe_error_summary(body: &str, status: u16) -> String {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    truncate_error(super::http::upstream_error_message(&parsed, status))
}

/// 错误摘要截到 512 字节（按 UTF-8 字符边界），避免把整段上游 body 回给管理面。
fn truncate_error(mut message: String) -> String {
    const MAX: usize = 512;
    if message.len() > MAX {
        let end = message.floor_char_boundary(MAX);
        message.truncate(end);
    }
    message
}

/// `Instant` 经过的毫秒，夹到 `u64`。
fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

// --- 上游模型列表 ---

/// 上游模型列表的路径段（相对 `base_url`）：OpenAI 与 Anthropic 均为 `{base}/models`。
const UPSTREAM_MODELS_PATH: &str = "/models";

/// 拉取上游模型列表的草稿请求：仅含出站相关字段，渠道无需已保存。
///
/// 管理面新建渠道向导可在保存前同步模型；`timeout_ms` 沿用为本次请求超时。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpstreamModelsDraft {
    protocol: Protocol,
    base_url: String,
    api_key: String,
    timeout_ms: u64,
}

/// 上游模型列表响应：模型 id 数组，保持上游返回顺序，排序由调用方负责。
#[derive(Debug, Serialize)]
struct UpstreamModelsView {
    models: Vec<String>,
}

/// 按渠道草稿拉取上游模型列表：GET `{base_url}/models`。
///
/// OpenAI（chat/responses）与 Anthropic（messages）的模型列表同为
/// `{"data": [{"id": ...}]}` 形态，故统一解析；认证头按协议复用 `OutboundAuth`。
/// 上游不可达/非 2xx/响应形态非法均映射为 502 `upstream_error`。
async fn list_upstream_models(
    State(deps): State<AdminDeps>,
    body: Result<Json<UpstreamModelsDraft>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<UpstreamModelsView>, AdminError> {
    let Json(draft) = body.map_err(AdminError::bad_body)?;
    if draft.base_url.trim().is_empty() {
        return Err(AdminError::InvalidBody("base_url 不能为空".to_string()));
    }
    reject_non_http_url(&draft.base_url)?;
    if draft.api_key.trim().is_empty() {
        return Err(AdminError::InvalidBody("api_key 不能为空".to_string()));
    }
    if draft.timeout_ms < 1 {
        return Err(AdminError::InvalidBody("timeout_ms 不能小于 1".to_string()));
    }
    // 借临时 Channel 复用 OutboundAuth；与出站认证无关的字段取不影响认证的缺省值。
    let channel = Channel {
        name: String::new(),
        protocol: draft.protocol,
        base_url: draft.base_url,
        api_key: draft.api_key,
        models: Vec::new(),
        model_aliases: HashMap::new(),
        priority: 0,
        weight: 1,
        timeout_ms: draft.timeout_ms,
        max_retries: 0,
        enabled: true,
        model_group: crate::store::resources::DEFAULT_MODEL_GROUP.to_string(),
    };
    let url = format!(
        "{}{}",
        channel.base_url.trim_end_matches('/'),
        UPSTREAM_MODELS_PATH
    );
    let send = deps
        .client
        .get(&url)
        .timeout(Duration::from_millis(channel.timeout_ms))
        .apply_outbound_auth(&channel)
        .send()
        .await;
    let response = match send {
        Ok(response) => response,
        Err(err) => {
            return Err(AdminError::Upstream(truncate_error(
                upstream_unreachable_message(&err),
            )));
        }
    };
    let status_code = response.status().as_u16();
    let body_text = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status_code) {
        return Err(AdminError::Upstream(probe_error_summary(
            &body_text,
            status_code,
        )));
    }
    let models = parse_upstream_models(&body_text)?;
    Ok(Json(UpstreamModelsView { models }))
}

/// 从 `{"data": [{"id": ...}]}` 解析模型 id 数组；无 `id` 的条目跳过。
fn parse_upstream_models(body: &str) -> Result<Vec<String>, AdminError> {
    let parsed: Value = serde_json::from_str(body)
        .map_err(|_| AdminError::Upstream("上游响应不是合法 JSON".to_string()))?;
    let data = parsed
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| AdminError::Upstream("上游响应缺少 data 数组".to_string()))?;
    Ok(data
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect())
}

/// 出站请求发送失败的错误摘要：超时与连接失败措辞与探测保持一致。
fn upstream_unreachable_message(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "请求超时".to_string()
    } else {
        format!("上游不可达: {err}")
    }
}

// --- 写库 + 换快照公共原语 ---

/// 一次写操作的事务：开启 → 执行 → 提交。写失败（事务内报错）则回滚，库不动。
///
/// 各 CRUD 处理器内联调用（事务生命周期与处理器局部资源绑定，不宜用闭包抽象），
/// 此处只提供事务开启/提交的 sqlx 错误到 `AdminError` 的映射。
fn db_err(err: sqlx::Error) -> AdminError {
    AdminError::Store(StoreError::Query(err))
}

/// 提交后全量重载快照并原子替换，使新资源即时生效且与库一致。
///
/// 只有写事务提交成功才会走到这里；重载失败返回 500，此时库已提交而快照未换，
/// 属极端存储错误，交由运营重试。
async fn reload_and_swap(deps: &AdminDeps) -> Result<(), AdminError> {
    let new_snapshot = runtime::load_snapshot(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    runtime::swap_snapshot(&deps.snapshot, new_snapshot).await;
    Ok(())
}

/// 从库读回一条令牌记录；不存在返回 `NotFound`。
async fn read_token_record(
    deps: &AdminDeps,
    token_key: &str,
) -> Result<store::resources::TokenRecord, AdminError> {
    store::resources::get_token_record(&deps.pool, token_key)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("令牌 {token_key} 不存在")))
}

/// 从当前快照按 id 读回一条渠道记录；不存在返回 `NotFound`。
async fn read_channel_record(deps: &AdminDeps, id: i64) -> Result<ChannelRecord, AdminError> {
    let snapshot = deps.snapshot.read().await;
    snapshot
        .channels
        .iter()
        .find(|record| record.id == id)
        .cloned()
        .ok_or_else(|| AdminError::NotFound(format!("渠道 {id} 不存在")))
}

/// 从当前快照读回一条价格；不存在返回 `NotFound`。
async fn read_price(deps: &AdminDeps, channel_id: i64, model: &str) -> Result<Price, AdminError> {
    let snapshot = deps.snapshot.read().await;
    snapshot
        .price_for_channel(channel_id, model)
        .cloned()
        .ok_or_else(|| AdminError::NotFound(format!("价格 渠道 {channel_id} / {model} 不存在")))
}

/// 价格行引用的渠道必须存在。
fn reject_unknown_price_channel(
    snapshot: &crate::runtime::RuntimeSnapshot,
    channel_id: i64,
) -> Result<(), AdminError> {
    if snapshot
        .channels
        .iter()
        .any(|record| record.id == channel_id)
    {
        Ok(())
    } else {
        Err(AdminError::NotFound(format!("渠道 {channel_id} 不存在")))
    }
}

/// 价格模型名必须是该渠道已登记的可调用名（清单或别名 key）。
fn reject_unlisted_price_callable(
    snapshot: &crate::runtime::RuntimeSnapshot,
    channel_id: i64,
    model: &str,
) -> Result<(), AdminError> {
    let record = snapshot
        .channels
        .iter()
        .find(|record| record.id == channel_id)
        .ok_or_else(|| AdminError::NotFound(format!("渠道 {channel_id} 不存在")))?;
    if channel_lists_callable(&record.channel, model) {
        Ok(())
    } else {
        Err(AdminError::InvalidBody(format!(
            "模型 {model} 未在渠道 {channel_id} 的清单或别名中登记"
        )))
    }
}

/// 从当前快照读回一个模型组；不存在返回 `NotFound`。
async fn read_model_group(deps: &AdminDeps, name: &str) -> Result<ModelGroup, AdminError> {
    let snapshot = deps.snapshot.read().await;
    snapshot
        .model_groups
        .get(name)
        .cloned()
        .ok_or_else(|| AdminError::NotFound(format!("模型组 {name} 不存在")))
}

/// 从当前快照读回一个统一模型；不存在返回 `NotFound`。
async fn read_unified_model(deps: &AdminDeps, id: &str) -> Result<UnifiedModel, AdminError> {
    let snapshot = deps.snapshot.read().await;
    snapshot
        .unified_models
        .get(id)
        .cloned()
        .ok_or_else(|| AdminError::NotFound(format!("统一模型 {id} 不存在")))
}

/// 令牌绑定的组必须已存在。
async fn reject_unknown_group(deps: &AdminDeps, group: &str) -> Result<(), AdminError> {
    let snapshot = deps.snapshot.read().await;
    if snapshot.model_groups.contains_key(group) {
        Ok(())
    } else {
        Err(AdminError::NotFound(format!("模型组 {group} 不存在")))
    }
}

/// 普通用户只能绑到自己可用名单里的组；root/admin 只需组存在。
async fn reject_unassigned_group(
    deps: &AdminDeps,
    identity: &ManagementIdentity,
    group: &str,
) -> Result<(), AdminError> {
    if identity.role().at_least(ManagementRole::Admin) {
        return Ok(());
    }
    let snapshot = deps.snapshot.read().await;
    let Some(user) = snapshot.users.get(&identity.user_id()) else {
        return Err(AdminError::Forbidden);
    };
    if user.assigned_groups.contains(group) {
        Ok(())
    } else {
        Err(AdminError::InvalidBody("模型组不在可用名单中".to_string()))
    }
}

/// 渠道默认组：空白视为不自动入组。
fn normalize_channel_group(channel: &mut Channel) {
    channel.model_group = channel.model_group.trim().to_string();
    if channel.model_group.is_empty() {
        channel.model_group = crate::store::resources::DEFAULT_MODEL_GROUP.to_string();
    }
}

/// 把本次新加入渠道的可调用名钉进渠道默认组；`default` 不入组。
async fn enroll_channel_models(
    conn: &mut sqlx::SqliteConnection,
    channel_id: i64,
    previous: Option<&Channel>,
    next: &Channel,
) -> Result<(), AdminError> {
    if next.model_group == crate::store::resources::DEFAULT_MODEL_GROUP {
        return Ok(());
    }
    let added = crate::store::resources::newly_callable_names(previous, next);
    crate::store::resources::union_channel_callables_into_group(
        conn,
        &next.model_group,
        channel_id,
        &added,
    )
    .await
    .map_err(AdminError::Store)
}

// --- 输入校验 ---

/// 校验令牌字段：键/名非空、key 仅 ASCII 字母数字与 `._-`、累计上限非负。
fn validate_token(token: &Token) -> Result<(), AdminError> {
    reject_token_key_shape(token)?;
    if token.name.trim().is_empty() {
        return Err(AdminError::InvalidBody("name 不能为空".to_string()));
    }
    if let Some(limit) = token.limit_usd_micros
        && limit < 0
    {
        return Err(AdminError::InvalidBody(
            "limit_usd_micros 不能为负".to_string(),
        ));
    }
    if let Some(rpm) = token.rate_limit_rpm
        && i64::try_from(rpm).is_err()
    {
        return Err(AdminError::InvalidBody(
            "rate_limit_rpm 超出范围".to_string(),
        ));
    }
    if token.model_group.trim().is_empty() {
        return Err(AdminError::InvalidBody("model_group 不能为空".to_string()));
    }
    Ok(())
}

/// 路径/body 中的 key 须在查库前校验，以免非法字符被 404 盖住。
fn reject_token_key_shape(token: &Token) -> Result<(), AdminError> {
    if token.token_key.trim().is_empty() {
        return Err(AdminError::InvalidBody("token_key 不能为空".to_string()));
    }
    if !token_key_charset_ok(&token.token_key) {
        return Err(AdminError::InvalidBody(
            "token_key 仅允许 ASCII 字母、数字与 ._-".to_string(),
        ));
    }
    Ok(())
}

/// 令牌 key 字符集：系统生成的 `ks-` + 字母数字，以及测试/存量 ASCII key。
fn token_key_charset_ok(key: &str) -> bool {
    key.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// 规整模型组：名字去空白；钉渠道须已登记；统一 ID 须已存在；保序去重。
fn normalize_model_group(
    group: &mut ModelGroup,
    snapshot: &crate::runtime::RuntimeSnapshot,
) -> Result<(), AdminError> {
    group.name = group.name.trim().to_string();
    if group.name.is_empty() {
        return Err(AdminError::InvalidBody("name 不能为空".to_string()));
    }
    group.models = normalize_group_models(std::mem::take(&mut group.models), snapshot)?;
    Ok(())
}

/// 统一成员读视图：写契约仍是 `UnifiedMember`（不含 `available`）。
#[derive(Debug, Serialize)]
struct UnifiedMemberView {
    channel_id: i64,
    model: String,
    available: bool,
}

/// 统一模型读视图。
#[derive(Debug, Serialize)]
struct UnifiedModelView {
    id: String,
    models: Vec<UnifiedMemberView>,
    hide: bool,
}

fn unified_model_view(
    model: &UnifiedModel,
    snapshot: &crate::runtime::RuntimeSnapshot,
) -> UnifiedModelView {
    UnifiedModelView {
        id: model.id.clone(),
        models: model
            .models
            .iter()
            .map(|member| UnifiedMemberView {
                channel_id: member.channel_id,
                model: member.model.clone(),
                available: member_is_available(snapshot, member),
            })
            .collect(),
        hide: model.hide,
    }
}

fn member_is_available(snapshot: &crate::runtime::RuntimeSnapshot, member: &UnifiedMember) -> bool {
    snapshot.channels.iter().any(|record| {
        record.id == member.channel_id
            && record.channel.enabled
            && channel_lists_callable(&record.channel, &member.model)
    })
}

/// 规整统一模型：ID 去空白；成员须非空、钉在已有渠道的已登记名上、保序去重。
fn normalize_unified_model(
    model: &mut UnifiedModel,
    snapshot: &crate::runtime::RuntimeSnapshot,
) -> Result<(), AdminError> {
    model.id = model.id.trim().to_string();
    if model.id.is_empty() {
        return Err(AdminError::InvalidBody("id 不能为空".to_string()));
    }
    model.models = normalize_unified_members(std::mem::take(&mut model.models), snapshot)?;
    if model.models.is_empty() {
        return Err(AdminError::InvalidBody(
            "统一模型至少要有一个已登记成员".to_string(),
        ));
    }
    Ok(())
}

/// 规整统一成员：trim 模型名、拒绝空名、渠道必须存在且已登记该名、保序去重。
fn normalize_unified_members(
    members: Vec<UnifiedMember>,
    snapshot: &crate::runtime::RuntimeSnapshot,
) -> Result<Vec<UnifiedMember>, AdminError> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(members.len());
    for mut member in members {
        member.model = member.model.trim().to_string();
        if member.model.is_empty() {
            return Err(AdminError::InvalidBody("models 不能含空名".to_string()));
        }
        let Some(record) = snapshot
            .channels
            .iter()
            .find(|record| record.id == member.channel_id)
        else {
            return Err(AdminError::InvalidBody(format!(
                "渠道 {} 不存在",
                member.channel_id
            )));
        };
        if !channel_lists_callable(&record.channel, &member.model) {
            return Err(AdminError::InvalidBody(format!(
                "成员 {} 不是渠道 {} 的已登记模型",
                member.model, record.channel.name
            )));
        }
        if seen.insert((member.channel_id, member.model.clone())) {
            out.push(member);
        }
    }
    Ok(out)
}

/// 规整组名单：钉渠道须已登记；统一 ID 须已存在；保序去重。
fn normalize_group_models(
    models: Vec<GroupModel>,
    snapshot: &crate::runtime::RuntimeSnapshot,
) -> Result<Vec<GroupModel>, AdminError> {
    let mut seen: HashSet<(u8, i64, String)> = HashSet::new();
    let mut out = Vec::with_capacity(models.len());
    for entry in models {
        match entry {
            GroupModel::Unified { id } => {
                let id = id.trim().to_string();
                if id.is_empty() {
                    return Err(AdminError::InvalidBody("models 不能含空名".to_string()));
                }
                if !snapshot.unified_models.contains_key(&id) {
                    return Err(AdminError::InvalidBody(format!("统一模型 {id} 不存在")));
                }
                if seen.insert((0, 0, id.clone())) {
                    out.push(GroupModel::Unified { id });
                }
            }
            GroupModel::Source { channel_id, model } => {
                let model = model.trim().to_string();
                if model.is_empty() {
                    return Err(AdminError::InvalidBody("models 不能含空名".to_string()));
                }
                let Some(record) = snapshot
                    .channels
                    .iter()
                    .find(|record| record.id == channel_id)
                else {
                    return Err(AdminError::InvalidBody(format!("渠道 {channel_id} 不存在")));
                };
                if !channel_lists_callable(&record.channel, &model) {
                    return Err(AdminError::InvalidBody(format!(
                        "成员 {} 不是渠道 {} 的已登记模型",
                        model, record.channel.name
                    )));
                }
                if seen.insert((1, channel_id, model.clone())) {
                    out.push(GroupModel::Source { channel_id, model });
                }
            }
        }
    }
    Ok(out)
}

/// 校验渠道字段：名/上游地址/密钥非空，权重至少为 1。
fn validate_channel(channel: &Channel) -> Result<(), AdminError> {
    if channel.name.trim().is_empty() {
        return Err(AdminError::InvalidBody("name 不能为空".to_string()));
    }
    if channel.base_url.trim().is_empty() {
        return Err(AdminError::InvalidBody("base_url 不能为空".to_string()));
    }
    reject_non_http_url(&channel.base_url)?;
    if channel.api_key.trim().is_empty() {
        return Err(AdminError::InvalidBody("api_key 不能为空".to_string()));
    }
    if channel.weight < 1 {
        return Err(AdminError::InvalidBody("weight 不能小于 1".to_string()));
    }
    Ok(())
}

/// 探测与渠道草稿仅允许 http/https，避免 `file://` 等 scheme 打到本机文件。
fn reject_non_http_url(raw: &str) -> Result<(), AdminError> {
    let parsed = reqwest::Url::parse(raw.trim())
        .map_err(|_| AdminError::InvalidBody("base_url 不是合法绝对 URL".to_string()))?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        other => Err(AdminError::InvalidBody(format!(
            "探测 URL 仅支持 http/https，收到 {other}"
        ))),
    }
}

/// 与 Web UI `maskTokenKey` 同款：按 Unicode 标量计长度，大于 16 时保留前后 8 个字符。
fn mask_token_key(key: &str) -> String {
    const EDGE: usize = 8;
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= EDGE * 2 {
        key.to_string()
    } else {
        let prefix: String = chars[..EDGE].iter().collect();
        let suffix: String = chars[chars.len() - EDGE..].iter().collect();
        format!("{prefix}******{suffix}")
    }
}

/// 同一渠道上非恒等别名的 key 与 value 都在 `models` 里则拒绝：该名既是独立主模型，又被改写成另一个已登记主模型。
fn reject_alias_occupancy(channel: &Channel) -> Result<(), AdminError> {
    let listed: HashSet<&String> = channel.models.iter().collect();
    for (alias, canonical) in &channel.model_aliases {
        if alias == canonical {
            continue;
        }
        if listed.contains(alias) && listed.contains(canonical) {
            return Err(AdminError::Conflict(format!(
                "别名 {alias} 占用清单中的同名主模型（指向 {canonical}）。同一渠道上一个名字不能既是独立主模型，又是指向其他主模型的别名"
            )));
        }
    }
    Ok(())
}

/// 保存后的启用渠道集合若同一别名指向不同真名，拒绝并提示改用统一模型。
fn reject_alias_conflict(
    existing: &[ChannelRecord],
    incoming: &Channel,
    replace_id: Option<i64>,
) -> Result<(), AdminError> {
    let mut channels: Vec<&Channel> = Vec::with_capacity(existing.len() + 1);
    for record in existing {
        if replace_id != Some(record.id) {
            channels.push(&record.channel);
        }
    }
    channels.push(incoming);
    match super::routing::find_alias_conflict(&channels) {
        Some(conflict) => Err(AdminError::Conflict(format!(
            "别名 {} 在启用渠道间指向不同真名（{} 与 {}）。一对多请到模型页「归一化」（Tab 2）用统一模型，不要用别名",
            conflict.alias, conflict.existing, conflict.conflicting
        ))),
        None => Ok(()),
    }
}

/// 保存后若未隐藏的统一模型 ID 与已登记模型/别名同名，拒绝。
///
/// `incoming_channel` / `replace_id` 用于渠道保存：用新定义替换指定 id 后再算已登记名。
/// `incoming_unified` 用于统一模型保存：用新定义替换同 id 后再检查。
fn reject_unhidden_unified_collision<'a>(
    existing: &'a [ChannelRecord],
    incoming_channel: Option<&'a Channel>,
    replace_id: Option<i64>,
    unified_models: impl IntoIterator<Item = &'a UnifiedModel>,
    incoming_unified: Option<&'a UnifiedModel>,
) -> Result<(), AdminError> {
    let mut channels: Vec<&Channel> = Vec::with_capacity(existing.len() + 1);
    for record in existing {
        if replace_id != Some(record.id) {
            channels.push(&record.channel);
        }
    }
    if let Some(channel) = incoming_channel {
        channels.push(channel);
    }
    let registered = crate::store::resources::registered_callable_names(channels);

    let mut models: Vec<&UnifiedModel> = Vec::new();
    for model in unified_models {
        if incoming_unified.is_none_or(|incoming| incoming.id != model.id) {
            models.push(model);
        }
    }
    if let Some(model) = incoming_unified {
        models.push(model);
    }
    for model in models {
        if crate::store::resources::unhidden_unified_id_collides(&model.id, model.hide, &registered)
        {
            return Err(AdminError::Conflict(format!(
                "统一模型 {} 与已登记模型或别名同名且未隐藏。开隐藏则该名只表示统一模型，否则请换 ID",
                model.id
            )));
        }
    }
    Ok(())
}

/// 校验价格字段：四档单价均非负。
fn validate_price(price: &Price) -> Result<(), AdminError> {
    if price.input_micros < 0 || price.output_micros < 0 {
        return Err(AdminError::InvalidBody(
            "input/output 单价不能为负".to_string(),
        ));
    }
    if matches!(price.cache_read_micros, Some(value) if value < 0) {
        return Err(AdminError::InvalidBody(
            "cache_read 单价不能为负".to_string(),
        ));
    }
    if matches!(price.cache_write_micros, Some(value) if value < 0) {
        return Err(AdminError::InvalidBody(
            "cache_write 单价不能为负".to_string(),
        ));
    }
    Ok(())
}

/// 管理面错误：全部以统一结构化 JSON 返回给调用方。
enum AdminError {
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
    fn bad_body(rejection: axum::extract::rejection::JsonRejection) -> Self {
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
            AdminError::Store(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_error",
                err.to_string(),
            ),
        };
        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}
