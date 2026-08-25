//! 渠道资源管理：渠道定义 CRUD 与模型组联动。

use std::collections::HashSet;

use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
};
use serde::{Deserialize, Serialize};

use crate::gateway::{logging, routing};
use crate::store;
use crate::store::resources::{Channel, ChannelRecord};

use super::auth::ManagementIdentity;
use super::models::reject_unhidden_unified_collision;
use super::{
    AdminDeps, AdminError, BulkDeleteBody, BulkDeleteResult, begin_write, db_err, reload_and_swap,
    validate_bulk_targets,
};

pub(super) fn routes() -> Router<AdminDeps> {
    Router::new()
        .route(
            "/channels",
            get(list_channels)
                .post(create_channel)
                .delete(delete_channels),
        )
        .route("/channels/{id}", put(update_channel).delete(delete_channel))
}

/// 渠道名录：admin+ 只读的最小投影，不含密钥与上游地址。
///
/// 模型页要判断「某个已登记名挂在哪条渠道、那条渠道还在不在」。没有这个端点时
/// admin 调 `GET /channels` 拿 403，前端渠道表为空，于是把「不知道」渲染成了
/// 「渠道已失效」——所有组员都误标红。投影只回渲染判断真正需要的字段：
/// 出站地址与密钥仍是 root-only 的运营机密。
pub(super) fn summary_routes() -> Router<AdminDeps> {
    Router::new().route("/channels/summary", get(list_channel_summaries))
}

pub(super) fn model_routes() -> Router<AdminDeps> {
    Router::new().route(
        "/channel-models",
        axum::routing::delete(delete_channel_models),
    )
}

// --- 渠道 ---

/// 渠道名录条目：身份 + 名称 + 协议 + 启用态 + 可调用名。
///
/// 带上 `protocol` 是因为日志页要标出「入站 ⇄ 出站」的协议转换，那张映射表按
/// 渠道名查协议；缺它则 admin 的日志详情永远显示「出站未知」。
#[derive(Debug, Serialize)]
struct ChannelSummaryView {
    id: i64,
    name: String,
    protocol: crate::config::Protocol,
    enabled: bool,
    models: Vec<String>,
    model_aliases: std::collections::HashMap<String, String>,
}

/// 列出全部渠道名录。路由层已限制为 admin+；响应只含模型视图所需的安全字段。
async fn list_channel_summaries(
    State(deps): State<AdminDeps>,
) -> Result<Json<Vec<ChannelSummaryView>>, AdminError> {
    let snapshot = deps.snapshot.read().await;
    Ok(Json(
        snapshot
            .channels
            .iter()
            .map(|record| ChannelSummaryView {
                id: record.id,
                name: record.channel.name.clone(),
                protocol: record.channel.protocol,
                enabled: record.channel.enabled,
                models: record.channel.models.clone(),
                model_aliases: record.channel.model_aliases.clone(),
            })
            .collect(),
    ))
}

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
    fn from_record(mut record: ChannelRecord) -> Self {
        record.channel.keys = record.keys.iter().map(|key| key.to_wire()).collect();
        ChannelView {
            id: record.id,
            channel: record.channel,
        }
    }
}

