//! 计费：四档价格快照与费用计算，全程整数 micro-USD。
//!
//! 价格表经管理 API 维护，库内以「每 1M tokens 的 micro-USD」整数存储；费用
//! 计算只做整数乘除。缓存档缺省时该档为 0，不回退 `input`；reasoning tokens
//! 不单独计价（计入 output，已在 usage 折算）。cache 写入可按 1h TTL 细分：
//! 价格行配置了 1h 费率即分档计价，未配置整行按 `cache_write` 单一费率。
//! 不为媒体内容引入新计价维度。

use crate::core::ir::Usage;
use crate::store::resources::Price;
use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;

/// 费用计算失败；任何一种错误都必须阻止结算，不能截断或饱和后继续扣款。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ThisError)]
pub enum Error {
    #[error("单价不能为负数")]
    NegativePrice,
    #[error("折扣前费用不能为负数")]
    NegativeBaseCost,
    #[error("折扣率超出合法范围")]
    InvalidDiscount,
    #[error("费用超出 micro-USD 整数范围")]
    AmountOverflow,
}

/// 一次完整费用计算的原价和实收。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Charge {
    pub base_cost_usd_micros: i64,
    pub cost_usd_micros: i64,
}

/// 单模型价格档快照（micro-USD / 1M tokens），计费时点固化，供日志与对账。
///
/// `cache_write_1h_micros` 为 0 表示价格行未配置 1h TTL 档，1h 写入明细随
/// 其余写入按 `cache_write_micros` 计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PriceSnapshot {
    pub input_micros: i64,
    pub output_micros: i64,
    pub cache_read_micros: i64,
    pub cache_write_micros: i64,
    pub cache_write_1h_micros: i64,
}

impl PriceSnapshot {
    /// 从库加载的价格行构造快照；缓存档 `None`（未配置）时该档为 0。
    pub fn from_store_price(price: &Price) -> Self {
        Self {
            input_micros: price.input_micros,
            output_micros: price.output_micros,
            cache_read_micros: price.cache_read_micros.unwrap_or(0),
            cache_write_micros: price.cache_write_micros.unwrap_or(0),
            cache_write_1h_micros: price.cache_write_1h_micros.unwrap_or(0),
        }
    }
}

/// 折扣率下限：0 表示免费档。
pub const MIN_DISCOUNT_BP: i64 = 0;
/// 折扣率上限：1_000_000 万分比 = 100 倍，防止误配成天文数字。
pub const MAX_DISCOUNT_BP: i64 = 1_000_000;
/// 原价折扣率：10000 万分比 = 100%。
pub const DEFAULT_DISCOUNT_BP: i64 = 10_000;

/// 计算 `usage` 对应的整数 micro-USD 费用：四分量 × 各自单价，整数微元截断。
///
/// 价格行配置了 1h 档时 cache 写入分档：1h 明细 × 1h 费率 + 其余写入 ×
/// `cache_write` 费率，各段独立截断，1h 明细钳制在写入总数之内。未配置 1h 档
/// （费率 0 快照）时整行按 `cache_write` 单一费率一次截断——必须单段计算，
/// 两段分别截断再求和在非整除边界会少于单一费率的结果。
///
/// 这是渠道原价，不套用套餐折扣；折扣在总额上再做一次整数乘除。
pub fn cost_micros(usage: &Usage, price: &PriceSnapshot) -> Result<i64, Error> {
    let write_cost = if price.cache_write_1h_micros > 0 {
        let write_1h = usage.cache_write_1h_tokens.min(usage.cache_write_tokens);
        let write_rest = usage.cache_write_tokens - write_1h;
        component_cost(write_1h, price.cache_write_1h_micros)?
            .checked_add(component_cost(write_rest, price.cache_write_micros)?)
            .ok_or(Error::AmountOverflow)?
    } else {
        component_cost(usage.cache_write_tokens, price.cache_write_micros)?
    };
    let components = [
        component_cost(usage.input_tokens, price.input_micros)?,
        component_cost(usage.output_tokens, price.output_micros)?,
        component_cost(usage.cache_read_tokens, price.cache_read_micros)?,
        write_cost,
    ];
    components.into_iter().try_fold(0i64, |total, component| {
        total.checked_add(component).ok_or(Error::AmountOverflow)
    })
}

