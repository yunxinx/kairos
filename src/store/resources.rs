//! 运行时资源存储：渠道、令牌、价格、模型组、统一模型与运行时开关的读写原语。
//!
//! 资源 CRUD 写操作接受 `&mut SqliteConnection`，可组合进事务；读操作接受
//! `&SqlitePool`。金额一律整数 micro-USD（ADR-0002）。wire 协议类型复用
//! `crate::config::Protocol`，落库为其 serde rename 字符串。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, SqliteConnection, SqlitePool};

use crate::config::Protocol;
use crate::store::StoreError;

/// 内置模型组名：未指定分组的令牌与未放入其他组的可调用名落在此组。
pub const DEFAULT_MODEL_GROUP: &str = "default";

/// serde 缺省：令牌未写 `model_group` 时绑到内置 `default`。
pub fn default_model_group() -> String {
    DEFAULT_MODEL_GROUP.to_string()
}

/// 渠道：指向一个上游端点的出站接入单元。
///
/// 管理 API 以其 JSON 形态作为 wire 契约（`protocol`/`models`/`model_aliases` 等
/// 字段直接序列化），故派生 `Serialize`/`Deserialize`；`deny_unknown_fields` 使
/// 字段拼写错误直接报错而非静默忽略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
    pub model_aliases: HashMap<String, String>,
    pub priority: u32,
    pub weight: u32,
    pub timeout_ms: u64,
    pub max_retries: u32,
    /// 是否启用：禁用的渠道不参与路由候选与失败切换。
    pub enabled: bool,
    /// 添加可调用名时并入的模型组；[`DEFAULT_MODEL_GROUP`] 表示不自动入组。
    #[serde(default = "default_model_group")]
    pub model_group: String,
}

/// 渠道的完整只读视图：库生成的稳定身份 + 定义字段。
///
/// `id` 由存储层维护，不属于可写契约（`Channel` JSON），故不派生 serde。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRecord {
    /// 库生成的稳定身份；管理 API 以此定位渠道，改名不改变 id。
    pub id: i64,
    pub channel: Channel,
}

/// 令牌：认证与计费的最小单位；余额独立存 `token_balance` 表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Token {
    pub token_key: String,
    pub name: String,
    /// 累计结算上限（micro-USD）；`None` 表示无上限。
    pub limit_usd_micros: Option<i64>,
    /// 是否启用：禁用的令牌在网关认证阶段被拒绝。
    pub enabled: bool,
    /// 绑定的模型组名；缺省为 [`DEFAULT_MODEL_GROUP`]。
    #[serde(default = "default_model_group")]
    pub model_group: String,
}

/// 模型组：令牌的可调用名允许名单（渠道模型、别名 key、统一模型 ID）。
///
/// 管理 API 以其 JSON 形态作为 wire 契约；`deny_unknown_fields` 使字段拼写
/// 错误直接报错而非静默忽略。渠道可指定默认组以便添加时入组，但组本身不按渠道划分。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelGroup {
    pub name: String,
    pub models: Vec<String>,
}

/// 统一模型的一条成员：钉在某一渠道上的已登记可调用名。
///
/// 同一名字挂在不同渠道上是不同成员。管理 API 以其 JSON 形态作为 wire 契约。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnifiedMember {
    pub channel_id: i64,
    pub model: String,
}

impl std::fmt::Display for UnifiedMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}（渠道 {}）", self.model, self.channel_id)
    }
}

/// 统一模型：一个下游可调用名，按顺序尝试若干钉渠道的成员（ADR-0004）。
///
/// 管理 API 以其 JSON 形态作为 wire 契约；`deny_unknown_fields` 使字段拼写
/// 错误直接报错而非静默忽略。统一 ID 本身没有价格行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnifiedModel {
    /// 下游请求的可调用名。
    pub id: String,
    /// 有序成员（渠道 × 可调用名）；一次请求只出站一条。
    pub models: Vec<UnifiedMember>,
    /// 开隐藏则同名已登记模型在组内只表示本统一模型；默认影响下游列表。
    #[serde(default)]
    pub hide: bool,
}

/// 渠道已登记的可调用名：`models` ∪ 别名 key。
pub fn channel_callable_names(channel: &Channel) -> HashSet<String> {
    let mut names: HashSet<String> = channel.models.iter().cloned().collect();
    names.extend(channel.model_aliases.keys().cloned());
    names
}

/// 该渠道是否把 `model` 当作可调用名（清单条目或别名 key）。
pub fn channel_lists_callable(channel: &Channel, model: &str) -> bool {
    channel.models.iter().any(|name| name == model) || channel.model_aliases.contains_key(model)
}

/// 相对上一版渠道新出现的可调用名（清单 ∪ 别名 key）。
///
/// `previous` 为 `None` 时全部视为新增（新建渠道）。
pub fn newly_callable_names(previous: Option<&Channel>, next: &Channel) -> Vec<String> {
    let next_names = channel_callable_names(next);
    let previous_names = previous.map(channel_callable_names).unwrap_or_default();
    let mut added: Vec<String> = next_names.difference(&previous_names).cloned().collect();
    added.sort();
    added
}

/// 把名字并入指定模型组的显式名单（已在组内的跳过，保序追加）。
///
/// 组不存在返回 [`StoreError::InvalidResource`]。
pub async fn union_names_into_group(
    conn: &mut SqliteConnection,
    group_name: &str,
    names: &[String],
) -> Result<(), StoreError> {
    if names.is_empty() {
        return Ok(());
    }
    let Some(mut group) = get_model_group(conn, group_name).await? else {
        return Err(StoreError::InvalidResource(format!(
            "模型组 {group_name} 不存在"
        )));
    };
    let mut seen: HashSet<String> = group.models.iter().cloned().collect();
    for name in names {
        if seen.insert(name.clone()) {
            group.models.push(name.clone());
        }
    }
    upsert_model_group(conn, &group).await
}

/// 按名读一个模型组；不存在返回 `None`。
pub async fn get_model_group(
    conn: &mut SqliteConnection,
    name: &str,
) -> Result<Option<ModelGroup>, StoreError> {
    let row = sqlx::query("SELECT name, models_json FROM model_groups WHERE name = ?")
        .bind(name)
        .fetch_optional(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    row.as_ref().map(map_model_group).transpose()
}

/// 渠道已登记的可调用名：各渠道 `models` ∪ 别名 key（含禁用渠道）。
///
/// 禁用渠道的名字仍算已登记：统一模型保存时可以引用；请求时再按启用渠道路由，
/// 没有启用候选才视为该成员失效。
pub fn registered_callable_names<'a>(
    channels: impl IntoIterator<Item = &'a Channel>,
) -> HashSet<String> {
    let mut names = HashSet::new();
    for channel in channels {
        names.extend(channel_callable_names(channel));
    }
    names
}

/// 未隐藏的统一模型 ID 与已登记模型/别名同名时无法在同组并存。
pub fn unhidden_unified_id_collides(id: &str, hide: bool, registered: &HashSet<String>) -> bool {
    !hide && registered.contains(id)
}

/// 令牌绑定组是否允许调用该名。
///
/// 自定义组只看显式名单。`default` 另含未出现在任何其他组名单中的名字
/// （未指定分组的可调用名视为 default）。
pub fn group_allows(groups: &HashMap<String, ModelGroup>, group_name: &str, model: &str) -> bool {
    if let Some(group) = groups.get(group_name)
        && group.models.iter().any(|name| name == model)
    {
        return true;
    }
    if group_name == DEFAULT_MODEL_GROUP {
        return !groups.iter().any(|(name, group)| {
            name != DEFAULT_MODEL_GROUP && group.models.iter().any(|item| item == model)
        });
    }
    false
}

