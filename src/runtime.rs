//! 运行时资源内存快照：网关请求路径读取的唯一资源视图。
//!
//! 启动时从 SQLite 加载渠道、令牌、价格、模型组、统一模型与运行时开关进
//! `RuntimeSnapshot`（不可变整体）。请求路径（认证、路由、计费准入、full_body、
//! body 上限、模型组允许名单、统一模型 failover）全部读快照；在途请求在准入时刻
//! 抓取一个 `Arc` 引用，不受后续原子替换影响。管理 API 写库成功后原子替换快照
//! （见 `SnapshotHandle`），是唯一动态入口。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::core::billing;
use crate::store::StoreError;
use crate::store::plans;
pub use crate::store::plans::PlanCapabilities;
use crate::store::resources::{
    self, SETTING_AUTH_THROTTLE_MAX_FAILURES, SETTING_AUTH_THROTTLE_WINDOW_SECS,
    SETTING_CATALOG_SYNC_INTERVAL_DAYS, SETTING_FULL_BODY, SETTING_LOG_BODY_MAX_BYTES,
    SETTING_MAX_REQUEST_BYTES, SETTING_MAX_RESPONSE_BYTES, SETTING_RATE_LIMIT_RPM,
    SETTING_RETRY_AFTER_CAP_SECS, SETTING_RETRY_BACKOFF_CAP_MS, SETTING_RETRY_BACKOFF_MS,
    SETTING_SSE_REASSEMBLY_MAX_BYTES,
};
use crate::store::users::{self, ManagementRole};

pub use crate::store::resources::{
    DEFAULT_AUTH_THROTTLE_MAX_FAILURES, DEFAULT_AUTH_THROTTLE_WINDOW_SECS,
    DEFAULT_LOG_BODY_MAX_BYTES, DEFAULT_MAX_REQUEST_BYTES, DEFAULT_MAX_RESPONSE_BYTES,
    DEFAULT_RATE_LIMIT_RPM, DEFAULT_RETRY_AFTER_CAP_SECS, DEFAULT_RETRY_BACKOFF_CAP_MS,
    DEFAULT_RETRY_BACKOFF_MS, DEFAULT_SSE_REASSEMBLY_MAX_BYTES,
};

/// 网关运行时资源的内存快照：不可变整体，原子替换。
///
/// 请求路径在准入时刻抓取一个 `Arc<RuntimeSnapshot>` 引用，整个请求生命周期内
/// 只读该引用——即使管理 API 之后替换了快照，在途请求仍按准入时刻的资源走完。
#[derive(Debug, Clone)]
pub struct RuntimeSnapshot {
    /// 渠道（路由候选，含库生成 id），按 store 返回顺序。
    pub channels: Vec<resources::ChannelRecord>,
    /// 同名可调用名的显式渠道尝试顺序；缺少行的候选由路由按渠道 id 兜底。
    pub channel_model_order: Vec<resources::ChannelModelOrder>,
    /// 令牌定义，按 `token_key` 索引（认证查找）。
    pub tokens: HashMap<String, resources::Token>,
    /// 价格表，外层按渠道稳定 id、内层按可调用名索引（计费准入）。
    pub prices: HashMap<i64, HashMap<String, resources::Price>>,
    /// 模型组，按组名索引（令牌允许名单）。
    pub model_groups: HashMap<String, resources::ModelGroup>,
    /// 统一模型，按可调用名索引（有序 failover）。
    pub unified_models: HashMap<String, resources::UnifiedModel>,
    /// 管理用户：启停、角色与所挂套餐（入站令牌准入）。
    pub users: HashMap<i64, RuntimeUser>,
    /// 套餐运行时投影，按套餐 id 索引（模型组名单与套餐级限速/折扣）。
    pub plans: HashMap<i64, RuntimePlan>,
    /// 是否落完整请求/响应 body（来自 `full_body` 开关）。
    pub full_body: bool,
    /// 入站请求体大小上限（字节，来自 `max_request_bytes` 开关）。
    pub max_request_bytes: u64,
    /// 上游非流式响应体大小上限（字节，来自 `max_response_bytes` 开关）。
    pub max_response_bytes: u64,
    /// 请求日志 body 截断上限（字节，来自 `log_body_max_bytes` 开关）。
    pub log_body_max_bytes: u64,
    /// 价格目录自动同步间隔（天，来自 `catalog_sync_interval_days`；`0` 为只手动）。
    pub catalog_sync_interval_days: u64,
    /// 同一 IP 窗口内允许的认证失败次数（`0` 关闭限流）。
    pub auth_throttle_max_failures: u64,
    /// 认证失败计数窗口（秒）。
    pub auth_throttle_window_secs: u64,
    /// SSE 重装缓冲上限（字节）。
    pub sse_reassembly_max_bytes: u64,
    /// 同渠道重试基础间隔（毫秒）。
    pub retry_backoff_ms: u64,
    /// 同渠道指数退避封顶（毫秒）。
    pub retry_backoff_cap_ms: u64,
    /// 上游 `Retry-After` 最大等待（秒）。
    pub retry_after_cap_secs: u64,
    /// 未单独配置限速的令牌使用的每分钟请求兜底；`0` 表示不设全局上限。
    pub rate_limit_rpm: u64,
}

