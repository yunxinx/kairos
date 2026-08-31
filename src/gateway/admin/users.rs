//! 管理用户与会话端点：登录、自助账户、用户运营、钱包和模型组分配。

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::gateway::logging;
use crate::store;
use crate::store::StoreError;
use crate::store::plans;
use crate::store::users::{self, ManagementRole, NewUser, UserRecord};

use super::auth::{
    ManagementCapability, ManagementIdentity, SESSION_COOKIE, request_is_secure,
    session_from_headers, session_from_request,
};
use super::tokens;
use super::{
    AdminDeps, AdminError, BulkDeleteBody, BulkDeleteResult, begin_write, db_err,
    format_usd_micros, map_user_store_err, reject_user_management, reload_and_swap,
    validate_bulk_targets,
};

pub(super) fn admin_routes() -> Router<AdminDeps> {
    Router::new()
        .route(
            "/users",
            get(list_management_users)
                .post(create_user)
                .delete(delete_users),
        )
        .route(
            "/users/{id}",
            get(get_management_user)
                .put(update_user)
                .delete(delete_user),
        )
        .route("/users/{id}/tokens", get(tokens::list_user_tokens))
        .route(
            "/users/{id}/balance-adjustments",
            post(super::user_balance::adjust_user_balance),
        )
}

pub(super) fn root_routes() -> Router<AdminDeps> {
    Router::new()
}

pub(super) fn signed_in_routes() -> Router<AdminDeps> {
    Router::new()
        .route("/me", get(get_me).put(update_me))
        .route("/logout", post(logout))
}

pub(super) fn public_routes() -> Router<AdminDeps> {
    Router::new().route("/login", post(login))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginBody {
    email: String,
    password: String,
}

#[derive(Debug, Serialize)]
pub(super) struct UserView {
    id: i64,
    email: String,
    display_name: String,
    role: ManagementRole,
    enabled: bool,
    avatar: Option<String>,
    rate_limit_rpm: Option<u64>,
    plan_id: Option<i64>,
}

impl UserView {
    pub(super) fn from_record(record: UserRecord) -> Self {
        Self {
            id: record.id,
            email: record.email,
            display_name: record.display_name,
            role: record.role,
            enabled: record.enabled,
            avatar: record.avatar,
            rate_limit_rpm: record.rate_limit_rpm,
            plan_id: record.plan_id,
        }
    }
}

#[derive(Debug, Serialize)]
struct LoginView {
    expires_at: i64,
    user: UserView,
}

/// 邮箱密码换会话；令牌通过 HttpOnly Cookie 交给浏览器。
async fn login(
    State(deps): State<AdminDeps>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<LoginBody>, axum::extract::rejection::JsonRejection>,
) -> Result<impl IntoResponse, AdminError> {
    let snapshot = deps.snapshot.read().await;
    let max_failures = snapshot.auth_throttle_max_failures;
    let window = snapshot.auth_throttle_window();
    drop(snapshot);
    let ip = addr.ip();
    if deps.throttle.is_blocked(ip, max_failures, window) {
        return Err(AdminError::RateLimited);
    }
    let body = body.map_err(AdminError::bad_body)?.0;
    // 形状封顶先于限流记账、Argon2 与审计写入：超长字段只会白白消耗 CPU，
    // email 还会原样进审计行（放大 system_log）；控制字符可伪造多行日志。
    validate_login_shape(&body.email, &body.password)?;
    let Some(user) = users::authenticate_password(&deps.pool, &body.email, &body.password)
        .await
        .map_err(AdminError::Store)?
    else {
        deps.throttle.record_failure(ip, max_failures, window);
        // 失败登录记 warn 且不带 actor：此刻还没认出是谁，邮箱只是对方声称的。
        store::record_audit_detached(
            &deps.pool,
            None,
            "warn",
            "auth",
            &store::SystemLogEvent::new(
                "auth.login_failed",
                serde_json::json!({ "email": body.email, "ip": ip.to_string() }),
                format!("登录失败：{}（IP：{}）", body.email, ip),
            ),
        )
        .await;
        return Err(AdminError::Unauthorized);
    };
    let now = logging::unix_millis();
    let mut tx = begin_write(&deps).await?;
    let (token, expires_at) = users::issue_session(&mut tx, user.id, now)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    store::record_audit_detached(
        &deps.pool,
        Some(store::Actor {
            user_id: user.id,
            email: &user.email,
        }),
        "info",
        "auth",
        &store::SystemLogEvent::new(
            "auth.login_succeeded",
            serde_json::json!({ "ip": ip.to_string() }),
            format!("登录成功（IP：{ip}）"),
        ),
    )
    .await;
    let secure = if request_is_secure(&headers) {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{SESSION_COOKIE}={token}; Path=/api; HttpOnly; SameSite=Strict{secure}; Max-Age={}",
        (expires_at - now).max(0) / 1000
    );
    let cookie = HeaderValue::from_str(&cookie).map_err(|_| {
        AdminError::Store(StoreError::InvalidResource(
            "会话 Cookie 生成失败".to_string(),
        ))
    })?;
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(LoginView {
            expires_at,
            user: UserView::from_record(user),
        }),
    ))
}

