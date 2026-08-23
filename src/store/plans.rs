//! 套餐存储：运行时快照所需的套餐投影与管理端仍在用的套餐名单读写。
//!
//! 旧「按人分配模型组」表已由迁移删除；套餐模型组名单是用户可用模型组的唯一来源。
//! 快照加载只取请求路径用到的字段，不读备注与内部名（比照 `users::list_users_for_snapshot`）。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection, SqlitePool};

use crate::core::billing;
use crate::store::StoreError;

/// 内置 `standard` 档固定 id：新建普通用户的默认套餐。
pub const STANDARD_PLAN_ID: i64 = 1;
/// 内置 `admin` 档固定 id：新建管理员的默认套餐。
pub const ADMIN_PLAN_ID: i64 = 2;

/// 管理面套餐完整投影。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PlanRecord {
    pub id: i64,
    pub internal_name: String,
    pub display_name: String,
    pub note: String,
    pub note_visible_to_admin: bool,
    pub discount_bp: i64,
    pub default_rpm: Option<u64>,
    pub shared_rpm: Option<u64>,
    pub initial_grant_usd_micros: i64,
    pub capabilities: PlanCapabilities,
    pub shared_with_admin: bool,
    pub builtin: bool,
    pub created_at: i64,
    pub groups: Vec<String>,
}

/// 管理面套餐写入字段。
#[derive(Debug, Clone)]
pub struct PlanInput {
    pub internal_name: String,
    pub display_name: String,
    pub note: String,
    pub note_visible_to_admin: bool,
    pub discount_bp: i64,
    pub default_rpm: Option<u64>,
    pub shared_rpm: Option<u64>,
    pub initial_grant_usd_micros: i64,
    pub capabilities: PlanCapabilities,
    pub shared_with_admin: bool,
    pub groups: Vec<String>,
}

/// 套餐管理面能力开关。
///
/// 生效能力 = 套餐开关 ∩ 角色；本结构只描述套餐侧配置，不参与角色判断。
/// `#[serde(default)]` 使新增字段不破坏存量行，缺省均为关闭。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanCapabilities {
    pub manage_users: bool,
    pub assign_plan: bool,
    pub view_logs_stats: bool,
    pub settle_waive: bool,
    pub toggle_user_tokens: bool,
    pub view_own_plan_groups: bool,
    pub view_other_groups: bool,
    pub edit_prices: bool,
    pub edit_model_groups: bool,
    pub edit_unified_models: bool,
    pub edit_price_catalog: bool,
}

/// 从套餐行的 JSON 字段解析能力开关。
pub(crate) fn parse_capabilities_json(
    plan_id: i64,
    raw: &str,
) -> Result<PlanCapabilities, StoreError> {
    serde_json::from_str(raw).map_err(|_| {
        StoreError::InvalidResource(format!("套餐 {plan_id} 的 capabilities_json 非法"))
    })
}

/// 把能力开关编码成套餐行的 JSON 字段，供管理写路径复用。
pub fn serialize_capabilities_json(capabilities: &PlanCapabilities) -> Result<String, StoreError> {
    serde_json::to_string(capabilities)
        .map_err(|err| StoreError::InvalidResource(format!("套餐能力序列化失败: {err}")))
}

/// 按 id 读取一档套餐的能力开关。
pub(crate) async fn load_plan_capabilities(
    pool: &SqlitePool,
    plan_id: i64,
) -> Result<PlanCapabilities, StoreError> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT capabilities_json FROM plans WHERE id = ?")
            .bind(plan_id)
            .fetch_optional(pool)
            .await
            .map_err(StoreError::Query)?;
    let Some(raw) = raw else {
        return Err(StoreError::InvalidResource(format!(
            "套餐 {plan_id} 不存在"
        )));
    };
    parse_capabilities_json(plan_id, &raw)
}

/// 快照加载用的套餐投影；不携带备注、内部名、分享与创建时间等管理面字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanSnapshot {
    pub(crate) id: i64,
    pub(crate) discount_bp: i64,
    pub(crate) default_rpm: Option<u64>,
    pub(crate) shared_rpm: Option<u64>,
    pub(crate) groups: HashSet<String>,
    pub(crate) capabilities: PlanCapabilities,
}

