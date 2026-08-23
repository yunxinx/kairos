//! 套餐存储：运行时快照所需的套餐投影与管理端仍在用的套餐名单读写。
//!
//! 旧「按人分配模型组」表已由迁移删除；套餐模型组名单是用户可用模型组的唯一来源。
//! 快照加载只取请求路径用到的字段，不读备注与内部名（比照 `users::list_users_for_snapshot`）。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection, SqlitePool};

use crate::store::StoreError;

/// 内置 `standard` 档固定 id：新建普通用户的默认套餐。
pub const STANDARD_PLAN_ID: i64 = 1;
/// 内置 `admin` 档固定 id：新建管理员的默认套餐。
pub const ADMIN_PLAN_ID: i64 = 2;

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
        let capabilities: PlanCapabilities =
            serde_json::from_str(&capabilities_json).map_err(|_| {
                StoreError::InvalidResource(format!("套餐 {id} 的 capabilities_json 非法"))
            })?;
        out.push(PlanSnapshot {
            id,
            discount_bp: row.try_get("discount_bp").map_err(StoreError::Query)?,
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
