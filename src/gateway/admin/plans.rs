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
use super::{AdminDeps, AdminError, begin_write, db_err, reject_user_management, reload_and_swap};

pub(super) fn admin_routes() -> Router<AdminDeps> {
    Router::new()
        .route("/plans", get(list_plans))
        .route("/plans/{id}", get(get_plan))
        .route("/users/{id}/plan", put(assign_plan))
}

pub(super) fn root_routes() -> Router<AdminDeps> {
    Router::new()
        .route("/plans", post(create_plan))
        .route("/plans/{id}", put(update_plan).delete(delete_plan))
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
            builtin: record.builtin,
            created_at: record.created_at,
            groups: record.groups,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanBody {
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

fn default_discount() -> i64 {
    10_000
}

impl PlanBody {
    fn into_input(self) -> plans::PlanInput {
        plans::PlanInput {
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
    body: Result<Json<PlanBody>, axum::extract::rejection::JsonRejection>,
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
    body: Result<Json<PlanBody>, axum::extract::rejection::JsonRejection>,
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
    let plan = plans::get_plan_on_conn(&mut tx, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("套餐 {id} 不存在")))?;
    if plan.builtin {
        return Err(AdminError::Conflict("内置套餐不能删除".to_string()));
    }
    let users_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE plan_id = ?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_err)?;
    if users_count > 0 && !force {
        return Err(AdminError::Conflict(
            "套餐仍有用户挂载，需 force 删除".to_string(),
        ));
    }
    if force {
        sqlx::query("UPDATE users SET plan_id = CASE WHEN role = 'admin' THEN ? ELSE ? END WHERE plan_id = ?")
            .bind(plans::ADMIN_PLAN_ID).bind(plans::STANDARD_PLAN_ID).bind(id)
            .execute(&mut *tx).await.map_err(db_err)?;
    }
    sqlx::query("DELETE FROM plans WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
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
