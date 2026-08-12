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
    let rows = sqlx::query("SELECT token_key, name, limit_usd_micros FROM tokens")
        .fetch_all(pool)
        .await
        .map_err(StoreError::Query)?;

    let tokens = rows
        .iter()
        .map(|row| {
            Ok(Token {
                token_key: row.try_get("token_key").map_err(StoreError::Query)?,
                name: row.try_get("name").map_err(StoreError::Query)?,
                limit_usd_micros: row.try_get("limit_usd_micros").map_err(StoreError::Query)?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    Ok(tokens)
}

/// 新增或整体替换一个令牌（按 `token_key`），同一事务内幂等。
pub async fn upsert_token(conn: &mut SqliteConnection, token: &Token) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO tokens (token_key, name, limit_usd_micros) VALUES (?, ?, ?) \
         ON CONFLICT(token_key) DO UPDATE SET \
           name = excluded.name, limit_usd_micros = excluded.limit_usd_micros",
    )
    .bind(&token.token_key)
    .bind(&token.name)
    .bind(token.limit_usd_micros)
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

    /// 令牌播种 → 读回往返一致；limit 为 NULL 表示无上限。
    #[tokio::test]
    async fn token_upsert_then_list_roundtrip() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        let token = Token {
            token_key: "sk-a".to_string(),
            name: "dev".to_string(),
            limit_usd_micros: Some(5_000_000),
        };
        upsert_token(&mut conn, &token).await.expect("应能写令牌");

        let tokens = list_tokens(&pool).await.expect("应能读令牌");
        assert_eq!(tokens, vec![token]);
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
            },
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
            },
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