/// 按万分比折扣把渠道原价换算为实收，只截断一次。
///
/// `discount_bp` 必须已在 [`MIN_DISCOUNT_BP`] 与 [`MAX_DISCOUNT_BP`] 之间；
/// 库加载与写入侧负责校验，调用方直接使用。
pub fn discounted_cost_micros(base_cost_usd_micros: i64, discount_bp: i64) -> Result<i64, Error> {
    if base_cost_usd_micros < 0 {
        return Err(Error::NegativeBaseCost);
    }
    if !(MIN_DISCOUNT_BP..=MAX_DISCOUNT_BP).contains(&discount_bp) {
        return Err(Error::InvalidDiscount);
    }
    let discounted =
        base_cost_usd_micros as i128 * discount_bp as i128 / DEFAULT_DISCOUNT_BP as i128;
    i64::try_from(discounted).map_err(|_| Error::AmountOverflow)
}

/// 计算原价与折后实收；任一步失败都不产生部分结果。
pub fn charge_micros(
    usage: &Usage,
    price: &PriceSnapshot,
    discount_bp: i64,
) -> Result<Charge, Error> {
    let base_cost_usd_micros = cost_micros(usage, price)?;
    let cost_usd_micros = discounted_cost_micros(base_cost_usd_micros, discount_bp)?;
    Ok(Charge {
        base_cost_usd_micros,
        cost_usd_micros,
    })
}

/// 用 `max_tokens` 作为 output 用量的粗估费用，挡住极端输出上限。
pub fn estimate_max_output_cost_micros(
    max_tokens: u32,
    output_micros_per_1m: i64,
) -> Result<i64, Error> {
    cost_micros(
        &Usage {
            output_tokens: u64::from(max_tokens),
            ..Usage::default()
        },
        &PriceSnapshot {
            output_micros: output_micros_per_1m,
            ..PriceSnapshot::default()
        },
    )
}

