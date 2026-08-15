//! 管理 API：独立管理监听 + 静态 admin key 认证 + 资源 CRUD + 嵌入式 Web UI。
//!
//! 管理面与协议面物理隔离：配置文件中可选的管理监听地址（`admin_listen`）配置了
//! 才启动，未配置即管理面整体关闭，协议监听不注册任何管理路由。所有资源 API
//! 以静态 `admin_key`（Bearer）认证；`webui/dist` 静态资源与 SPA 回退挂在 fallback
//! 上、免认证。产物缺失时管理面退化为纯 API。
//!
//! 资源 CRUD（令牌/渠道/价格）：写库（事务）→ 原子替换内存快照 → 返回变更后
//! 资源；写失败则库与快照都不动。非法输入返回结构化错误，写操作返回变更后资源。
//! 另承载设置读写（`/settings`）、令牌余额相对调整（`/tokens/{key}/balance`）、
//! 请求日志分页查询（`/logs`）、只读聚合（`/stats`、`/stats/lifetime`）、渠道连通性探测
//! （`/channels/{id}/test`）与按渠道草稿拉取上游模型列表（`/channels/models`）。

use std::collections::HashMap;
use std::time::{Duration, Instant};

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
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::{
    config::Protocol,
    core::ir::{ChatRequest, ContentPart, Message, Role},
    runtime, store,
    store::StoreError,
    store::resources::{Channel, ChannelRecord, Price, Settings, Token},
};

use super::http::OutboundAuth;
use super::protocol;

/// 管理面依赖：存储连接池 + 运行时快照句柄（写后原子替换）+ 出站 HTTP 客户端。
#[derive(Clone)]
struct AdminDeps {
    pool: SqlitePool,
    snapshot: crate::runtime::SnapshotHandle,
    client: reqwest::Client,
}

/// 组装管理面路由：资源 CRUD 挂在 admin key 认证中间件之后；静态 UI 为 fallback。
///
/// 路由以领域词直出（`/tokens`、`/channels`、`/prices`），集合端点 GET 列出、
/// POST 新建；单资源端点 PUT 整体替换、DELETE 删除。UI 静态资源与未匹配的 GET
/// 深链不经认证中间件。
pub fn router(
    pool: SqlitePool,
    snapshot: crate::runtime::SnapshotHandle,
    admin_key: String,
) -> Router {
    // 未配置自定义 TLS/DNS 时，rustls 后端下 `ClientBuilder::build` 只在
    // builder 事先记下错误时失败；本路径未设置会失败的选项。
    let client = reqwest::Client::builder()
        .build()
        .expect("未配置会失败的 ClientBuilder 选项，rustls 客户端应能构建");
    let deps = AdminDeps {
        pool,
        snapshot,
        client,
    };
    Router::new()
        .route("/tokens", get(list_tokens).post(create_token))
        .route(
            "/tokens/{token_key}",
            put(update_token).delete(delete_token),
        )
        .route("/tokens/{token_key}/balance", post(adjust_token_balance))
        .route("/channels", get(list_channels).post(create_channel))
        .route("/channels/models", post(list_upstream_models))
        .route("/channels/{id}", put(update_channel).delete(delete_channel))
        .route("/channels/{id}/test", post(test_channel))
        .route("/prices", get(list_prices).post(create_price))
        .route("/prices/{model}", put(update_price).delete(delete_price))
        .route("/settings", get(get_settings).put(update_settings))
        .route("/logs", get(query_logs))
        .route("/stats", get(get_stats))
        .route("/stats/lifetime", get(get_lifetime_stats))
        .route_layer(middleware::from_fn_with_state(admin_key, admin_auth))
        // fallback 不走 route_layer：静态资源与 SPA 回退免认证；API 路由仍受中间件保护。
        .fallback(super::webui::serve)
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

/// 令牌读响应 wire 契约：定义字段 + 生命周期元数据（写契约仍是 `Token`）。
#[derive(Debug, Serialize)]
struct TokenView {
    token_key: String,
    name: String,
    limit_usd_micros: Option<i64>,
    enabled: bool,
    created_at: i64,
    last_used_at: Option<i64>,
}

impl TokenView {
    /// 从存储层记录构造 wire 视图。
    fn from_record(record: store::resources::TokenRecord) -> Self {
        Self {
            token_key: record.token.token_key,
            name: record.token.name,
            limit_usd_micros: record.token.limit_usd_micros,
            enabled: record.token.enabled,
            created_at: record.created_at,
            last_used_at: record.last_used_at,
        }
    }
}

/// 列出全部令牌（按 `token_key` 排序，保证确定性）。
///
/// 直接读库而非快照：`last_used_at` 随请求路径刷新、不进快照，列表需要库内最新值。
async fn list_tokens(State(deps): State<AdminDeps>) -> Result<Json<Vec<TokenView>>, AdminError> {
    let mut records = store::resources::list_token_records(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    records.sort_by(|a, b| a.token.token_key.cmp(&b.token.token_key));
    Ok(Json(
        records.into_iter().map(TokenView::from_record).collect(),
    ))
}

/// 新建令牌请求契约：不接受指定 key，key 由系统高熵生成。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenCreate {
    name: String,
    limit_usd_micros: Option<i64>,
    enabled: bool,
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
        enabled: create.enabled,
    };
    validate_token(&token)?;
    let now = super::logging::unix_millis();
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_token(&mut tx, &token, now)
        .await
        .map_err(AdminError::Store)?;
    // 令牌定义与余额分离存储：新建时同步建零额余额行，使令牌可被后续充值
    // （`adjust_balance` 的 UPDATE 只改已有行，缺行会报 MissingToken），否则
    // 新令牌永远无法被运营充值使用。
    crate::store::ensure_token_balance(&mut tx, &token.token_key, 0.0, now)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let created = read_token_record(&deps, &token.token_key).await?;
    Ok((StatusCode::CREATED, Json(TokenView::from_record(created))))
}

