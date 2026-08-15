//! 运行时资源内存快照：网关请求路径读取的唯一资源视图。
//!
//! 启动时从 SQLite 加载渠道、令牌、价格与运行时开关进 `RuntimeSnapshot`（不可变
//! 整体）。请求路径（认证、路由、计费准入、full_body、body 上限）全部读快照；
//! 在途请求在准入时刻抓取一个 `Arc` 引用，不受后续原子替换影响。管理 API 写库
//! 成功后原子替换快照（见 `SnapshotHandle`），是唯一动态入口。

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::store::StoreError;
use crate::store::resources::{self, SETTING_FULL_BODY, SETTING_MAX_REQUEST_BYTES};

/// 入站请求体大小上限的默认值（字节）：覆盖常规 base64 图片，与参考网关 bifrost
/// 的 `max_request_body_size_mb: 100` 对齐；运营可经管理 API 在线调整。
pub const DEFAULT_MAX_REQUEST_BYTES: u64 = 100 * 1024 * 1024;

/// 网关运行时资源的内存快照：不可变整体，原子替换。
///
/// 请求路径在准入时刻抓取一个 `Arc<RuntimeSnapshot>` 引用，整个请求生命周期内
/// 只读该引用——即使管理 API 之后替换了快照，在途请求仍按准入时刻的资源走完。
#[derive(Debug, Clone)]
pub struct RuntimeSnapshot {
    /// 渠道（路由候选，含库生成 id），按 store 返回顺序。
    pub channels: Vec<resources::ChannelRecord>,
    /// 令牌定义，按 `token_key` 索引（认证查找）。
    pub tokens: HashMap<String, resources::Token>,
    /// 价格表，按模型名索引（计费准入）。
    pub prices: HashMap<String, resources::Price>,
    /// 是否落完整请求/响应 body（来自 `full_body` 开关）。
    pub full_body: bool,
    /// 入站请求体大小上限（字节，来自 `max_request_bytes` 开关）。
    pub max_request_bytes: u64,
}

/// 快照的原子替换句柄：管理 API 写库成功后写入新快照，请求路径读当前快照。
///
/// 外层 `RwLock` 承载「替换」语义（管理 API 持写锁换掉整个 `Arc`），请求路径只
/// 在读锁内短暂克隆 `Arc` 即释放锁，之后持有该引用独立运行。
pub type SnapshotHandle = Arc<RwLock<Arc<RuntimeSnapshot>>>;

/// 从库加载全部运行时资源进内存快照。
///
/// 四类资源分别读取：渠道/令牌/价格直接装载，运行时开关经 `load_settings` 解析
/// 出 `full_body` 与 `max_request_bytes`（缺省用默认值）。
pub async fn load_snapshot(pool: &SqlitePool) -> Result<RuntimeSnapshot, StoreError> {
    let channels = resources::list_channel_records(pool).await?;
    let token_rows = resources::list_tokens(pool).await?;
    let price_rows = resources::list_prices(pool).await?;
    let settings = resources::list_settings(pool).await?;

    let tokens = token_rows
        .into_iter()
        .map(|token| (token.token_key.clone(), token))
        .collect();
    let prices = price_rows
        .into_iter()
        .map(|price| (price.model.clone(), price))
        .collect();

    Ok(RuntimeSnapshot {
        channels,
        tokens,
        prices,
        full_body: load_full_body(&settings),
        max_request_bytes: load_max_request_bytes(&settings),
    })
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
    settings
        .get(SETTING_MAX_REQUEST_BYTES)
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_REQUEST_BYTES)
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
        assert!(!snap.full_body, "full_body 缺省关闭");
        assert_eq!(
            snap.max_request_bytes, DEFAULT_MAX_REQUEST_BYTES,
            "body 上限缺省用默认值"
        );
    }

    /// 播种资源与开关后加载：快照反映库内状态。
    #[tokio::test]
    async fn seeded_db_loads_resources_and_settings() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");

        resources::insert_channel(
            &mut conn,
            &resources::Channel {
                name: "c1".to_string(),
                protocol: Protocol::OpenAiChat,
                base_url: "https://api.example.com/v1".to_string(),
                api_key: "k".to_string(),
                models: vec!["gpt-4o".to_string()],
                model_aliases: HashMap::new(),
                priority: 1,
                weight: 1,
                timeout_ms: 1000,
                max_retries: 0,
                enabled: true,
            },
        )
        .await
        .expect("应能写渠道");
        resources::upsert_token(
            &mut conn,
            &resources::Token {
                token_key: "sk-a".to_string(),
                name: "dev".to_string(),
                limit_usd_micros: None,
                enabled: true,
            },
            1,
        )
        .await
        .expect("应能写令牌");
        resources::upsert_price(
            &mut conn,
            &resources::Price {
                model: "gpt-4o".to_string(),
                input_micros: 2_500_000,
                output_micros: 10_000_000,
                cache_read_micros: None,
                cache_write_micros: None,
            },
        )
        .await
        .expect("应能写价格");
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
        assert!(snap.tokens.contains_key("sk-a"));
        assert!(snap.prices.contains_key("gpt-4o"));
        assert!(snap.full_body, "开关应生效");
        assert_eq!(snap.max_request_bytes, 8_000_000);
    }
}
