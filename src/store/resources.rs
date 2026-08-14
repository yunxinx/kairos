//! 运行时资源存储：渠道、令牌、价格与运行时开关四类资源的读写原语。
//!
//! 资源 CRUD 写操作接受 `&mut SqliteConnection`，可组合进事务；读操作接受
//! `&SqlitePool`。金额一律整数 micro-USD（ADR-0002）。wire 协议类型复用
//! `crate::config::Protocol`，落库为其 serde rename 字符串。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, SqliteConnection, SqlitePool};

use crate::config::Protocol;
use crate::store::StoreError;

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

/// 单模型四档单价（micro-USD / 1M tokens）；缓存档 `None` 表示回退 input 价。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
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

/// 运行时设置的聚合契约：`full_body` 与入站请求体上限。
///
/// 落库时拆成两条键值记录（`settings` 表），管理 API 以其 JSON 形态作为
/// wire 契约（成对读写），故派生 `Serialize`/`Deserialize` 并拒绝未知字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// 是否落完整请求/响应 body。
    pub full_body: bool,
    /// 入站请求体大小上限（字节）。
    pub max_request_bytes: u64,
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

/// 读出全部渠道。
pub async fn list_channels(pool: &SqlitePool) -> Result<Vec<Channel>, StoreError> {
    let rows = sqlx::query(
        "SELECT name, protocol, base_url, api_key, models_json, model_aliases_json, \
         priority, weight, timeout_ms, max_retries FROM channels",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;

    let mut channels = Vec::with_capacity(rows.len());
    for row in rows {
        channels.push(map_channel_row(&row)?);
    }
    Ok(channels)
}

/// 把渠道行映射为 `Channel`。
fn map_channel_row(row: &sqlx::sqlite::SqliteRow) -> Result<Channel, StoreError> {
    let name: String = row.try_get("name").map_err(StoreError::Query)?;
    let protocol_wire: String = row.try_get("protocol").map_err(StoreError::Query)?;
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

    Ok(Channel {
        base_url: row.try_get("base_url").map_err(StoreError::Query)?,
        api_key: row.try_get("api_key").map_err(StoreError::Query)?,
        priority: row.try_get("priority").map_err(StoreError::Query)?,
        weight: row.try_get("weight").map_err(StoreError::Query)?,
        timeout_ms: row.try_get("timeout_ms").map_err(StoreError::Query)?,
        max_retries: row.try_get("max_retries").map_err(StoreError::Query)?,
        name,
        protocol: protocol_from_wire(&protocol_wire)?,
        models,
        model_aliases,
    })
}

/// 新增或整体替换一个渠道（按 `name`），同一事务内幂等。
pub async fn upsert_channel(
    conn: &mut SqliteConnection,
    channel: &Channel,
) -> Result<(), StoreError> {
    let models_json = serde_json::to_string(&channel.models).map_err(serde_error)?;
    let aliases_json = serde_json::to_string(&channel.model_aliases).map_err(serde_error)?;

    sqlx::query(
        "INSERT INTO channels \
         (name, protocol, base_url, api_key, models_json, model_aliases_json, \
          priority, weight, timeout_ms, max_retries) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(name) DO UPDATE SET \
           protocol = excluded.protocol, base_url = excluded.base_url, api_key = excluded.api_key, \
           models_json = excluded.models_json, model_aliases_json = excluded.model_aliases_json, \
           priority = excluded.priority, weight = excluded.weight, \
           timeout_ms = excluded.timeout_ms, max_retries = excluded.max_retries",
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
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;

    Ok(())
}

/// 按 `name` 删除渠道；不存在视为成功（幂等）。
pub async fn delete_channel(conn: &mut SqliteConnection, name: &str) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM channels WHERE name = ?")
        .bind(name)
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
        "SELECT token_key, name, limit_usd_micros, enabled, created_at, last_used_at \
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
        "SELECT token_key, name, limit_usd_micros, enabled, created_at, last_used_at \
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
        "INSERT INTO tokens (token_key, name, limit_usd_micros, enabled, created_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(token_key) DO UPDATE SET \
           name = excluded.name, limit_usd_micros = excluded.limit_usd_micros, \
           enabled = excluded.enabled",
    )
    .bind(&token.token_key)
    .bind(&token.name)
    .bind(token.limit_usd_micros)
    .bind(token.enabled)
    .bind(created_at)
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

/// 读出全部价格（每模型一行）。
pub async fn list_prices(pool: &SqlitePool) -> Result<Vec<Price>, StoreError> {
    let rows = sqlx::query(
        "SELECT model, input_micros, output_micros, cache_read_micros, cache_write_micros \
         FROM prices",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;

    let prices = rows
        .iter()
        .map(|row| {
            Ok(Price {
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

/// 新增或整体替换一个模型的价格（按 `model`），同一事务内幂等。
pub async fn upsert_price(conn: &mut SqliteConnection, price: &Price) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO prices \
         (model, input_micros, output_micros, cache_read_micros, cache_write_micros) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(model) DO UPDATE SET \
           input_micros = excluded.input_micros, output_micros = excluded.output_micros, \
           cache_read_micros = excluded.cache_read_micros, \
           cache_write_micros = excluded.cache_write_micros",
    )
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

/// 按 `model` 删除价格；不存在视为成功（幂等）。
pub async fn delete_price(conn: &mut SqliteConnection, model: &str) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM prices WHERE model = ?")
        .bind(model)
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

/// 成对写入运行时设置：`full_body` 与 `max_request_bytes` 一同落库（幂等）。
///
/// 供管理 API 的 `/settings` 写操作使用：两条开关在同一个事务内写入，保证聚合
/// 契约的原子性。读回经 `list_settings` 由调用方聚合。
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
    Ok(())
}

/// 把 serde 序列化错误包装为存储层错误。
fn serde_error(err: serde_json::Error) -> StoreError {
    StoreError::InvalidResource(format!("JSON 序列化失败: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
        }
    }

    /// 渠道播种 → 读回往返一致（含集合字段与协议）。
    #[tokio::test]
    async fn channel_upsert_then_list_roundtrip() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        upsert_channel(&mut conn, &sample_channel())
            .await
            .expect("应能写渠道");

        let channels = list_channels(&pool).await.expect("应能读渠道");
        assert_eq!(channels, vec![sample_channel()]);
    }

    /// 同 name 再次 upsert 为整体替换，不产生重复行。
    #[tokio::test]
    async fn channel_upsert_overwrites_same_name() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        upsert_channel(&mut conn, &sample_channel())
            .await
            .expect("应能写渠道");
        let mut updated = sample_channel();
        updated.timeout_ms = 3_000;
        upsert_channel(&mut conn, &updated)
            .await
            .expect("应能覆盖渠道");

        let channels = list_channels(&pool).await.expect("应能读渠道");
        assert_eq!(channels.len(), 1, "覆盖后仍为单行");
        assert_eq!(channels[0].timeout_ms, 3_000);
    }

    /// 删除渠道后读回为空；删除不存在的 name 幂等成功。
    #[tokio::test]
    async fn channel_delete_is_idempotent() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        upsert_channel(&mut conn, &sample_channel())
            .await
            .expect("应能写渠道");
        delete_channel(&mut conn, "c1").await.expect("应能删渠道");
        delete_channel(&mut conn, "c1")
            .await
            .expect("重复删除应幂等");

        assert!(list_channels(&pool).await.expect("应能读").is_empty());
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
    }

    /// 价格播种 → 读回往返一致；缓存档 NULL 保留。
    #[tokio::test]
    async fn price_upsert_then_list_roundtrip() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        let price = Price {
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
            },
        )
        .await
        .expect("应能覆盖设置");
        let map = list_settings(&pool).await.expect("应能读开关");
        assert_eq!(map[SETTING_MAX_REQUEST_BYTES], Value::from(1_000u64));
        assert_eq!(map[SETTING_FULL_BODY], Value::Bool(true));
    }

    /// 事务中途失败不污染库：事务内有效写入随回滚一并撤销。
    #[tokio::test]
    async fn failed_transaction_does_not_pollute() {
        let (_dir, pool) = test_pool().await;

        // 事务内先写一条有效渠道，再执行一条必然失败的语句。
        let mut tx = pool.begin().await.expect("应能开启事务");
        upsert_channel(&mut tx, &sample_channel())
            .await
            .expect("事务内写渠道应成功");
        let err = sqlx::query("INSERT INTO channels (name) VALUES (?)")
            .bind("缺列")
            .execute(&mut *tx)
            .await;
        assert!(err.is_err(), "缺列语句应失败");
        tx.rollback().await.expect("应能回滚");

        assert!(
            list_channels(&pool).await.expect("应能读渠道").is_empty(),
            "回滚后事务内写入不应残留"
        );
    }
}