/// 用户与套餐的绑定：root 不挂档，用类型把「没有套餐」这个合法状态表达出来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanBinding {
    /// root：全部模型组可用，原价结算，只受系统兜底限速。
    Unrestricted,
    /// 普通用户/管理员所挂的套餐 id。
    Plan(i64),
}

/// 套餐的运行时投影：请求路径只读折扣、限速默认值、共享 RPM 和模型组名单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePlan {
    pub discount_bp: i64,
    pub default_rpm: Option<u64>,
    pub shared_rpm: Option<u64>,
    pub groups: HashSet<String>,
    pub capabilities: PlanCapabilities,
}

/// 快照里的管理用户：请求路径只读启停、角色、所挂套餐与用户级限速。
#[derive(Debug, Clone)]
pub struct RuntimeUser {
    pub role: ManagementRole,
    pub enabled: bool,
    pub plan: PlanBinding,
    pub rate_limit_rpm: Option<u64>,
}

impl RuntimeSnapshot {
    /// 按渠道稳定 id + 可调用名取单价。
    pub fn price_for_channel(&self, channel_id: i64, model: &str) -> Option<&resources::Price> {
        self.prices.get(&channel_id)?.get(model)
    }

    /// 令牌所属用户当前套餐的折扣率（万分比）。
    ///
    /// root 不挂档，恒为原价；套餐缺失按原价处理（正常数据不会发生）。
    pub fn discount_bp_for_token(&self, token: &resources::Token) -> i64 {
        match self.users.get(&token.user_id).map(|user| user.plan) {
            Some(PlanBinding::Unrestricted) | None => billing::DEFAULT_DISCOUNT_BP,
            Some(PlanBinding::Plan(plan_id)) => self
                .plans
                .get(&plan_id)
                .map(|plan| plan.discount_bp)
                .unwrap_or(billing::DEFAULT_DISCOUNT_BP),
        }
    }

    /// 令牌组是否仍可调用：组非空、用户启用、组存在；所挂套餐名单是唯一来源。
    ///
    /// root 走 `Unrestricted` 直通；空组（删组后置空）一律不可调用。
    pub fn token_group_assigned(&self, token: &resources::Token) -> bool {
        if token.model_group.is_empty() {
            return false;
        }
        let Some(user) = self.users.get(&token.user_id) else {
            return false;
        };
        if !user.enabled {
            return false;
        }
        if !self.model_groups.contains_key(&token.model_group) {
            return false;
        }
        match user.plan {
            PlanBinding::Unrestricted => true,
            PlanBinding::Plan(plan_id) => self
                .plans
                .get(&plan_id)
                .is_some_and(|plan| plan.groups.contains(&token.model_group)),
        }
    }

    /// 认证失败计数窗口；库内为 0 时按 1 秒处理，避免 `Duration::from_secs(0)` 让窗口立刻过期。
    pub fn auth_throttle_window(&self) -> Duration {
        Duration::from_secs(self.auth_throttle_window_secs.max(1))
    }