/// 当前令牌在其模型组与统一模型隐藏规则下可见的可调用名（排序后）。
///
/// 候选为渠道已登记名、统一模型 ID，以及该组显式名单。隐藏开启且该统一 ID
/// 对本组可见时，被收进的成员（与统一 ID 同名者除外）从列表拿掉；调用准入
/// 仍只看 [`group_allows`]，隐藏不额外拦调用。
pub fn visible_model_ids<'a>(
    groups: &HashMap<String, ModelGroup>,
    unified_models: &HashMap<String, UnifiedModel>,
    channels: impl IntoIterator<Item = &'a Channel>,
    group_name: &str,
) -> Vec<String> {
    let mut names = registered_callable_names(channels);
    names.extend(unified_models.keys().cloned());
    if let Some(group) = groups.get(group_name) {
        names.extend(group.models.iter().cloned());
    }
    names.retain(|name| group_allows(groups, group_name, name));

    let hidden_members: HashSet<String> = unified_models
        .values()
        .filter(|model| model.hide && names.contains(&model.id))
        .flat_map(|model| {
            model
                .models
                .iter()
                .map(|member| member.model.as_str())
                .filter(|member| *member != model.id)
                .map(str::to_string)
        })
        .collect();
    names.retain(|name| !hidden_members.contains(name));

    let mut ids: Vec<String> = names.into_iter().collect();
    ids.sort();
    ids
}

/// 令牌的完整只读视图：定义字段 + 生命周期元数据（创建/最后使用时间）。
///
/// 生命周期字段由存储层维护，不属于可写契约，故不派生 serde。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRecord {
    pub token: Token,
    /// 创建时刻（unix 毫秒）；迁移前的存量令牌为 0。
    pub created_at: i64,
    /// 最后使用时刻（unix 毫秒）；`None` 表示从未使用。
    pub last_used_at: Option<i64>,
}

/// 某一渠道上某一已登记模型名的四档单价（micro-USD / 1M tokens）；
/// 缓存档 `None` 表示该档不计价。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    /// 渠道稳定身份；与 `model` 一起构成价格行主键。
    pub channel_id: i64,
    pub model: String,
    pub input_micros: i64,
    pub output_micros: i64,
    pub cache_read_micros: Option<i64>,
    pub cache_write_micros: Option<i64>,
}

/// 运行时开关键：是否落完整请求/响应 body。
pub const SETTING_FULL_BODY: &str = "full_body";
/// 运行时开关键：入站请求体大小上限（字节）。
pub const SETTING_MAX_REQUEST_BYTES: &str = "max_request_bytes";
/// 运行时开关键：请求日志 body 截断上限（字节），与入站请求上限独立。
pub const SETTING_LOG_BODY_MAX_BYTES: &str = "log_body_max_bytes";
/// 运行时开关键：价格目录自动同步间隔（天）；`0` 表示只手动同步。
pub const SETTING_CATALOG_SYNC_INTERVAL_DAYS: &str = "catalog_sync_interval_days";
/// 运行时开关键：同一 IP 窗口内允许的认证失败次数；`0` 表示关闭限流。
pub const SETTING_AUTH_THROTTLE_MAX_FAILURES: &str = "auth_throttle_max_failures";
/// 运行时开关键：认证失败计数窗口（秒）。
pub const SETTING_AUTH_THROTTLE_WINDOW_SECS: &str = "auth_throttle_window_secs";
/// 运行时开关键：SSE 重装缓冲上限（字节）。
pub const SETTING_SSE_REASSEMBLY_MAX_BYTES: &str = "sse_reassembly_max_bytes";
/// 运行时开关键：同渠道重试基础间隔（毫秒）。
pub const SETTING_RETRY_BACKOFF_MS: &str = "retry_backoff_ms";
/// 运行时开关键：同渠道指数退避封顶（毫秒）。
pub const SETTING_RETRY_BACKOFF_CAP_MS: &str = "retry_backoff_cap_ms";
/// 运行时开关键：上游 `Retry-After` 最大等待（秒）。
pub const SETTING_RETRY_AFTER_CAP_SECS: &str = "retry_after_cap_secs";
/// 目录元数据键：上次成功同步的 unix 毫秒；缺省表示从未同步。不在 Settings 契约里。
pub const SETTING_CATALOG_SYNCED_AT: &str = "catalog_synced_at";

/// 入站请求体大小上限的缺省值（字节）：覆盖常规 base64 图片，与参考网关 bifrost
/// 的 `max_request_body_size_mb: 100` 对齐。
pub const DEFAULT_MAX_REQUEST_BYTES: u64 = 100 * 1024 * 1024;
/// 请求日志 body 截断缺省值（字节）：full_body 开启时单行日志的封顶，避免复用
/// 入站 100MB 上限把 SQLite 撑慢。
pub const DEFAULT_LOG_BODY_MAX_BYTES: u64 = 1024 * 1024;
/// 认证失败限流次数缺省值。
pub const DEFAULT_AUTH_THROTTLE_MAX_FAILURES: u64 = 30;
/// 认证失败计数窗口缺省值（秒）。
pub const DEFAULT_AUTH_THROTTLE_WINDOW_SECS: u64 = 60;
/// SSE 重装缓冲上限缺省值（字节）。
pub const DEFAULT_SSE_REASSEMBLY_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// 同渠道重试基础间隔缺省值（毫秒）。
pub const DEFAULT_RETRY_BACKOFF_MS: u64 = 200;
/// 同渠道指数退避封顶缺省值（毫秒）。
pub const DEFAULT_RETRY_BACKOFF_CAP_MS: u64 = 5_000;
/// 上游 `Retry-After` 最大等待缺省值（秒）。
pub const DEFAULT_RETRY_AFTER_CAP_SECS: u64 = 60;

/// serde 缺省：PUT 省略该键时与空库加载一致。
fn default_auth_throttle_max_failures() -> u64 {
    DEFAULT_AUTH_THROTTLE_MAX_FAILURES
}
/// serde 缺省：PUT 省略该键时与空库加载一致。
fn default_auth_throttle_window_secs() -> u64 {
    DEFAULT_AUTH_THROTTLE_WINDOW_SECS
}
/// serde 缺省：PUT 省略该键时与空库加载一致。
fn default_sse_reassembly_max_bytes() -> u64 {
    DEFAULT_SSE_REASSEMBLY_MAX_BYTES
}
/// serde 缺省：PUT 省略该键时与空库加载一致。
fn default_retry_backoff_ms() -> u64 {
    DEFAULT_RETRY_BACKOFF_MS
}
/// serde 缺省：PUT 省略该键时与空库加载一致。
fn default_retry_backoff_cap_ms() -> u64 {
    DEFAULT_RETRY_BACKOFF_CAP_MS
}
/// serde 缺省：PUT 省略该键时与空库加载一致。
fn default_retry_after_cap_secs() -> u64 {
    DEFAULT_RETRY_AFTER_CAP_SECS
}
/// serde 缺省：PUT 省略该键时与空库加载一致。
fn default_log_body_max_bytes() -> u64 {
    DEFAULT_LOG_BODY_MAX_BYTES
}

