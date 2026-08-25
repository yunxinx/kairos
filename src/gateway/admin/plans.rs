//! 套餐目录管理与用户换档。

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};

use crate::gateway::logging;
use crate::store::{
    self, plans,
    users::{self, ManagementRole},
};

use super::auth::{ManagementCapability, ManagementIdentity};
use super::{
    AdminDeps, AdminError, BulkDeleteBody, BulkDeleteResult, begin_write, db_err,
    reject_user_management, reload_and_swap, validate_bulk_targets,
};

pub(super) fn admin_routes() -> Router<AdminDeps> {
    Router::new()
        .route("/plans", get(list_plans))
        .route("/plans/{id}", get(get_plan))
        .route("/users/{id}/plan", put(assign_plan))
}

pub(super) fn root_routes() -> Router<AdminDeps> {
    Router::new()
        .route("/plans", post(create_plan).delete(delete_plans))
        .route("/plans/{id}", put(update_plan).delete(delete_plan))
        .route("/plans/{id}/default", put(set_default_plan))
}

#[derive(Debug, Serialize)]
struct PlanView {
    id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    internal_name: Option<String>,
    display_name: String,
    note: String,
    note_visible_to_admin: bool,
    discount_bp: i64,
    default_rpm: Option<u64>,
    shared_rpm: Option<u64>,
    initial_grant_usd_micros: i64,
    capabilities: plans::PlanCapabilities,
    shared_with_admin: bool,
    /// 受众：决定前端是否展示管理面能力开关。
    audience: plans::PlanAudience,
    /// 是否本受众的新用户默认档。
    is_default: bool,
    builtin: bool,
    created_at: i64,
    groups: Vec<String>,
}