    /// SSE 重装缓冲上限（与 `Vec` 长度比较用）。
    pub fn sse_reassembly_max(&self) -> usize {
        usize::try_from(self.sse_reassembly_max_bytes).unwrap_or(usize::MAX)
    }

    /// 请求日志 body 截断上限（与 `Vec` 长度比较用）。
    pub fn log_body_max(&self) -> usize {
        usize::try_from(self.log_body_max_bytes).unwrap_or(usize::MAX)
    }

    /// 把快照中的运行时开关聚合成管理 API 的 Settings 契约。
    pub fn to_settings(&self) -> resources::Settings {
        resources::Settings {
            full_body: self.full_body,
            max_request_bytes: self.max_request_bytes,
            max_response_bytes: self.max_response_bytes,
            log_body_max_bytes: self.log_body_max_bytes,
            catalog_sync_interval_days: self.catalog_sync_interval_days,
            auth_throttle_max_failures: self.auth_throttle_max_failures,
            auth_throttle_window_secs: self.auth_throttle_window_secs,
            sse_reassembly_max_bytes: self.sse_reassembly_max_bytes,
            retry_backoff_ms: self.retry_backoff_ms,
            retry_backoff_cap_ms: self.retry_backoff_cap_ms,
            retry_after_cap_secs: self.retry_after_cap_secs,
            rate_limit_rpm: self.rate_limit_rpm,
        }
    }
}

/// 快照的原子替换句柄：管理 API 写库成功后写入新快照，请求路径读当前快照。
///
/// 外层 `RwLock` 承载「替换」语义（管理 API 持写锁换掉整个 `Arc`），请求路径只
/// 在读锁内短暂克隆 `Arc` 即释放锁，之后持有该引用独立运行。
pub type SnapshotHandle = Arc<RwLock<Arc<RuntimeSnapshot>>>;

