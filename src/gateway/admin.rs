//! 管理 API：独立管理监听 + 静态 admin key 认证 + 资源 CRUD。
//!
//! 管理面与协议面物理隔离：配置文件中可选的管理监听地址（`admin_listen`）配置了
//! 才启动，未配置即管理面整体关闭，协议监听不注册任何管理路由。所有端点以静态
//! `admin_key`（Bearer）认证。
//!
//! 资源 CRUD（令牌/渠道/价格）：写库（事务）→ 原子替换内存快照 → 返回变更后
//! 资源；写失败则库与快照都不动。非法输入返回结构化错误，写操作返回变更后资源。
//! 设置、余额调整与请求日志查询属 04 票，本模块不涉及。

use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use serde_json::json;
use sqlx::SqlitePool;

use crate::{
    runtime,
    store::StoreError,
    store::resources::{Channel, Price, Token},
};

/// 管理面依赖：存储连接池 + 运行时快照句柄（写后原子替换）。
#[derive(Clone)]
struct AdminDeps {
    pool: SqlitePool,
    snapshot: crate::runtime::SnapshotHandle,
}

/// 组装管理面路由：三组资源 CRUD，全部挂在 admin key 认证中间件之后。
///
/// 路由以领域词直出（`/tokens`、`/channels`、`/prices`），集合端点 GET 列出、
/// POST 新建；单资源端点 PUT 整体替换、DELETE 删除。
pub fn router(
    pool: SqlitePool,
    snapshot: crate::runtime::SnapshotHandle,
    admin_key: String,
) -> Router {
    let deps = AdminDeps { pool, snapshot };
    Router::new()
        .route("/tokens", get(list_tokens).post(create_token))
        .route(
            "/tokens/{token_key}",
            put(update_token).delete(delete_token),
        )
        .route("/channels", get(list_channels).post(create_channel))
        .route(
            "/channels/{name}",
            put(update_channel).delete(delete_channel),
        )
        .route("/prices", get(list_prices).post(create_price))
        .route("/prices/{model}", put(update_price).delete(delete_price))
        .route_layer(middleware::from_fn_with_state(admin_key, admin_auth))
        .with_state(deps)
}

/// admin key 认证中间件：请求需带 `Authorization: Bearer <admin_key>`，否则 401。
///
/// 管理面与协议面监听隔离，本中间件只作用于管理路由；认证失败返回结构化错误。
async fn admin_auth(State(admin_key): State<String>, request: Request, next: Next) -> Response {
    let authorized = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        == Some(admin_key.as_str());
    if authorized {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(
                json!({ "error": { "code": "unauthorized", "message": "无效或缺失的 admin key" } }),
            ),
        )
            .into_response()
    }
}

// --- 令牌 ---

/// 列出全部令牌（按 `token_key` 排序，保证确定性）。
async fn list_tokens(State(deps): State<AdminDeps>) -> Result<Json<Vec<Token>>, AdminError> {
    let snapshot = deps.snapshot.read().await;
    let mut tokens: Vec<Token> = snapshot.tokens.values().cloned().collect();
    tokens.sort_by(|a, b| a.token_key.cmp(&b.token_key));
    Ok(Json(tokens))
}