/// 解析路径中的渠道 id；非整数不标识任何渠道，按不存在处理（404）。
pub(super) fn parse_channel_id(raw: String) -> Result<i64, AdminError> {
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
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<Channel>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<ChannelView>), AdminError> {
    let mut channel = body.map_err(AdminError::bad_body)?;
    normalize_channel_group(&mut channel);
    validate_channel(&channel)?;
    let mut tx = begin_write(&deps).await?;
    reject_unknown_group_on_conn(&mut tx, &channel.model_group).await?;
    let channels = crate::store::resources::list_channel_records_on_conn(&mut tx)
        .await
        .map_err(AdminError::Store)?;
    let unified_models = crate::store::resources::list_unified_models_on_conn(&mut tx)
        .await
        .map_err(AdminError::Store)?;
    if channels
        .iter()
        .any(|record| record.channel.name == channel.name)
    {
        return Err(AdminError::Conflict(format!(
            "渠道 {} 已存在",
            channel.name
        )));
    }
    reject_alias_occupancy(&channel)?;
    reject_alias_conflict(&channels, &channel, None)?;
    reject_unhidden_unified_collision(
        &channels,
        Some(&channel),
        None,
        unified_models.iter(),
        None,
    )?;
    let id = crate::store::resources::insert_channel(&mut tx, &channel)
        .await
        .map_err(AdminError::Store)?;
    enroll_channel_models(&mut tx, id, None, &channel).await?;
    store::record_audit(
        &mut tx,
        identity.actor(),
        "channels",
        &format!(
            "创建渠道 {} ({}) protocol={} base_url={}",
            id,
            channel.name,
            logging::protocol_name(channel.protocol),
            channel.base_url
        ),
    )
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
    Extension(identity): Extension<ManagementIdentity>,
    Path(raw_id): Path<String>,
    body: Result<Json<Channel>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ChannelView>, AdminError> {
    let id = parse_channel_id(raw_id)?;
    let mut channel = body.map_err(AdminError::bad_body)?;
    normalize_channel_group(&mut channel);
    validate_channel(&channel)?;
    let mut tx = begin_write(&deps).await?;
    reject_unknown_group_on_conn(&mut tx, &channel.model_group).await?;
    let channels = crate::store::resources::list_channel_records_on_conn(&mut tx)
        .await
        .map_err(AdminError::Store)?;
    let unified_models = crate::store::resources::list_unified_models_on_conn(&mut tx)
        .await
        .map_err(AdminError::Store)?;
    let current = channels
        .iter()
        .find(|record| record.id == id)
        .ok_or_else(|| AdminError::NotFound(format!("渠道 {id} 不存在")))?;
    if channel.name != current.channel.name
        && channels
            .iter()
            .any(|record| record.channel.name == channel.name)
    {
        return Err(AdminError::Conflict(format!(
            "渠道 {} 已存在",
            channel.name
        )));
    }
    reject_alias_occupancy(&channel)?;
    reject_alias_conflict(&channels, &channel, Some(id))?;
    reject_unhidden_unified_collision(
        &channels,
        Some(&channel),
        Some(id),
        unified_models.iter(),
        None,
    )?;
    let previous = current.channel.clone();
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
    store::record_audit(
        &mut tx,
        identity.actor(),
        "channels",
        &format!(
            "修改渠道 {} ({} → {}) enabled={} base_url={}",
            id, previous.name, channel.name, channel.enabled, channel.base_url
        ),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_channel_record(&deps, id).await?;
    Ok(Json(ChannelView::from_record(updated)))
}

/// 在持有写事务时确认渠道默认组仍存在，避免先读快照后提交的竞态。
async fn reject_unknown_group_on_conn(
    conn: &mut sqlx::SqliteConnection,
    group: &str,
) -> Result<(), AdminError> {
    if crate::store::resources::get_model_group(conn, group)
        .await
        .map_err(AdminError::Store)?
        .is_some()
    {
        Ok(())
    } else {
        Err(AdminError::NotFound(format!("模型组 {group} 不存在")))
    }
}

/// 删除渠道（按路径 `id`）：不存在则 404，否则删除并返回被删渠道视图。
async fn delete_channel(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(raw_id): Path<String>,
) -> Result<Json<ChannelView>, AdminError> {
    let id = parse_channel_id(raw_id)?;
    let deleted = read_channel_record(&deps, id).await?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::delete_channel(&mut tx, id)
        .await
        .map_err(AdminError::Store)?;
    store::record_audit(
        &mut tx,
        identity.actor(),
        "channels",
        &format!("删除渠道 {} ({})", id, deleted.channel.name),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(ChannelView::from_record(deleted)))
}

async fn delete_channels(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<BulkDeleteBody<i64>>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<BulkDeleteResult<i64>>, AdminError> {
    let targets = validate_bulk_targets(body.map_err(AdminError::bad_body)?.0.targets)?;
    let mut tx = begin_write(&deps).await?;
    let records = crate::store::resources::list_channel_records_on_conn(&mut tx)
        .await
        .map_err(AdminError::Store)?;
    for id in &targets {
        if !records.iter().any(|record| record.id == *id) {
            return Err(AdminError::NotFound(format!("渠道 {id} 不存在")));
        }
    }
    for id in &targets {
        crate::store::resources::delete_channel(&mut tx, *id)
            .await
            .map_err(AdminError::Store)?;
    }
    store::record_audit(
        &mut tx,
        identity.actor(),
        "channels",
        &format!("批量删除 {} 条渠道", targets.len()),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(BulkDeleteResult::new(targets)))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelModelTarget {
    channel_id: i64,
    model: String,
}

async fn delete_channel_models(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<BulkDeleteBody<ChannelModelTarget>>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<BulkDeleteResult<ChannelModelTarget>>, AdminError> {
    let targets = validate_bulk_targets(body.map_err(AdminError::bad_body)?.0.targets)?;
    let mut tx = begin_write(&deps).await?;
    let mut records = crate::store::resources::list_channel_records_on_conn(&mut tx)
        .await
        .map_err(AdminError::Store)?;

    for target in &targets {
        let record = records
            .iter()
            .find(|record| record.id == target.channel_id)
            .ok_or_else(|| AdminError::NotFound(format!("渠道 {} 不存在", target.channel_id)))?;
        if !record
            .channel
            .models
            .iter()
            .any(|model| model == &target.model)
        {
            return Err(AdminError::NotFound(format!(
                "渠道 {} 的清单模型 {} 不存在",
                target.channel_id, target.model
            )));
        }
    }

    for record in &mut records {
        let mut drop = HashSet::new();
        for target in targets
            .iter()
            .filter(|target| target.channel_id == record.id)
        {
            drop.insert(target.model.clone());
            for (alias, canonical) in &record.channel.model_aliases {
                if canonical == &target.model {
                    drop.insert(alias.clone());
                }
            }
        }
        if drop.is_empty() {
            continue;
        }
        record.channel.models.retain(|model| !drop.contains(model));
        record
            .channel
            .model_aliases
            .retain(|alias, canonical| !drop.contains(alias) && !drop.contains(canonical));
        crate::store::resources::update_channel(&mut tx, record.id, &record.channel)
            .await
            .map_err(AdminError::Store)?;
        crate::store::resources::retain_channel_prices(
            &mut tx,
            record.id,
            &crate::store::resources::channel_callable_names(&record.channel),
        )
        .await
        .map_err(AdminError::Store)?;
    }
    store::record_audit(
        &mut tx,
        identity.actor(),
        "channels",
        &format!("批量移除 {} 条渠道模型清单项", targets.len()),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(BulkDeleteResult::new(targets)))
}
pub(super) async fn read_channel_record(
    deps: &AdminDeps,
    id: i64,
) -> Result<ChannelRecord, AdminError> {
    let snapshot = deps.snapshot.read().await;
    snapshot
        .channels
        .iter()
        .find(|record| record.id == id)
        .cloned()
        .ok_or_else(|| AdminError::NotFound(format!("渠道 {id} 不存在")))
}

fn normalize_channel_group(channel: &mut Channel) {
    channel.model_group = channel.model_group.trim().to_string();
    if channel.model_group.is_empty() {
        channel.model_group = crate::store::resources::DEFAULT_MODEL_GROUP.to_string();
    }
    for key in &mut channel.keys {
        key.name = key.name.trim().to_string();
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
fn validate_channel(channel: &Channel) -> Result<(), AdminError> {
    if channel.name.trim().is_empty() {
        return Err(AdminError::InvalidBody("name 不能为空".to_string()));
    }
    if channel.base_url.trim().is_empty() {
        return Err(AdminError::InvalidBody("base_url 不能为空".to_string()));
    }
    reject_non_http_url(&channel.base_url)?;
    if channel.keys.is_empty() {
        return Err(AdminError::InvalidBody("keys 不能为空".to_string()));
    }
    let mut key_names = HashSet::with_capacity(channel.keys.len());
    for key in &channel.keys {
        if key.name.trim().is_empty() {
            return Err(AdminError::InvalidBody("密钥 name 不能为空".to_string()));
        }
        if key.api_key.trim().is_empty() {
            return Err(AdminError::InvalidBody("密钥 api_key 不能为空".to_string()));
        }
        if key.weight < 0 {
            return Err(AdminError::InvalidBody(
                "密钥 weight 不能小于 0".to_string(),
            ));
        }
        if !key_names.insert(key.name.as_str()) {
            return Err(AdminError::Conflict(format!(
                "渠道内密钥名称 {} 重复",
                key.name
            )));
        }
    }
    Ok(())
}

/// 探测与渠道草稿仅允许 http/https，避免 `file://` 等 scheme 打到本机文件。
pub(super) fn reject_non_http_url(raw: &str) -> Result<(), AdminError> {
    let parsed = reqwest::Url::parse(raw.trim())
        .map_err(|_| AdminError::InvalidBody("base_url 不是合法绝对 URL".to_string()))?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        other => Err(AdminError::InvalidBody(format!(
            "探测 URL 仅支持 http/https，收到 {other}"
        ))),
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
    match routing::find_alias_conflict(&channels) {
        Some(conflict) => Err(AdminError::Conflict(format!(
            "别名 {} 在启用渠道间指向不同真名（{} 与 {}）。一对多请到模型页「归一化」（Tab 2）用统一模型，不要用别名",
            conflict.alias, conflict.existing, conflict.conflicting
        ))),
        None => Ok(()),
    }
}
