//! 价格目录缓存管理。

use axum::{
    Extension, Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use serde::Deserialize;

use crate::catalog;
use crate::gateway::logging;
use crate::store::catalog::{CatalogMeta, CatalogModel, CatalogView};

use super::auth::{ManagementCapability, ManagementIdentity};
use super::{AdminDeps, AdminError, db_err, parse_comma_list};

pub(super) fn routes() -> Router<AdminDeps> {
    Router::new()
        .route("/catalog", get(get_catalog).put(put_catalog))
        .route("/catalog/meta", get(get_catalog_meta))
        .route("/catalog/sync", post(sync_catalog))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogPut {
    models: Vec<CatalogModel>,
}

/// `GET /catalog` 查询参数。两个都缺省（或空串）时返回全表，兼容 PUT 后 roundtrip。
///
/// `provider_id` 为逗号分隔的提供方 id。axum 标准 `Query` 走 `serde_urlencoded`，
/// 同一键重复（`?provider_id=a&provider_id=b`）不会反序列化成 `Vec`（那是
/// `axum_extra::extract::Query`）；故用单个字符串。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogQuery {
    q: Option<String>,
    provider_id: Option<String>,
}

/// 读价格目录缓存；可按 `q` / `provider_id` 过滤。
async fn get_catalog(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    query: Result<Query<CatalogQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<CatalogView>, AdminError> {
    identity.require_capability(ManagementCapability::EditPriceCatalog)?;
    let params = query
        .map_err(|rejection| AdminError::InvalidBody(format!("查询参数非法: {rejection}")))?
        .0;
    let q = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty());
    let provider_ids = parse_comma_list(params.provider_id.as_deref());
    let view = crate::store::catalog::load_catalog_view(&deps.pool, q, &provider_ids)
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(view))
}

/// 读目录元数据：上次同步时刻与提供方列表，不返回模型行。
async fn get_catalog_meta(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
) -> Result<Json<CatalogMeta>, AdminError> {
    identity.require_capability(ManagementCapability::EditPriceCatalog)?;
    let meta = crate::store::catalog::load_catalog_meta(&deps.pool)
        .await
        .map_err(AdminError::Store)?;
    Ok(Json(meta))
}

/// 整表替换价格目录缓存（供导入与测试播种）；同步时刻记为现在。
async fn put_catalog(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<CatalogPut>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<CatalogView>, AdminError> {
    identity.require_capability(ManagementCapability::EditPriceCatalog)?;
    let Json(CatalogPut { models }) = body.map_err(AdminError::bad_body)?;
    let synced_at = logging::unix_millis();
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::catalog::replace_catalog_models(&mut tx, &models)
        .await
        .map_err(AdminError::Store)?;
    crate::store::catalog::set_catalog_synced_at(&mut tx, synced_at)
        .await
        .map_err(AdminError::Store)?;
    tx.commit().await.map_err(db_err)?;
    Ok(Json(CatalogView {
        synced_at: Some(synced_at),
        models,
    }))
}

/// 从 models.dev 拉取并替换价格目录缓存。
async fn sync_catalog(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
) -> Result<Json<CatalogView>, AdminError> {
    identity.require_capability(ManagementCapability::EditPriceCatalog)?;
    let view =
        catalog::fetch_and_replace(&deps.pool, &deps.client, catalog::MODELS_DEV_CATALOG_URL)
            .await
            .map_err(catalog_err)?;
    Ok(Json(view))
}

/// 目录拉取失败视为上游错误；存储失败保持 500。
fn catalog_err(err: catalog::CatalogError) -> AdminError {
    match err {
        catalog::CatalogError::Store(err) => AdminError::Store(err),
        other => AdminError::Upstream(other.to_string()),
    }
}
