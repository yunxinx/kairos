//! 运行时设置管理。

use axum::{Extension, Json, Router, extract::State, routing::get};

use crate::store;
use crate::store::resources::Settings;

use super::auth::ManagementIdentity;
use super::{AdminDeps, AdminError, db_err, reload_and_swap};

pub(super) fn routes() -> Router<AdminDeps> {
    Router::new().route("/settings", get(get_settings).put(update_settings))
}

// --- 设置 ---

/// 读当前运行时设置：从内存快照直接取（与请求路径读同一份真值）。
async fn get_settings(State(deps): State<AdminDeps>) -> Result<Json<Settings>, AdminError> {
    let updated = read_settings(&deps).await?;
    Ok(Json(updated))
}

/// 整体更新运行时设置：写库 → 换快照 → 返回变更后设置。
///
/// 设置变更后经快照原子替换即时生效：入站请求体上限、认证限流、SSE 重装上限
/// 与同渠道退避的变更立刻作用于后续请求。
async fn update_settings(
    State(deps): State<AdminDeps>,
    Extension(identity): Extension<ManagementIdentity>,
    body: Result<Json<Settings>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Settings>, AdminError> {
    let settings = body.map_err(AdminError::bad_body)?;
    validate_settings(&settings)?;
    let before = read_settings(&deps).await?;
    let mut tx = deps.pool.begin().await.map_err(db_err)?;
    crate::store::resources::upsert_settings(&mut tx, &settings)
        .await
        .map_err(AdminError::Store)?;
    let changes = settings_changes(&before, &settings);
    if !changes.is_empty() {
        // 设置改动直接影响网关行为（body 上限、限流、重试），必须可追溯到人。
        store::record_audit(
            &mut tx,
            identity.actor(),
            "settings",
            &store::SystemLogEvent::new(
                "settings.updated",
                serde_json::json!({ "changes": changes }),
                format!("修改设置：{}", changes.join("；")),
            ),
        )
        .await
        .map_err(AdminError::Store)?;
    }
    tx.commit().await.map_err(db_err)?;
    reload_and_swap(&deps).await?;
    let updated = read_settings(&deps).await?;
    Ok(Json(updated))
}

/// 列出两版设置之间实际变化的项，形如 `键 旧 → 新`。
fn settings_changes(before: &Settings, after: &Settings) -> Vec<String> {
    let mut changes = Vec::new();
    macro_rules! diff {
        ($field:ident) => {
            if before.$field != after.$field {
                changes.push(format!(
                    "{} {} → {}",
                    stringify!($field),
                    before.$field,
                    after.$field
                ));
            }
        };
    }
    diff!(full_body);
    diff!(max_request_bytes);
    diff!(max_response_bytes);
    diff!(log_body_max_bytes);
    diff!(catalog_sync_interval_days);
    diff!(auth_throttle_max_failures);
    diff!(auth_throttle_window_secs);
    diff!(sse_reassembly_max_bytes);
    diff!(retry_backoff_ms);
    diff!(retry_backoff_cap_ms);
    diff!(retry_after_cap_secs);
    diff!(rate_limit_rpm);
    changes
}

/// 校验设置字段：须为正的阈值写成 0 属运营误配；认证失败次数允许 0（关闭限流）。
fn validate_settings(settings: &Settings) -> Result<(), AdminError> {
    if settings.max_request_bytes == 0 {
        return Err(AdminError::InvalidBody(
            "max_request_bytes 必须大于 0".to_string(),
        ));
    }
    if settings.max_response_bytes == 0 {
        return Err(AdminError::InvalidBody(
            "max_response_bytes 必须大于 0".to_string(),
        ));
    }
    if settings.auth_throttle_window_secs == 0 {
        return Err(AdminError::InvalidBody(
            "auth_throttle_window_secs 必须大于 0".to_string(),
        ));
    }
    if settings.sse_reassembly_max_bytes == 0 {
        return Err(AdminError::InvalidBody(
            "sse_reassembly_max_bytes 必须大于 0".to_string(),
        ));
    }
    if settings.retry_backoff_ms == 0 {
        return Err(AdminError::InvalidBody(
            "retry_backoff_ms 必须大于 0".to_string(),
        ));
    }
    if settings.retry_backoff_cap_ms == 0 {
        return Err(AdminError::InvalidBody(
            "retry_backoff_cap_ms 必须大于 0".to_string(),
        ));
    }
    if settings.retry_backoff_cap_ms < settings.retry_backoff_ms {
        return Err(AdminError::InvalidBody(
            "retry_backoff_cap_ms 不能小于 retry_backoff_ms".to_string(),
        ));
    }
    if settings.retry_after_cap_secs == 0 {
        return Err(AdminError::InvalidBody(
            "retry_after_cap_secs 必须大于 0".to_string(),
        ));
    }
    if settings.log_body_max_bytes == 0 {
        return Err(AdminError::InvalidBody(
            "log_body_max_bytes 必须大于 0".to_string(),
        ));
    }
    Ok(())
}

/// 从当前快照读回设置。
async fn read_settings(deps: &AdminDeps) -> Result<Settings, AdminError> {
    let snapshot = deps.snapshot.read().await;
    Ok(snapshot.to_settings())
}