impl PlanView {
    fn from_record(record: plans::PlanRecord, is_root: bool) -> Self {
        let expose_note = is_root || record.note_visible_to_admin;
        Self {
            id: record.id,
            internal_name: is_root.then_some(record.internal_name),
            display_name: record.display_name,
            note: if expose_note {
                record.note
            } else {
                String::new()
            },
            note_visible_to_admin: record.note_visible_to_admin,
            discount_bp: record.discount_bp,
            default_rpm: record.default_rpm,
            shared_rpm: record.shared_rpm,
            initial_grant_usd_micros: record.initial_grant_usd_micros,
            capabilities: record.capabilities,
            shared_with_admin: record.shared_with_admin,
            audience: record.audience,
            is_default: record.is_default,
            builtin: record.builtin,
            created_at: record.created_at,
            groups: record.groups,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatePlanBody {
    internal_name: String,
    display_name: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    note_visible_to_admin: bool,
    #[serde(default = "default_discount")]
    discount_bp: i64,
    default_rpm: Option<u64>,
    shared_rpm: Option<u64>,
    #[serde(default)]
    initial_grant_usd_micros: i64,
    #[serde(default)]
    capabilities: plans::PlanCapabilities,
    #[serde(default)]
    shared_with_admin: bool,
    /// 受众；缺省普通用户档，创建后不可改。
    #[serde(default)]
    audience: plans::PlanAudience,
    /// 是否设为本受众的新用户默认档。
    #[serde(default)]
    is_default: bool,
    #[serde(default)]
    groups: Vec<String>,
}

fn default_discount() -> i64 {
    10_000
}

impl CreatePlanBody {
    fn into_input(self) -> plans::PlanCreateInput {
        plans::PlanCreateInput {
            internal_name: self.internal_name.trim().to_string(),
            display_name: self.display_name.trim().to_string(),
            note: self.note,
            note_visible_to_admin: self.note_visible_to_admin,
            discount_bp: self.discount_bp,
            default_rpm: self.default_rpm,
            shared_rpm: self.shared_rpm,
            initial_grant_usd_micros: self.initial_grant_usd_micros,
            capabilities: self.capabilities,
            shared_with_admin: self.shared_with_admin,
            audience: self.audience,
            is_default: self.is_default,
            groups: self.groups,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdatePlanBody {
    internal_name: String,
    display_name: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    note_visible_to_admin: bool,
    #[serde(default = "default_discount")]
    discount_bp: i64,
    default_rpm: Option<u64>,
    shared_rpm: Option<u64>,
    #[serde(default)]
    initial_grant_usd_micros: i64,
    #[serde(default)]
    capabilities: plans::PlanCapabilities,
    #[serde(default)]
    shared_with_admin: bool,
    #[serde(default)]
    groups: Vec<String>,
}

impl UpdatePlanBody {
    fn into_input(self) -> plans::PlanUpdateInput {
        plans::PlanUpdateInput {
            internal_name: self.internal_name.trim().to_string(),
            display_name: self.display_name.trim().to_string(),
            note: self.note,
            note_visible_to_admin: self.note_visible_to_admin,
            discount_bp: self.discount_bp,
            default_rpm: self.default_rpm,
            shared_rpm: self.shared_rpm,
            initial_grant_usd_micros: self.initial_grant_usd_micros,
            capabilities: self.capabilities,
            shared_with_admin: self.shared_with_admin,
            groups: self.groups,
        }
    }
}

async fn list_plans(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
) -> Result<Json<Vec<PlanView>>, AdminError> {
    let is_root = identity.role() == ManagementRole::Root;
    let records = plans::list_plans(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    let views = records
        .into_iter()
        .filter(|plan| is_root || plan.shared_with_admin)
        .map(|plan| PlanView::from_record(plan, is_root))
        .collect();
    Ok(Json(views))
}

async fn get_plan(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
) -> Result<Json<PlanView>, AdminError> {
    let is_root = identity.role() == ManagementRole::Root;
    let plan = plans::get_plan(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("套餐 {id} 不存在")))?;
    if !is_root && !plan.shared_with_admin {
        return Err(AdminError::NotFound(format!("套餐 {id} 不存在")));
    }
    Ok(Json(PlanView::from_record(plan, is_root)))
}

async fn create_plan(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<CreatePlanBody>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<PlanView>), AdminError> {
    let input = body.map_err(AdminError::bad_body)?.0.into_input();
    let mut tx = begin_write(&deps).await?;
    reject_duplicate_name(&mut tx, &input.internal_name, None).await?;
    let plan = plans::insert_plan(&mut tx, &input, logging::unix_millis())
        .await
        .map_err(map_plan_error)?;
    store::record_audit(
        &mut tx,
        identity.actor(),
        "plans",
        &format!("创建套餐 {} ({})", plan.id, plan.internal_name),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let plan = plans::get_plan(&deps.pool, plan.id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| {
            AdminError::Store(store::StoreError::InvalidResource(
                "套餐提交后不可读".to_string(),
            ))
        })?;
    Ok((StatusCode::CREATED, Json(PlanView::from_record(plan, true))))
}

async fn update_plan(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
    body: Result<Json<UpdatePlanBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<PlanView>, AdminError> {
    let input = body.map_err(AdminError::bad_body)?.0.into_input();
    let mut tx = begin_write(&deps).await?;
    let before = plans::get_plan_on_conn(&mut tx, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("套餐 {id} 不存在")))?;
    reject_duplicate_name(&mut tx, &input.internal_name, Some(id)).await?;
    plans::update_plan(&mut tx, id, &input)
        .await
        .map_err(map_plan_error)?;
    store::record_audit(
        &mut tx,
        identity.actor(),
        "plans",
        &format!("修改套餐 {} ({})", id, before.internal_name),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let plan = plans::get_plan(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| {
            AdminError::Store(store::StoreError::InvalidResource(format!(
                "套餐 {id} 提交后不可读"
            )))
        })?;
    Ok(Json(PlanView::from_record(plan, true)))
}

/// 把默认身份转移给指定套餐；该命令没有“关闭默认”的反向状态。
async fn set_default_plan(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
) -> Result<Json<PlanView>, AdminError> {
    let mut tx = begin_write(&deps).await?;
    let before = plans::get_plan_on_conn(&mut tx, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("套餐 {id} 不存在")))?;
    let changed = !before.is_default;
    let plan = if changed {
        let plan = plans::set_default_plan(&mut tx, id)
            .await
            .map_err(map_plan_error)?;
        store::record_audit(
            &mut tx,
            identity.actor(),
            "plans",
            &format!("把套餐 {} ({}) 设为默认档", id, before.internal_name),
        )
        .await
        .map_err(AdminError::Store)?;
        plan
    } else {
        before
    };
    tx.commit().await.map_err(db_err)?;
    if changed {
        reload_and_swap(&deps).await?;
    }
    Ok(Json(PlanView::from_record(plan, true)))
}

#[derive(Debug, Deserialize, Default)]
struct ForceQuery {
    force: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteBody {
    force: bool,
}

async fn delete_plan(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
    Query(query): Query<ForceQuery>,
    body: Option<Json<DeleteBody>>,
) -> Result<Json<PlanView>, AdminError> {
    let force = query
        .force
        .or_else(|| body.map(|body| body.0.force))
        .unwrap_or(false);
    let mut tx = begin_write(&deps).await?;
    let mut deleted =
        delete_plans_on_conn(&mut tx, &std::collections::HashSet::from([id]), force).await?;
    let plan = deleted.pop().ok_or_else(|| {
        AdminError::Store(store::StoreError::InvalidResource(
            "套餐删除结果为空".to_string(),
        ))
    })?;
    store::record_audit(
        &mut tx,
        identity.actor(),
        "plans",
        &format!(
            "删除套餐 {} ({}){}",
            id,
            plan.internal_name,
            if force { "（强制）" } else { "" }
        ),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(PlanView::from_record(plan, true)))
}

async fn delete_plans(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Query(query): Query<ForceQuery>,
    body: Result<Json<BulkDeleteBody<i64>>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<BulkDeleteResult<i64>>, AdminError> {
    let targets = validate_bulk_targets(body.map_err(AdminError::bad_body)?.0.targets)?;
    let force = query.force.unwrap_or(false);
    let target_set: std::collections::HashSet<i64> = targets.iter().copied().collect();
    let mut tx = begin_write(&deps).await?;
    let deleted = delete_plans_on_conn(&mut tx, &target_set, force).await?;
    store::record_audit(
        &mut tx,
        identity.actor(),
        "plans",
        &format!("批量删除 {} 个套餐", deleted.len()),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(BulkDeleteResult::new(targets)))
}

async fn delete_plans_on_conn(
    conn: &mut sqlx::SqliteConnection,
    targets: &std::collections::HashSet<i64>,
    force: bool,
) -> Result<Vec<plans::PlanRecord>, AdminError> {
    let mut records = Vec::with_capacity(targets.len());
    for id in targets {
        let plan = plans::get_plan_on_conn(conn, *id)
            .await
            .map_err(AdminError::Store)?
            .ok_or_else(|| AdminError::NotFound(format!("套餐 {id} 不存在")))?;
        if plan.builtin {
            return Err(AdminError::Conflict("内置套餐不能删除".to_string()));
        }
        records.push(plan);
    }
    records.sort_by_key(|plan| plan.id);

    for plan in &records {
        let users_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE plan_id = ?")
            .bind(plan.id)
            .fetch_one(&mut *conn)
            .await
            .map_err(db_err)?;
        if users_count > 0 && !force {
            return Err(AdminError::Conflict(format!(
                "套餐 {} 仍有用户挂载，需 force 删除",
                plan.id
            )));
        }
    }

    let admin_target = replacement_plan_id(conn, plans::PlanAudience::Admin, targets).await?;
    let user_target = replacement_plan_id(conn, plans::PlanAudience::User, targets).await?;
    for audience in [plans::PlanAudience::User, plans::PlanAudience::Admin] {
        if records
            .iter()
            .any(|plan| plan.audience == audience && plan.is_default)
        {
            for plan in records.iter().filter(|plan| plan.audience == audience) {
                sqlx::query("UPDATE plans SET is_default = 0 WHERE id = ?")
                    .bind(plan.id)
                    .execute(&mut *conn)
                    .await
                    .map_err(db_err)?;
            }
            let target = match audience {
                plans::PlanAudience::Admin => admin_target,
                plans::PlanAudience::User => user_target,
            };
            sqlx::query("UPDATE plans SET is_default = 1 WHERE id = ?")
                .bind(target)
                .execute(&mut *conn)
                .await
                .map_err(db_err)?;
        }
    }

    if force {
        for plan in &records {
            sqlx::query(
                "UPDATE users SET plan_id = CASE WHEN role = 'admin' THEN ? ELSE ? END \
                 WHERE plan_id = ?",
            )
            .bind(admin_target)
            .bind(user_target)
            .bind(plan.id)
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
        }
    }
    for plan in &records {
        sqlx::query("DELETE FROM plans WHERE id = ?")
            .bind(plan.id)
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
    }
    Ok(records)
}

async fn replacement_plan_id(
    conn: &mut sqlx::SqliteConnection,
    audience: plans::PlanAudience,
    targets: &std::collections::HashSet<i64>,
) -> Result<i64, AdminError> {
    let configured = plans::default_plan_id_on_conn(conn, audience)
        .await
        .map_err(AdminError::Store)?
        .filter(|id| !targets.contains(id));
    let fallback = match audience {
        plans::PlanAudience::Admin => plans::ADMIN_PLAN_ID,
        plans::PlanAudience::User => plans::STANDARD_PLAN_ID,
    };
    let target = configured.unwrap_or(fallback);
    let plan = plans::get_plan_on_conn(conn, target)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| {
            AdminError::Store(store::StoreError::InvalidResource(format!(
                "替代套餐 {target} 不存在"
            )))
        })?;
    if plan.audience != audience || targets.contains(&target) {
        return Err(AdminError::Store(store::StoreError::InvalidResource(
            "删除套餐时找不到同受众替代档".to_string(),
        )));
    }
    Ok(target)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignPlanBody {
    plan_id: i64,
}

async fn assign_plan(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
    body: Result<Json<AssignPlanBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<super::users::UserView>, AdminError> {
    identity.require_capability(ManagementCapability::AssignPlan)?;
    let plan_id = body.map_err(AdminError::bad_body)?.0.plan_id;
    let mut tx = begin_write(&deps).await?;
    let target = users::get_user_on_conn(&mut tx, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    reject_user_management(&identity, &target, None)?;
    let plan = plans::get_plan_on_conn(&mut tx, plan_id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("套餐 {plan_id} 不存在")))?;
    if identity.role() == ManagementRole::Admin && !plan.shared_with_admin {
        return Err(AdminError::Forbidden);
    }
    if users::plan_audience_for_role(target.role) != Some(plan.audience) {
        return Err(AdminError::InvalidBody(format!(
            "角色 {} 不能绑定 {} 受众套餐",
            target.role.as_str(),
            plan.audience.as_str()
        )));
    }
    users::set_user_plan(&mut tx, id, plan_id)
        .await
        .map_err(super::map_user_store_err)?;
    store::record_audit(
        &mut tx,
        identity.actor(),
        "users",
        &format!(
            "用户 {} ({}) 换套餐 {} ({})",
            id, target.email, plan_id, plan.display_name
        ),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let user = users::get_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    Ok(Json(super::users::UserView::from_record(user)))
}

async fn reject_duplicate_name(
    conn: &mut sqlx::SqliteConnection,
    name: &str,
    except: Option<i64>,
) -> Result<(), AdminError> {
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM plans WHERE internal_name = ?")
        .bind(name)
        .fetch_optional(&mut *conn)
        .await
        .map_err(db_err)?;
    if found.is_some_and(|id| Some(id) != except) {
        return Err(AdminError::Conflict(
            "套餐 internal_name 已存在".to_string(),
        ));
    }
    Ok(())
}

fn map_plan_error(err: store::StoreError) -> AdminError {
    match err {
        store::StoreError::InvalidResource(message) => AdminError::InvalidBody(message),
        other => AdminError::Store(other),
    }
}