/// 整体替换令牌（按路径 `token_key`，路径权威）：写库 + 换快照 + 返回新令牌。
///
/// upsert 的冲突分支不触碰创建时间与最后使用时间，属性编辑不重置生命周期元数据。
async fn update_token(
    State(deps): State<AdminDeps>,
    Path(token_key): Path<String>,
    body: Result<Json<Token>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<TokenView>, AdminError> {
    let mut token = body.map_err(AdminError::bad_body)?;
    token.token_key = token_key;
    validate_token(&token)?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_token(&mut tx, &token, super::logging::unix_millis())
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_token_record(&deps, &token.token_key).await?;
    Ok(Json(TokenView::from_record(updated)))
}

/// 删除令牌：不存在则 404，否则删除并返回被删令牌。
///
/// 同事务先删余额、后删令牌定义：`token_balance.token_key` 外键指向 `tokens`
/// （ON DELETE CASCADE 兜底）；显式清理余额行，不依赖级联语义。余额行残留会
/// 让同 key 重建的令牌复活旧余额。
async fn delete_token(
    State(deps): State<AdminDeps>,
    Path(token_key): Path<String>,
) -> Result<Json<TokenView>, AdminError> {
    let deleted = read_token_record(&deps, &token_key).await?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::delete_token_balance(&mut tx, &token_key)
        .await
        .map_err(AdminError::Store)?;
    crate::store::resources::delete_token(&mut tx, &token_key)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(TokenView::from_record(deleted)))
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
    let channel = body.map_err(AdminError::bad_body)?;
    validate_channel(&channel)?;
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
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    let id = crate::store::resources::insert_channel(&mut tx, &channel)
        .await
        .map_err(AdminError::Store)?;
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
    let channel = body.map_err(AdminError::bad_body)?;
    validate_channel(&channel)?;
    {
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
    }
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::update_channel(&mut tx, id, &channel)
        .await
        .map_err(AdminError::Store)?;
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
    keyword: Option<String>,
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

/// 分页查询请求日志（时间倒序），按令牌/模型/综合关键字/时间范围过滤，只读。
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
    filter.keyword = params.keyword.filter(|keyword| !keyword.trim().is_empty());
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
    token_count: u64,
    channel_count: u64,
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
    query: Result<Query<StatsQueryParams>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<StatsView>, AdminError> {
    let params = query
        .map_err(|rejection| AdminError::InvalidBody(format!("查询参数非法: {rejection}")))?
        .0;
    let days = store::clamp_stats_days(params.days);
    let stats = store::query_stats(&deps.pool, days)
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
#[derive(Debug, Serialize)]
struct LifetimeStatsView {
    request_count: u64,
    cost_usd_micros: i64,
    total_tokens: u64,
}

/// 只读全量累计：请求数 / 成功结算费用 / 四分量 token 合计。
async fn get_lifetime_stats(
    State(deps): State<AdminDeps>,
) -> Result<Json<LifetimeStatsView>, AdminError> {
    let stats = store::query_lifetime_stats(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(LifetimeStatsView {
        request_count: stats.request_count,
        cost_usd_micros: stats.cost_usd_micros,
        total_tokens: stats.total_tokens,
    }))
}

// --- 渠道连通性探测 ---

/// 渠道探测结果：可达性、状态码、延迟、失败时的错误摘要。
///
/// 探测不经令牌认证/计费、不落 `request_log`。超时沿用渠道 `timeout_ms`。
#[derive(Debug, Serialize)]
struct ChannelProbeResult {
    reachable: bool,
    status_code: Option<u16>,
    latency_ms: u64,
    error: Option<String>,
}

/// 向渠道 `base_url` 发一条最小非流式请求，按渠道协议编码，回报可达性。
async fn test_channel(
    State(deps): State<AdminDeps>,
    Path(raw_id): Path<String>,
) -> Result<Json<ChannelProbeResult>, AdminError> {
    let id = parse_channel_id(raw_id)?;
    let record = read_channel_record(&deps, id).await?;
    let channel = record.channel;
    let model = channel
        .models
        .first()
        .ok_or_else(|| AdminError::InvalidBody(format!("渠道 {id} 未配置模型，无法探测")))?;
    let request = minimal_probe_request(model);
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
            ChannelProbeResult {
                reachable: true,
                status_code: Some(status_code),
                latency_ms: elapsed_ms(started),
                error,
            }
        }
        Err(err) => ChannelProbeResult {
            reachable: false,
            status_code: None,
            latency_ms: elapsed_ms(started),
            error: Some(truncate_error(upstream_unreachable_message(&err))),
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

/// 校验渠道字段：名/上游地址/密钥非空，权重至少为 1。
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
    if channel.weight < 1 {
        return Err(AdminError::InvalidBody("weight 不能小于 1".to_string()));
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
            AdminError::NotFound(msg) => (StatusCode::NOT_FOUND, "not_found", msg),
            AdminError::Conflict(msg) => (StatusCode::CONFLICT, "conflict", msg),
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