/// 运行时设置的聚合契约：日志、网关保护与价格目录同步间隔。
///
/// 落库时拆成键值记录（`settings` 表），管理 API 以其 JSON 形态作为
/// wire 契约（成对读写），故派生 `Serialize`/`Deserialize` 并拒绝未知字段。
/// `catalog_synced_at` 不在此契约：它是目录元数据，随 `GET /catalog` 或 `GET /catalog/meta` 返回。
/// 新增字段在 PUT 省略时取与空库相同的缺省值，避免旧客户端把保护阈值写成 0。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// 是否落完整请求/响应 body。
    pub full_body: bool,
    /// 入站请求体大小上限（字节）。
    pub max_request_bytes: u64,
    /// 请求日志 body 截断上限（字节）；与 `max_request_bytes` 独立。
    #[serde(default = "default_log_body_max_bytes")]
    pub log_body_max_bytes: u64,
    /// 价格目录自动同步间隔（天）；`0` 表示只手动同步。
    #[serde(default)]
    pub catalog_sync_interval_days: u64,
    /// 同一 IP 窗口内允许的认证失败次数；`0` 表示关闭限流。
    #[serde(default = "default_auth_throttle_max_failures")]
    pub auth_throttle_max_failures: u64,
    /// 认证失败计数窗口（秒）。
    #[serde(default = "default_auth_throttle_window_secs")]
    pub auth_throttle_window_secs: u64,
    /// SSE 重装缓冲上限（字节）。
    #[serde(default = "default_sse_reassembly_max_bytes")]
    pub sse_reassembly_max_bytes: u64,
    /// 同渠道重试基础间隔（毫秒）。
    #[serde(default = "default_retry_backoff_ms")]
    pub retry_backoff_ms: u64,
    /// 同渠道指数退避封顶（毫秒）。
    #[serde(default = "default_retry_backoff_cap_ms")]
    pub retry_backoff_cap_ms: u64,
    /// 上游 `Retry-After` 最大等待（秒）。
    #[serde(default = "default_retry_after_cap_secs")]
    pub retry_after_cap_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            full_body: false,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            log_body_max_bytes: DEFAULT_LOG_BODY_MAX_BYTES,
            catalog_sync_interval_days: 0,
            auth_throttle_max_failures: DEFAULT_AUTH_THROTTLE_MAX_FAILURES,
            auth_throttle_window_secs: DEFAULT_AUTH_THROTTLE_WINDOW_SECS,
            sse_reassembly_max_bytes: DEFAULT_SSE_REASSEMBLY_MAX_BYTES,
            retry_backoff_ms: DEFAULT_RETRY_BACKOFF_MS,
            retry_backoff_cap_ms: DEFAULT_RETRY_BACKOFF_CAP_MS,
            retry_after_cap_secs: DEFAULT_RETRY_AFTER_CAP_SECS,
        }
    }
}

/// `Protocol` 落库用的 wire 字符串。
fn protocol_to_wire(p: Protocol) -> &'static str {
    match p {
        Protocol::OpenAiChat => "openai_chat",
        Protocol::OpenAiResponses => "openai_responses",
        Protocol::AnthropicMessages => "anthropic_messages",
    }
}

/// 从库中读出 `Protocol`。
fn protocol_from_wire(s: &str) -> Result<Protocol, StoreError> {
    match s {
        "openai_chat" => Ok(Protocol::OpenAiChat),
        "openai_responses" => Ok(Protocol::OpenAiResponses),
        "anthropic_messages" => Ok(Protocol::AnthropicMessages),
        other => Err(StoreError::InvalidResource(format!(
            "未知渠道协议: {other}"
        ))),
    }
}

