//! 令牌管理：属性、启停与余额分别走窄接口。

use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

use crate::gateway::logging;
use crate::store;
use crate::store::plans;
use crate::store::resources::{Token, TokenAttributes, TokenRecord};
use crate::store::users::{self, ManagementRole};

use super::auth::{ManagementCapability, ManagementIdentity};
use super::{
    AdminDeps, AdminError, BulkDeleteBody, BulkDeleteResult, begin_write, db_err, reload_and_swap,
    validate_bulk_targets,
};

pub(super) fn routes() -> Router<AdminDeps> {
    Router::new()
        .route(
            "/tokens",
            get(list_tokens).post(create_token).delete(delete_tokens),
        )
        .route("/tokens/{id}", put(update_token).delete(delete_token))
        .route("/tokens/{id}/enabled", put(set_token_enabled))
        .route(
            "/tokens/{id}/balance-adjustments",
            post(super::token_balance::adjust_token_balance),
        )
}

/// 令牌读响应 wire 契约：定义字段 + 生命周期元数据 + 该令牌累计结算。
#[derive(Debug, Serialize)]
pub(super) struct TokenView {
    pub(super) id: i64,
    pub(super) token_key: String,
    pub(super) name: String,
    pub(super) limit_usd_micros: Option<i64>,
    pub(super) rate_limit_rpm: Option<u64>,
    pub(super) enabled: bool,
    pub(super) model_group: String,
    pub(super) created_at: i64,
    pub(super) last_used_at: Option<i64>,
    pub(super) settled_usd_micros: i64,
    /// 可用余额；`None` 表示无限额。
    pub(super) balance_usd_micros: Option<i64>,
}

impl TokenView {
    fn from_record(record: TokenRecord, settled_usd_micros: i64) -> Result<Self, AdminError> {
        let balance_usd_micros =
            available_balance(record.token.limit_usd_micros, settled_usd_micros)?;
        Ok(Self {
            id: record.id,
            token_key: record.token.token_key,
            name: record.token.name,
            limit_usd_micros: record.token.limit_usd_micros,
            rate_limit_rpm: record.token.rate_limit_rpm,
            enabled: record.token.enabled,
            model_group: record.token.model_group,
            created_at: record.created_at,
            last_used_at: record.last_used_at,
            settled_usd_micros,
            balance_usd_micros,
        })
    }

    fn from_record_masked(
        record: TokenRecord,
        settled_usd_micros: i64,
    ) -> Result<Self, AdminError> {
        let masked = mask_token_key(&record.token.token_key);
        let mut view = Self::from_record(record, settled_usd_micros)?;
        view.token_key = masked;
        Ok(view)
    }
}

pub(super) fn available_balance(
    limit_usd_micros: Option<i64>,
    settled_usd_micros: i64,
) -> Result<Option<i64>, AdminError> {
    limit_usd_micros
        .map(|limit| {
            limit.checked_sub(settled_usd_micros).ok_or_else(|| {
                AdminError::Store(store::StoreError::InvalidResource(
                    "令牌余额超出整数范围".to_string(),
                ))
            })
        })
        .transpose()
}

async fn token_view(pool: &SqlitePool, record: TokenRecord) -> Result<TokenView, AdminError> {
    let settled = store::get_token_settled(pool, &record.token.token_key)
        .await
        .map_err(AdminError::Store)?;
    TokenView::from_record(record, settled)
}

async fn token_view_masked(
    pool: &SqlitePool,
    record: TokenRecord,
) -> Result<TokenView, AdminError> {
    let settled = store::get_token_settled(pool, &record.token.token_key)
        .await
        .map_err(AdminError::Store)?;
    TokenView::from_record_masked(record, settled)
}

