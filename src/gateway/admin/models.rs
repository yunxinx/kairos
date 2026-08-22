//! 定价、模型组与统一模型管理。

use std::collections::HashSet;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
};
use serde::Serialize;

use crate::store::resources::{
    Channel, ChannelRecord, GroupModel, ModelGroup, Price, UnifiedMember, UnifiedModel,
    channel_lists_callable,
};

use super::{AdminDeps, AdminError, db_err, reload_and_swap};

pub(super) fn routes() -> Router<AdminDeps> {
    Router::new()
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
pub(super) async fn reject_unknown_group(deps: &AdminDeps, group: &str) -> Result<(), AdminError> {
    let snapshot = deps.snapshot.read().await;
    if snapshot.model_groups.contains_key(group) {
        Ok(())
    } else {
        Err(AdminError::NotFound(format!("模型组 {group} 不存在")))
    }
}

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
pub(super) fn reject_unhidden_unified_collision<'a>(
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