/// 从库加载全部运行时资源进内存快照。
///
/// 四类资源分别读取：渠道/令牌/价格直接装载，运行时开关经 `load_settings` 解析
/// 出日志、网关保护与目录同步等设置（缺省用默认值）。
pub async fn load_snapshot(pool: &SqlitePool) -> Result<RuntimeSnapshot, StoreError> {
    let channels = resources::list_channel_records_without_secrets(pool).await?;
    let channel_model_order = resources::list_channel_model_orders(pool).await?;
    let token_rows = resources::list_tokens(pool).await?;
    let price_rows = resources::list_prices(pool).await?;
    let group_rows = resources::list_model_groups(pool).await?;
    let unified_rows = resources::list_unified_models(pool).await?;
    let plan_rows = plans::list_plans_for_snapshot(pool).await?;
    let settings = resources::list_settings(pool).await?;

    let tokens = token_rows
        .into_iter()
        .map(|token| (token.token_key.clone(), token))
        .collect();
    let mut prices: HashMap<i64, HashMap<String, resources::Price>> = HashMap::new();
    for price in price_rows {
        prices
            .entry(price.channel_id)
            .or_default()
            .insert(price.model.clone(), price);
    }
    let model_groups = group_rows
        .into_iter()
        .map(|group| (group.name.clone(), group))
        .collect();
    let unified_models = unified_rows
        .into_iter()
        .map(|model| (model.id.clone(), model))
        .collect();
    // 只取请求路径要用的字段：头像不进快照（见 list_users_for_snapshot）。
    let plans = plan_rows
        .into_iter()
        .map(|plan| {
            (
                plan.id,
                RuntimePlan {
                    discount_bp: plan.discount_bp,
                    default_rpm: plan.default_rpm,
                    shared_rpm: plan.shared_rpm,
                    groups: plan.groups,
                    capabilities: plan.capabilities,
                },
            )
        })
        .collect();

    let user_rows = users::list_users_for_snapshot(pool).await?;
    let mut users: HashMap<i64, RuntimeUser> = HashMap::new();
    for user in user_rows {
        let plan = match user.plan_id {
            Some(plan_id) => PlanBinding::Plan(plan_id),
            None if user.id == resources::ROOT_USER_ID => PlanBinding::Unrestricted,
            None => {
                return Err(StoreError::InvalidResource(format!(
                    "用户 {} 缺少套餐，只有 root 允许不挂档",
                    user.id
                )));
            }
        };
        users.insert(
            user.id,
            RuntimeUser {
                role: user.role,
                enabled: user.enabled,
                plan,
                rate_limit_rpm: user.rate_limit_rpm,
            },
        );
    }

    Ok(RuntimeSnapshot {
        channels,
        channel_model_order,
        tokens,
        prices,
        model_groups,
        unified_models,
        users,
        plans,
        full_body: load_full_body(&settings),
        max_request_bytes: load_max_request_bytes(&settings),
        max_response_bytes: load_u64(
            &settings,
            SETTING_MAX_RESPONSE_BYTES,
            DEFAULT_MAX_RESPONSE_BYTES,
        ),
        log_body_max_bytes: load_u64(
            &settings,
            SETTING_LOG_BODY_MAX_BYTES,
            DEFAULT_LOG_BODY_MAX_BYTES,
        ),
        catalog_sync_interval_days: load_catalog_sync_interval_days(&settings),
        auth_throttle_max_failures: load_u64(
            &settings,
            SETTING_AUTH_THROTTLE_MAX_FAILURES,
            DEFAULT_AUTH_THROTTLE_MAX_FAILURES,
        ),
        auth_throttle_window_secs: load_u64(
            &settings,
            SETTING_AUTH_THROTTLE_WINDOW_SECS,
            DEFAULT_AUTH_THROTTLE_WINDOW_SECS,
        ),
        sse_reassembly_max_bytes: load_u64(
            &settings,
            SETTING_SSE_REASSEMBLY_MAX_BYTES,
            DEFAULT_SSE_REASSEMBLY_MAX_BYTES,
        ),
        retry_backoff_ms: load_u64(
            &settings,
            SETTING_RETRY_BACKOFF_MS,
            DEFAULT_RETRY_BACKOFF_MS,
        ),
        retry_backoff_cap_ms: load_u64(
            &settings,
            SETTING_RETRY_BACKOFF_CAP_MS,
            DEFAULT_RETRY_BACKOFF_CAP_MS,
        ),
        retry_after_cap_secs: load_u64(
            &settings,
            SETTING_RETRY_AFTER_CAP_SECS,
            DEFAULT_RETRY_AFTER_CAP_SECS,
        ),
        rate_limit_rpm: load_u64(&settings, SETTING_RATE_LIMIT_RPM, DEFAULT_RATE_LIMIT_RPM),
    })
}

/// 从开关表解析无符号整数：缺键或非整数时用 `default`。
fn load_u64(settings: &HashMap<String, Value>, key: &str, default: u64) -> u64 {
    settings.get(key).and_then(Value::as_u64).unwrap_or(default)
}