pub(super) async fn list_user_tokens(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<TokenView>>, AdminError> {
    identity.require_capability(ManagementCapability::ToggleUserTokens)?;
    let target = users::get_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {id} 不存在")))?;
    super::reject_user_management(&identity, &target, None)?;
    let records = store::resources::list_token_records_for_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?;
    let settled = store::list_token_settled_for_user(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?;
    let views = records
        .into_iter()
        .map(|record| {
            let amount = settled.get(&record.token.token_key).copied().unwrap_or(0);
            TokenView::from_record_masked(record, amount)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(views))
}

pub(super) async fn list_tokens(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
) -> Result<Json<Vec<TokenView>>, AdminError> {
    let records = store::resources::list_token_records_for_user(&deps.pool, identity.user_id())
        .await
        .map_err(AdminError::Store)?;
    let settled = store::list_token_settled_for_user(&deps.pool, identity.user_id())
        .await
        .map_err(AdminError::Store)?;
    let views = records
        .into_iter()
        .map(|record| {
            let amount = settled.get(&record.token.token_key).copied().unwrap_or(0);
            TokenView::from_record(record, amount)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(views))
}

/// 按库生成的 id 读回一条令牌记录；不存在返回 `NotFound`。
async fn read_token_record(deps: &AdminDeps, id: i64) -> Result<TokenRecord, AdminError> {
    store::resources::get_token_record(&deps.pool, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("令牌 {id} 不存在")))
}

/// 新建后按 key 回读：`create_token` 自己生成 key，尚不知道库发的 id。
async fn read_token_record_by_key(
    deps: &AdminDeps,
    token_key: &str,
) -> Result<TokenRecord, AdminError> {
    store::resources::get_token_record_by_key(&deps.pool, token_key)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound("令牌不存在".to_string()))
}

/// 解析路径中的令牌 id；非整数不标识任何令牌，按不存在处理（404）。
pub(super) fn parse_token_id(raw: &str) -> Result<i64, AdminError> {
    raw.parse::<i64>()
        .map_err(|_| AdminError::NotFound(format!("令牌 {raw} 不存在")))
}

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

fn validate_token_attributes(attributes: &TokenAttributes) -> Result<(), AdminError> {
    if attributes.name.trim().is_empty() {
        return Err(AdminError::InvalidBody("name 不能为空".to_string()));
    }
    if let Some(rpm) = attributes.rate_limit_rpm
        && i64::try_from(rpm).is_err()
    {
        return Err(AdminError::InvalidBody(
            "rate_limit_rpm 超出范围".to_string(),
        ));
    }
    if attributes.model_group.trim().is_empty() {
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
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
}

/// 事务内校验令牌可绑定的组：组必须存在；非 root 的组还必须在所挂套餐名单里。
///
/// 授权依据与写入须同一写事务（`mod.rs` 的 `BEGIN IMMEDIATE` 原则）：先读快照
/// 再开事务的间隙里，组可能刚被删除或刚从该套餐名单撤下。
async fn reject_invalid_group_binding(
    conn: &mut SqliteConnection,
    identity: &ManagementIdentity,
    group: &str,
) -> Result<(), AdminError> {
    if crate::store::resources::get_model_group(conn, group)
        .await
        .map_err(AdminError::Store)?
        .is_none()
    {
        return Err(AdminError::NotFound(format!("模型组 {group} 不存在")));
    }
    let plan_id = match (identity.role(), identity.plan_id()) {
        // root 不挂套餐，等价于运行时的 `PlanBinding::Unrestricted`。
        (ManagementRole::Root, None) => return Ok(()),
        (_, Some(plan_id)) => plan_id,
        (_, None) => return Err(AdminError::InvalidBody("用户未挂套餐".to_string())),
    };
    let assigned = plans::list_plan_groups_on_conn(conn, plan_id)
        .await
        .map_err(AdminError::Store)?;
    if assigned.iter().any(|name| name == group) {
        Ok(())
    } else {
        Err(AdminError::InvalidBody("模型组不在可用名单中".to_string()))
    }
}

/// 与 Web UI `maskTokenKey` 同款：长 key 保留前后 8 个字符，短 key 完全掩码。
pub(super) fn mask_token_key(key: &str) -> String {
    const EDGE: usize = 8;
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= EDGE * 2 {
        "******".to_string()
    } else {
        let prefix: String = chars[..EDGE].iter().collect();
        let suffix: String = chars[chars.len() - EDGE..].iter().collect();
        format!("{prefix}******{suffix}")
    }
}

/// 完整定义与删除只属于令牌所有者。
pub(super) fn reject_cross_owner_mutation(
    identity: &ManagementIdentity,
    existing: &TokenRecord,
) -> Result<(), AdminError> {
    if existing.token.user_id == identity.user_id() {
        Ok(())
    } else {
        Err(AdminError::Forbidden)
    }
}

/// admin/root 可启停普通用户令牌；普通用户只能操作自己的令牌。
async fn reject_token_toggle_on_conn(
    conn: &mut SqliteConnection,
    identity: &ManagementIdentity,
    existing: &TokenRecord,
) -> Result<(), AdminError> {
    if existing.token.user_id == identity.user_id() {
        return Ok(());
    }
    identity.require_capability(ManagementCapability::ToggleUserTokens)?;
    if !identity.role().at_least(ManagementRole::Admin) {
        return Err(AdminError::Forbidden);
    }
    let owner = users::get_user_on_conn(conn, existing.token.user_id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("用户 {} 不存在", existing.token.user_id)))?;
    if owner.role != ManagementRole::User {
        return Err(AdminError::Forbidden);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TokenEnabledUpdate {
    enabled: bool,
}

/// 记录令牌启停变更；事件与实际写入共用同一事务。
async fn record_enabled_change(
    conn: &mut SqliteConnection,
    identity: &ManagementIdentity,
    existing: &TokenRecord,
    enabled: bool,
) -> Result<(), AdminError> {
    store::record_audit(
        conn,
        identity.actor(),
        "tokens",
        &store::SystemLogEvent::new(
            "tokens.enabled_changed",
            serde_json::json!({
                "user_id": existing.token.user_id,
                "token_id": existing.id,
                "token_name": existing.token.name,
                "before_enabled": existing.token.enabled,
                "enabled": enabled,
            }),
            format!(
                "修改用户 {} 的令牌 {}（{}）状态 {} → {}",
                existing.token.user_id,
                existing.id,
                existing.token.name,
                if existing.token.enabled {
                    "启用"
                } else {
                    "停用"
                },
                if enabled { "启用" } else { "停用" }
            ),
        ),
    )
    .await
    .map_err(AdminError::Store)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TokenCreate {
    name: String,
    /// 新令牌的初始可用余额；`None` 表示无限额。
    balance_usd_micros: Option<i64>,
    #[serde(default)]
    rate_limit_rpm: Option<u64>,
    enabled: bool,
    #[serde(default = "crate::store::resources::default_model_group")]
    model_group: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TokenUpdate {
    name: String,
    #[serde(default)]
    rate_limit_rpm: Option<u64>,
    enabled: bool,
    #[serde(default = "crate::store::resources::default_model_group")]
    model_group: String,
    /// 可选余额命令；存在时与属性更新共用同一事务。
    #[serde(default)]
    balance_change: Option<super::token_balance::TokenBalanceCommand>,
}

const TOKEN_KEY_PREFIX: &str = "ks-";
const TOKEN_KEY_RANDOM_LEN: usize = 64;

fn generate_token_key() -> String {
    use rand::distr::{Alphanumeric, SampleString};
    let random_part = Alphanumeric.sample_string(&mut rand::rng(), TOKEN_KEY_RANDOM_LEN);
    format!("{TOKEN_KEY_PREFIX}{random_part}")
}

pub(super) async fn create_token(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<TokenCreate>, axum::extract::rejection::JsonRejection>,
) -> Result<(axum::http::StatusCode, Json<TokenView>), AdminError> {
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
        // 新令牌尚无累计结算，因此初始余额与累计上限数值相同。
        limit_usd_micros: create.balance_usd_micros,
        rate_limit_rpm: create.rate_limit_rpm,
        enabled: create.enabled,
        model_group: create.model_group.trim().to_string(),
        user_id: identity.user_id(),
    };
    validate_token(&token)?;
    let now = logging::unix_millis();
    let mut tx = begin_write(&deps).await?;
    reject_invalid_group_binding(&mut tx, &identity, &token.model_group).await?;
    crate::store::resources::insert_token(&mut tx, &token, now)
        .await
        .map_err(AdminError::Store)?;
    crate::store::initialize_token_settlement(&mut tx, &token.token_key, 0, now)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let created = read_token_record_by_key(&deps, &token.token_key).await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(token_view(&deps.pool, created).await?),
    ))
}

pub(super) async fn update_token(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(raw_id): Path<String>,
    body: Result<Json<TokenUpdate>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<TokenView>, AdminError> {
    let id = parse_token_id(&raw_id)?;
    let update = body.map_err(AdminError::bad_body)?.0;
    let balance_change = update.balance_change;
    let attributes = TokenAttributes {
        name: update.name,
        rate_limit_rpm: update.rate_limit_rpm,
        enabled: update.enabled,
        model_group: update.model_group.trim().to_string(),
    };
    let mut tx = begin_write(&deps).await?;
    let existing = store::resources::get_token_record_on_conn(&mut tx, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("令牌 {id} 不存在")))?;
    reject_cross_owner_mutation(&identity, &existing)?;
    validate_token_attributes(&attributes)?;
    reject_invalid_group_binding(&mut tx, &identity, &attributes.model_group).await?;
    crate::store::resources::update_token_attributes(&mut tx, id, &attributes)
        .await
        .map_err(AdminError::Store)?;
    if existing.token.enabled != attributes.enabled {
        record_enabled_change(&mut tx, &identity, &existing, attributes.enabled).await?;
    }
    if let Some(command) = balance_change {
        super::token_balance::apply_token_balance_command(
            &mut tx,
            &identity,
            id,
            &existing,
            command,
            &attributes.name,
        )
        .await?;
    }
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_token_record(&deps, id).await?;
    token_view(&deps.pool, updated).await.map(Json)
}

/// 幂等地设置令牌启用状态。
///
/// 这是跨归属运营唯一开放的令牌写接口；请求体只有 `enabled`，因此不能夹带名称、
/// 限额、模型组或密钥变更。所有授权事实与写入都在同一 SQLite 写事务中读取。
pub(super) async fn set_token_enabled(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(raw_id): Path<String>,
    body: Result<Json<TokenEnabledUpdate>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<TokenView>, AdminError> {
    let id = parse_token_id(&raw_id)?;
    let enabled = body.map_err(AdminError::bad_body)?.0.enabled;
    let mut tx = begin_write(&deps).await?;
    let existing = store::resources::get_token_record_on_conn(&mut tx, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("令牌 {id} 不存在")))?;
    reject_token_toggle_on_conn(&mut tx, &identity, &existing).await?;
    let cross_owner = existing.token.user_id != identity.user_id();
    if existing.token.enabled != enabled {
        store::resources::set_token_enabled(&mut tx, id, enabled)
            .await
            .map_err(AdminError::Store)?;
        record_enabled_change(&mut tx, &identity, &existing, enabled).await?;
    }
    tx.commit().await.map_err(db_err)?;
    if existing.token.enabled != enabled {
        reload_and_swap(&deps).await?;
    }
    let updated = read_token_record(&deps, id).await?;
    if cross_owner {
        token_view_masked(&deps.pool, updated).await.map(Json)
    } else {
        token_view(&deps.pool, updated).await.map(Json)
    }
}

pub(super) async fn delete_token(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    Path(raw_id): Path<String>,
) -> Result<Json<TokenView>, AdminError> {
    let id = parse_token_id(&raw_id)?;
    let mut tx = begin_write(&deps).await?;
    let deleted = store::resources::get_token_record_on_conn(&mut tx, id)
        .await
        .map_err(AdminError::Store)?
        .ok_or_else(|| AdminError::NotFound(format!("令牌 {id} 不存在")))?;
    reject_cross_owner_mutation(&identity, &deleted)?;
    let settled = store::get_token_settled_on_conn(&mut tx, &deleted.token.token_key)
        .await
        .map_err(AdminError::Store)?;
    store::delete_token_balance(&mut tx, &deleted.token.token_key)
        .await
        .map_err(AdminError::Store)?;
    store::resources::delete_token(&mut tx, id)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    TokenView::from_record(deleted, settled).map(Json)
}

async fn delete_tokens(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<BulkDeleteBody<i64>>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<BulkDeleteResult<i64>>, AdminError> {
    let targets = validate_bulk_targets(body.map_err(AdminError::bad_body)?.0.targets)?;
    let mut tx = begin_write(&deps).await?;
    let mut records = Vec::with_capacity(targets.len());
    for id in &targets {
        let record = store::resources::get_token_record_on_conn(&mut tx, *id)
            .await
            .map_err(AdminError::Store)?
            .ok_or_else(|| AdminError::NotFound(format!("令牌 {id} 不存在")))?;
        reject_cross_owner_mutation(&identity, &record)?;
        records.push(record);
    }
    for record in &records {
        store::delete_token_balance(&mut tx, &record.token.token_key)
            .await
            .map_err(AdminError::Store)?;
        store::resources::delete_token(&mut tx, record.id)
            .await
            .map_err(AdminError::Store)?;
    }
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    Ok(Json(BulkDeleteResult::new(targets)))
}

#[cfg(test)]
mod tests {
    use super::mask_token_key;

    #[test]
    fn token_keys_are_always_masked_for_aggregate_views() {
        assert_eq!(mask_token_key("sk-short"), "******");
        assert_eq!(mask_token_key("0123456789abcdef"), "******");
        assert_eq!(
            mask_token_key("0123456789abcdefg"),
            "01234567******9abcdefg"
        );
    }
}
