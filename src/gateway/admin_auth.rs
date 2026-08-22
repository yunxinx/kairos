//! 管理面认证与会话授权。
//!
//! 这个模块把凭证解析、会话查验和角色中间件集中在一个窄接口后面；资源处理器只
//! 接收已经解析好的 [`ManagementIdentity`]，不再重复理解 Bearer 或限流细节。

use std::net::SocketAddr;

use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::store;
use crate::store::users::{self, ManagementRole, UserRecord};

use super::admin::AdminError;
use super::http::extract_bearer;

/// 管理认证中间件状态：认证失败限流 + 会话查库。
#[derive(Clone)]
pub(super) struct AdminAuth {
    pub(super) throttle: super::throttle::AuthThrottle,
    pub(super) snapshot: crate::runtime::SnapshotHandle,
    pub(super) pool: sqlx::SqlitePool,
}

/// 已认证主体：一条未吊销的管理会话对应用户。
#[derive(Clone)]
pub(super) struct ManagementIdentity {
    pub(super) user: UserRecord,
}

impl ManagementIdentity {
    pub(super) fn role(&self) -> ManagementRole {
        self.user.role
    }

    pub(super) fn user_id(&self) -> i64 {
        self.user.id
    }

    /// 审计事件的操作者。
    pub(super) fn actor(&self) -> store::Actor<'_> {
        store::Actor {
            user_id: self.user.id,
            email: &self.user.email,
        }
    }

    /// 只读聚合与日志查询的归属范围：普通用户钉自己，admin/root 不限。
    pub(super) fn owner_scope(&self) -> Option<i64> {
        if self.role().at_least(ManagementRole::Admin) {
            None
        } else {
            Some(self.user_id())
        }
    }
}

/// 管理认证：Bearer 仅为未吊销的会话。失败 401，窗口内过多则 429。
pub(super) async fn admin_auth(
    State(auth): State<AdminAuth>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    mut request: Request,
    next: Next,
) -> Response {
    let ip = addr.ip();
    let snapshot = auth.snapshot.read().await;
    let max_failures = snapshot.auth_throttle_max_failures;
    let window = snapshot.auth_throttle_window();
    drop(snapshot);
    let provided = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(extract_bearer)
        .unwrap_or("");
    let now = super::logging::unix_millis();
    match users::user_for_session(&auth.pool, provided, now).await {
        Ok(users::SessionLookup::Valid(user)) => {
            request.extensions_mut().insert(ManagementIdentity { user });
            next.run(request).await
        }
        Ok(users::SessionLookup::Unknown) => {
            if auth.throttle.is_blocked(ip, max_failures, window) {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(
                        json!({ "error": { "code": "rate_limited", "message": "认证尝试过于频繁，请稍后再试" } }),
                    ),
                )
                    .into_response();
            }
            auth.throttle.record_failure(ip, max_failures, window);
            unauthorized_response()
        }
        Ok(users::SessionLookup::Malformed | users::SessionLookup::Inactive) => {
            unauthorized_response()
        }
        Err(err) => AdminError::Store(err).into_response(),
    }
}

pub(super) fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": { "code": "unauthorized", "message": "无效或缺失的管理凭证" } })),
    )
        .into_response()
}

pub(super) fn forbidden_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": { "code": "forbidden", "message": "权限不足" } })),
    )
        .into_response()
}

pub(super) async fn require_root(request: Request, next: Next) -> Response {
    require_min_role(request, next, ManagementRole::Root).await
}

pub(super) async fn require_admin(request: Request, next: Next) -> Response {
    require_min_role(request, next, ManagementRole::Admin).await
}

async fn require_min_role(request: Request, next: Next, min: ManagementRole) -> Response {
    let Some(identity) = request.extensions().get::<ManagementIdentity>() else {
        return unauthorized_response();
    };
    if identity.role().at_least(min) {
        next.run(request).await
    } else {
        forbidden_response()
    }
}