/// 新建令牌：已存在则冲突（409），否则写库 + 换快照 + 返回新令牌。
async fn create_token(
    State(deps): State<AdminDeps>,
    body: Result<Json<Token>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<Token>), AdminError> {
    let token = body.map_err(AdminError::bad_body)?;
    validate_token(&token)?;
    {
        let snapshot = deps.snapshot.read().await;
        if snapshot.tokens.contains_key(&token.token_key) {
            return Err(AdminError::Conflict(format!(
                "令牌 {} 已存在",
                token.token_key
            )));
        }
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_token(&mut tx, &token)
        .await
        .map_err(AdminError::Store)?;
    // 令牌定义与余额分离存储：新建时同步建零额余额行，使令牌可被后续充值
    // （`adjust_balance` 的 UPDATE 只改已有行，缺行会报 MissingToken），否则
    // 新令牌永远无法被运营充值使用。
    crate::store::ensure_token_balance(
        &mut tx,
        &token.token_key,
        0.0,
        super::logging::unix_millis(),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let created = read_token(&deps, &token.token_key).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// 整体替换令牌（按路径 `token_key`，路径权威）：写库 + 换快照 + 返回新令牌。
async fn update_token(
    State(deps): State<AdminDeps>,
    Path(token_key): Path<String>,
    body: Result<Json<Token>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Token>, AdminError> {
    let mut token = body.map_err(AdminError::bad_body)?;
    token.token_key = token_key;
    validate_token(&token)?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_token(&mut tx, &token)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_token(&deps, &token.token_key).await?;
    Ok(Json(updated))
}

/// 删除令牌：不存在则 404，否则删除并返回被删令牌。
///
/// 同事务清理余额行：残留的余额行会让同 key 重建的令牌复活旧余额。
async fn delete_token(
    State(deps): State<AdminDeps>,
    Path(token_key): Path<String>,
) -> Result<Json<Token>, AdminError> {
    let deleted = read_token(&deps, &token_key).await?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::delete_token(&mut tx, &token_key)
        .await
        .map_err(AdminError::Store)?;
    crate::store::delete_token_balance(&mut tx, &token_key)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(deleted))
}

// --- 渠道 ---

/// 列出全部渠道（保持快照顺序）。
async fn list_channels(State(deps): State<AdminDeps>) -> Result<Json<Vec<Channel>>, AdminError> {
    let snapshot = deps.snapshot.read().await;
    Ok(Json(snapshot.channels.clone()))
}

/// 新建渠道：同名已存在则冲突（409），否则写库 + 换快照 + 返回新渠道。
async fn create_channel(
    State(deps): State<AdminDeps>,
    body: Result<Json<Channel>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<Channel>), AdminError> {
    let channel = body.map_err(AdminError::bad_body)?;
    validate_channel(&channel)?;
    {
        let snapshot = deps.snapshot.read().await;
        if snapshot.channels.iter().any(|c| c.name == channel.name) {
            return Err(AdminError::Conflict(format!(
                "渠道 {} 已存在",
                channel.name
            )));
        }
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_channel(&mut tx, &channel)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let created = read_channel(&deps, &channel.name).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// 整体替换渠道（按路径 `name`，路径权威）：写库 + 换快照 + 返回新渠道。
async fn update_channel(
    State(deps): State<AdminDeps>,
    Path(name): Path<String>,
    body: Result<Json<Channel>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Channel>, AdminError> {
    let mut channel = body.map_err(AdminError::bad_body)?;
    channel.name = name;
    validate_channel(&channel)?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_channel(&mut tx, &channel)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_channel(&deps, &channel.name).await?;
    Ok(Json(updated))
}

/// 删除渠道：不存在则 404，否则删除并返回被删渠道。
async fn delete_channel(
    State(deps): State<AdminDeps>,
    Path(name): Path<String>,
) -> Result<Json<Channel>, AdminError> {
    let deleted = read_channel(&deps, &name).await?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::delete_channel(&mut tx, &name)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(deleted))
}

// --- 价格 ---

/// 列出全部价格（按 `model` 排序，保证确定性）。
async fn list_prices(State(deps): State<AdminDeps>) -> Result<Json<Vec<Price>>, AdminError> {
    let snapshot = deps.snapshot.read().await;
    let mut prices: Vec<Price> = snapshot.prices.values().cloned().collect();
    prices.sort_by(|a, b| a.model.cmp(&b.model));
    Ok(Json(prices))
}

/// 新建价格：同模型已存在则冲突（409），否则写库 + 换快照 + 返回新价格。
async fn create_price(
    State(deps): State<AdminDeps>,
    body: Result<Json<Price>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<Price>), AdminError> {
    let price = body.map_err(AdminError::bad_body)?;
    validate_price(&price)?;
    {
        let snapshot = deps.snapshot.read().await;
        if snapshot.prices.contains_key(&price.model) {
            return Err(AdminError::Conflict(format!("价格 {} 已存在", price.model)));
        }
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_price(&mut tx, &price)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let created = read_price(&deps, &price.model).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// 整体替换价格（按路径 `model`，路径权威）：写库 + 换快照 + 返回新价格。
async fn update_price(
    State(deps): State<AdminDeps>,
    Path(model): Path<String>,
    body: Result<Json<Price>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Price>, AdminError> {
    let mut price = body.map_err(AdminError::bad_body)?;
    price.model = model;
    validate_price(&price)?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_price(&mut tx, &price)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_price(&deps, &price.model).await?;
    Ok(Json(updated))
}

/// 删除价格：不存在则 404，否则删除并返回被删价格。
async fn delete_price(
    State(deps): State<AdminDeps>,
    Path(model): Path<String>,
) -> Result<Json<Price>, AdminError> {
    let deleted = read_price(&deps, &model).await?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::delete_price(&mut tx, &model)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(deleted))
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

/// 从当前快照读回一个令牌；不存在返回 `NotFound`。
async fn read_token(deps: &AdminDeps, token_key: &str) -> Result<Token, AdminError> {
    let snapshot = deps.snapshot.read().await;
    snapshot
        .tokens
        .get(token_key)
        .cloned()
        .ok_or_else(|| AdminError::NotFound(format!("令牌 {token_key} 不存在")))
}

/// 从当前快照读回一个渠道；不存在返回 `NotFound`。
async fn read_channel(deps: &AdminDeps, name: &str) -> Result<Channel, AdminError> {
    let snapshot = deps.snapshot.read().await;
    snapshot
        .channels
        .iter()
        .find(|channel| channel.name == name)
        .cloned()
        .ok_or_else(|| AdminError::NotFound(format!("渠道 {name} 不存在")))
}

/// 从当前快照读回一个价格；不存在返回 `NotFound`。
async fn read_price(deps: &AdminDeps, model: &str) -> Result<Price, AdminError> {
    let snapshot = deps.snapshot.read().await;
    snapshot
        .prices
        .get(model)
        .cloned()
        .ok_or_else(|| AdminError::NotFound(format!("价格 {model} 不存在")))
}

// --- 输入校验 ---

/// 校验令牌字段：键/名非空、累计上限非负。
fn validate_token(token: &Token) -> Result<(), AdminError> {
    if token.token_key.trim().is_empty() {
        return Err(AdminError::InvalidBody("token_key 不能为空".to_string()));
    }
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
    Ok(())
}

/// 校验渠道字段：名/上游地址/密钥非空。
fn validate_channel(channel: &Channel) -> Result<(), AdminError> {
    if channel.name.trim().is_empty() {
        return Err(AdminError::InvalidBody("name 不能为空".to_string()));
    }
    if channel.base_url.trim().is_empty() {
        return Err(AdminError::InvalidBody("base_url 不能为空".to_string()));
    }
    if channel.api_key.trim().is_empty() {
        return Err(AdminError::InvalidBody("api_key 不能为空".to_string()));
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
    /// 资源不存在（404）。
    NotFound(String),
    /// 新建时资源已存在（409）。
    Conflict(String),
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
            AdminError::NotFound(msg) => (StatusCode::NOT_FOUND, "not_found", msg),
            AdminError::Conflict(msg) => (StatusCode::CONFLICT, "conflict", msg),
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
