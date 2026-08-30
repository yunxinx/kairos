//! models.dev 价格目录：解析公开 JSON、拉取替换缓存、按设置间隔定时同步。
//!
//! 目录只服务管理面填价，失败不影响协议监听。定时循环读库内间隔，不进请求快照。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sqlx::SqlitePool;
use thiserror::Error;

use crate::store;
use crate::store::StoreError;
use crate::store::catalog::{
    CatalogModel, CatalogView, catalog_synced_at, replace_catalog_models, set_catalog_synced_at,
};
use crate::store::resources::{SETTING_CATALOG_SYNC_INTERVAL_DAYS, list_settings};

/// models.dev 公开目录地址。
pub const MODELS_DEV_CATALOG_URL: &str = "https://models.dev/api.json";

/// 定时循环的检查间隔：目录同步以「天」计，一小时醒一次足够。
const SYNC_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// 拉取公开目录的超时：JSON 约 1MB，一分钟足够。
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

const MICROS_PER_USD: f64 = 1_000_000.0;

/// 目录拉取或解析失败。
#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("无法拉取价格目录: {0}")]
    Fetch(reqwest::Error),
    #[error("价格目录 HTTP {0}")]
    Http(u16),
    #[error("价格目录不是合法 JSON")]
    InvalidJson,
    #[error("{0}")]
    Store(#[from] StoreError),
}

/// 把 models.dev `api.json` 展开为扁平目录行；没有 `cost` 的模型跳过。
fn parse_models_dev(json: &str) -> Result<Vec<CatalogModel>, CatalogError> {
    let root: Value = serde_json::from_str(json).map_err(|_| CatalogError::InvalidJson)?;
    let providers = root.as_object().ok_or(CatalogError::InvalidJson)?;
    let mut models = Vec::new();
    for (provider_key, provider) in providers {
        let provider_id = provider
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(provider_key);
        let provider_name = provider
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(provider_id);
        let Some(listed) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (model_key, model) in listed {
            let Some(cost) = model.get("cost") else {
                continue;
            };
            if !cost.is_object() {
                continue;
            }
            let model_id = model.get("id").and_then(Value::as_str).unwrap_or(model_key);
            models.push(CatalogModel {
                provider_id: provider_id.to_string(),
                provider_name: provider_name.to_string(),
                model_id: model_id.to_string(),
                input_micros: catalog_dollars_to_micros(cost.get("input").and_then(Value::as_f64)),
                output_micros: catalog_dollars_to_micros(
                    cost.get("output").and_then(Value::as_f64),
                ),
                cache_read_micros: catalog_dollars_to_micros(
                    cost.get("cache_read").and_then(Value::as_f64),
                ),
                cache_write_micros: catalog_dollars_to_micros(
                    cost.get("cache_write").and_then(Value::as_f64),
                ),
            });
        }
    }
    models.sort_by(|left, right| {
        left.provider_id
            .cmp(&right.provider_id)
            .then(left.model_id.cmp(&right.model_id))
    });
    Ok(models)
}

/// 目录美元/1M tokens → micro-USD；非法或负值视为缺档。
fn catalog_dollars_to_micros(value: Option<f64>) -> Option<i64> {
    let value = value?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let micros = (value * MICROS_PER_USD).round();
    if micros >= i64::MAX as f64 {
        return None;
    }
    Some(micros as i64)
}

/// 当前 unix 毫秒。
fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// 从 `url` 拉取目录并整表替换缓存。
pub async fn fetch_and_replace(
    pool: &SqlitePool,
    client: &reqwest::Client,
    url: &str,
) -> Result<CatalogView, CatalogError> {
    let response = client
        .get(url)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(CatalogError::Fetch)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(CatalogError::Http(status));
    }
    let body = response.text().await.map_err(CatalogError::Fetch)?;
    let models = parse_models_dev(&body)?;
    let synced_at = unix_millis();
    let mut tx = pool.begin().await.map_err(StoreError::Query)?;
    replace_catalog_models(&mut tx, &models).await?;
    set_catalog_synced_at(&mut tx, synced_at).await?;
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(CatalogView {
        synced_at: Some(synced_at),
        models,
    })
}

/// 若设置了间隔且已到期（含从未同步），拉取 models.dev。
async fn sync_if_due(
    pool: &SqlitePool,
    client: &reqwest::Client,
) -> Result<Option<CatalogView>, CatalogError> {
    let settings = list_settings(pool).await?;
    let interval_days = settings
        .get(SETTING_CATALOG_SYNC_INTERVAL_DAYS)
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if interval_days == 0 {
        return Ok(None);
    }
    let synced_at = catalog_synced_at(pool).await?;
    let due = match synced_at {
        None => true,
        Some(at) => {
            let elapsed = unix_millis().saturating_sub(at);
            elapsed >= (interval_days as i64).saturating_mul(86_400_000)
        }
    };
    if !due {
        return Ok(None);
    }
    Ok(Some(
        fetch_and_replace(pool, client, MODELS_DEV_CATALOG_URL).await?,
    ))
}

/// 管理面进程内循环：按小时检查是否该同步公开目录。
pub async fn run_sync_loop(pool: SqlitePool, client: reqwest::Client) {
    loop {
        if let Err(err) = sync_if_due(&pool, &client).await {
            store::record_system_error(
                &pool,
                "catalog",
                &store::SystemLogEvent::new(
                    "catalog.sync_failed",
                    serde_json::json!({ "error": err.to_string() }),
                    format!("价格目录定时同步失败: {err}"),
                ),
            )
            .await;
        }
        tokio::time::sleep(SYNC_CHECK_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_models_dev_expands_cost_and_skips_missing() {
        let json = r#"{
            "openai": {
                "id": "openai",
                "name": "OpenAI",
                "models": {
                    "gpt-4o": {
                        "id": "gpt-4o",
                        "cost": { "input": 2.5, "output": 10, "cache_read": 1.25 }
                    },
                    "no-cost": { "id": "no-cost" }
                }
            },
            "cortecs": {
                "id": "cortecs",
                "name": "Cortecs",
                "models": {
                    "gpt-4o": {
                        "id": "gpt-4o",
                        "cost": { "input": 0.15, "output": 0.6 }
                    }
                }
            }
        }"#;
        let models = parse_models_dev(json).expect("应能解析");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].provider_id, "cortecs");
        assert_eq!(models[0].input_micros, Some(150_000));
        assert_eq!(models[0].output_micros, Some(600_000));
        assert_eq!(models[0].cache_read_micros, None);
        assert_eq!(models[1].provider_id, "openai");
        assert_eq!(models[1].model_id, "gpt-4o");
        assert_eq!(models[1].input_micros, Some(2_500_000));
        assert_eq!(models[1].output_micros, Some(10_000_000));
        assert_eq!(models[1].cache_read_micros, Some(1_250_000));
        assert_eq!(models[1].cache_write_micros, None);
    }

    #[test]
    fn catalog_dollars_rejects_negative_and_nan() {
        assert_eq!(catalog_dollars_to_micros(Some(-1.0)), None);
        assert_eq!(catalog_dollars_to_micros(Some(f64::NAN)), None);
        assert_eq!(catalog_dollars_to_micros(None), None);
        assert_eq!(catalog_dollars_to_micros(Some(0.0)), Some(0));
        assert_eq!(
            catalog_dollars_to_micros(Some(i64::MAX as f64 / MICROS_PER_USD)),
            None,
            "不能把超出 i64 的 2^63 饱和成 i64::MAX"
        );
    }
}
