//! 管理 API：独立管理监听 + 静态 admin key 认证 + 资源 CRUD。
//!
//! 管理面与协议面物理隔离：配置文件中可选的管理监听地址（`admin_listen`）配置了
//! 才启动，未配置即管理面整体关闭，协议监听不注册任何管理路由。所有端点以静态
//! `admin_key`（Bearer）认证。
//!
//! 资源 CRUD（令牌/渠道/价格）：写库（事务）→ 原子替换内存快照 → 返回变更后
//! 资源；写失败则库与快照都不动。非法输入返回结构化错误，写操作返回变更后资源。
//! 另承载设置读写（`/settings`）、令牌余额相对调整（`/tokens/{key}/balance`）与
//! 请求日志分页查询（`/logs`）。

use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use base64::prelude::{BASE64_STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;

use crate::{
    runtime, store,
    store::StoreError,
    store::resources::{Channel, Price, Settings, Token},
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
        .route("/tokens/{token_key}/balance", post(adjust_token_balance))
        .route("/channels", get(list_channels).post(create_channel))
        .route(
            "/channels/{name}",
            put(update_channel).delete(delete_channel),
        )
        .route("/prices", get(list_prices).post(create_price))
        .route("/prices/{model}", put(update_price).delete(delete_price))
        .route("/settings", get(get_settings).put(update_settings))
        .route("/logs", get(query_logs))
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

// --- 设置 ---

/// 读当前运行时设置：从内存快照直接取（与请求路径读同一份真值）。
async fn get_settings(State(deps): State<AdminDeps>) -> Result<Json<Settings>, AdminError> {
    let updated = read_settings(&deps).await?;
    Ok(Json(updated))
}

/// 整体更新运行时设置：写库 → 换快照 → 返回变更后设置。
///
/// 设置变更后经快照原子替换即时生效：入站请求体上限的变更立刻拦截超限请求，
/// full_body 开关的变更立刻作用于后续请求的日志落库。
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

/// 校验设置字段：入站请求体上限须为正（0 会拒绝一切请求，属运营误配）。
fn validate_settings(settings: &Settings) -> Result<(), AdminError> {
    if settings.max_request_bytes == 0 {
        return Err(AdminError::InvalidBody(
            "max_request_bytes 必须大于 0".to_string(),
        ));
    }
    Ok(())
}

/// 从当前快照读回设置。
async fn read_settings(deps: &AdminDeps) -> Result<Settings, AdminError> {
    let snapshot = deps.snapshot.read().await;
    Ok(Settings {
        full_body: snapshot.full_body,
        max_request_bytes: snapshot.max_request_bytes,
    })
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

/// 相对调整令牌余额（充值/扣减），库内原子完成，返回调整后余额。
///
/// 不动令牌定义（修改令牌属性不重置余额）；余额独立存 `token_balance` 表，不参与
/// 快照替换。令牌不存在返回 404；余额行缺失先在事务内建零额行再调整。
async fn adjust_token_balance(
    State(deps): State<AdminDeps>,
    Path(token_key): Path<String>,
    body: Result<Json<BalanceAdjustment>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<BalanceView>, AdminError> {
    let adjustment = body.map_err(AdminError::bad_body)?;
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
    model: Option<String>,
    from_created_at: Option<i64>,
    to_created_at: Option<i64>,
    page: Option<u64>,
    page_size: Option<u64>,
}

/// 请求日志条目 wire 契约：完整 body 以 base64 编码（二进制安全）。
#[derive(Debug, Serialize)]
struct LogEntry {
    id: i64,
    created_at: i64,
    token_name: String,
    token_key: String,
    inbound_protocol: String,
    model: String,
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
    request_body: Option<String>,
    response_body: Option<String>,
}

impl LogEntry {
    /// 从存储行构造 wire 条目；完整 body 字节以 base64 编码。
    fn from_store_log(log: store::RequestLog) -> Self {
        Self {
            id: log.id,
            created_at: log.created_at,
            token_name: log.token_name,
            token_key: log.token_key,
            inbound_protocol: log.inbound_protocol,
            model: log.model,
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
}

/// 分页查询请求日志（时间倒序），按令牌/模型/时间范围过滤，只读。
///
/// `page`/`page_size` 缺省 1/20；`page_size` 上限 200（由存储层夹取），响应的
/// `page`/`page_size` 反映实际采用值。非法查询参数（如非数字页码）返回结构化 400。
async fn query_logs(
    State(deps): State<AdminDeps>,
    query: Result<Query<LogQueryParams>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<LogPage>, AdminError> {
    let params = query
        .map_err(|rejection| AdminError::InvalidBody(format!("查询参数非法: {rejection}")))?
        .0;
    let mut filter =
        store::RequestLogQuery::new(params.page.unwrap_or(1), params.page_size.unwrap_or(20));
    filter.token_key = params.token_key;
    filter.model = params.model;
    filter.from_created_at = params.from_created_at;
    filter.to_created_at = params.to_created_at;

    let rows = store::query_request_logs(&deps.pool, &filter)
        .await
        .map_err(AdminError::Store)?;
    let total = store::count_request_logs(&deps.pool, &filter)
        .await
        .map_err(AdminError::Store)?;
    let items = rows.into_iter().map(LogEntry::from_store_log).collect();
    Ok(Json(LogPage {
        items,
        page: filter.page,
        page_size: filter.page_size,
        total,
    }))
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