/// 单分量费用：`tokens × 单价 / 1M`，用 i128 防大 token 数溢出。
fn component_cost(tokens: u64, micros_per_1m: i64) -> Result<i64, Error> {
    if micros_per_1m < 0 {
        return Err(Error::NegativePrice);
    }
    let cost = tokens as i128 * micros_per_1m as i128 / 1_000_000;
    i64::try_from(cost).map_err(|_| Error::AmountOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            cache_write_1h_tokens: 0,
            raw: None,
        }
    }

    /// 构一个库加载来的价格行（micro-USD / 1M tokens）。
    fn price(input: i64, output: i64, cache_read: Option<i64>, cache_write: Option<i64>) -> Price {
        Price {
            channel_id: 1,
            model: "gpt-4o".to_string(),
            input_micros: input,
            output_micros: output,
            cache_read_micros: cache_read,
            cache_write_micros: cache_write,
            cache_write_1h_micros: None,
        }
    }

    /// 四档独立计价：2.5/10.0/1.25/10.0 USD 每 1M，各 1M tokens → 各档微元。
    #[test]
    fn cost_charges_each_dimension_by_its_price() {
        let price = PriceSnapshot::from_store_price(&price(
            2_500_000,
            10_000_000,
            Some(1_250_000),
            Some(10_000_000),
        ));
        let u = usage(1_000_000, 1_000_000, 1_000_000, 1_000_000);
        assert_eq!(
            cost_micros(&u, &price).expect("合法费用应可表示"),
            2_500_000 + 10_000_000 + 1_250_000 + 10_000_000
        );
    }

    /// 缓存档缺省时该档为 0，有 cache token 也不按 input 计价。
    #[test]
    fn unconfigured_cache_tier_is_zero() {
        let price = PriceSnapshot::from_store_price(&price(2_500_000, 10_000_000, None, None));
        assert_eq!(price.cache_read_micros, 0);
        assert_eq!(price.cache_write_micros, 0);
        // 1M cache_read tokens × 未配置档 0 → 0 微元，不按 input 2.5 计价。
        let u = usage(0, 0, 1_000_000, 0);
        assert_eq!(cost_micros(&u, &price), Ok(0));
        // cache_write 同样：有 token 也不按 input 计价。
        let u = usage(0, 0, 0, 1_000_000);
        assert_eq!(cost_micros(&u, &price), Ok(0));
    }

    /// 混合用量按各自档位累加，结果精确为整数微元。
    #[test]
    fn mixed_usage_sums_exact_micros() {
        let price =
            PriceSnapshot::from_store_price(&price(2_500_000, 10_000_000, Some(1_250_000), None));
        // input 100 + output 40 + cache_read 200 tokens。
        let u = usage(100, 40, 200, 0);
        // 100*2.5/1M + 40*10/1M + 200*1.25/1M = 0.00025 + 0.0004 + 0.00025 USD
        // = 250 + 400 + 250 = 900 微元。
        assert_eq!(cost_micros(&u, &price), Ok(900));
    }

    /// 零用量 → 零费用。
    #[test]
    fn zero_usage_is_free() {
        let price = PriceSnapshot::from_store_price(&price(2_500_000, 10_000_000, None, None));
        let u = usage(0, 0, 0, 0);
        assert_eq!(cost_micros(&u, &price), Ok(0));
    }

    /// 配置 1h 档后写入分档计价：1h 明细 × 1h 费率 + 其余写入 × 基础费率。
    #[test]
    fn configured_1h_tier_splits_write_cost() {
        let price = PriceSnapshot::from_store_price(&Price {
            cache_write_1h_micros: Some(20_000_000),
            ..price(0, 0, None, Some(10_000_000))
        });
        let mut u = usage(0, 0, 0, 1_000_000);
        u.cache_write_1h_tokens = 400_000;
        // 0.4 × 20 + 0.6 × 10 = 14 USD = 14_000_000 微元。
        assert_eq!(cost_micros(&u, &price), Ok(14_000_000));
    }

    /// 1h 明细超过写入总数时钳制在总数内，不出现负的剩余写入。
    #[test]
    fn one_hour_detail_clamped_to_write_total() {
        let price = PriceSnapshot::from_store_price(&Price {
            cache_write_1h_micros: Some(20_000_000),
            ..price(0, 0, None, Some(10_000_000))
        });
        let mut u = usage(0, 0, 0, 300_000);
        u.cache_write_1h_tokens = 500_000;
        // 钳制后 1h = 300_000、剩余 = 0：300_000 × 20 / 1M = 6_000 微元。
        assert_eq!(cost_micros(&u, &price), Ok(6_000_000));
    }

    /// 未配置 1h 档时整行按单一费率：整除用量下与不分档一致。
    #[test]
    fn unconfigured_1h_tier_bills_single_rate() {
        let price =
            PriceSnapshot::from_store_price(&price(2_500_000, 10_000_000, None, Some(10_000_000)));
        let mut u = usage(0, 0, 0, 700_000);
        u.cache_write_1h_tokens = 200_000;
        // 700_000 × 10 / 1M = 7_000_000 微元，1h 明细不拆价。
        assert_eq!(cost_micros(&u, &price), Ok(7_000_000));
    }

    /// 未配置 1h 档且用量非整除：必须按写入总数一次截断。两段分别截断再求和
    /// 会少于单一费率结果（3+2 < 6），该不变量禁止为分段公式取代。
    #[test]
    fn unconfigured_1h_tier_truncates_once_on_total() {
        let price = PriceSnapshot::from_store_price(&price(0, 0, None, Some(3)));
        let mut u = usage(0, 0, 0, 2_000_000);
        u.cache_write_1h_tokens = 1_000_001;
        // 2_000_000 × 3 / 1M = 6 微元；分段截断只能得到 3 + 2 = 5。
        assert_eq!(cost_micros(&u, &price), Ok(6));
    }

    /// 小数价格（如 0.15 USD/1M）大量 token 仍精确。
    #[test]
    fn fractional_price_exact_for_many_tokens() {
        let price = PriceSnapshot::from_store_price(&price(150_000, 600_000, None, None));
        // 1M input → 0.15 USD = 150_000 微元。
        let u = usage(1_000_000, 0, 0, 0);
        assert_eq!(cost_micros(&u, &price), Ok(150_000));
    }

    #[test]
    fn discounted_cost_uses_single_integer_division() {
        assert_eq!(discounted_cost_micros(4_250, 8_000), Ok(3_400));
        assert_eq!(discounted_cost_micros(4_250, 10_000), Ok(4_250));
        assert_eq!(discounted_cost_micros(4_250, 0), Ok(0));
        assert_eq!(discounted_cost_micros(1, 3_333), Ok(0));
        assert_eq!(discounted_cost_micros(999_999, 1_000_000), Ok(99_999_900));
    }

    #[test]
    fn estimate_max_output_uses_output_tier_only() {
        assert_eq!(
            estimate_max_output_cost_micros(600_000, 10_000_000),
            Ok(6_000_000)
        );
        assert_eq!(estimate_max_output_cost_micros(0, 10_000_000), Ok(0));
    }

    #[test]
    fn overflowing_discount_is_rejected_instead_of_wrapping_negative() {
        assert_eq!(
            discounted_cost_micros(i64::MAX, MAX_DISCOUNT_BP),
            Err(Error::AmountOverflow)
        );
    }

    #[test]
    fn component_and_total_overflow_are_rejected() {
        assert_eq!(
            cost_micros(
                &usage(u64::MAX, 0, 0, 0),
                &PriceSnapshot {
                    input_micros: i64::MAX,
                    ..PriceSnapshot::default()
                }
            ),
            Err(Error::AmountOverflow)
        );
        assert_eq!(
            cost_micros(
                &usage(1_000_000, 1_000_000, 0, 0),
                &PriceSnapshot {
                    input_micros: i64::MAX,
                    output_micros: 1,
                    ..PriceSnapshot::default()
                }
            ),
            Err(Error::AmountOverflow)
        );
    }

    proptest! {
        /// 受检折扣要么等于 i128 参考值，要么只因超出 i64 上界而失败。
        #[test]
        fn discounted_cost_matches_wide_integer_reference(
            base in 0i64..=i64::MAX,
            discount in MIN_DISCOUNT_BP..=MAX_DISCOUNT_BP,
        ) {
            let expected = base as i128 * discount as i128 / DEFAULT_DISCOUNT_BP as i128;
            match discounted_cost_micros(base, discount) {
                Ok(actual) => prop_assert_eq!(i128::from(actual), expected),
                Err(Error::AmountOverflow) => prop_assert!(expected > i128::from(i64::MAX)),
                Err(other) => prop_assert!(false, "合法输入不应产生 {other}"),
            }
        }
    }

    /// 快照构造不依赖 JSON 序列化，且缓存档缺省为 0 确定。
    #[test]
    fn snapshot_is_deterministic() {
        let a = PriceSnapshot::from_store_price(&price(
            2_500_000,
            10_000_000,
            Some(1_250_000),
            Some(10_000_000),
        ));
        let b = PriceSnapshot::from_store_price(&price(
            2_500_000,
            10_000_000,
            Some(1_250_000),
            Some(10_000_000),
        ));
        assert_eq!(a, b);
        let _ = json!({});
    }
}