/// 从开关表解析 `full_body`：缺省关闭。
fn load_full_body(settings: &HashMap<String, Value>) -> bool {
    settings
        .get(SETTING_FULL_BODY)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// 从开关表解析 `max_request_bytes`：缺省用 `DEFAULT_MAX_REQUEST_BYTES`。
fn load_max_request_bytes(settings: &HashMap<String, Value>) -> u64 {
    load_u64(
        settings,
        SETTING_MAX_REQUEST_BYTES,
        DEFAULT_MAX_REQUEST_BYTES,
    )
}

/// 从开关表解析 `catalog_sync_interval_days`：缺省 0（只手动）。
fn load_catalog_sync_interval_days(settings: &HashMap<String, Value>) -> u64 {
    load_u64(settings, SETTING_CATALOG_SYNC_INTERVAL_DAYS, 0)
}

/// 把一个库加载出的快照包装成原子替换句柄。
pub fn snapshot_handle(snapshot: RuntimeSnapshot) -> SnapshotHandle {
    Arc::new(RwLock::new(Arc::new(snapshot)))
}

/// 原子替换当前快照：管理 API 写库成功后调用，使新资源即时生效。
///
/// 在途请求已持有旧快照引用，不受本次替换影响；只有后续请求读到新快照。
pub async fn swap_snapshot(handle: &SnapshotHandle, snapshot: RuntimeSnapshot) {
    *handle.write().await = Arc::new(snapshot);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Protocol;
    use crate::store;

    /// 建一个临时 SQLite 连接池并跑完全部迁移。
    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let pool = store::open(&dir.path().join("test.db"))
            .await
            .expect("应能打开临时库");
        (dir, pool)
    }

    /// 空库加载：资源为空，开关取默认值。
    #[tokio::test]
    async fn empty_db_loads_default_snapshot() {
        let (_dir, pool) = test_pool().await;
        let snap = load_snapshot(&pool).await.expect("应能加载快照");
        assert!(snap.channels.is_empty());
        assert!(snap.tokens.is_empty());
        assert!(snap.prices.is_empty());
        assert_eq!(snap.model_groups.len(), 1, "空库应有内置 default 组");
        assert!(snap.unified_models.is_empty());
        let root = snap
            .users
            .get(&resources::ROOT_USER_ID)
            .expect("空库应有内置 root");
        assert!(root.enabled);
        assert_eq!(root.plan, PlanBinding::Unrestricted);
        assert_eq!(snap.plans.len(), 2, "空库应有内置 standard/admin 两档");
        assert!(snap.plans.contains_key(&1));
        assert!(snap.plans.contains_key(&2));
        assert!(
            snap.model_groups
                .contains_key(resources::DEFAULT_MODEL_GROUP)
        );
        assert!(!snap.full_body, "full_body 缺省关闭");
        assert_eq!(
            snap.max_request_bytes, DEFAULT_MAX_REQUEST_BYTES,
            "body 上限缺省用默认值"
        );
        assert_eq!(
            snap.max_response_bytes, DEFAULT_MAX_RESPONSE_BYTES,
            "出站响应上限缺省用默认值"
        );
        assert_eq!(
            snap.log_body_max_bytes, DEFAULT_LOG_BODY_MAX_BYTES,
            "日志 body 上限缺省 1MB"
        );
        assert_eq!(snap.catalog_sync_interval_days, 0, "目录同步缺省只手动");
        assert_eq!(
            snap.auth_throttle_max_failures,
            DEFAULT_AUTH_THROTTLE_MAX_FAILURES
        );
        assert_eq!(
            snap.auth_throttle_window_secs,
            DEFAULT_AUTH_THROTTLE_WINDOW_SECS
        );
        assert_eq!(
            snap.sse_reassembly_max_bytes,
            DEFAULT_SSE_REASSEMBLY_MAX_BYTES
        );
        assert_eq!(snap.retry_backoff_ms, DEFAULT_RETRY_BACKOFF_MS);
        assert_eq!(snap.retry_backoff_cap_ms, DEFAULT_RETRY_BACKOFF_CAP_MS);
        assert_eq!(snap.retry_after_cap_secs, DEFAULT_RETRY_AFTER_CAP_SECS);
        assert_eq!(snap.rate_limit_rpm, DEFAULT_RATE_LIMIT_RPM);
    }

    /// 播种资源与开关后加载：快照反映库内状态。
    #[tokio::test]
    async fn seeded_db_loads_resources_and_settings() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");

        let channel_id = resources::insert_channel(
            &mut conn,
            &resources::Channel {
                name: "c1".to_string(),
                protocol: Protocol::OpenAiChat,
                base_url: "https://api.example.com/v1".to_string(),
                keys: vec![resources::ChannelKey {
                    name: "default".to_string(),
                    api_key: "k".to_string(),
                    weight: 1,
                    enabled: true,
                    models: None,
                    blocked_models: None,
                }],
                models: vec!["gpt-4o".to_string()],
                model_aliases: HashMap::new(),
                timeout_ms: 1000,
                max_retries: 0,
                enabled: true,
                model_group: resources::DEFAULT_MODEL_GROUP.to_string(),
                reasoning_output: Default::default(),
                session_cache_key: Default::default(),
            },
        )
        .await
        .expect("应能写渠道");
        resources::insert_token(
            &mut conn,
            &resources::Token {
                token_key: "sk-a".to_string(),
                name: "dev".to_string(),
                limit_usd_micros: None,
                enabled: true,
                rate_limit_rpm: None,
                model_group: resources::DEFAULT_MODEL_GROUP.to_string(),
                user_id: resources::ROOT_USER_ID,
            },
            1,
        )
        .await
        .expect("应能写令牌");
        resources::upsert_price(
            &mut conn,
            &resources::Price {
                channel_id,
                model: "gpt-4o".to_string(),
                input_micros: 2_500_000,
                output_micros: 10_000_000,
                cache_read_micros: None,
                cache_write_micros: None,
            },
        )
        .await
        .expect("应能写价格");
        sqlx::query(
            "INSERT INTO channel_model_order (model, channel_id, position) VALUES ('gpt-4o', ?, 4)",
        )
        .bind(channel_id)
        .execute(&mut *conn)
        .await
        .expect("应能写顺序行");
        resources::upsert_unified_model(
            &mut conn,
            &resources::UnifiedModel {
                id: "coding".to_string(),
                models: vec![resources::UnifiedMember {
                    channel_id,
                    model: "gpt-4o".to_string(),
                }],
                hide: false,
            },
        )
        .await
        .expect("应能写统一模型");
        resources::set_setting(&mut conn, SETTING_FULL_BODY, &Value::Bool(true))
            .await
            .expect("应能写开关");
        resources::set_setting(
            &mut conn,
            SETTING_MAX_REQUEST_BYTES,
            &Value::from(8_000_000u64),
        )
        .await
        .expect("应能写开关");
        drop(conn);

        let snap = load_snapshot(&pool).await.expect("应能加载快照");
        assert_eq!(snap.channels.len(), 1);
        assert_eq!(snap.channels[0].channel.name, "c1");
        assert_eq!(
            snap.channel_model_order,
            vec![resources::ChannelModelOrder {
                model: "gpt-4o".to_string(),
                channel_id,
                position: 4,
            }]
        );
        assert!(snap.tokens.contains_key("sk-a"));
        assert_eq!(
            snap.tokens["sk-a"].model_group,
            resources::DEFAULT_MODEL_GROUP
        );
        assert!(snap.price_for_channel(channel_id, "gpt-4o").is_some());
        assert!(
            snap.model_groups
                .contains_key(resources::DEFAULT_MODEL_GROUP)
        );
        assert_eq!(
            snap.unified_models["coding"].models,
            vec![resources::UnifiedMember {
                channel_id,
                model: "gpt-4o".to_string(),
            }]
        );
        assert!(snap.full_body, "开关应生效");
        assert_eq!(snap.max_request_bytes, 8_000_000);
    }

    /// 内置套餐进快照：standard 含 default，admin 名单为空；用户按 plan_id 绑档。
    #[tokio::test]
    async fn snapshot_loads_builtin_plans_and_user_bindings() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        sqlx::query(
            "INSERT INTO users (email, display_name, role, enabled, created_at, plan_id) \
             VALUES ('coder@example.com', 'Coder', 'user', 1, 0, 1)",
        )
        .execute(&mut *conn)
        .await
        .expect("应能插入测试用户");
        drop(conn);

        let snap = load_snapshot(&pool).await.expect("应能加载快照");
        let standard = snap.plans.get(&1).expect("应有 standard 档");
        assert!(standard.groups.contains(resources::DEFAULT_MODEL_GROUP));
        assert_eq!(standard.default_rpm, None);
        assert_eq!(standard.shared_rpm, None);
        let admin = snap.plans.get(&2).expect("应有 admin 档");
        assert!(admin.groups.is_empty());
        assert_eq!(
            admin.capabilities,
            PlanCapabilities {
                manage_users: true,
                assign_plan: true,
                view_logs_stats: true,
                settle_waive: true,
                toggle_user_tokens: true,
                view_own_plan_groups: true,
                ..PlanCapabilities::default()
            }
        );

        let coder = snap
            .users
            .values()
            .find(|user| user.role == ManagementRole::User)
            .expect("应有普通用户");
        assert_eq!(coder.plan, PlanBinding::Plan(1));
    }
}