/// 读出快照所需的全部套餐及其模型组名单。
pub(crate) async fn list_plans_for_snapshot(
    pool: &SqlitePool,
) -> Result<Vec<PlanSnapshot>, StoreError> {
    let rows = sqlx::query(
        "SELECT id, discount_bp, default_rpm, shared_rpm, capabilities_json \
         FROM plans ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;

    let group_rows = sqlx::query(
        "SELECT plan_id, group_name FROM plan_model_groups ORDER BY plan_id, group_name",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;

    let mut groups_by_plan: std::collections::HashMap<i64, HashSet<String>> =
        std::collections::HashMap::new();
    for row in &group_rows {
        let plan_id: i64 = row.try_get("plan_id").map_err(StoreError::Query)?;
        let group_name: String = row.try_get("group_name").map_err(StoreError::Query)?;
        groups_by_plan
            .entry(plan_id)
            .or_default()
            .insert(group_name);
    }

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: i64 = row.try_get("id").map_err(StoreError::Query)?;
        let default_rpm: Option<i64> = row.try_get("default_rpm").map_err(StoreError::Query)?;
        let shared_rpm: Option<i64> = row.try_get("shared_rpm").map_err(StoreError::Query)?;
        let capabilities_json: String = row
            .try_get("capabilities_json")
            .map_err(StoreError::Query)?;
        let capabilities = parse_capabilities_json(id, &capabilities_json)?;
        let discount_bp: i64 = row.try_get("discount_bp").map_err(StoreError::Query)?;
        if !(billing::MIN_DISCOUNT_BP..=billing::MAX_DISCOUNT_BP).contains(&discount_bp) {
            return Err(StoreError::InvalidResource(format!(
                "套餐 {id} 的 discount_bp 超出合法范围: {discount_bp}"
            )));
        }
        out.push(PlanSnapshot {
            id,
            discount_bp,
            default_rpm: rpm_from_db(default_rpm)?,
            shared_rpm: rpm_from_db(shared_rpm)?,
            groups: groups_by_plan.remove(&id).unwrap_or_default(),
            capabilities,
        });
    }
    Ok(out)
}

/// 按套餐读模型组名单（排序）。
pub(crate) async fn list_plan_groups(
    pool: &SqlitePool,
    plan_id: i64,
) -> Result<Vec<String>, StoreError> {
    let mut conn = pool.acquire().await.map_err(StoreError::Query)?;
    list_plan_groups_on_conn(&mut conn, plan_id).await
}

/// 在现有连接/事务上按套餐读模型组名单（排序）。
pub(crate) async fn list_plan_groups_on_conn(
    conn: &mut SqliteConnection,
    plan_id: i64,
) -> Result<Vec<String>, StoreError> {
    if plan_exists_on_conn(conn, plan_id).await? {
        let rows = sqlx::query(
            "SELECT group_name FROM plan_model_groups WHERE plan_id = ? ORDER BY group_name",
        )
        .bind(plan_id)
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
        let mut names = Vec::with_capacity(rows.len());
        for row in &rows {
            names.push(row.try_get("group_name").map_err(StoreError::Query)?);
        }
        Ok(names)
    } else {
        Ok(Vec::new())
    }
}

/// 整体替换套餐模型组名单；空名单表示该档没有任何可用组。
///
/// 这是旧按人分配表删除后的过渡期操作：管理端 `/users/{id}/model-groups`
/// 在 06 号票移除前，直接编辑该用户当前所挂套餐的名单。
pub(crate) async fn replace_plan_groups(
    conn: &mut SqliteConnection,
    plan_id: i64,
    groups: &[String],
) -> Result<Vec<String>, StoreError> {
    if !plan_exists_on_conn(conn, plan_id).await? {
        return Err(StoreError::InvalidResource(format!(
            "套餐 {plan_id} 不存在"
        )));
    }
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for group in groups {
        let name = group.trim();
        if name.is_empty() {
            return Err(StoreError::InvalidResource("模型组名不能为空".to_string()));
        }
        if !seen.insert(name.to_string()) {
            continue;
        }
        if crate::store::resources::get_model_group(conn, name)
            .await?
            .is_none()
        {
            return Err(StoreError::InvalidResource(format!("模型组 {name} 不存在")));
        }
        unique.push(name.to_string());
    }
    sqlx::query("DELETE FROM plan_model_groups WHERE plan_id = ?")
        .bind(plan_id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    for name in &unique {
        sqlx::query("INSERT INTO plan_model_groups (plan_id, group_name) VALUES (?, ?)")
            .bind(plan_id)
            .bind(name)
            .execute(&mut *conn)
            .await
            .map_err(StoreError::Query)?;
    }
    unique.sort();
    Ok(unique)
}

/// 读出全部套餐的模型组归属，供管理列表批量组装。
pub(crate) async fn list_all_plan_groups(
    pool: &SqlitePool,
) -> Result<Vec<(i64, String)>, StoreError> {
    let rows = sqlx::query(
        "SELECT plan_id, group_name FROM plan_model_groups ORDER BY plan_id, group_name",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push((
            row.try_get("plan_id").map_err(StoreError::Query)?,
            row.try_get("group_name").map_err(StoreError::Query)?,
        ));
    }
    Ok(out)
}

async fn plan_exists_on_conn(
    conn: &mut SqliteConnection,
    plan_id: i64,
) -> Result<bool, StoreError> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM plans WHERE id = ?")
        .bind(plan_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(exists.is_some())
}

fn rpm_from_db(value: Option<i64>) -> Result<Option<u64>, StoreError> {
    value
        .map(|v| {
            u64::try_from(v)
                .map_err(|_| StoreError::InvalidResource("数据库中的 RPM 为负数".to_string()))
        })
        .transpose()
}

fn rpm_to_db(value: Option<u64>) -> Result<Option<i64>, StoreError> {
    value
        .map(|v| {
            i64::try_from(v)
                .map_err(|_| StoreError::InvalidResource("RPM 超出 SQLite 整数范围".to_string()))
        })
        .transpose()
}

fn bool_from_db(value: i64) -> bool {
    value != 0
}

async fn map_plan_row(
    conn: &mut SqliteConnection,
    row: &sqlx::sqlite::SqliteRow,
) -> Result<PlanRecord, StoreError> {
    let id: i64 = row.try_get("id").map_err(StoreError::Query)?;
    let raw: String = row
        .try_get("capabilities_json")
        .map_err(StoreError::Query)?;
    Ok(PlanRecord {
        id,
        internal_name: row.try_get("internal_name").map_err(StoreError::Query)?,
        display_name: row.try_get("display_name").map_err(StoreError::Query)?,
        note: row.try_get("note").map_err(StoreError::Query)?,
        note_visible_to_admin: bool_from_db(
            row.try_get("note_visible_to_admin")
                .map_err(StoreError::Query)?,
        ),
        discount_bp: row.try_get("discount_bp").map_err(StoreError::Query)?,
        default_rpm: rpm_from_db(row.try_get("default_rpm").map_err(StoreError::Query)?)?,
        shared_rpm: rpm_from_db(row.try_get("shared_rpm").map_err(StoreError::Query)?)?,
        initial_grant_usd_micros: row
            .try_get("initial_grant_usd_micros")
            .map_err(StoreError::Query)?,
        capabilities: parse_capabilities_json(id, &raw)?,
        shared_with_admin: bool_from_db(
            row.try_get("shared_with_admin")
                .map_err(StoreError::Query)?,
        ),
        builtin: bool_from_db(row.try_get("builtin").map_err(StoreError::Query)?),
        created_at: row.try_get("created_at").map_err(StoreError::Query)?,
        groups: list_plan_groups_on_conn(conn, id).await?,
    })
}

/// 读取全部套餐（按 id 排序）。
pub async fn list_plans(pool: &SqlitePool) -> Result<Vec<PlanRecord>, StoreError> {
    let mut conn = pool.acquire().await.map_err(StoreError::Query)?;
    let rows = sqlx::query(
        "SELECT id, internal_name, display_name, note, note_visible_to_admin, \
        discount_bp, default_rpm, shared_rpm, initial_grant_usd_micros, capabilities_json, \
        shared_with_admin, builtin, created_at FROM plans ORDER BY id",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(map_plan_row(&mut conn, row).await?);
    }
    Ok(out)
}

/// 按 id 读取套餐。
pub async fn get_plan(pool: &SqlitePool, id: i64) -> Result<Option<PlanRecord>, StoreError> {
    let mut conn = pool.acquire().await.map_err(StoreError::Query)?;
    get_plan_on_conn(&mut conn, id).await
}

/// 在现有连接上按 id 读取套餐。
pub async fn get_plan_on_conn(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<Option<PlanRecord>, StoreError> {
    let row = sqlx::query(
        "SELECT id, internal_name, display_name, note, note_visible_to_admin, \
        discount_bp, default_rpm, shared_rpm, initial_grant_usd_micros, capabilities_json, \
        shared_with_admin, builtin, created_at FROM plans WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    match row {
        Some(row) => Ok(Some(map_plan_row(conn, &row).await?)),
        None => Ok(None),
    }
}

/// 读取套餐起步金；调用方应在创建用户事务中使用。
pub async fn initial_grant_on_conn(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<i64, StoreError> {
    sqlx::query_scalar("SELECT initial_grant_usd_micros FROM plans WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(StoreError::Query)?
        .ok_or_else(|| StoreError::InvalidResource(format!("套餐 {id} 不存在")))
}

/// 新建套餐，返回带数据库 id 的资源。
pub async fn insert_plan(
    conn: &mut SqliteConnection,
    input: &PlanInput,
    now: i64,
) -> Result<PlanRecord, StoreError> {
    validate_input(input)?;
    let capabilities_json = serialize_capabilities_json(&input.capabilities)?;
    let default_rpm = rpm_to_db(input.default_rpm)?;
    let shared_rpm = rpm_to_db(input.shared_rpm)?;
    let result = sqlx::query(
        "INSERT INTO plans (internal_name, display_name, note, note_visible_to_admin, \
         discount_bp, default_rpm, shared_rpm, initial_grant_usd_micros, capabilities_json, \
         shared_with_admin, builtin, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?)",
    )
    .bind(&input.internal_name)
    .bind(&input.display_name)
    .bind(&input.note)
    .bind(input.note_visible_to_admin)
    .bind(input.discount_bp)
    .bind(default_rpm)
    .bind(shared_rpm)
    .bind(input.initial_grant_usd_micros)
    .bind(capabilities_json)
    .bind(input.shared_with_admin)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    let id = result.last_insert_rowid();
    replace_plan_groups(conn, id, &input.groups).await?;
    get_plan_on_conn(conn, id)
        .await?
        .ok_or_else(|| StoreError::InvalidResource(format!("套餐 {id} 创建后不可读")))
}

/// 整体更新套餐（id 与 builtin 身份不变）。
pub async fn update_plan(
    conn: &mut SqliteConnection,
    id: i64,
    input: &PlanInput,
) -> Result<PlanRecord, StoreError> {
    validate_input(input)?;
    let capabilities_json = serialize_capabilities_json(&input.capabilities)?;
    let result = sqlx::query(
        "UPDATE plans SET internal_name = ?, display_name = ?, note = ?, note_visible_to_admin = ?, \
         discount_bp = ?, default_rpm = ?, shared_rpm = ?, initial_grant_usd_micros = ?, \
         capabilities_json = ?, shared_with_admin = ? WHERE id = ?",
    )
    .bind(&input.internal_name)
    .bind(&input.display_name)
    .bind(&input.note)
    .bind(input.note_visible_to_admin)
    .bind(input.discount_bp)
    .bind(rpm_to_db(input.default_rpm)?)
    .bind(rpm_to_db(input.shared_rpm)?)
    .bind(input.initial_grant_usd_micros)
    .bind(capabilities_json)
    .bind(input.shared_with_admin)
    .bind(id)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    if result.rows_affected() == 0 {
        return Err(StoreError::InvalidResource(format!("套餐 {id} 不存在")));
    }
    replace_plan_groups(conn, id, &input.groups).await?;
    get_plan_on_conn(conn, id)
        .await?
        .ok_or_else(|| StoreError::InvalidResource(format!("套餐 {id} 更新后不可读")))
}

fn validate_input(input: &PlanInput) -> Result<(), StoreError> {
    if input.internal_name.trim().is_empty() || input.display_name.trim().is_empty() {
        return Err(StoreError::InvalidResource("套餐名称不能为空".to_string()));
    }
    if !(billing::MIN_DISCOUNT_BP..=billing::MAX_DISCOUNT_BP).contains(&input.discount_bp) {
        return Err(StoreError::InvalidResource(
            "discount_bp 超出合法范围".to_string(),
        ));
    }
    if input.initial_grant_usd_micros < 0 {
        return Err(StoreError::InvalidResource("起步金不能为负数".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_json_defaults_new_fields_to_off() {
        let capabilities =
            parse_capabilities_json(7, "{\"manage_users\":true}").expect("缺省字段应能解析");
        assert!(capabilities.manage_users);
        assert!(!capabilities.assign_plan);
        assert!(!capabilities.edit_price_catalog);
    }

    #[test]
    fn capability_json_roundtrips_all_named_switches() {
        let capabilities = PlanCapabilities {
            manage_users: true,
            assign_plan: true,
            view_logs_stats: true,
            settle_waive: true,
            toggle_user_tokens: true,
            view_own_plan_groups: true,
            view_other_groups: true,
            edit_prices: true,
            edit_model_groups: true,
            edit_unified_models: true,
            edit_price_catalog: true,
        };
        let raw = serialize_capabilities_json(&capabilities).expect("能力应能编码");
        assert_eq!(
            parse_capabilities_json(7, &raw).expect("编码结果应能回读"),
            capabilities
        );
    }
}