/// 登录入口的输入形状封顶：与写入路径共用 [`users::validate_email_shape`] /
/// [`users::validate_password_shape`]，两端标准恒一致（否则会把可写入的账号
/// 变成登不进来的自锁账户）。违规按 400 处理——这是请求形状错误，不是认证
/// 结果，不应消耗认证失败限流。
fn validate_login_shape(email: &str, password: &str) -> Result<(), AdminError> {
    users::validate_email_shape(email).map_err(map_user_store_err)?;
    users::validate_password_shape(password).map_err(map_user_store_err)?;
    Ok(())
}

/// 吊销当前会话；非 `ksess_` 前缀视为无操作，仍 204。
async fn logout(
    State(deps): State<AdminDeps>,
    request: Request,
) -> Result<impl IntoResponse, AdminError> {
    let provided = session_from_request(&request).unwrap_or("");
    users::revoke_session(&deps.pool, provided)
        .await
        .map_err(AdminError::Store)?;
    let secure = if request_is_secure(request.headers()) {
        "; Secure"
    } else {
        ""
    };
    let clear = HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}=; Path=/api; HttpOnly; SameSite=Strict{secure}; Max-Age=0"
    ))
    .map_err(|_| {
        AdminError::Store(StoreError::InvalidResource(
            "会话 Cookie 生成失败".to_string(),
        ))
    })?;
    Ok(([(header::SET_COOKIE, clear)], StatusCode::NO_CONTENT))
}

/// 当前用户：身份 + 可用组 + 钱包。
///
/// 刻意不带用量统计：`/me` 在每次进入受保护路由时都会被拉一次（`ensureMe`、
/// `loadTokenRows`），而用量是对整张 `request_log` 的全历史聚合、没有时间窗。
/// 统计留给 `GET /users/{id}`——那是运营按需打开的详情页。
#[derive(Debug, Serialize)]
struct MeView {
    id: i64,
    email: String,
    display_name: String,
    role: ManagementRole,
    enabled: bool,
    avatar: Option<String>,
    rate_limit_rpm: Option<u64>,
    plan_id: Option<i64>,
    plan_display_name: Option<String>,
    discount_bp: i64,
    assigned_groups: Vec<String>,
    capabilities: plans::PlanCapabilities,
    balance_usd_micros: i64,
    settled_usd_micros: i64,
}

