//! models.dev 价格目录：解析公开 JSON、拉取替换缓存、按设置间隔定时同步。
//!
//! 目录只服务管理面填价，失败不影响协议监听。定时循环读库内间隔，不进请求快照。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Value, value::RawValue};
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
    let providers: std::collections::HashMap<String, Box<RawValue>> =
        serde_json::from_str(json).map_err(|_| CatalogError::InvalidJson)?;
    let mut models = Vec::new();
    for (provider_key, provider) in providers {
        let Ok(provider) = serde_json::from_str::<ProviderWire>(provider.get()) else {
            continue;
        };
        let provider_id = provider
            .id
            .as_ref()
            .and_then(Value::as_str)
            .unwrap_or(provider_key.as_str());
        let provider_name = provider
            .name
            .as_ref()
            .and_then(Value::as_str)
            .unwrap_or(provider_id);
        let Some(listed) = provider.models else {
            continue;
        };
        for (model_key, model) in listed {
            let Ok(model) = serde_json::from_str::<ModelWire>(model.get()) else {
                continue;
            };
            let Some(cost) = model.cost else {
                continue;
            };
            let Ok(cost) = serde_json::from_str::<CostWire>(cost.get()) else {
                continue;
            };
            let model_id = model
                .id
                .as_ref()
                .and_then(Value::as_str)
                .unwrap_or(model_key.as_str());
            models.push(CatalogModel {
                provider_id: provider_id.to_string(),
                provider_name: provider_name.to_string(),
                model_id: model_id.to_string(),
                input_micros: catalog_dollars_to_micros(cost.input.as_deref()),
                output_micros: catalog_dollars_to_micros(cost.output.as_deref()),
                cache_read_micros: catalog_dollars_to_micros(cost.cache_read.as_deref()),
                cache_write_micros: catalog_dollars_to_micros(cost.cache_write.as_deref()),
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

#[derive(Debug, Deserialize)]
struct ProviderWire {
    id: Option<Value>,
    name: Option<Value>,
    models: Option<std::collections::HashMap<String, Box<RawValue>>>,
}

#[derive(Debug, Deserialize)]
struct ModelWire {
    id: Option<Value>,
    cost: Option<Box<RawValue>>,
}

#[derive(Debug, Deserialize)]
struct CostWire {
    input: Option<Box<RawValue>>,
    output: Option<Box<RawValue>>,
    cache_read: Option<Box<RawValue>>,
    cache_write: Option<Box<RawValue>>,
}

/// 目录美元/1M tokens → micro-USD；按十进制原文四舍五入，负值与超界值视为缺档。
fn catalog_dollars_to_micros(value: Option<&RawValue>) -> Option<i64> {
    decimal_dollars_to_micros(value?.get())
}

fn decimal_dollars_to_micros(raw: &str) -> Option<i64> {
    if raw.starts_with('-') {
        return None;
    }
    let (mantissa, exponent) = match raw.find('e').or_else(|| raw.find('E')) {
        Some(index) => {
            let exponent = raw.get(index + 1..)?.parse::<i64>().ok()?;
            (&raw[..index], exponent)
        }
        None => (raw, 0),
    };
    let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut coefficient = 0u128;
    for byte in integer.bytes().chain(fraction.bytes()) {
        if !byte.is_ascii_digit() {
            return None;
        }
        let digit = byte - b'0';
        coefficient = coefficient
            .checked_mul(10)?
            .checked_add(u128::from(digit))?;
    }
    if coefficient == 0 {
        return Some(0);
    }
    let fraction_digits = i64::try_from(fraction.len()).ok()?;
    let scale = fraction_digits.checked_sub(exponent)?.checked_sub(6)?;
    let micros = if scale > 0 {
        let divisor = match power_of_ten(u32::try_from(scale).ok()?) {
            Some(divisor) => divisor,
            None => return Some(0),
        };
        let whole = coefficient / divisor;
        let remainder = coefficient % divisor;
        if remainder >= divisor / 2 {
            whole.checked_add(1)?
        } else {
            whole
        }
    } else {
        let multiplier = power_of_ten(u32::try_from(scale.checked_neg()?).ok()?)?;
        coefficient.checked_mul(multiplier)?
    };
    i64::try_from(micros).ok()
}

fn power_of_ten(exponent: u32) -> Option<u128> {
    if exponent > 38 {
        return None;
    }
    let mut value = 1u128;
    for _ in 0..exponent {
        value = value.checked_mul(10)?;
    }
    Some(value)
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
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(StoreError::Query)?;
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
    fn catalog_dollars_rejects_negative_and_out_of_range_values() {
        let negative: Box<RawValue> = serde_json::from_str("-1").expect("应能解析数字");
        let max: Box<RawValue> =
            serde_json::from_str("9223372036854.775807").expect("应能解析上界数字");
        let overflow: Box<RawValue> =
            serde_json::from_str("9223372036854.775808").expect("应能解析越界数字");
        assert_eq!(catalog_dollars_to_micros(Some(&negative)), None);
        assert_eq!(catalog_dollars_to_micros(None), None);
        assert_eq!(catalog_dollars_to_micros(Some(&max)), Some(i64::MAX));
        assert_eq!(catalog_dollars_to_micros(Some(&overflow)), None);
    }

    #[test]
    fn catalog_dollars_rounds_decimal_source_exactly() {
        for (raw, expected) in [
            ("0", 0),
            ("0.0000004", 0),
            ("0.0000005", 1),
            ("1.2345674", 1_234_567),
            ("1.2345675", 1_234_568),
            ("2.5e-6", 3),
        ] {
            let value: Box<RawValue> = serde_json::from_str(raw).expect("应能解析目录数字");
            assert_eq!(catalog_dollars_to_micros(Some(&value)), Some(expected));
        }
    }
}
