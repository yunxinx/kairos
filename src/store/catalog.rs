//! 价格目录存储：models.dev 缓存行与上次同步时刻。
//!
//! 目录不进运行时快照：请求路径不计目录价。管理面填价与定时同步读写本表。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, SqliteConnection, SqlitePool};

use super::resources::{SETTING_CATALOG_SYNCED_AT, set_setting};
use super::{StoreError, like_substring_pattern, push_column_in, push_where_cond};

/// 目录中一条提供方 × 模型的四档单价（micro-USD / 1M tokens）。
///
/// 缺档为 `None`：目录未给出该档，填价时保持渠道现价或无法新建必填档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogModel {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub input_micros: Option<i64>,
    pub output_micros: Option<i64>,
    pub cache_read_micros: Option<i64>,
    pub cache_write_micros: Option<i64>,
}

/// 价格目录读视图：缓存行 + 上次成功同步时刻。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogView {
    /// 上次成功写入缓存的 unix 毫秒；从未同步为 `None`。
    pub synced_at: Option<i64>,
    pub models: Vec<CatalogModel>,
}

/// 目录提供方摘要：浏览器用，不拉整表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogProvider {
    pub id: String,
    pub name: String,
    pub count: u64,
}

/// 目录元数据：上次同步时刻 + 按提供方名排序的提供方列表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogMeta {
    /// 上次成功写入缓存的 unix 毫秒；从未同步为 `None`。
    pub synced_at: Option<i64>,
    pub providers: Vec<CatalogProvider>,
}

fn map_catalog_model_row(row: &sqlx::sqlite::SqliteRow) -> Result<CatalogModel, StoreError> {
    Ok(CatalogModel {
        provider_id: row.try_get("provider_id").map_err(StoreError::Query)?,
        provider_name: row.try_get("provider_name").map_err(StoreError::Query)?,
        model_id: row.try_get("model_id").map_err(StoreError::Query)?,
        input_micros: row.try_get("input_micros").map_err(StoreError::Query)?,
        output_micros: row.try_get("output_micros").map_err(StoreError::Query)?,
        cache_read_micros: row
            .try_get("cache_read_micros")
            .map_err(StoreError::Query)?,
        cache_write_micros: row
            .try_get("cache_write_micros")
            .map_err(StoreError::Query)?,
    })
}

/// 读出目录行（按提供方、模型 id 排序）。
///
/// `q` 对 `model_id` 做大小写不敏感子串匹配（SQLite `LIKE`，转义 `%`/`_`/`\`）。
/// `provider_ids` 非空时精确匹配其中任一提供方。两者都缺省则返回全表。
async fn list_catalog_models(
    pool: &SqlitePool,
    q: Option<&str>,
    provider_ids: &[String],
) -> Result<Vec<CatalogModel>, StoreError> {
    let keyword = q.map(str::trim).filter(|item| !item.is_empty());
    let mut qb = sqlx::QueryBuilder::new(
        "SELECT provider_id, provider_name, model_id, \
                input_micros, output_micros, cache_read_micros, cache_write_micros \
         FROM catalog_models",
    );
    let mut first = true;
    if let Some(keyword) = keyword {
        let pattern = like_substring_pattern(keyword);
        push_where_cond(&mut qb, &mut first, "model_id LIKE ");
        qb.push_bind(pattern);
        qb.push(" ESCAPE '\\'");
    }
    push_column_in(&mut qb, &mut first, "provider_id", provider_ids);
    qb.push(" ORDER BY provider_id, model_id");

    let rows = qb
        .build()
        .fetch_all(pool)
        .await
        .map_err(StoreError::Query)?;
    rows.iter().map(map_catalog_model_row).collect()
}

/// 按提供方名排序汇总目录提供方与模型数。
async fn list_catalog_providers(pool: &SqlitePool) -> Result<Vec<CatalogProvider>, StoreError> {
    let rows = sqlx::query(
        "SELECT provider_id, provider_name, COUNT(*) AS cnt \
         FROM catalog_models \
         GROUP BY provider_id, provider_name \
         ORDER BY provider_name, provider_id",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;

    rows.iter()
        .map(|row| {
            let count: i64 = row.try_get("cnt").map_err(StoreError::Query)?;
            Ok(CatalogProvider {
                id: row.try_get("provider_id").map_err(StoreError::Query)?,
                name: row.try_get("provider_name").map_err(StoreError::Query)?,
                count: count.max(0) as u64,
            })
        })
        .collect()
}

/// 整表替换目录缓存。
pub async fn replace_catalog_models(
    conn: &mut SqliteConnection,
    models: &[CatalogModel],
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM catalog_models")
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    for model in models {
        sqlx::query(
            "INSERT INTO catalog_models \
             (provider_id, provider_name, model_id, \
              input_micros, output_micros, cache_read_micros, cache_write_micros) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&model.provider_id)
        .bind(&model.provider_name)
        .bind(&model.model_id)
        .bind(model.input_micros)
        .bind(model.output_micros)
        .bind(model.cache_read_micros)
        .bind(model.cache_write_micros)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    }
    Ok(())
}

/// 读上次目录同步时刻；从未同步或值非法返回 `None`。
pub(crate) async fn catalog_synced_at(pool: &SqlitePool) -> Result<Option<i64>, StoreError> {
    let row = sqlx::query("SELECT setting_value FROM settings WHERE setting_key = ?")
        .bind(SETTING_CATALOG_SYNCED_AT)
        .fetch_optional(pool)
        .await
        .map_err(StoreError::Query)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let encoded: String = row.try_get("setting_value").map_err(StoreError::Query)?;
    let value: Value = serde_json::from_str(&encoded)
        .map_err(|_| StoreError::InvalidResource("catalog_synced_at 的值非法".to_string()))?;
    Ok(value.as_i64())
}

/// 写入目录上次成功同步时刻。
pub async fn set_catalog_synced_at(
    conn: &mut SqliteConnection,
    synced_at: i64,
) -> Result<(), StoreError> {
    set_setting(conn, SETTING_CATALOG_SYNCED_AT, &Value::from(synced_at)).await
}

/// 组装目录读视图。`q` 与 `provider_ids` 都缺省时返回全表。
pub async fn load_catalog_view(
    pool: &SqlitePool,
    q: Option<&str>,
    provider_ids: &[String],
) -> Result<CatalogView, StoreError> {
    Ok(CatalogView {
        synced_at: catalog_synced_at(pool).await?,
        models: list_catalog_models(pool, q, provider_ids).await?,
    })
}

/// 组装目录元数据（同步时刻 + 提供方列表），不读模型行。
pub async fn load_catalog_meta(pool: &SqlitePool) -> Result<CatalogMeta, StoreError> {
    Ok(CatalogMeta {
        synced_at: catalog_synced_at(pool).await?,
        providers: list_catalog_providers(pool).await?,
    })
}