async fn get_me(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
) -> Result<Json<MeView>, AdminError> {
    let user = users::get_user(&deps.pool, identity.user_id())
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {} 不存在", identity.user_id())))?;
    let plan = match user.plan_id {
        Some(plan_id) => plans::get_plan(&deps.pool, plan_id)
            .await
            .map_err(AdminError::Store)?,
        None => None,
    };
    let assigned_groups = plan
        .as_ref()
        .map(|plan| plan.groups.clone())
        .unwrap_or_default();
    let wallet = store::get_user_wallet(&deps.pool, user.id)
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(MeView {
        id: user.id,
        email: user.email,
        display_name: user.display_name,
        role: user.role,
        enabled: user.enabled,
        avatar: user.avatar,
        rate_limit_rpm: user.rate_limit_rpm,
        plan_id: user.plan_id,
        plan_display_name: plan.as_ref().map(|plan| plan.display_name.clone()),
        discount_bp: plan.as_ref().map_or(10_000, |plan| plan.discount_bp),
        assigned_groups,
        capabilities: identity.capabilities_for_view(),
        balance_usd_micros: wallet.balance_usd_micros,
        settled_usd_micros: wallet.settled_usd_micros,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MeUpdate {
    email: Option<String>,
    password: Option<String>,
    /// 改密码或改邮箱时必填；只改展示名/头像不必带。防止有人拿已窃会话静默换
    /// 口令或登录标识——邮箱是唯一登录标识，免密改邮箱等于永久劫持账户。
    current_password: Option<String>,
    display_name: Option<String>,
    avatar: Option<String>,
}

/// 当前用户改自己的邮箱、展示名或密码。
async fn update_me(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    headers: axum::http::HeaderMap,
    body: Result<Json<MeUpdate>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<UserView>, AdminError> {
    let update = body.map_err(AdminError::bad_body)?.0;
    let user_id = identity.user_id();
    let mut tx = begin_write(&deps).await?;
    let email_after = update.email.as_deref().map(users::normalize_email);
    let email_changed = email_after
        .as_deref()
        .is_some_and(|email| email != identity.user.email);
    // 邮箱是唯一登录标识：被盗会话若能静默改邮箱，就等于把账户永久劫持
    // （原主人无法再登录，改密吊销还会保住攻击者的当前会话）。与改密码同
    // 一威胁模型，同一条防线：改标识必须证明持有当前口令。
    if update.password.is_some() || email_changed {
        let Some(current) = update.current_password.as_deref() else {
            return Err(AdminError::InvalidBody(
                "修改密码或邮箱需要提供当前密码".to_string(),
            ));
        };
        let matches = users::password_matches_on_conn(&mut tx, user_id, current)
            .await
            .map_err(AdminError::Store)?;
        if !matches {
            return Err(AdminError::InvalidBody("当前密码不正确".to_string()));
        }
    }
    let password_changed = update.password.is_some();
    let mut changes = Vec::new();
    if let Some(email) = update.email.as_deref() {
        users::set_email(&mut tx, user_id, email)
            .await
            .map_err(map_user_store_err)?;
        if email_changed {
            changes.push(format!(
                "email {} → {}",
                identity.user.email,
                email_after.as_deref().unwrap_or(email)
            ));
        }
    }
    if let Some(password) = update.password.as_deref() {
        users::set_password(&mut tx, user_id, password)
            .await
            .map_err(map_user_store_err)?;
        changes.push("修改密码".to_string());
    }
    if let Some(display_name) = update.display_name.as_deref() {
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
        if name != identity.user.display_name {
            changes.push(format!(
                "display_name「{}」→「{}」",
                identity.user.display_name, name
            ));
        }
    }
    if let Some(avatar) = update.avatar.as_deref() {
        let avatar_val = if avatar.trim().is_empty() {
            None
        } else {
            Some(avatar)
        };
        users::set_avatar(&mut tx, user_id, avatar_val)
            .await
            .map_err(map_user_store_err)?;
        if avatar_val != identity.user.avatar.as_deref() {
            changes.push("更新头像".to_string());
        }
    }
    if email_changed || password_changed {
        users::revoke_user_sessions(&mut tx, user_id, session_from_headers(&headers))
            .await
            .map_err(map_user_store_err)?;
        changes.push("吊销其他会话".to_string());
    }
    if !changes.is_empty() {
        store::record_audit(
            &mut tx,
            identity.actor(),
            "users",
            &store::SystemLogEvent::new(
                "users.self_updated",
                serde_json::json!({
                    "user_id": user_id,
                    "email": identity.user.email,
                    "changes": changes,
                }),
                format!(
                    "用户 {} ({}) 修改自己的账户：{}",
                    user_id,
                    identity.user.email,
                    changes.join("；")
                ),
            ),
        )
        .await
        .map_err(AdminError::Store)?;
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
    #[serde(default)]
    plan_id: Option<i64>,
}

async fn create_user(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<UserCreate>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<UserView>), AdminError> {
    identity.require_capability(ManagementCapability::ManageUsers)?;
    let create = body.map_err(AdminError::bad_body)?.0;
    match (identity.role(), create.role) {
        // root 全局唯一：创建接口不接受 root，内置账号是唯一来源。
        (_, ManagementRole::Root) => return Err(AdminError::Forbidden),
        (ManagementRole::Root, _) | (ManagementRole::Admin, ManagementRole::User) => {}
        _ => return Err(AdminError::Forbidden),
    }
    let now = logging::unix_millis();
    let mut tx = begin_write(&deps).await?;
    // 默认档现在要查库（`plans.is_default`），故取默认与「是否算显式分配」都挪进事务：
    // 显式指定的档若正好就是该角色的默认档，不算跨档分配，无需 AssignPlan。
    let default_plan = users::default_plan_id_for_role(&mut tx, create.role)
        .await
        .map_err(AdminError::Store)?;
    if create
        .plan_id
        .is_some_and(|plan_id| Some(plan_id) != default_plan)
    {
        identity.require_capability(ManagementCapability::AssignPlan)?;
    }
    let selected_plan = create.plan_id.or(default_plan);
    let selected_plan = selected_plan
        .ok_or_else(|| AdminError::InvalidBody("root 不能作为新建用户角色".to_string()))?;
    let plan = plans::get_plan_on_conn(&mut tx, selected_plan)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::InvalidBody(format!("套餐 {selected_plan} 不存在")))?;
    let expected_audience = users::plan_audience_for_role(create.role)
        .ok_or_else(|| AdminError::InvalidBody("root 不能作为新建用户角色".to_string()))?;
    if plan.audience != expected_audience {
        return Err(AdminError::InvalidBody(format!(
            "角色 {} 不能绑定 {} 受众套餐",
            create.role.as_str(),
            plan.audience.as_str()
        )));
    }
    if identity.role() == ManagementRole::Admin && !plan.shared_with_admin {
        return Err(AdminError::Forbidden);
    }
    let user = users::insert_user_with_plan(
        &mut tx,
        NewUser {
            email: &create.email,
            display_name: &create.display_name,
            password: &create.password,
            role: create.role,
            rate_limit_rpm: create.rate_limit_rpm,
        },
        now,
        Some(selected_plan),
    )
    .await
    .map_err(map_user_store_err)?;
    store::record_audit(
        &mut tx,
        identity.actor(),
        "users",
        &store::SystemLogEvent::new(
            "users.created",
            serde_json::json!({
                "user_id": user.id,
                "email": user.email,
                "role": user.role.as_str(),
            }),
            format!(
                "创建用户 {} ({})，角色 {}",
                user.id,
                user.email,
                user.role.as_str()
            ),
        ),
    )
    .await
    .map_err(AdminError::Store)?;
    store::record_audit(
        &mut tx,
        identity.actor(),
        "billing",
        &store::SystemLogEvent::new(
            "billing.initial_grant_credited",
            serde_json::json!({
                "user_id": user.id,
                "email": user.email,
                "plan_name": plan.display_name,
                "amount_usd_micros": plan.initial_grant_usd_micros,
            }),
            format!(
                "新建用户 {} ({}) 按套餐 {} 入账起步金 {} USD",
                user.id,
                user.email,
                plan.display_name,
                format_usd_micros(plan.initial_grant_usd_micros)
            ),
        ),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok((StatusCode::CREATED, Json(UserView::from_record(user))))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UserUpdate {
    /// 修正登录邮箱（建号敲错等场景）：走 `users::set_email`，改后吊销目标会话。
    email: Option<String>,
    role: Option<ManagementRole>,
    enabled: Option<bool>,
    password: Option<String>,
    display_name: Option<String>,
    avatar: Option<String>,
    /// 与资料字段在同一事务中换套餐；只允许在角色不变时提交。
    plan_id: Option<i64>,
    /// 三态：字段缺省 = 不改，`null` = 清空（跟随全局兜底），数值 = 设为该值。
    #[serde(default, deserialize_with = "deserialize_double_option")]
    rate_limit_rpm: Option<Option<u64>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `rate_limit_rpm` 三态：字段缺省不改、`null` 清空、数值设值。
    #[test]
    fn user_update_rate_limit_rpm_distinguishes_absent_from_null() {
        let absent: UserUpdate = serde_json::from_value(json!({})).expect("空体应可解析");
        assert_eq!(absent.rate_limit_rpm, None, "字段缺省表示不改");

        let cleared: UserUpdate =
            serde_json::from_value(json!({ "rate_limit_rpm": null })).expect("null 应可解析");
        assert_eq!(cleared.rate_limit_rpm, Some(None), "null 表示清空");

        let set: UserUpdate =
            serde_json::from_value(json!({ "rate_limit_rpm": 60 })).expect("数值应可解析");
        assert_eq!(set.rate_limit_rpm, Some(Some(60)), "数值表示设值");
    }
}

/// 把 `null` 与「字段缺省」区分开，供 `Option<Option<T>>` 表达三态。
///
/// serde 对 `Option<T>` 的 `null` 走 `visit_none()`，直接落成外层 `None`，于是
/// `null` 与缺省不可区分——界面上清空输入框会保存成功但值不变（静默失败）。
/// 这里让外层由 `#[serde(default)]` 负责「缺省」，本函数只解内层，`null` 得以
/// 落成 `Some(None)`。
fn deserialize_double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

async fn update_user(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
    headers: axum::http::HeaderMap,
    body: Result<Json<UserUpdate>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<UserView>, AdminError> {
    identity.require_capability(ManagementCapability::ManageUsers)?;
    let update = body.map_err(AdminError::bad_body)?.0;
    let mut tx = begin_write(&deps).await?;
    let target = users::get_user_on_conn(&mut tx, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    reject_user_management(&identity, &target, update.role)?;

    let email_after = update.email.as_deref().map(users::normalize_email);
    let email_changed = email_after
        .as_deref()
        .is_some_and(|email| email != target.email);
    let role_changed = match update.role {
        Some(role) => role != target.role,
        None => false,
    };
    let enabled_changed = match update.enabled {
        Some(enabled) => enabled != target.enabled,
        None => false,
    };
    let display_name_after = match update.display_name.as_deref() {
        Some(raw) => {
            let name = raw.trim();
            if name.is_empty() {
                return Err(AdminError::InvalidBody("display_name 不能为空".to_string()));
            }
            Some(name.to_string())
        }
        None => None,
    };
    let display_name_changed = display_name_after
        .as_deref()
        .filter(|name| *name != target.display_name)
        .map(str::to_string);
    let avatar_after = update.avatar.as_deref().map(|avatar| {
        if avatar.trim().is_empty() {
            None
        } else {
            Some(avatar.to_string())
        }
    });
    let avatar_changed = match avatar_after.as_ref() {
        Some(after) => target.avatar.as_deref() != after.as_deref(),
        None => false,
    };
    let rpm_changed = match update.rate_limit_rpm {
        Some(rpm) => rpm != target.rate_limit_rpm,
        None => false,
    };
    let password_changed = update.password.is_some();
    let role_plan = if role_changed {
        let role = update.role.ok_or_else(|| {
            AdminError::Store(StoreError::InvalidResource(
                "角色变化缺少目标角色".to_string(),
            ))
        })?;
        let plan_id = users::default_plan_id_for_role(&mut tx, role)
            .await
            .map_err(AdminError::Store)?
            .ok_or_else(|| {
                AdminError::Store(StoreError::InvalidResource(
                    "非 root 角色缺少默认套餐".to_string(),
                ))
            })?;
        let plan = plans::get_plan_on_conn(&mut tx, plan_id)
            .await
            .map_err(AdminError::Store)?
            .ok_or_else(|| {
                AdminError::Store(StoreError::InvalidResource(format!(
                    "默认套餐 {plan_id} 不存在"
                )))
            })?;
        if users::plan_audience_for_role(role) != Some(plan.audience) {
            return Err(AdminError::Store(StoreError::InvalidResource(format!(
                "角色 {} 的默认套餐受众不匹配",
                role.as_str()
            ))));
        }
        Some((role, plan_id, plan.display_name))
    } else {
        None
    };

    let explicit_plan = if let Some(plan_id) = update.plan_id {
        if role_changed {
            return Err(AdminError::InvalidBody(
                "角色变化时不能同时指定套餐，请使用目标受众默认套餐".to_string(),
            ));
        }
        identity.require_capability(ManagementCapability::AssignPlan)?;
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
        (target.plan_id != Some(plan_id)).then_some((plan_id, plan.display_name))
    } else {
        None
    };

    // 规范化后的目标值与当前记录相同时，直接返回，避免无意义的写入、审计和快照替换。
    if !role_changed
        && !enabled_changed
        && !password_changed
        && !email_changed
        && display_name_changed.is_none()
        && !avatar_changed
        && !rpm_changed
        && explicit_plan.is_none()
    {
        return Ok(Json(UserView::from_record(target)));
    }

    if email_changed && let Some(email) = update.email.as_deref() {
        users::set_email(&mut tx, id, email)
            .await
            .map_err(map_user_store_err)?;
        // 登录标识变了，旧会话不能继续用；与改密同规则，操作者本人的当前会话保留。
        let keep = (identity.user_id() == id)
            .then(|| session_from_headers(&headers))
            .flatten();
        users::revoke_user_sessions(&mut tx, id, keep)
            .await
            .map_err(map_user_store_err)?;
    }
    if let Some((role, plan_id, _)) = role_plan.as_ref() {
        users::set_user_role(&mut tx, id, *role)
            .await
            .map_err(map_user_store_err)?;
        users::set_user_plan(&mut tx, id, *plan_id)
            .await
            .map_err(map_user_store_err)?;
    }
    if let Some((plan_id, _)) = explicit_plan.as_ref() {
        users::set_user_plan(&mut tx, id, *plan_id)
            .await
            .map_err(map_user_store_err)?;
    }
    if enabled_changed && let Some(enabled) = update.enabled {
        users::set_user_enabled(&mut tx, id, enabled)
            .await
            .map_err(map_user_store_err)?;
    }
    if let Some(password) = update.password {
        users::set_password(&mut tx, id, &password)
            .await
            .map_err(map_user_store_err)?;
        // 改密后吊销该用户的其他会话（留下当前这条）：否则已被窃取的会话在改密后
        // 仍有效整整 8 小时。
        let keep = (identity.user_id() == id)
            .then(|| session_from_headers(&headers))
            .flatten();
        users::revoke_user_sessions(&mut tx, id, keep)
            .await
            .map_err(map_user_store_err)?;
    }
    if let Some(name) = display_name_changed.as_deref() {
        sqlx::query("UPDATE users SET display_name = ? WHERE id = ?")
            .bind(name)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
    }
    if avatar_changed {
        let avatar = avatar_after.as_ref().and_then(|avatar| avatar.as_deref());
        users::set_avatar(&mut tx, id, avatar)
            .await
            .map_err(map_user_store_err)?;
    }
    if rpm_changed && let Some(rpm_update) = update.rate_limit_rpm {
        users::set_rate_limit_rpm(&mut tx, id, rpm_update)
            .await
            .map_err(map_user_store_err)?;
    }
    // 逐字段记变更前后值：审计要能回答「改了什么」，不只是「被改过」。
    let mut changes: Vec<String> = Vec::new();
    if email_changed {
        changes.push(format!(
            "email {} → {}（并吊销其他会话）",
            target.email,
            email_after.as_deref().unwrap_or(&target.email)
        ));
    }
    if let Some((role, plan_id, plan_name)) = role_plan.as_ref() {
        changes.push(format!(
            "role {} → {}，套餐 → {} ({})",
            target.role.as_str(),
            role.as_str(),
            plan_id,
            plan_name
        ));
    }
    if let Some((plan_id, plan_name)) = explicit_plan.as_ref() {
        changes.push(format!("套餐 → {} ({})", plan_id, plan_name));
    }
    if enabled_changed && let Some(enabled) = update.enabled {
        changes.push(format!("enabled {} → {}", target.enabled, enabled));
    }
    if password_changed {
        changes.push("重置密码（并吊销其他会话）".to_string());
    }
    if let Some(name) = display_name_changed.as_deref() {
        changes.push(format!(
            "display_name「{}」→「{}」",
            target.display_name, name
        ));
    }
    if avatar_changed {
        changes.push("更新头像".to_string());
    }
    if rpm_changed && let Some(rpm_update) = update.rate_limit_rpm {
        let before = target
            .rate_limit_rpm
            .map_or_else(|| "跟随全局".to_string(), |n| n.to_string());
        let after = rpm_update.map_or_else(|| "跟随全局".to_string(), |n| n.to_string());
        changes.push(format!("RPM 限流 {before} → {after}"));
    }
    if !changes.is_empty() {
        store::record_audit(
            &mut tx,
            identity.actor(),
            "users",
            &store::SystemLogEvent::new(
                "users.updated",
                serde_json::json!({
                    "user_id": id,
                    "email": target.email,
                    "changes": changes,
                }),
                format!("修改用户 {} ({})：{}", id, target.email, changes.join("；")),
            ),
        )
        .await
        .map_err(AdminError::Store)?;
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
    identity.require_capability(ManagementCapability::ManageUsers)?;
    let mut tx = begin_write(&deps).await?;
    let target = users::get_user_on_conn(&mut tx, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    reject_user_management(&identity, &target, None)?;
    users::delete_user(&mut tx, id, logging::unix_millis())
        .await
        .map_err(map_user_store_err)?;
    store::record_audit(
        &mut tx,
        identity.actor(),
        "users",
        &store::SystemLogEvent::new(
            "users.archived",
            serde_json::json!({ "user_id": id, "email": target.email }),
            format!(
                "归档用户 {} ({})：停用、令牌失效、会话吊销",
                id, target.email
            ),
        ),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_users(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<BulkDeleteBody<i64>>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<BulkDeleteResult<i64>>, AdminError> {
    identity.require_capability(ManagementCapability::ManageUsers)?;
    let targets = validate_bulk_targets(body.map_err(AdminError::bad_body)?.0.targets)?;
    let mut tx = begin_write(&deps).await?;
    let mut records = Vec::with_capacity(targets.len());
    for id in &targets {
        let target = users::get_user_on_conn(&mut tx, *id)
            .await
            .map_err(AdminError::Store)?
            .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
        reject_user_management(&identity, &target, None)?;
        records.push(target);
    }
    let now = logging::unix_millis();
    for target in &records {
        users::delete_user(&mut tx, target.id, now)
            .await
            .map_err(map_user_store_err)?;
    }
    store::record_audit(
        &mut tx,
        identity.actor(),
        "users",
        &store::SystemLogEvent::new(
            "users.bulk_archived",
            serde_json::json!({ "count": records.len() }),
            format!("批量归档 {} 个用户", records.len()),
        ),
    )
    .await
    .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(BulkDeleteResult::new(targets)))
}

#[derive(Debug, Serialize)]
struct UserAdminView {
    id: i64,
    email: String,
    display_name: String,
    role: ManagementRole,
    enabled: bool,
    // 不带 avatar：运营列表与详情都不渲染头像，而它可能是 MB 级 base64 data URL，
    // 逐个用户带上等于给 /users 平白挂几 MB。自己的头像走 /me。
    rate_limit_rpm: Option<u64>,
    plan_id: Option<i64>,
    plan_display_name: Option<String>,
    discount_bp: i64,
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
    identity: &ManagementIdentity,
    record: UserRecord,
    stats: Option<users::UserStatsRecord>,
) -> Result<UserAdminView, AdminError> {
    let plan = match record.plan_id {
        Some(plan_id) => plans::get_plan(pool, plan_id)
            .await
            .map_err(AdminError::Store)?,
        None => None,
    };
    let groups = visible_plan_groups(pool, identity, record.plan_id).await?;
    let wallet = store::get_user_wallet(pool, record.id)
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
        rate_limit_rpm: record.rate_limit_rpm,
        plan_id: record.plan_id,
        plan_display_name: plan.as_ref().map(|plan| plan.display_name.clone()),
        discount_bp: plan.as_ref().map_or(10_000, |plan| plan.discount_bp),
        assigned_groups: groups,
        balance_usd_micros: wallet.balance_usd_micros,
        settled_usd_micros: wallet.settled_usd_micros,
        request_count: stats.request_count,
        input_tokens: stats.input_tokens,
        output_tokens: stats.output_tokens,
        last_used_at: stats.last_used_at,
    })
}

/// 列出管理用户。
///
/// 三个维度（可用组、钱包、用量）都按批取回：逐个用户查会变成 2N+2 次查询，而
/// 未命中 stats 的用户还会各自触发一次全历史聚合。
async fn list_management_users(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
) -> Result<Json<Vec<UserAdminView>>, AdminError> {
    identity.require_capability(ManagementCapability::ManageUsers)?;
    let mut records = users::list_users(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    if identity.role() == ManagementRole::Admin {
        records.retain(|record| record.role == ManagementRole::User);
    }
    let stats_map = users::list_users_stats(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    let wallets = store::list_user_wallets(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    let mut groups_by_plan: HashMap<i64, Vec<String>> = HashMap::new();
    for (plan_id, group) in plans::list_all_plan_groups(&deps.pool)
        .await
        .map_err(AdminError::Store)?
    {
        groups_by_plan.entry(plan_id).or_default().push(group);
    }
    let plan_meta: HashMap<i64, (String, i64)> = plans::list_plans(&deps.pool)
        .await
        .map_err(AdminError::Store)?
        .into_iter()
        .map(|plan| (plan.id, (plan.display_name, plan.discount_bp)))
        .collect();
    let visible_groups = if identity.role() == ManagementRole::Root
        || identity.has_capability(ManagementCapability::ViewOtherGroups)
    {
        None
    } else if identity.has_capability(ManagementCapability::ViewOwnPlanGroups) {
        let own_plan_groups = match identity.plan_id() {
            Some(plan_id) => plans::list_plan_groups(&deps.pool, plan_id)
                .await
                .map_err(AdminError::Store)?,
            None => Vec::new(),
        };
        Some(own_plan_groups.into_iter().collect::<HashSet<String>>())
    } else {
        Some(HashSet::new())
    };
    let views = records
        .into_iter()
        .map(|record| -> Result<UserAdminView, AdminError> {
            let stats = stats_map.get(&record.id).cloned().unwrap_or_default();
            let wallet = wallets
                .get(&record.id)
                .copied()
                .ok_or_else(|| AdminError::Store(StoreError::MissingWallet(record.id)))?;
            let mut assigned_groups = record
                .plan_id
                .and_then(|plan_id| groups_by_plan.get(&plan_id).cloned())
                .unwrap_or_default();
            if let Some(visible_groups) = &visible_groups {
                assigned_groups.retain(|group| visible_groups.contains(group));
            }
            assigned_groups.sort();
            let (plan_display_name, discount_bp) = record
                .plan_id
                .and_then(|plan_id| plan_meta.get(&plan_id).cloned())
                .map_or((None, 10_000), |(name, discount)| (Some(name), discount));
            Ok(UserAdminView {
                id: record.id,
                email: record.email,
                display_name: record.display_name,
                role: record.role,
                enabled: record.enabled,
                rate_limit_rpm: record.rate_limit_rpm,
                plan_id: record.plan_id,
                plan_display_name,
                discount_bp,
                assigned_groups,
                balance_usd_micros: wallet.balance_usd_micros,
                settled_usd_micros: wallet.settled_usd_micros,
                request_count: stats.request_count,
                input_tokens: stats.input_tokens,
                output_tokens: stats.output_tokens,
                last_used_at: stats.last_used_at,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(views))
}

async fn get_management_user(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
) -> Result<Json<UserAdminView>, AdminError> {
    identity.require_capability(ManagementCapability::ManageUsers)?;
    let target = users::get_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    reject_user_management(&identity, &target, None)?;
    user_admin_view(&deps.pool, &identity, target, None)
        .await
        .map(Json)
}

/// 按当前管理员的模型组可见能力裁剪目标用户的套餐名单。
async fn visible_plan_groups(
    pool: &SqlitePool,
    identity: &ManagementIdentity,
    target_plan_id: Option<i64>,
) -> Result<Vec<String>, AdminError> {
    let Some(target_plan_id) = target_plan_id else {
        return Ok(Vec::new());
    };
    let groups = plans::list_plan_groups(pool, target_plan_id)
        .await
        .map_err(AdminError::Store)?;
    if identity.role() == ManagementRole::Root
        || identity.has_capability(ManagementCapability::ViewOtherGroups)
    {
        return Ok(groups);
    }
    if !identity.has_capability(ManagementCapability::ViewOwnPlanGroups) {
        return Ok(Vec::new());
    }
    let Some(own_plan_id) = identity.plan_id() else {
        return Ok(Vec::new());
    };
    if own_plan_id == target_plan_id {
        return Ok(groups);
    }
    let own_groups = plans::list_plan_groups(pool, own_plan_id)
        .await
        .map_err(AdminError::Store)?;
    let own_groups: HashSet<String> = own_groups.into_iter().collect();
    Ok(groups
        .into_iter()
        .filter(|group| own_groups.contains(group))
        .collect())
}