/// 读出全部渠道记录（含库生成的 `id`）。
pub async fn list_channel_records(pool: &SqlitePool) -> Result<Vec<ChannelRecord>, StoreError> {
    let rows = sqlx::query(
        "SELECT id, name, protocol, base_url, api_key, models_json, model_aliases_json, \
         priority, weight, timeout_ms, max_retries, enabled, model_group FROM channels",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;

    let mut channels = Vec::with_capacity(rows.len());
    for row in rows {
        channels.push(map_channel_record(&row)?);
    }
    Ok(channels)
}

/// 把渠道行映射为 `ChannelRecord`；`enabled` 以 0/1 整数落库，非 0 视为启用。
fn map_channel_record(row: &sqlx::sqlite::SqliteRow) -> Result<ChannelRecord, StoreError> {
    let name: String = row.try_get("name").map_err(StoreError::Query)?;
    let protocol_wire: String = row.try_get("protocol").map_err(StoreError::Query)?;
    let enabled: i64 = row.try_get("enabled").map_err(StoreError::Query)?;
    // 先解析集合字段（错误信息需要引用 name），再构造结构体以避免移动后借用。
    let models: Vec<String> = serde_json::from_str(
        &row.try_get::<String, _>("models_json")
            .map_err(StoreError::Query)?,
    )
    .map_err(|_| StoreError::InvalidResource(format!("渠道 {name} 的 models_json 非法")))?;
    let model_aliases: HashMap<String, String> = serde_json::from_str(
        &row.try_get::<String, _>("model_aliases_json")
            .map_err(StoreError::Query)?,
    )
    .map_err(|_| StoreError::InvalidResource(format!("渠道 {name} 的 model_aliases_json 非法")))?;

    Ok(ChannelRecord {
        id: row.try_get("id").map_err(StoreError::Query)?,
        channel: Channel {
            base_url: row.try_get("base_url").map_err(StoreError::Query)?,
            api_key: row.try_get("api_key").map_err(StoreError::Query)?,
            priority: row.try_get("priority").map_err(StoreError::Query)?,
            weight: row.try_get("weight").map_err(StoreError::Query)?,
            timeout_ms: row.try_get("timeout_ms").map_err(StoreError::Query)?,
            max_retries: row.try_get("max_retries").map_err(StoreError::Query)?,
            enabled: enabled != 0,
            name,
            protocol: protocol_from_wire(&protocol_wire)?,
            models,
            model_aliases,
            model_group: row.try_get("model_group").map_err(StoreError::Query)?,
        },
    })
}

/// 新增一个渠道，返回库生成的 `id`。
///
/// 同名由 `channels.name` UNIQUE 约束拒绝；调用方应在事务外先查重，
/// 以便把库错误映射为业务冲突而非 500。
pub async fn insert_channel(
    conn: &mut SqliteConnection,
    channel: &Channel,
) -> Result<i64, StoreError> {
    let models_json = serde_json::to_string(&channel.models).map_err(serde_error)?;
    let aliases_json = serde_json::to_string(&channel.model_aliases).map_err(serde_error)?;

    let result = sqlx::query(
        "INSERT INTO channels \
         (name, protocol, base_url, api_key, models_json, model_aliases_json, \
          priority, weight, timeout_ms, max_retries, enabled, model_group) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&channel.name)
    .bind(protocol_to_wire(channel.protocol))
    .bind(&channel.base_url)
    .bind(&channel.api_key)
    .bind(&models_json)
    .bind(&aliases_json)
    .bind(channel.priority)
    .bind(channel.weight)
    .bind(channel.timeout_ms as i64)
    .bind(channel.max_retries)
    .bind(channel.enabled)
    .bind(&channel.model_group)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;

    Ok(result.last_insert_rowid())
}

/// 按 `id` 整体替换渠道定义；`name` 变化即改名，`id` 保持不变。
pub async fn update_channel(
    conn: &mut SqliteConnection,
    id: i64,
    channel: &Channel,
) -> Result<(), StoreError> {
    let models_json = serde_json::to_string(&channel.models).map_err(serde_error)?;
    let aliases_json = serde_json::to_string(&channel.model_aliases).map_err(serde_error)?;

    sqlx::query(
        "UPDATE channels SET \
           name = ?, protocol = ?, base_url = ?, api_key = ?, \
           models_json = ?, model_aliases_json = ?, \
           priority = ?, weight = ?, timeout_ms = ?, max_retries = ?, enabled = ?, \
           model_group = ? \
         WHERE id = ?",
    )
    .bind(&channel.name)
    .bind(protocol_to_wire(channel.protocol))
    .bind(&channel.base_url)
    .bind(&channel.api_key)
    .bind(&models_json)
    .bind(&aliases_json)
    .bind(channel.priority)
    .bind(channel.weight)
    .bind(channel.timeout_ms as i64)
    .bind(channel.max_retries)
    .bind(channel.enabled)
    .bind(&channel.model_group)
    .bind(id)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;

    Ok(())
}

/// 按 `id` 删除渠道；不存在视为成功（幂等）。
pub async fn delete_channel(conn: &mut SqliteConnection, id: i64) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM channels WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 读出全部令牌。
pub async fn list_tokens(pool: &SqlitePool) -> Result<Vec<Token>, StoreError> {
    Ok(list_token_records(pool)
        .await?
        .into_iter()
        .map(|record| record.token)
        .collect())
}

/// 读出全部令牌记录（含创建/最后使用时间等生命周期元数据）。
pub async fn list_token_records(pool: &SqlitePool) -> Result<Vec<TokenRecord>, StoreError> {
    let rows = sqlx::query(
        "SELECT token_key, name, limit_usd_micros, enabled, created_at, last_used_at, model_group \
         FROM tokens",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;

    rows.iter().map(map_token_record).collect()
}

/// 按 `token_key` 读出一条令牌记录；不存在返回 `None`。
pub async fn get_token_record(
    pool: &SqlitePool,
    token_key: &str,
) -> Result<Option<TokenRecord>, StoreError> {
    let row = sqlx::query(
        "SELECT token_key, name, limit_usd_micros, enabled, created_at, last_used_at, model_group \
         FROM tokens WHERE token_key = ?",
    )
    .bind(token_key)
    .fetch_optional(pool)
    .await
    .map_err(StoreError::Query)?;

    row.as_ref().map(map_token_record).transpose()
}

/// 把令牌行映射为 `TokenRecord`；`enabled` 以 0/1 整数落库，非 0 视为启用。
fn map_token_record(row: &sqlx::sqlite::SqliteRow) -> Result<TokenRecord, StoreError> {
    let enabled: i64 = row.try_get("enabled").map_err(StoreError::Query)?;
    Ok(TokenRecord {
        token: Token {
            token_key: row.try_get("token_key").map_err(StoreError::Query)?,
            name: row.try_get("name").map_err(StoreError::Query)?,
            limit_usd_micros: row.try_get("limit_usd_micros").map_err(StoreError::Query)?,
            enabled: enabled != 0,
            model_group: row.try_get("model_group").map_err(StoreError::Query)?,
        },
        created_at: row.try_get("created_at").map_err(StoreError::Query)?,
        last_used_at: row.try_get("last_used_at").map_err(StoreError::Query)?,
    })
}

/// 新增或整体替换一个令牌（按 `token_key`），同一事务内幂等。
///
/// `created_at` 仅在首次插入时落库；冲突覆盖不改创建时间，也不改 `last_used_at`
/// （编辑属性不算使用）。
pub async fn upsert_token(
    conn: &mut SqliteConnection,
    token: &Token,
    created_at: i64,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO tokens (token_key, name, limit_usd_micros, enabled, created_at, model_group) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(token_key) DO UPDATE SET \
           name = excluded.name, limit_usd_micros = excluded.limit_usd_micros, \
           enabled = excluded.enabled, model_group = excluded.model_group",
    )
    .bind(&token.token_key)
    .bind(&token.name)
    .bind(token.limit_usd_micros)
    .bind(token.enabled)
    .bind(created_at)
    .bind(&token.model_group)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// 刷新令牌的最后使用时间；供网关在请求通过计费准入后调用。
pub async fn touch_token_used(
    conn: &mut SqliteConnection,
    token_key: &str,
    at: i64,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE tokens SET last_used_at = ? WHERE token_key = ?")
        .bind(at)
        .bind(token_key)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 按 `token_key` 删除令牌；不存在视为成功（幂等）。
pub async fn delete_token(conn: &mut SqliteConnection, token_key: &str) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM tokens WHERE token_key = ?")
        .bind(token_key)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 读出全部模型组。
pub async fn list_model_groups(pool: &SqlitePool) -> Result<Vec<ModelGroup>, StoreError> {
    let rows = sqlx::query("SELECT name, models_json FROM model_groups")
        .fetch_all(pool)
        .await
        .map_err(StoreError::Query)?;

    rows.iter().map(map_model_group).collect()
}

/// 把模型组行映射为 `ModelGroup`。
fn map_model_group(row: &sqlx::sqlite::SqliteRow) -> Result<ModelGroup, StoreError> {
    let name: String = row.try_get("name").map_err(StoreError::Query)?;
    let models: Vec<String> = serde_json::from_str(
        &row.try_get::<String, _>("models_json")
            .map_err(StoreError::Query)?,
    )
    .map_err(|_| StoreError::InvalidResource(format!("模型组 {name} 的 models_json 非法")))?;
    Ok(ModelGroup { name, models })
}

/// 新增或整体替换一个模型组（按 `name`），同一事务内幂等。
pub async fn upsert_model_group(
    conn: &mut SqliteConnection,
    group: &ModelGroup,
) -> Result<(), StoreError> {
    let models_json = serde_json::to_string(&group.models).map_err(serde_error)?;
    sqlx::query(
        "INSERT INTO model_groups (name, models_json) VALUES (?, ?) \
         ON CONFLICT(name) DO UPDATE SET models_json = excluded.models_json",
    )
    .bind(&group.name)
    .bind(&models_json)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// 按 `name` 删除模型组；不存在视为成功（幂等）。
///
/// 调用方须先处理令牌绑定：内置 `default` 与仍有令牌的组不应走到这里。
pub async fn delete_model_group(conn: &mut SqliteConnection, name: &str) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM model_groups WHERE name = ?")
        .bind(name)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 统计绑定到指定模型组的令牌数。
pub async fn count_tokens_in_group(
    conn: &mut SqliteConnection,
    group: &str,
) -> Result<u64, StoreError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tokens WHERE model_group = ?")
        .bind(group)
        .fetch_one(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(count.max(0) as u64)
}

/// 读出全部统一模型。
pub async fn list_unified_models(pool: &SqlitePool) -> Result<Vec<UnifiedModel>, StoreError> {
    let rows = sqlx::query("SELECT id, models_json, hide FROM unified_models")
        .fetch_all(pool)
        .await
        .map_err(StoreError::Query)?;

    rows.iter().map(map_unified_model).collect()
}

/// 把统一模型行映射为 `UnifiedModel`；`hide` 以 0/1 整数落库，非 0 视为开启。
fn map_unified_model(row: &sqlx::sqlite::SqliteRow) -> Result<UnifiedModel, StoreError> {
    let id: String = row.try_get("id").map_err(StoreError::Query)?;
    let hide: i64 = row.try_get("hide").map_err(StoreError::Query)?;
    let models: Vec<UnifiedMember> = serde_json::from_str(
        &row.try_get::<String, _>("models_json")
            .map_err(StoreError::Query)?,
    )
    .map_err(|_| StoreError::InvalidResource(format!("统一模型 {id} 的 models_json 非法")))?;
    Ok(UnifiedModel {
        id,
        models,
        hide: hide != 0,
    })
}

/// 新增或整体替换一个统一模型（按 `id`），同一事务内幂等。
pub async fn upsert_unified_model(
    conn: &mut SqliteConnection,
    model: &UnifiedModel,
) -> Result<(), StoreError> {
    let models_json = serde_json::to_string(&model.models).map_err(serde_error)?;
    sqlx::query(
        "INSERT INTO unified_models (id, models_json, hide) VALUES (?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET models_json = excluded.models_json, hide = excluded.hide",
    )
    .bind(&model.id)
    .bind(&models_json)
    .bind(model.hide)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// 按 `id` 删除统一模型；不存在视为成功（幂等）。
pub async fn delete_unified_model(conn: &mut SqliteConnection, id: &str) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM unified_models WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 把绑定到 `from_group` 的令牌改回内置 `default`。
pub async fn rebind_tokens_to_default(
    conn: &mut SqliteConnection,
    from_group: &str,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE tokens SET model_group = ? WHERE model_group = ?")
        .bind(DEFAULT_MODEL_GROUP)
        .bind(from_group)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 把绑定到 `from_group` 的渠道默认组改回内置 `default`。
pub async fn rebind_channels_to_default(
    conn: &mut SqliteConnection,
    from_group: &str,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE channels SET model_group = ? WHERE model_group = ?")
        .bind(DEFAULT_MODEL_GROUP)
        .bind(from_group)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 判断令牌定义是否存在。
///
/// 供余额调整等「先校验存在再写余额」的原语在事务内使用，与后续写持同一写锁，
/// 避免并发删除令牌后仍写出一条孤儿余额行、被后续重建令牌复活。
pub async fn token_exists(
    conn: &mut SqliteConnection,
    token_key: &str,
) -> Result<bool, StoreError> {
    let row = sqlx::query("SELECT 1 FROM tokens WHERE token_key = ?")
        .bind(token_key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(row.is_some())
}

/// 读出全部价格（每渠道每模型一行）。
pub async fn list_prices(pool: &SqlitePool) -> Result<Vec<Price>, StoreError> {
    let rows = sqlx::query(
        "SELECT channel_id, model, input_micros, output_micros, cache_read_micros, cache_write_micros \
         FROM prices",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;

    let prices = rows
        .iter()
        .map(|row| {
            Ok(Price {
                channel_id: row.try_get("channel_id").map_err(StoreError::Query)?,
                model: row.try_get("model").map_err(StoreError::Query)?,
                input_micros: row.try_get("input_micros").map_err(StoreError::Query)?,
                output_micros: row.try_get("output_micros").map_err(StoreError::Query)?,
                cache_read_micros: row
                    .try_get("cache_read_micros")
                    .map_err(StoreError::Query)?,
                cache_write_micros: row
                    .try_get("cache_write_micros")
                    .map_err(StoreError::Query)?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    Ok(prices)
}

/// 新增或整体替换一条价格（按 `channel_id` + `model`），同一事务内幂等。
pub async fn upsert_price(conn: &mut SqliteConnection, price: &Price) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO prices \
         (channel_id, model, input_micros, output_micros, cache_read_micros, cache_write_micros) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(channel_id, model) DO UPDATE SET \
           input_micros = excluded.input_micros, output_micros = excluded.output_micros, \
           cache_read_micros = excluded.cache_read_micros, \
           cache_write_micros = excluded.cache_write_micros",
    )
    .bind(price.channel_id)
    .bind(&price.model)
    .bind(price.input_micros)
    .bind(price.output_micros)
    .bind(price.cache_read_micros)
    .bind(price.cache_write_micros)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// 按渠道与模型名删除价格；不存在视为成功（幂等）。
pub async fn delete_price(
    conn: &mut SqliteConnection,
    channel_id: i64,
    model: &str,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM prices WHERE channel_id = ? AND model = ?")
        .bind(channel_id)
        .bind(model)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 删掉该渠道上已不在可调用名集合中的价格行。
pub async fn retain_channel_prices(
    conn: &mut SqliteConnection,
    channel_id: i64,
    names: &HashSet<String>,
) -> Result<(), StoreError> {
    if names.is_empty() {
        sqlx::query("DELETE FROM prices WHERE channel_id = ?")
            .bind(channel_id)
            .execute(&mut *conn)
            .await
            .map_err(StoreError::Query)?;
        return Ok(());
    }
    let listed = serde_json::to_string(names).map_err(serde_error)?;
    sqlx::query(
        "DELETE FROM prices WHERE channel_id = ? \
         AND model NOT IN (SELECT value FROM json_each(?))",
    )
    .bind(channel_id)
    .bind(&listed)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// 写入一个运行时开关（`setting_value` 以 JSON 编码），幂等。
pub async fn set_setting(
    conn: &mut SqliteConnection,
    key: &str,
    value: &Value,
) -> Result<(), StoreError> {
    let encoded = serde_json::to_string(value).map_err(serde_error)?;
    sqlx::query(
        "INSERT INTO settings (setting_key, setting_value) VALUES (?, ?) \
         ON CONFLICT(setting_key) DO UPDATE SET setting_value = excluded.setting_value",
    )
    .bind(key)
    .bind(&encoded)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// 读出全部运行时开关，键为开关名、值为 JSON。
pub async fn list_settings(pool: &SqlitePool) -> Result<HashMap<String, Value>, StoreError> {
    let rows = sqlx::query("SELECT setting_key, setting_value FROM settings")
        .fetch_all(pool)
        .await
        .map_err(StoreError::Query)?;

    let mut settings = HashMap::with_capacity(rows.len());
    for row in rows {
        let key: String = row.try_get("setting_key").map_err(StoreError::Query)?;
        let encoded: String = row.try_get("setting_value").map_err(StoreError::Query)?;
        let value: Value = serde_json::from_str(&encoded)
            .map_err(|_| StoreError::InvalidResource(format!("开关 {key} 的值非法")))?;
        settings.insert(key, value);
    }
    Ok(settings)
}

/// 按 `key` 删除一个运行时开关；不存在视为成功（幂等）。
pub async fn delete_setting(conn: &mut SqliteConnection, key: &str) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM settings WHERE setting_key = ?")
        .bind(key)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 成对写入运行时设置：契约内全部键一同落库（幂等）。
///
/// 供管理 API 的 `/settings` 写操作使用：各键在同一个事务内写入，保证聚合
/// 契约的原子性。读回经 `list_settings` 由调用方聚合。`catalog_synced_at` 不在此写入。
pub async fn upsert_settings(
    conn: &mut SqliteConnection,
    settings: &Settings,
) -> Result<(), StoreError> {
    set_setting(conn, SETTING_FULL_BODY, &Value::Bool(settings.full_body)).await?;
    set_setting(
        conn,
        SETTING_MAX_REQUEST_BYTES,
        &Value::from(settings.max_request_bytes),
    )
    .await?;
    set_setting(
        conn,
        SETTING_LOG_BODY_MAX_BYTES,
        &Value::from(settings.log_body_max_bytes),
    )
    .await?;
    set_setting(
        conn,
        SETTING_CATALOG_SYNC_INTERVAL_DAYS,
        &Value::from(settings.catalog_sync_interval_days),
    )
    .await?;
    set_setting(
        conn,
        SETTING_AUTH_THROTTLE_MAX_FAILURES,
        &Value::from(settings.auth_throttle_max_failures),
    )
    .await?;
    set_setting(
        conn,
        SETTING_AUTH_THROTTLE_WINDOW_SECS,
        &Value::from(settings.auth_throttle_window_secs),
    )
    .await?;
    set_setting(
        conn,
        SETTING_SSE_REASSEMBLY_MAX_BYTES,
        &Value::from(settings.sse_reassembly_max_bytes),
    )
    .await?;
    set_setting(
        conn,
        SETTING_RETRY_BACKOFF_MS,
        &Value::from(settings.retry_backoff_ms),
    )
    .await?;
    set_setting(
        conn,
        SETTING_RETRY_BACKOFF_CAP_MS,
        &Value::from(settings.retry_backoff_cap_ms),
    )
    .await?;
    set_setting(
        conn,
        SETTING_RETRY_AFTER_CAP_SECS,
        &Value::from(settings.retry_after_cap_secs),
    )
    .await?;
    Ok(())
}

/// 把 serde 序列化错误包装为存储层错误。
fn serde_error(err: serde_json::Error) -> StoreError {
    StoreError::InvalidResource(format!("JSON 序列化失败: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    /// 建一个内存外的临时 SQLite 连接池，并跑完全部迁移。
    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let pool = crate::store::open(&dir.path().join("test.db"))
            .await
            .expect("应能打开临时库");
        (dir, pool)
    }

    fn sample_channel() -> Channel {
        let mut aliases = HashMap::new();
        aliases.insert("fast".to_string(), "gpt-4o-mini".to_string());
        Channel {
            name: "c1".to_string(),
            protocol: Protocol::OpenAiChat,
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "sk-x".to_string(),
            models: vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()],
            model_aliases: aliases,
            priority: 1,
            weight: 2,
            timeout_ms: 120_000,
            max_retries: 2,
            enabled: true,
            model_group: DEFAULT_MODEL_GROUP.to_string(),
        }
    }

    /// 新建渠道时全部可调用名算新增；更新只算相对上一版新出现的名字。
    #[test]
    fn newly_callable_names_diffs_models_and_alias_keys() {
        let previous = sample_channel();
        let mut next = sample_channel();
        next.models.push("gpt-5".to_string());
        next.model_aliases
            .insert("flash".to_string(), "gpt-4o-mini".to_string());
        next.models.retain(|name| name != "gpt-4o");
        assert_eq!(
            newly_callable_names(None, &next),
            vec![
                "fast".to_string(),
                "flash".to_string(),
                "gpt-4o-mini".to_string(),
                "gpt-5".to_string()
            ]
        );
        assert_eq!(
            newly_callable_names(Some(&previous), &next),
            vec!["flash".to_string(), "gpt-5".to_string()]
        );
    }

    /// 并入组时跳过已有名字、保序追加；删渠道模型不经过此路径。
    #[tokio::test]
    async fn union_names_into_group_appends_missing_only() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        upsert_model_group(
            &mut conn,
            &ModelGroup {
                name: "coding".to_string(),
                models: vec!["gpt-4o".to_string()],
            },
        )
        .await
        .expect("应能写组");
        union_names_into_group(
            &mut conn,
            "coding",
            &["gpt-4o".to_string(), "fast".to_string(), "mini".to_string()],
        )
        .await
        .expect("应能并入");
        let group = get_model_group(&mut conn, "coding")
            .await
            .expect("应能读组")
            .expect("组应存在");
        assert_eq!(
            group.models,
            vec!["gpt-4o".to_string(), "fast".to_string(), "mini".to_string()]
        );
    }

    /// 渠道插入 → 读回往返一致（含库生成的 id、集合字段与协议）。
    #[tokio::test]
    async fn channel_insert_then_list_roundtrip() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        let id = insert_channel(&mut conn, &sample_channel())
            .await
            .expect("应能写渠道");
        assert!(id > 0, "插入应返回库生成的 id");

        let records = list_channel_records(&pool).await.expect("应能读渠道");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, id);
        assert_eq!(records[0].channel, sample_channel());
    }

    /// 按 id 整体替换：可改字段也可改名，id 保持不变，不产生重复行。
    #[tokio::test]
    async fn channel_update_by_id_replaces_definition_and_renames() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        let id = insert_channel(&mut conn, &sample_channel())
            .await
            .expect("应能写渠道");
        let mut updated = sample_channel();
        updated.name = "c1-renamed".to_string();
        updated.timeout_ms = 3_000;
        updated.enabled = false;
        update_channel(&mut conn, id, &updated)
            .await
            .expect("应能按 id 覆盖渠道");

        let records = list_channel_records(&pool).await.expect("应能读渠道");
        assert_eq!(records.len(), 1, "覆盖后仍为单行");
        assert_eq!(records[0].id, id, "改名不应改变 id");
        assert_eq!(records[0].channel, updated, "字段与改名应整体生效");
    }

    /// 按 id 删除渠道后读回为空；重复删除同一 id 幂等成功。
    #[tokio::test]
    async fn channel_delete_is_idempotent() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        let id = insert_channel(&mut conn, &sample_channel())
            .await
            .expect("应能写渠道");
        delete_channel(&mut conn, id).await.expect("应能删渠道");
        delete_channel(&mut conn, id).await.expect("重复删除应幂等");

        assert!(
            list_channel_records(&pool)
                .await
                .expect("应能读")
                .is_empty()
        );
    }

    /// 令牌播种 → 读回往返一致；limit 为 NULL 表示无上限，enabled 缺省启用。
    #[tokio::test]
    async fn token_upsert_then_list_roundtrip() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        let token = Token {
            token_key: "sk-a".to_string(),
            name: "dev".to_string(),
            limit_usd_micros: Some(5_000_000),
            enabled: true,
            model_group: DEFAULT_MODEL_GROUP.to_string(),
        };
        upsert_token(&mut conn, &token, 1_700_000_000_000)
            .await
            .expect("应能写令牌");

        let tokens = list_tokens(&pool).await.expect("应能读令牌");
        assert_eq!(tokens, vec![token]);
    }

    /// 生命周期元数据：创建时间首次插入后固定，覆盖更新不改；最后使用时间可刷新。
    #[tokio::test]
    async fn token_record_tracks_lifecycle() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        let token = Token {
            token_key: "sk-a".to_string(),
            name: "dev".to_string(),
            limit_usd_micros: None,
            enabled: true,
            model_group: DEFAULT_MODEL_GROUP.to_string(),
        };
        upsert_token(&mut conn, &token, 1_000)
            .await
            .expect("应能写令牌");

        let record = get_token_record(&pool, "sk-a")
            .await
            .expect("应能读记录")
            .expect("记录应存在");
        assert_eq!(record.created_at, 1_000);
        assert_eq!(record.last_used_at, None, "未使用时为空");

        // 覆盖更新（带不同的 created_at 入参）不改已有创建时间。
        let mut renamed = token.clone();
        renamed.name = "v2".to_string();
        renamed.enabled = false;
        upsert_token(&mut conn, &renamed, 9_999)
            .await
            .expect("应能覆盖令牌");
        touch_token_used(&mut conn, "sk-a", 2_000)
            .await
            .expect("应能刷新最后使用时间");

        let record = get_token_record(&pool, "sk-a")
            .await
            .expect("应能读记录")
            .expect("记录应存在");
        assert_eq!(record.token.name, "v2");
        assert!(!record.token.enabled, "覆盖应更新启用状态");
        assert_eq!(record.created_at, 1_000, "覆盖不应重置创建时间");
        assert_eq!(record.last_used_at, Some(2_000));

        assert!(
            get_token_record(&pool, "sk-none")
                .await
                .expect("应能查询")
                .is_none(),
            "不存在的令牌返回 None"
        );
    }

    /// 令牌定义存在性判断：存在返回 true，不存在返回 false。
    #[tokio::test]
    async fn token_exists_truthiness() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        assert!(
            !token_exists(&mut conn, "sk-a").await.expect("应能查询"),
            "未播种的令牌不存在"
        );
        upsert_token(
            &mut conn,
            &Token {
                token_key: "sk-a".to_string(),
                name: "dev".to_string(),
                limit_usd_micros: None,
                enabled: true,
                model_group: DEFAULT_MODEL_GROUP.to_string(),
            },
            1,
        )
        .await
        .expect("应能写令牌");
        assert!(
            token_exists(&mut conn, "sk-a").await.expect("应能查询"),
            "播种后的令牌应存在"
        );
    }

    /// 令牌属性更新不触碰余额：余额存 token_balance 表，令牌表只存定义。
    #[tokio::test]
    async fn token_attr_update_keeps_balance_separate() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");

        upsert_token(
            &mut conn,
            &Token {
                token_key: "sk-a".to_string(),
                name: "v1".to_string(),
                limit_usd_micros: None,
                enabled: true,
                model_group: DEFAULT_MODEL_GROUP.to_string(),
            },
            1,
        )
        .await
        .expect("应能写令牌");
        crate::store::ensure_token_balance(&mut conn, "sk-a", 3.0, 1)
            .await
            .expect("应能初始化余额");
        let before = list_tokens(&pool).await.expect("应能读令牌");
        assert_eq!(before[0].limit_usd_micros, None);

        upsert_token(
            &mut conn,
            &Token {
                token_key: "sk-a".to_string(),
                name: "v2".to_string(),
                limit_usd_micros: Some(9_000_000),
                enabled: true,
                model_group: DEFAULT_MODEL_GROUP.to_string(),
            },
            2,
        )
        .await
        .expect("应能更新令牌");

        let balance = crate::store::get_token_balance(&mut conn, "sk-a")
            .await
            .expect("应能读余额")
            .expect("余额应存在");
        assert_eq!(balance.balance_usd_micros, 3_000_000, "改属性不应重置余额");
        let after = list_tokens(&pool).await.expect("应能读令牌");
        assert_eq!(after[0].name, "v2");
        assert_eq!(after[0].limit_usd_micros, Some(9_000_000));
        assert_eq!(after[0].model_group, DEFAULT_MODEL_GROUP);
    }

    /// 空库即有内置 default；CRUD 往返；删除幂等。
    #[tokio::test]
    async fn model_group_default_exists_and_roundtrips() {
        let (_dir, pool) = test_pool().await;
        let groups = list_model_groups(&pool).await.expect("应能读组");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, DEFAULT_MODEL_GROUP);
        assert!(groups[0].models.is_empty());

        let mut conn = pool.acquire().await.expect("应能获取连接");
        let coding = ModelGroup {
            name: "coding".to_string(),
            models: vec!["gpt-4o".to_string(), "fast".to_string()],
        };
        upsert_model_group(&mut conn, &coding)
            .await
            .expect("应能写组");
        let groups = list_model_groups(&pool).await.expect("应能读组");
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().any(|g| g == &coding));

        delete_model_group(&mut conn, "coding")
            .await
            .expect("应能删组");
        delete_model_group(&mut conn, "coding")
            .await
            .expect("重复删除应幂等");
        let groups = list_model_groups(&pool).await.expect("应能读组");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, DEFAULT_MODEL_GROUP);
    }

    /// 令牌改绑后计数变化；改回 default 后原组计数为 0。
    #[tokio::test]
    async fn rebind_tokens_to_default_clears_group_membership() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        upsert_model_group(
            &mut conn,
            &ModelGroup {
                name: "coding".to_string(),
                models: vec!["gpt-4o".to_string()],
            },
        )
        .await
        .expect("应能写组");
        upsert_token(
            &mut conn,
            &Token {
                token_key: "sk-a".to_string(),
                name: "dev".to_string(),
                limit_usd_micros: None,
                enabled: true,
                model_group: "coding".to_string(),
            },
            1,
        )
        .await
        .expect("应能写令牌");
        assert_eq!(
            count_tokens_in_group(&mut conn, "coding")
                .await
                .expect("应能计数"),
            1
        );
        rebind_tokens_to_default(&mut conn, "coding")
            .await
            .expect("应能改绑");
        assert_eq!(
            count_tokens_in_group(&mut conn, "coding")
                .await
                .expect("应能计数"),
            0
        );
        let tokens = list_tokens(&pool).await.expect("应能读令牌");
        assert_eq!(tokens[0].model_group, DEFAULT_MODEL_GROUP);
    }

    /// default 含未放入其他组的名字；只出现在自定义组的名字对 default 不允许。
    #[test]
    fn group_allows_implicit_default_and_explicit_lists() {
        let mut groups = HashMap::new();
        groups.insert(
            DEFAULT_MODEL_GROUP.to_string(),
            ModelGroup {
                name: DEFAULT_MODEL_GROUP.to_string(),
                models: vec!["also-in-default".to_string()],
            },
        );
        groups.insert(
            "coding".to_string(),
            ModelGroup {
                name: "coding".to_string(),
                models: vec!["gpt-4o".to_string(), "also-in-default".to_string()],
            },
        );
        assert!(group_allows(&groups, "coding", "gpt-4o"));
        assert!(!group_allows(&groups, "coding", "fast"));
        assert!(
            group_allows(&groups, DEFAULT_MODEL_GROUP, "fast"),
            "未放入其他组的名字视为 default"
        );
        assert!(
            !group_allows(&groups, DEFAULT_MODEL_GROUP, "gpt-4o"),
            "只在 coding 的名字不隐式属于 default"
        );
        assert!(group_allows(
            &groups,
            DEFAULT_MODEL_GROUP,
            "also-in-default"
        ));
        assert!(!group_allows(&groups, "ghost", "fast"));
    }

    /// 列表随组过滤；隐藏拿掉被收进成员、保留统一 ID；组外名字不出现。
    #[test]
    fn visible_model_ids_filters_group_and_hide() {
        let channel = sample_channel();
        let mut groups = HashMap::new();
        groups.insert(
            DEFAULT_MODEL_GROUP.to_string(),
            ModelGroup {
                name: DEFAULT_MODEL_GROUP.to_string(),
                models: vec![],
            },
        );
        groups.insert(
            "coding".to_string(),
            ModelGroup {
                name: "coding".to_string(),
                models: vec!["gpt-4o".to_string(), "coding".to_string()],
            },
        );
        let mut unified = HashMap::new();
        unified.insert(
            "coding".to_string(),
            UnifiedModel {
                id: "coding".to_string(),
                models: vec![UnifiedMember {
                    channel_id: 1,
                    model: "gpt-4o".to_string(),
                }],
                hide: true,
            },
        );

        let default_ids = visible_model_ids(&groups, &unified, [&channel], DEFAULT_MODEL_GROUP);
        assert_eq!(default_ids, vec!["fast", "gpt-4o-mini"]);

        let coding_ids = visible_model_ids(&groups, &unified, [&channel], "coding");
        assert_eq!(coding_ids, vec!["coding"]);
        assert!(
            !coding_ids.iter().any(|id| id == "gpt-4o"),
            "隐藏后被收进模型不出现在列表"
        );
        assert!(
            !coding_ids.iter().any(|id| id == "fast"),
            "组外模型不出现在列表"
        );
    }

    /// 已登记名 = 各渠道 models ∪ 别名 key；禁用渠道仍计入。
    #[test]
    fn registered_callable_names_includes_disabled_and_aliases() {
        let mut enabled = sample_channel();
        let mut disabled = sample_channel();
        disabled.name = "off".to_string();
        disabled.enabled = false;
        disabled.models = vec!["only-on-disabled".to_string()];
        disabled.model_aliases.clear();
        enabled.models = vec!["gpt-4o".to_string()];
        let names = registered_callable_names([&enabled, &disabled]);
        assert!(names.contains("gpt-4o"));
        assert!(names.contains("fast"), "别名 key 应计入");
        assert!(
            names.contains("only-on-disabled"),
            "禁用渠道的模型仍算已登记"
        );
        assert!(!names.contains("gpt-4o-mini"), "别名 value 不参与匹配");
    }

    /// 未隐藏且 ID 已登记 → 撞名；开隐藏或 ID 未登记则否。
    #[test]
    fn unhidden_unified_id_collides_only_when_registered_and_visible() {
        let registered = HashSet::from(["gpt-4o".to_string()]);
        assert!(unhidden_unified_id_collides("gpt-4o", false, &registered));
        assert!(!unhidden_unified_id_collides("gpt-4o", true, &registered));
        assert!(!unhidden_unified_id_collides("coding", false, &registered));
    }

    /// 统一模型 CRUD 往返；删除幂等；hide 以 0/1 落库。
    #[tokio::test]
    async fn unified_model_upsert_then_list_roundtrip() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        let model = UnifiedModel {
            id: "coding".to_string(),
            models: vec![
                UnifiedMember {
                    channel_id: 1,
                    model: "gpt-4o".to_string(),
                },
                UnifiedMember {
                    channel_id: 1,
                    model: "fast".to_string(),
                },
            ],
            hide: true,
        };
        upsert_unified_model(&mut conn, &model)
            .await
            .expect("应能写统一模型");
        let listed = list_unified_models(&pool).await.expect("应能读");
        assert_eq!(listed, vec![model.clone()]);

        delete_unified_model(&mut conn, "coding")
            .await
            .expect("应能删");
        delete_unified_model(&mut conn, "coding")
            .await
            .expect("重复删除应幂等");
        assert!(list_unified_models(&pool).await.expect("应能读").is_empty());
    }

    /// 价格播种 → 读回往返一致；缓存档 NULL 保留。
    #[tokio::test]
    async fn price_upsert_then_list_roundtrip() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        let channel_id = insert_channel(
            &mut conn,
            &Channel {
                name: "p".to_string(),
                protocol: crate::config::Protocol::OpenAiChat,
                base_url: "http://127.0.0.1:9".to_string(),
                api_key: "sk".to_string(),
                models: vec!["gpt-4o".to_string()],
                model_aliases: HashMap::new(),
                priority: 0,
                weight: 1,
                timeout_ms: 1000,
                max_retries: 0,
                enabled: true,
                model_group: DEFAULT_MODEL_GROUP.to_string(),
            },
        )
        .await
        .expect("应能写渠道");
        let price = Price {
            channel_id,
            model: "gpt-4o".to_string(),
            input_micros: 2_500_000,
            output_micros: 10_000_000,
            cache_read_micros: Some(1_250_000),
            cache_write_micros: None,
        };
        upsert_price(&mut conn, &price).await.expect("应能写价格");

        let prices = list_prices(&pool).await.expect("应能读价格");
        assert_eq!(prices, vec![price]);
    }

    /// 同一可调用名在两条渠道上各自一行；改其中一行不影响另一行。
    #[tokio::test]
    async fn same_model_prices_are_independent_per_channel() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        let mut left = sample_channel();
        left.name = "left".to_string();
        let mut right = sample_channel();
        right.name = "right".to_string();
        let left_id = insert_channel(&mut conn, &left)
            .await
            .expect("应能写左渠道");
        let right_id = insert_channel(&mut conn, &right)
            .await
            .expect("应能写右渠道");

        let mut left_price = Price {
            channel_id: left_id,
            model: "gpt-4o".to_string(),
            input_micros: 1_000_000,
            output_micros: 2_000_000,
            cache_read_micros: None,
            cache_write_micros: None,
        };
        let right_price = Price {
            channel_id: right_id,
            model: "gpt-4o".to_string(),
            input_micros: 9_000_000,
            output_micros: 8_000_000,
            cache_read_micros: None,
            cache_write_micros: None,
        };
        upsert_price(&mut conn, &left_price)
            .await
            .expect("应能写左渠道价格");
        upsert_price(&mut conn, &right_price)
            .await
            .expect("应能写右渠道价格");

        left_price.input_micros = 3_000_000;
        upsert_price(&mut conn, &left_price)
            .await
            .expect("应能改左渠道价格");

        let prices = list_prices(&pool).await.expect("应能读价格");
        let listed_left = prices
            .iter()
            .find(|price| price.channel_id == left_id && price.model == "gpt-4o")
            .expect("左渠道价格应在");
        let listed_right = prices
            .iter()
            .find(|price| price.channel_id == right_id && price.model == "gpt-4o")
            .expect("右渠道价格应在");
        assert_eq!(listed_left.input_micros, 3_000_000);
        assert_eq!(listed_right.input_micros, 9_000_000);

        retain_channel_prices(&mut conn, left_id, &HashSet::new())
            .await
            .expect("应能清掉左渠道价格");
        let prices = list_prices(&pool).await.expect("应能读价格");
        assert!(
            prices.iter().all(|price| price.channel_id != left_id),
            "左渠道价格应被 retain 清掉"
        );
        assert!(
            prices
                .iter()
                .any(|price| price.channel_id == right_id && price.model == "gpt-4o"),
            "右渠道价格应保留"
        );
    }

    /// 开关写入 → 读回往返一致；删除幂等。
    #[tokio::test]
    async fn setting_set_list_delete_roundtrip() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        set_setting(&mut conn, SETTING_FULL_BODY, &Value::Bool(true))
            .await
            .expect("应能写开关");
        set_setting(
            &mut conn,
            SETTING_MAX_REQUEST_BYTES,
            &Value::from(10_000_000u64),
        )
        .await
        .expect("应能写开关");

        let settings = list_settings(&pool).await.expect("应能读开关");
        assert_eq!(
            settings[SETTING_FULL_BODY],
            Value::Bool(true),
            "布尔开关往返一致"
        );
        assert_eq!(
            settings[SETTING_MAX_REQUEST_BYTES],
            Value::from(10_000_000u64),
            "整数开关往返一致"
        );

        delete_setting(&mut conn, SETTING_FULL_BODY)
            .await
            .expect("应能删开关");
        delete_setting(&mut conn, SETTING_FULL_BODY)
            .await
            .expect("重复删除应幂等");
        let settings = list_settings(&pool).await.expect("应能读开关");
        assert!(!settings.contains_key(SETTING_FULL_BODY));
        assert!(settings.contains_key(SETTING_MAX_REQUEST_BYTES));
    }

    /// 设置成对写入 → 读回往返一致；覆盖后单份值更新。
    #[tokio::test]
    async fn settings_upsert_roundtrip() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        let settings = Settings {
            full_body: true,
            max_request_bytes: 8_000_000,
            catalog_sync_interval_days: 0,
            ..Settings::default()
        };
        upsert_settings(&mut conn, &settings)
            .await
            .expect("应能写设置");

        let map = list_settings(&pool).await.expect("应能读开关");
        assert_eq!(map[SETTING_FULL_BODY], Value::Bool(true));
        assert_eq!(map[SETTING_MAX_REQUEST_BYTES], Value::from(8_000_000u64));

        // 覆盖：仅改上限，full_body 保留。
        upsert_settings(
            &mut conn,
            &Settings {
                full_body: true,
                max_request_bytes: 1_000,
                catalog_sync_interval_days: 7,
                auth_throttle_max_failures: 10,
                ..Settings::default()
            },
        )
        .await
        .expect("应能覆盖设置");
        let map = list_settings(&pool).await.expect("应能读开关");
        assert_eq!(map[SETTING_MAX_REQUEST_BYTES], Value::from(1_000u64));
        assert_eq!(map[SETTING_FULL_BODY], Value::Bool(true));
        assert_eq!(map[SETTING_CATALOG_SYNC_INTERVAL_DAYS], Value::from(7u64));
        assert_eq!(map[SETTING_AUTH_THROTTLE_MAX_FAILURES], Value::from(10u64));
    }

    /// 事务中途失败不污染库：事务内有效写入随回滚一并撤销。
    #[tokio::test]
    async fn failed_transaction_does_not_pollute() {
        let (_dir, pool) = test_pool().await;

        // 事务内先写一条有效渠道，再执行一条必然失败的语句。
        let mut tx = pool.begin().await.expect("应能开启事务");
        insert_channel(&mut tx, &sample_channel())
            .await
            .expect("事务内写渠道应成功");
        let err = sqlx::query("INSERT INTO channels (name) VALUES (?)")
            .bind("缺列")
            .execute(&mut *tx)
            .await;
        assert!(err.is_err(), "缺列语句应失败");
        tx.rollback().await.expect("应能回滚");

        assert!(
            list_channel_records(&pool)
                .await
                .expect("应能读渠道")
                .is_empty(),
            "回滚后事务内写入不应残留"
        );
    }
}
