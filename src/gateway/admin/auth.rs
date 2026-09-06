//! 管理面认证与会话授权。
//!
//! 这个模块把 Cookie 会话解析、会话查验和角色中间件集中在一个窄接口后面；资源
//! 处理器只接收已经解析好的 [`ManagementIdentity`]，不再重复理解凭证或限流细节。

use axum::{
    Json,
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::gateway::logging;
use crate::store;
use crate::store::plans::{self, PlanCapabilities};
use crate::store::users::{self, ManagementRole, UserRecord};

use super::AdminError;

pub(super) const SESSION_COOKIE: &str = "kairos_session";

/// 管理认证中间件状态：会话查库。
#[derive(Clone)]
pub(super) struct AdminAuth {
    pub(super) pool: sqlx::SqlitePool,
}

/// 已认证主体：一条未吊销的管理会话对应用户。
#[derive(Clone)]
pub(super) struct ManagementIdentity {
    pub(super) user: UserRecord,
    /// 当前请求从所挂套餐解析出的能力开关；不随会话持久化。
    pub(super) capabilities: PlanCapabilities,
}

/// 管理面可被套餐收窄的能力。
#[derive(Debug, Clone, Copy)]
pub(super) enum ManagementCapability {
    ManageUsers,
    AssignPlan,
    ViewLogsStats,
    SettleWaive,
    ToggleUserTokens,
    ViewOwnPlanGroups,
    ViewOtherGroups,
    ViewChannels,
    ViewPrices,
    ViewModelGroups,
    ViewUnifiedModels,
    EditPrices,
    EditModelGroups,
    EditUnifiedModels,
    EditPriceCatalog,
}

impl ManagementCapability {
    fn is_enabled(self, capabilities: &PlanCapabilities) -> bool {
        match self {
            Self::ManageUsers => capabilities.manage_users,
            Self::AssignPlan => capabilities.assign_plan,
            Self::ViewLogsStats => capabilities.view_logs_stats,
            Self::SettleWaive => capabilities.settle_waive,
            Self::ToggleUserTokens => capabilities.toggle_user_tokens,
            Self::ViewOwnPlanGroups => capabilities.view_own_plan_groups,
            Self::ViewOtherGroups => capabilities.view_other_groups,
            Self::ViewChannels => capabilities.view_channels,
            Self::ViewPrices => capabilities.view_prices,
            Self::ViewModelGroups => capabilities.view_model_groups,
            Self::ViewUnifiedModels => capabilities.view_unified_models,
            Self::EditPrices => capabilities.edit_prices,
            Self::EditModelGroups => capabilities.edit_model_groups,
            Self::EditUnifiedModels => capabilities.edit_unified_models,
            Self::EditPriceCatalog => capabilities.edit_price_catalog,
        }
    }
}

impl ManagementIdentity {
    pub(super) fn role(&self) -> ManagementRole {
        self.user.role
    }

    pub(super) fn user_id(&self) -> i64 {
        self.user.id
    }

    /// 当前用户所挂套餐；root 为 `None`。
    pub(super) fn plan_id(&self) -> Option<i64> {
        self.user.plan_id
    }

    /// 前端可见的生效能力；root 的角色天花板等价于全部开启。
    pub(super) fn capabilities_for_view(&self) -> PlanCapabilities {
        if self.role() == ManagementRole::Root {
            PlanCapabilities {
                manage_users: true,
                assign_plan: true,
                view_logs_stats: true,
                settle_waive: true,
                toggle_user_tokens: true,
                view_own_plan_groups: true,
                view_other_groups: true,
                view_channels: true,
                view_prices: true,
                view_model_groups: true,
                view_unified_models: true,
                edit_prices: true,
                edit_model_groups: true,
                edit_unified_models: true,
                edit_price_catalog: true,
            }
        } else {
            self.capabilities
        }
    }

    /// 角色天花板与套餐开关的交集；root 不受套餐开关约束。
    pub(super) fn has_capability(&self, capability: ManagementCapability) -> bool {
        match self.role() {
            ManagementRole::Root => true,
            ManagementRole::Admin => capability.is_enabled(&self.capabilities),
            ManagementRole::User => false,
        }
    }

    /// 校验管理员能力。调用方已经在 `admin_plus` 层之后，仍必须在具体路由再检查。
    pub(super) fn require_capability(
        &self,
        capability: ManagementCapability,
    ) -> Result<(), AdminError> {
        if self.has_capability(capability) {
            Ok(())
        } else {
            Err(AdminError::Forbidden)
        }
    }

    /// 只收窄 admin 的能力；普通用户保留角色本身已有的自助权限。
    pub(super) fn require_admin_capability(
        &self,
        capability: ManagementCapability,
    ) -> Result<(), AdminError> {
        if self.role() == ManagementRole::User {
            Ok(())
        } else {
            self.require_capability(capability)
        }
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

/// 管理认证：Cookie 仅接受未吊销的会话，失败统一返回 401。
///
/// 会话令牌是高熵凭证；无效、过期、吊销与 GC 后的旧令牌都不能消耗密码登录的
/// 失败预算。密码爆破限流只存在于 `/login`，两类凭证不共享状态。
pub(super) async fn admin_auth(
    State(auth): State<AdminAuth>,
    mut request: Request,
    next: Next,
) -> Response {
    let provided = session_from_request(&request).unwrap_or("");
    let now = logging::unix_millis();
    match users::user_for_session(&auth.pool, provided, now).await {
        Ok(users::SessionLookup::Valid(user)) => {
            let capabilities = match user.role {
                ManagementRole::Root => PlanCapabilities::default(),
                ManagementRole::Admin | ManagementRole::User => {
                    let Some(plan_id) = user.plan_id else {
                        return AdminError::Store(store::StoreError::InvalidResource(format!(
                            "用户 {} 缺少套餐",
                            user.id
                        )))
                        .into_response();
                    };
                    match plans::load_plan_access_profile(&auth.pool, plan_id).await {
                        Ok(profile)
                            if users::plan_audience_for_role(user.role)
                                == Some(profile.audience) =>
                        {
                            profile.capabilities
                        }
                        // 存量脏绑定按最小权限运行，不能让 user 档能力对 admin 生效。
                        Ok(_) => PlanCapabilities::default(),
                        Err(err) => return AdminError::Store(err).into_response(),
                    }
                }
            };
            request
                .extensions_mut()
                .insert(ManagementIdentity { user, capabilities });
            next.run(request).await
        }
        Ok(
            users::SessionLookup::Unknown
            | users::SessionLookup::Malformed
            | users::SessionLookup::Inactive,
        ) => unauthorized_response(),
        Err(err) => AdminError::Store(err).into_response(),
    }
}

pub(super) fn session_from_request(request: &Request) -> Option<&str> {
    session_from_headers(request.headers())
}

pub(super) fn session_from_headers(headers: &axum::http::HeaderMap) -> Option<&str> {
    for value in headers.get_all(header::COOKIE) {
        let Ok(cookies) = value.to_str() else {
            continue;
        };
        if let Some(session) = cookies.split(';').find_map(|part| {
            let part = part.trim();
            let (name, value) = part.split_once('=')?;
            if name == SESSION_COOKIE && !value.is_empty() {
                Some(value)
            } else {
                None
            }
        }) {
            return Some(session);
        }
    }
    None
}

pub(super) fn request_is_secure(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("https"))
}

fn has_same_origin(headers: &axum::http::HeaderMap) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(source) = headers
        .get(header::ORIGIN)
        .or_else(|| headers.get(header::REFERER))
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let scheme = if request_is_secure(headers) {
        "https"
    } else {
        "http"
    };
    let Ok(expected) = reqwest::Url::parse(&format!("{scheme}://{host}")) else {
        return false;
    };
    let Ok(source) = reqwest::Url::parse(source) else {
        return false;
    };
    source.scheme() == expected.scheme()
        && source.host_str() == expected.host_str()
        && source.port_or_known_default() == expected.port_or_known_default()
}

/// 管理面的写请求必须带同源浏览器信号，避免 Cookie 被跨站请求自动携带。
pub(super) async fn same_origin_guard(request: Request, next: Next) -> Response {
    let requires_check = matches!(
        request.method(),
        &Method::POST | &Method::PUT | &Method::PATCH | &Method::DELETE
    );
    if requires_check && !has_same_origin(request.headers()) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": { "code": "forbidden", "message": "请求来源不受信任" } })),
        )
            .into_response();
    }
    next.run(request).await
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
