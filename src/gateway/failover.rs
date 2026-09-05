//! 渠道 failover 编排：统一处理重试预算、可重试错误、渠道内密钥轮换与最终
//! 错误归因。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use crate::config::Protocol;
use crate::store::channel_keys::eligible_channel_keys;
use crate::store::resources::MAX_REQUEST_TIMEOUT_MS;
use crate::store::resources::{ChannelRecord, StoredChannelKey};

use super::{protocol, routing};

/// 同渠道重试的退避参数（来自运行时设置）。
#[derive(Debug, Clone, Copy)]
pub(super) struct RetryBackoff {
    /// 无 `Retry-After` 时的基础间隔。
    pub base: Duration,
    /// 指数退避封顶。
    pub cap: Duration,
    /// 上游 `Retry-After` 的最大等待。
    pub after_cap: Duration,
}

impl RetryBackoff {
    /// 由设置毫秒/秒构造；各字段至少为 1，避免零间隔忙等。
    pub(super) fn from_ms(base_ms: u64, cap_ms: u64, after_cap_secs: u64) -> Self {
        Self {
            base: Duration::from_millis(base_ms.max(1)),
            cap: Duration::from_millis(cap_ms.max(1)),
            after_cap: Duration::from_secs(after_cap_secs.max(1)),
        }
    }
}

/// 单次出站调用的结果。
pub(super) enum Outbound {
    /// 成功：响应已就绪，可直接交给下游。
    Success(Response),
    /// 可重试错误（网络错误/429/5xx）。
    Retryable {
        channel: String,
        status: Option<u16>,
        message: String,
        /// 上游 `Retry-After` 的 delta-seconds；无则按指数退避。
        retry_after: Option<Duration>,
    },
    /// 不可重试错误（其他 4xx）。
    Fatal {
        channel: String,
        status: u16,
        message: String,
    },
    /// 网关本地计费准入拒绝（下游钱包或令牌累计额度不足）：请求未出站，
    /// 故障在下游域而非渠道域。与上游返回的 402 严格区分——只有后者
    /// 才参与渠道冷却记账。
    BillingDenied { channel: String, message: String },
}

/// 指数退避的随机抖动幅度：最终等待为计算值的 80%–120%。
const RETRY_JITTER_MIN: f64 = 0.8;
const RETRY_JITTER_MAX: f64 = 1.2;

/// 同渠道下一次重试前等待的时长。
///
/// 有 `Retry-After` 时用其值（仍受 `after_cap` 封顶），不加抖动——那是上游指定的
/// 等待。无则走指数退避，并施加 ±20% 抖动，避免 429 恢复瞬间所有在途重试同拍。
pub(super) fn retry_delay(
    attempt_no: u32,
    retry_after: Option<Duration>,
    backoff: RetryBackoff,
) -> Duration {
    if let Some(after) = retry_after {
        return after.min(backoff.after_cap);
    }
    jitter_delay(exponential_delay(attempt_no, backoff)).min(backoff.cap)
}

/// 无抖动的指数退避：`base * 2^attempt`，封顶 `cap`。
fn exponential_delay(attempt_no: u32, backoff: RetryBackoff) -> Duration {
    let shift = attempt_no.min(16);
    backoff.base.saturating_mul(1u32 << shift).min(backoff.cap)
}

/// 把等待乘以 `[0.8, 1.2]` 均匀随机因子。
fn jitter_delay(base: Duration) -> Duration {
    let factor: f64 = rand::random_range(RETRY_JITTER_MIN..=RETRY_JITTER_MAX);
    base.mul_f64(factor)
}

/// 按渠道路由顺序发起出站调用，遇可重试错误自动 failover；渠道内密钥按
/// 请求级轮换状态在 key 粒度上恢复失败（[`KeyRotation`]）。
///
/// `attempt` 每次收到渠道记录与本次要用的密钥；`log_failure` 接收（渠道名、
/// 状态码、是否已 failover、返回下游的错误响应体 wire 字节、失败尝试的密钥
/// 名）：wire 字节先于日志构造，保证 full_body 开启时失败日志也能记录实际
/// 返回下游的入站响应。
/// 全部候选渠道耗尽后返回下游的最后一次失败归因。
struct FinalFailure {
    channel: String,
    status: Option<u16>,
    message: String,
    /// 最后一次失败尝试所用的密钥名；无密钥上下文（无候选渠道）时为空。
    key_name: String,
}

/// 只有 401 能稳定归因到单把凭证失效。402 属于账号计费域，403 还可能表示
/// 模型权限或组织策略；后二者不能污染跨请求的密钥健康状态。
fn is_credential_failure(status: u16) -> bool {
    status == 401
}

const AUTH_FAILURE_COOLDOWN: Duration = Duration::from_secs(5 * 60);
/// 冷却表是保护性缓存而非事实存储；达到上限时停止接纳新记录，已有记录仍按
/// TTL 自然清理。这样资源消耗只与配置规模有关，不受错误请求数量控制。
const MAX_KEY_COOLDOWNS: usize = 4_096;

/// 上游密钥认证失败后的跨请求冷却表。
#[derive(Clone)]
pub(super) struct KeyCooldowns {
    inner: Arc<Mutex<HashMap<i64, Instant>>>,
}

impl KeyCooldowns {
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn is_available(&self, key_id: i64, now: Instant) -> bool {
        let mut entries = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        match entries.get(&key_id).copied() {
            Some(until) if until > now => false,
            Some(_) => {
                entries.remove(&key_id);
                true
            }
            None => true,
        }
    }

    /// 清理已结束的冷却记录，避免长期不再命中的密钥让表持续增长。
    fn prune_expired(&self, now: Instant) {
        let mut entries = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|_, until| *until > now);
    }

    fn mark_auth_failure(&self, key_id: i64, now: Instant) {
        let until = now.checked_add(AUTH_FAILURE_COOLDOWN).unwrap_or(now);
        let mut entries = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|_, expires_at| *expires_at > now);
        if entries.contains_key(&key_id) || entries.len() < MAX_KEY_COOLDOWNS {
            entries.insert(key_id, until);
        }
    }

    fn clear(&self, key_id: i64) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key_id);
    }
}

/// 上游渠道故障后的冷却时长：期间路由跳过该渠道，到期自然恢复。
const CHANNEL_FAILURE_COOLDOWN: Duration = Duration::from_secs(5 * 60);
/// 连续可重试失败达到该次数即进入冷却；成功出站清零。
const CHANNEL_RETRY_FAILURE_THRESHOLD: u32 = 3;
/// 渠道冷却表是保护性缓存而非事实存储；达到上限时停止接纳新记录，已有记录
/// 仍按 TTL 自然清理。这样资源消耗只与配置规模有关，不受错误请求数量控制。
const MAX_CHANNEL_COOLDOWNS: usize = 4_096;

/// 单个渠道的健康账目：连续失败计数与冷却到期时刻同表承载。
#[derive(Debug, Clone, Copy)]
struct ChannelHealth {
    /// 自上次成功出站以来的连续可重试失败次数；成功清零，随冷却到期记录一并清除。
    consecutive_failures: u32,
    /// 冷却到期时刻；未冷却时为 `None`（仅计数）。
    cooldown_until: Option<Instant>,
}

impl ChannelHealth {
    fn is_cooling(&self, now: Instant) -> bool {
        self.cooldown_until.is_some_and(|until| until > now)
    }

    /// 冷却已到期；仅计数的记录（无到期时刻）不算到期，交由成功清零回收。
    fn is_expired(&self, now: Instant) -> bool {
        self.cooldown_until.is_some_and(|until| until <= now)
    }
}

/// 计算冷却到期时刻；时钟不支持前推时退化为当前时刻（立即到期）。
fn cooldown_expiry(now: Instant) -> Instant {
    now.checked_add(CHANNEL_FAILURE_COOLDOWN).unwrap_or(now)
}

/// 上游渠道故障后的跨请求冷却表：上游返回的 402/403 立即冷却，可重试失败
/// （网络错误/429/5xx）按渠道跨请求连续计次、达阈值冷却；成功出站清零。
///
/// 网关本地计费准入拒绝不属于渠道故障，记账调用方必须以
/// [`Outbound::BillingDenied`] 区分来源，不得把本地 402 记入本表。
#[derive(Clone)]
pub struct ChannelCooldowns {
    inner: Arc<Mutex<HashMap<i64, ChannelHealth>>>,
}

impl Default for ChannelCooldowns {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelCooldowns {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 渠道当前是否可出站：无记录、仅计数或冷却已到期均可用。
    ///
    /// 到期即恢复到无失败状态——首个请求即试探，成功清零、失败重新累计；
    /// 到期记录由 [`Self::prune_expired`] 统一回收，此处只读不改。
    fn is_available(&self, channel_id: i64, now: Instant) -> bool {
        let entries = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        match entries.get(&channel_id) {
            Some(health) => !health.is_cooling(now),
            None => true,
        }
    }

    /// 清理已结束的冷却记录，避免长期不再命中的渠道让表持续增长；
    /// 仅计数未冷却的记录不在此回收，由成功清零或后续冷却接手。
    fn prune_expired(&self, now: Instant) {
        let mut entries = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|_, health| !health.is_expired(now));
    }

    /// 记一次上游返回的账号/权限域故障（402/403）：该渠道立即进入冷却。
    fn mark_policy_failure(&self, channel_id: i64, now: Instant) {
        let mut entries = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|_, health| !health.is_expired(now));
        if let Some(health) = entries.get_mut(&channel_id) {
            health.cooldown_until = Some(cooldown_expiry(now));
        } else if entries.len() < MAX_CHANNEL_COOLDOWNS {
            entries.insert(
                channel_id,
                ChannelHealth {
                    consecutive_failures: 0,
                    cooldown_until: Some(cooldown_expiry(now)),
                },
            );
        }
    }

    /// 记一次可重试失败（网络错误/429/5xx）：连续计次，达到阈值即冷却。
    ///
    /// 冷却期间不会再有出站尝试，计数冻结在触发值；到期记录先行回收，
    /// 使恢复后的渠道从零重新累计。
    fn mark_retryable_failure(&self, channel_id: i64, now: Instant) {
        let mut entries = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|_, health| !health.is_expired(now));
        if !entries.contains_key(&channel_id) && entries.len() >= MAX_CHANNEL_COOLDOWNS {
            return;
        }
        let health = entries.entry(channel_id).or_insert(ChannelHealth {
            consecutive_failures: 0,
            cooldown_until: None,
        });
        health.consecutive_failures = health.consecutive_failures.saturating_add(1);
        if health.consecutive_failures >= CHANNEL_RETRY_FAILURE_THRESHOLD {
            health.cooldown_until = Some(cooldown_expiry(now));
        }
    }

    /// 成功出站后清零该渠道的连续失败计数：失败序列只有成功能打断。
    fn clear(&self, channel_id: i64) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&channel_id);
    }

    /// 当前处于冷却中的渠道：`(渠道 id, 冷却到期时刻, 连续失败计数)`。
    /// 供管理面健康视图只读展示，顺序按渠道 id。
    pub(super) fn cooling_channels(&self, now: Instant) -> Vec<(i64, Instant, u32)> {
        let entries = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rows: Vec<(i64, Instant, u32)> = entries
            .iter()
            .filter_map(|(id, health)| {
                let until = health.cooldown_until.filter(|until| *until > now)?;
                Some((*id, until, health.consecutive_failures))
            })
            .collect();
        rows.sort_by_key(|(id, _, _)| *id);
        rows
    }
}

/// 一次失败切换使用的协议编码、退避参数与跨请求密钥/渠道冷却。
pub(super) struct FailoverPolicy<'a> {
    pub(super) inbound_protocol: Protocol,
    pub(super) retry_backoff: RetryBackoff,
    pub(super) key_cooldowns: &'a KeyCooldowns,
    /// 渠道域跨请求冷却：循环入口跳过冷却中的渠道，失败记账、成功清零。
    pub(super) channel_cooldowns: &'a ChannelCooldowns,
}

/// 渠道级预首字节预算：`request_timeout_ms` 夹到合法区间，至少 1ms。
///
/// 预算在渠道入口重锚（连接、响应头、流首 peek、同渠道重试退避共享该渠道
/// 的预算），换渠道 / 统一模型换成员时按新渠道重新起算。
pub(super) fn channel_request_budget(channel: &crate::store::resources::Channel) -> Duration {
    Duration::from_millis(channel.request_timeout_ms.clamp(1, MAX_REQUEST_TIMEOUT_MS))
}

/// 在请求总截止时刻内完成一次退避。返回 false 表示总预算已耗尽，调用方必须
/// 停止继续发起 provider 请求。
async fn wait_for_retry(delay: Duration, deadline: tokio::time::Instant) -> bool {
    tokio::time::timeout_at(deadline, tokio::time::sleep(delay))
        .await
        .is_ok()
}

pub(super) async fn run_failover<'a, A, L>(
    route: &'a routing::Route,
    channels: &'a [ChannelRecord],
    model: &str,
    mut attempt: A,
    log_failure: L,
    policy: FailoverPolicy<'_>,
) -> Response
where
    A: FnMut(
        &'a ChannelRecord,
        &'a StoredChannelKey,
        tokio::time::Instant,
    ) -> BoxFuture<'a, Outbound>,
    L: Fn(&str, u16, bool, &[u8], &str) -> BoxFuture<'a, ()>,
{
    let mut last_failure: Option<FinalFailure> = None;
    let now = Instant::now();
    policy.key_cooldowns.prune_expired(now);
    policy.channel_cooldowns.prune_expired(now);

    'channels: for index in &route.channel_indices {
        let Some(record) = channels.get(*index) else {
            continue;
        };
        // 冷却中的渠道整体跳过：不出站、不记账，剩余候选照常评估。
        if !policy
            .channel_cooldowns
            .is_available(record.id, Instant::now())
        {
            continue;
        }
        let Some(first_key_id) = route.selected_key_id(record.id) else {
            continue;
        };
        // 预首字节预算在本渠道入口重锚：同渠道的密钥轮换与重试退避共享，
        // 切换到下一渠道时由新渠道的 request_timeout_ms 重新起算。
        let channel_deadline = tokio::time::Instant::now()
            .checked_add(channel_request_budget(&record.channel))
            .unwrap_or_else(tokio::time::Instant::now);
        // 轮换池：启用且允许该模型的密钥按存储顺序，旋转到粘性首选开头。
        // 准入已保证非空；快照在准入后变更时兜底跳过该渠道。
        let now = Instant::now();
        let mut pool: Vec<&StoredChannelKey> = eligible_channel_keys(&record.keys, model)
            .into_iter()
            .filter(|key| policy.key_cooldowns.is_available(key.id, now))
            .collect();
        if let Some(position) = pool.iter().position(|key| key.id == first_key_id) {
            pool.rotate_left(position);
        }
        if pool.is_empty() {
            continue;
        }
        let mut rotation = KeyRotation::new(pool);
        let mut retries_used = 0u32;

        loop {
            if tokio::time::Instant::now() >= channel_deadline {
                last_failure = Some(FinalFailure {
                    channel: record.channel.name.clone(),
                    status: Some(504),
                    message: "渠道请求时限已耗尽".to_string(),
                    key_name: rotation.current().name.clone(),
                });
                break 'channels;
            }
            rotation.mark_attempted();
            let key = rotation.current();
            match attempt(record, key, channel_deadline).await {
                Outbound::Success(response) => {
                    policy.key_cooldowns.clear(key.id);
                    policy.channel_cooldowns.clear(record.id);
                    return response;
                }
                Outbound::Fatal {
                    channel,
                    status,
                    message,
                } if is_credential_failure(status) => {
                    // 认证失效是该 key 的问题而非请求的问题：请求级标记后换
                    // 下一把立即重试；渠道内全失效才切渠道。不消耗重试预算
                    // （上限是池大小，每把 key 至多失效一次）。
                    policy
                        .key_cooldowns
                        .mark_auth_failure(key.id, Instant::now());
                    if matches!(rotation.invalidate_current(), Rotation::Depleted) {
                        last_failure = Some(FinalFailure {
                            channel: channel.clone(),
                            status: Some(status),
                            message,
                            key_name: key.name.clone(),
                        });
                        break;
                    }
                }
                Outbound::BillingDenied { channel, message } => {
                    // 计费准入拒绝源于下游钱包/令牌额度域，不是渠道故障：
                    // 只保留归因并切换下一渠道，不给渠道记冷却。
                    last_failure = Some(FinalFailure {
                        channel,
                        status: Some(402),
                        message,
                        key_name: key.name.clone(),
                    });
                    break;
                }
                Outbound::Fatal {
                    channel,
                    status,
                    message,
                } if status == 402 || status == 403 => {
                    // 账号余额与模型权限故障以渠道/账号域为单位：立即冷却该
                    // 渠道，让后续请求不再撞同一堵墙；本请求继续换下一渠道。
                    policy
                        .channel_cooldowns
                        .mark_policy_failure(record.id, Instant::now());
                    last_failure = Some(FinalFailure {
                        channel: channel.clone(),
                        status: Some(status),
                        message,
                        key_name: key.name.clone(),
                    });
                    break;
                }
                Outbound::Fatal {
                    channel,
                    status,
                    message,
                } => {
                    let body = upstream_error_body(
                        status,
                        &message,
                        &channel,
                        false,
                        policy.inbound_protocol,
                    );
                    let wire = serde_json::to_vec(&body).unwrap_or_default();
                    log_failure(&channel, status, false, &wire, &key.name).await;
                    return (
                        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                        Json(body),
                    )
                        .into_response();
                }
                Outbound::Retryable {
                    channel,
                    status,
                    message,
                    retry_after,
                } => {
                    // 可重试失败（网络错误/429/5xx）按渠道跨请求连续计次：
                    // 达到阈值后让后续请求先跳过该渠道，给上游自愈窗口。
                    policy
                        .channel_cooldowns
                        .mark_retryable_failure(record.id, Instant::now());
                    // 429 可能由密钥、账号、组织或模型配额域产生。即使还有未试
                    // 密钥也先服从同一退避，避免在共享配额域内无间隔连打；轮换
                    // 本身仍不额外消耗渠道的同 key 重试预算。
                    let advanced = if status == Some(429) {
                        rotation.after_rate_limit()
                    } else {
                        Rotation::Exhausted
                    };
                    if matches!(advanced, Rotation::Fresh) {
                        if !wait_for_retry(
                            retry_delay(retries_used, retry_after, policy.retry_backoff),
                            channel_deadline,
                        )
                        .await
                        {
                            last_failure = Some(FinalFailure {
                                channel,
                                status: Some(504),
                                message: "渠道请求时限已耗尽".to_string(),
                                key_name: key.name.clone(),
                            });
                            break 'channels;
                        }
                        continue;
                    }
                    last_failure = Some(FinalFailure {
                        channel: channel.clone(),
                        status,
                        message,
                        key_name: key.name.clone(),
                    });
                    if retries_used
                        >= record
                            .channel
                            .max_retries
                            .min(crate::store::resources::MAX_CHANNEL_RETRIES)
                    {
                        break;
                    }
                    if !wait_for_retry(
                        retry_delay(retries_used, retry_after, policy.retry_backoff),
                        channel_deadline,
                    )
                    .await
                    {
                        last_failure = Some(FinalFailure {
                            channel,
                            status: Some(504),
                            message: "渠道请求时限已耗尽".to_string(),
                            key_name: key.name.clone(),
                        });
                        break 'channels;
                    }
                    retries_used += 1;
                }
            }
        }
    }

    let FinalFailure {
        channel,
        status,
        message,
        key_name,
    } = last_failure.unwrap_or_else(|| FinalFailure {
        channel: "unknown".to_string(),
        status: None,
        message: "所有渠道均不可用".to_string(),
        key_name: String::new(),
    });
    let auth_exhausted = status.is_some_and(is_credential_failure);
    let status_code = if auth_exhausted {
        502
    } else {
        status.unwrap_or(502)
    };
    let body = if auth_exhausted {
        protocol::encode_error(502, "没有可用的上游", policy.inbound_protocol)
    } else {
        upstream_error_body(
            status_code,
            &message,
            &channel,
            true,
            policy.inbound_protocol,
        )
    };
    let wire = serde_json::to_vec(&body).unwrap_or_default();
    log_failure(&channel, status_code, true, &wire, &key_name).await;
    (
        StatusCode::from_u16(status_code).unwrap_or(StatusCode::BAD_GATEWAY),
        Json(body),
    )
        .into_response()
}

/// 渠道内密钥轮换的去向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rotation {
    /// 轮换到一把可用的密钥：立即重试。
    Fresh,
    /// 池内密钥本轮已全部试过（或无可轮换目标）：按重试预算计次退避。
    Exhausted,
    /// 池内已无可用密钥：切下一渠道。
    Depleted,
}

/// 请求级渠道内密钥轮换状态。
///
/// 池为该模型可用（启用且允许该模型）的密钥，按存储顺序、起点是粘性首选。
/// 两本账：`tried` 记本轮已尝试（429 轮换在整池试完前免退避、不消耗重试
/// 预算），`dead` 记认证失效（请求级，不再使用）。粘性缓存由准入维护，
/// 本状态不回写——首选 key 的职责保持在粘性选择器。
struct KeyRotation<'k> {
    pool: Vec<&'k StoredChannelKey>,
    index: usize,
    tried: HashSet<i64>,
    dead: HashSet<i64>,
}

impl<'k> KeyRotation<'k> {
    fn new(pool: Vec<&'k StoredChannelKey>) -> Self {
        Self {
            pool,
            index: 0,
            tried: HashSet::new(),
            dead: HashSet::new(),
        }
    }

    /// 当前应使用的密钥。
    ///
    /// # Panics
    ///
    /// 池为空时 panic；[`KeyRotation::new`] 的调用方（`run_failover`）在构造
    /// 前已保证池非空（准入同时保证每条候选渠道至少有一把可用密钥）。
    fn current(&self) -> &'k StoredChannelKey {
        self.pool[self.index]
    }

    fn mark_attempted(&mut self) {
        self.tried.insert(self.current().id);
    }

    /// 429 后的轮换：换下一把未试过的可用 key（免退避）；整池试过则轮回并
    /// 要求计次；全部认证失效则切渠道。
    fn after_rate_limit(&mut self) -> Rotation {
        match self.next_alive() {
            Some(next) if !self.tried.contains(&self.pool[next].id) => {
                self.index = next;
                Rotation::Fresh
            }
            Some(next) => {
                self.index = next;
                Rotation::Exhausted
            }
            None => Rotation::Depleted,
        }
    }

    /// 认证失效（401）：标记当前 key 并换下一把可用 key；全部失效
    /// 返回 [`Rotation::Depleted`]。
    fn invalidate_current(&mut self) -> Rotation {
        self.dead.insert(self.current().id);
        match self.next_alive() {
            Some(next) => {
                self.index = next;
                Rotation::Fresh
            }
            None => Rotation::Depleted,
        }
    }

    /// 从当前位置 cyclic 后找一个未失效的 key；无则 `None`。
    fn next_alive(&self) -> Option<usize> {
        (1..=self.pool.len())
            .map(|step| (self.index + step) % self.pool.len())
            .find(|&index| !self.dead.contains(&self.pool[index].id))
    }
}

/// 构造带网关归因的错误响应体。
pub(super) fn upstream_error_body(
    status: u16,
    message: &str,
    channel: &str,
    failover: bool,
    inbound_protocol: Protocol,
) -> Value {
    let mut body = protocol::encode_error(status, message, inbound_protocol);
    let gateway = json!({ "channel": channel, "failover": failover });
    if let Value::Object(map) = &mut body
        && let Some(error) = map.get_mut("error").and_then(Value::as_object_mut)
    {
        error.insert("gateway".into(), gateway);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::{
        ChannelCooldowns, FailoverPolicy, KeyCooldowns, RetryBackoff, exponential_delay,
        jitter_delay, retry_delay,
    };
    use std::time::{Duration, Instant};

    fn default_backoff() -> RetryBackoff {
        RetryBackoff::from_ms(200, 5_000, 60)
    }

    #[test]
    fn exponential_delay_is_exponential_and_capped() {
        let backoff = default_backoff();
        assert_eq!(exponential_delay(0, backoff), Duration::from_millis(200));
        assert_eq!(exponential_delay(1, backoff), Duration::from_millis(400));
        assert_eq!(exponential_delay(2, backoff), Duration::from_millis(800));
        assert_eq!(exponential_delay(16, backoff), Duration::from_secs(5));
    }

    #[test]
    fn retry_delay_honors_retry_after_without_jitter() {
        let backoff = default_backoff();
        assert_eq!(
            retry_delay(0, Some(Duration::from_secs(3)), backoff),
            Duration::from_secs(3)
        );
        assert_eq!(
            retry_delay(0, Some(Duration::from_secs(120)), backoff),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn jitter_delay_does_not_exceed_cap() {
        let backoff = default_backoff();
        for _ in 0..32 {
            let delay = retry_delay(16, None, backoff);
            assert!(
                delay <= backoff.cap,
                "抖动后等待 {delay:?} 不应超过封顶 {:?}",
                backoff.cap
            );
        }
    }

    #[test]
    fn retry_delay_uses_configured_base_and_caps() {
        let backoff = RetryBackoff::from_ms(100, 1_000, 10);
        assert_eq!(exponential_delay(0, backoff), Duration::from_millis(100));
        assert_eq!(exponential_delay(4, backoff), Duration::from_millis(1_000));
        assert_eq!(
            retry_delay(0, Some(Duration::from_secs(30)), backoff),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn expired_key_cooldowns_are_pruned() {
        let cooldowns = KeyCooldowns::new();
        let now = Instant::now();
        let live_until = now
            .checked_add(Duration::from_secs(10))
            .expect("测试时间应可计算");
        cooldowns
            .inner
            .lock()
            .expect("冷却表锁不应被污染")
            .insert(1, now);
        cooldowns
            .inner
            .lock()
            .expect("冷却表锁不应被污染")
            .insert(2, live_until);

        cooldowns.prune_expired(
            now.checked_add(Duration::from_secs(1))
                .expect("测试时间应可计算"),
        );

        let entries = cooldowns.inner.lock().expect("冷却表锁不应被污染");
        assert!(!entries.contains_key(&1));
        assert_eq!(entries.get(&2), Some(&live_until));
    }

    // ---- 渠道冷却表 ----

    /// 冷却到期后一毫秒的时刻，供恢复断言使用。
    fn after(now: Instant, millis: u64) -> Instant {
        now.checked_add(Duration::from_millis(millis.max(1)))
            .expect("测试时间应可计算")
    }

    #[test]
    fn upstream_policy_failure_cools_channel_immediately() {
        let cooldowns = ChannelCooldowns::new();
        let now = Instant::now();
        cooldowns.mark_policy_failure(1, now);

        assert!(
            !cooldowns.is_available(1, now),
            "上游 402/403 应立即冷却渠道"
        );
        assert_eq!(
            cooldowns.cooling_channels(now),
            vec![(1, now.checked_add(Duration::from_secs(5 * 60)).unwrap(), 0)],
            "冷却行应含到期时刻，连续可重试失败计数为 0"
        );

        // 到期自然恢复：恢复后计数随之清空。
        let later = after(now, 5 * 60 * 1000);
        assert!(cooldowns.is_available(1, later), "到期后渠道应恢复可用");
        assert!(cooldowns.cooling_channels(later).is_empty());
    }

    #[test]
    fn retryable_failures_accumulate_across_requests_until_threshold() {
        let cooldowns = ChannelCooldowns::new();
        let mut now = Instant::now();
        // 每次记账之间穿插可用性检查，模拟跨请求的先查后记。
        cooldowns.mark_retryable_failure(7, now);
        now = after(now, 1);
        assert!(cooldowns.is_available(7, now), "单次失败只计不冷却");
        cooldowns.mark_retryable_failure(7, now);
        now = after(now, 1);
        assert!(cooldowns.is_available(7, now), "两次失败仍不冷却");
        cooldowns.mark_retryable_failure(7, now);

        assert!(!cooldowns.is_available(7, now), "第三次连续失败进入冷却");
        assert_eq!(
            cooldowns.cooling_channels(now),
            vec![(7, now.checked_add(Duration::from_secs(5 * 60)).unwrap(), 3)]
        );
    }

    #[test]
    fn success_clears_the_failure_streak() {
        let cooldowns = ChannelCooldowns::new();
        let now = Instant::now();
        cooldowns.mark_retryable_failure(7, now);
        cooldowns.mark_retryable_failure(7, now);
        cooldowns.clear(7);

        assert!(cooldowns.is_available(7, now), "成功清零后立即可用");
        assert!(
            cooldowns.cooling_channels(now).is_empty(),
            "清零不留展示记录"
        );

        // 清零后须重新计满才冷却。
        cooldowns.mark_retryable_failure(7, now);
        cooldowns.mark_retryable_failure(7, now);
        assert!(cooldowns.is_available(7, now), "重新累计未满不冷却");
    }

    #[test]
    fn expired_cooldown_releases_the_channel_and_resets_the_streak() {
        let cooldowns = ChannelCooldowns::new();
        let now = Instant::now();
        cooldowns.mark_retryable_failure(7, now);
        cooldowns.mark_retryable_failure(7, now);
        cooldowns.mark_retryable_failure(7, now);

        let expired = after(now, 5 * 60 * 1000);
        cooldowns.prune_expired(expired);
        assert!(cooldowns.is_available(7, expired), "到期自然恢复");
        // 到期后计数从零重新累计：两次失败不足以再冷却。
        cooldowns.mark_retryable_failure(7, expired);
        cooldowns.mark_retryable_failure(7, after(expired, 1));
        assert!(
            cooldowns.is_available(7, after(expired, 1)),
            "恢复后的渠道应回到无失败状态"
        );
    }

    #[test]
    fn capacity_stops_admitting_new_channels() {
        let cooldowns = ChannelCooldowns::new();
        let now = Instant::now();
        for id in 0..super::MAX_CHANNEL_COOLDOWNS as i64 {
            cooldowns.mark_retryable_failure(id, now);
        }
        // 表满后新渠道不再接纳：记不上账，也就不会被冷却。
        let outsider = super::MAX_CHANNEL_COOLDOWNS as i64 + 1;
        cooldowns.mark_retryable_failure(outsider, now);
        assert!(
            cooldowns.is_available(outsider, now),
            "容量上限应停止接纳新渠道"
        );
        assert!(cooldowns.cooling_channels(now).is_empty());
        // 已在表内的渠道仍按阈值正常冷却。
        cooldowns.mark_retryable_failure(0, now);
        cooldowns.mark_retryable_failure(0, now);
        assert!(!cooldowns.is_available(0, now), "表内渠道不受上限影响");
    }

    #[test]
    fn jitter_delay_stays_within_plus_minus_20_percent() {
        let base = Duration::from_millis(200);
        let lo = Duration::from_millis(160);
        let hi = Duration::from_millis(240);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..40 {
            let got = jitter_delay(base);
            assert!(
                got >= lo && got <= hi,
                "jitter {got:?} 应在 [{lo:?}, {hi:?}]"
            );
            seen.insert(got);
        }
        assert!(seen.len() > 1, "±20% 抖动应产生多于一个等待值");
    }

    // ---- 渠道内密钥轮换 ----

    use super::routing;
    use super::{KeyRotation, Outbound, Rotation, run_failover};
    use crate::config::Protocol;
    use crate::store::resources::{ChannelRecord, StoredChannelKey};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use futures_util::future::BoxFuture;

    fn key(id: i64, name: &str) -> StoredChannelKey {
        StoredChannelKey::new(
            id,
            1,
            name.into(),
            format!("k-{name}"),
            1,
            true,
            None,
            None,
            0,
        )
    }

    fn pool(keys: &[StoredChannelKey]) -> Vec<&StoredChannelKey> {
        keys.iter().collect()
    }

    #[test]
    fn rate_limit_rotates_fresh_then_counts_after_pool_exhausted() {
        let keys = [key(1, "a"), key(2, "b")];
        let mut rotation = KeyRotation::new(pool(&keys));
        rotation.mark_attempted();
        assert_eq!(rotation.current().name, "a");

        // 整池试完前：轮换免退避。
        assert_eq!(rotation.after_rate_limit(), Rotation::Fresh);
        rotation.mark_attempted();
        assert_eq!(rotation.current().name, "b");

        // 整池试完：轮回 + 计次。
        assert_eq!(rotation.after_rate_limit(), Rotation::Exhausted);
        rotation.mark_attempted();
        assert_eq!(rotation.current().name, "a");
        assert_eq!(rotation.after_rate_limit(), Rotation::Exhausted);
    }

    #[test]
    fn auth_invalidated_keys_are_skipped_until_pool_depleted() {
        let keys = [key(1, "a"), key(2, "b")];
        let mut rotation = KeyRotation::new(pool(&keys));
        rotation.mark_attempted();

        // a 认证失效 → 轮换到 b。
        assert_eq!(rotation.invalidate_current(), Rotation::Fresh);
        rotation.mark_attempted();
        assert_eq!(rotation.current().name, "b");

        // b 也失效 → 渠道内无可用密钥，切渠道。
        assert_eq!(rotation.invalidate_current(), Rotation::Depleted);
    }

    #[test]
    fn auth_rotation_skips_previously_dead_keys_on_cycle() {
        let keys = [key(1, "a"), key(2, "b")];
        let mut rotation = KeyRotation::new(pool(&keys));
        rotation.mark_attempted();
        // a 先因认证失效退场，随后 429 的轮换不得再落到 a。
        assert_eq!(rotation.invalidate_current(), Rotation::Fresh);
        rotation.mark_attempted();
        assert_eq!(rotation.after_rate_limit(), Rotation::Exhausted);
        assert_eq!(rotation.current().name, "b");
    }

    #[test]
    fn single_key_pool_behaves_as_plain_same_key_retry() {
        let keys = [key(1, "a")];
        let mut rotation = KeyRotation::new(pool(&keys));
        rotation.mark_attempted();
        // 单 key 渠道：429 轮换原地踏步（走计次退避路径），auth 失效直接切渠道。
        assert_eq!(rotation.after_rate_limit(), Rotation::Exhausted);
        assert_eq!(rotation.current().name, "a");
        assert_eq!(rotation.invalidate_current(), Rotation::Depleted);
    }

    // ---- run_failover 的轮换语义（attempt 闭包驱动）----

    use axum::body::Body;
    use std::sync::{Arc, Mutex as StdMutex};

    fn record_with_keys(id: i64, keys: Vec<StoredChannelKey>, max_retries: u32) -> ChannelRecord {
        ChannelRecord {
            id,
            keys: keys.clone(),
            channel: crate::store::resources::Channel {
                name: format!("ch-{id}"),
                protocol: Protocol::OpenAiChat,
                base_url: "http://localhost".to_string(),
                keys: Vec::new(),
                models: vec!["m".to_string()],
                model_aliases: Default::default(),
                timeout_ms: 1000,
                request_timeout_ms: 120_000,
                max_retries,
                enabled: true,
                model_group: crate::store::resources::DEFAULT_MODEL_GROUP.to_string(),
                reasoning_output: Default::default(),
                session_cache_key: Default::default(),
                injects_cache_breakpoints: false,
                abort_on_disconnect: true,
            },
        }
    }

    fn route_of(records: &[ChannelRecord], first_picks: &[(i64, i64)]) -> routing::Route {
        routing::Route {
            selected_key_ids: first_picks.iter().copied().collect(),
            channel_indices: (0..records.len()).collect(),
        }
    }

    fn ok_response() -> Response {
        (StatusCode::OK, Body::empty()).into_response()
    }

    /// 无侧害的失败日志闭包。
    async fn no_log() {}

    fn no_failure_log<'a>() -> impl Fn(&str, u16, bool, &[u8], &str) -> BoxFuture<'a, ()> {
        |_channel, _status, _failover, _wire, _key| Box::pin(no_log())
    }

    #[tokio::test]
    async fn rate_limit_rotation_is_free_until_pool_exhausted() {
        let keys = vec![key(1, "a"), key(2, "b")];
        let records = vec![record_with_keys(1, keys.clone(), 0)];
        let route = route_of(&records, &[(1, keys[0].id)]);
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let seen_for_attempt = seen.clone();

        // max_retries=0：a 的 429 轮换到未试过的 b 恢复，不消耗重试预算。
        let response = run_failover(
            &route,
            &records,
            "m",
            move |_record, key, _channel_deadline| {
                seen_for_attempt.lock().unwrap().push(key.name.clone());
                let key_name = key.name.clone();
                Box::pin(async move {
                    if key_name == "a" {
                        Outbound::Retryable {
                            channel: "ch-1".to_string(),
                            status: Some(429),
                            message: "rate limited".to_string(),
                            retry_after: None,
                        }
                    } else {
                        Outbound::Success(ok_response())
                    }
                })
            },
            no_failure_log(),
            FailoverPolicy {
                inbound_protocol: Protocol::OpenAiChat,
                retry_backoff: RetryBackoff::from_ms(10_000, 10_000, 10),
                key_cooldowns: &KeyCooldowns::new(),
                channel_cooldowns: &ChannelCooldowns::new(),
            },
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[tokio::test]
    async fn server_error_retries_same_key_within_budget() {
        let keys = vec![key(1, "a"), key(2, "b")];
        let records = vec![record_with_keys(1, keys.clone(), 1)];
        let route = route_of(&records, &[(1, keys[0].id)]);
        let seen = Arc::new(StdMutex::new(Vec::new()));

        // 5xx 与 key 无关：同 key 退避重试（b 不出场），预算耗尽返回错误。
        let seen_for_attempt = seen.clone();
        let response = run_failover(
            &route,
            &records,
            "m",
            move |_record, key, _channel_deadline| {
                seen_for_attempt.lock().unwrap().push(key.name.clone());
                Box::pin(async {
                    Outbound::Retryable {
                        channel: "ch-1".to_string(),
                        status: Some(500),
                        message: "boom".to_string(),
                        retry_after: None,
                    }
                })
            },
            no_failure_log(),
            FailoverPolicy {
                inbound_protocol: Protocol::OpenAiChat,
                retry_backoff: RetryBackoff::from_ms(1, 1, 1),
                key_cooldowns: &KeyCooldowns::new(),
                channel_cooldowns: &ChannelCooldowns::new(),
            },
        )
        .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["a".to_string(), "a".to_string()],
            "5xx 重试维持同一把 key，不轮换"
        );
    }

    #[tokio::test]
    async fn auth_depleted_channel_switches_to_next_record() {
        let first = vec![key(1, "a"), key(2, "b")];
        let second = vec![key(3, "c")];
        let records = vec![
            record_with_keys(1, first.clone(), 0),
            record_with_keys(2, second.clone(), 0),
        ];
        let route = route_of(&records, &[(1, first[0].id), (2, second[0].id)]);
        let seen = Arc::new(StdMutex::new(Vec::new()));

        // 渠道内全部 key 认证失效才切下一渠道；结算/日志归接手渠道。
        let seen_for_attempt = seen.clone();
        let response = run_failover(
            &route,
            &records,
            "m",
            move |_record, key, _channel_deadline| {
                seen_for_attempt.lock().unwrap().push(key.name.clone());
                let key_name = key.name.clone();
                Box::pin(async move {
                    if key_name == "c" {
                        Outbound::Success(ok_response())
                    } else {
                        Outbound::Fatal {
                            channel: "ch-1".to_string(),
                            status: 401,
                            message: "forbidden".to_string(),
                        }
                    }
                })
            },
            no_failure_log(),
            FailoverPolicy {
                inbound_protocol: Protocol::OpenAiChat,
                retry_backoff: RetryBackoff::from_ms(10_000, 10_000, 10),
                key_cooldowns: &KeyCooldowns::new(),
                channel_cooldowns: &ChannelCooldowns::new(),
            },
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    // ---- 渠道冷却的 failover 语义 ----

    #[tokio::test]
    async fn cooled_channel_is_skipped_but_remaining_candidates_proceed() {
        let first = vec![key(1, "a")];
        let second = vec![key(2, "b")];
        let records = vec![
            record_with_keys(1, first.clone(), 0),
            record_with_keys(2, second.clone(), 0),
        ];
        let route = route_of(&records, &[(1, first[0].id), (2, second[0].id)]);
        let cooldowns = ChannelCooldowns::new();
        cooldowns.mark_policy_failure(1, Instant::now());
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let seen_for_attempt = seen.clone();

        let response = run_failover(
            &route,
            &records,
            "m",
            move |record, _key, _channel_deadline| {
                seen_for_attempt.lock().unwrap().push(record.id);
                Box::pin(async { Outbound::Success(ok_response()) })
            },
            no_failure_log(),
            FailoverPolicy {
                inbound_protocol: Protocol::OpenAiChat,
                retry_backoff: RetryBackoff::from_ms(1, 1, 1),
                key_cooldowns: &KeyCooldowns::new(),
                channel_cooldowns: &cooldowns,
            },
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![2],
            "冷却中的渠道虽在候选列表但不出站，后续候选照常"
        );
    }

    #[tokio::test]
    async fn upstream_402_cools_the_channel_for_future_requests() {
        let first = vec![key(1, "a")];
        let second = vec![key(2, "b")];
        let records = vec![
            record_with_keys(1, first.clone(), 0),
            record_with_keys(2, second.clone(), 0),
        ];
        let route = route_of(&records, &[(1, first[0].id), (2, second[0].id)]);
        let cooldowns = ChannelCooldowns::new();

        let response = run_failover(
            &route,
            &records,
            "m",
            move |record, _key, _channel_deadline| {
                let record_id = record.id;
                Box::pin(async move {
                    if record_id == 1 {
                        Outbound::Fatal {
                            channel: "ch-1".to_string(),
                            status: 402,
                            message: "上游余额不足".to_string(),
                        }
                    } else {
                        Outbound::Success(ok_response())
                    }
                })
            },
            no_failure_log(),
            FailoverPolicy {
                inbound_protocol: Protocol::OpenAiChat,
                retry_backoff: RetryBackoff::from_ms(1, 1, 1),
                key_cooldowns: &KeyCooldowns::new(),
                channel_cooldowns: &cooldowns,
            },
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            !cooldowns.is_available(1, Instant::now()),
            "上游 402 应立即冷却该渠道"
        );
        assert!(cooldowns.is_available(2, Instant::now()));
    }

    #[tokio::test]
    async fn local_billing_denial_does_not_cool_the_channel() {
        let keys = vec![key(1, "a")];
        let records = vec![record_with_keys(1, keys.clone(), 0)];
        let route = route_of(&records, &[(1, keys[0].id)]);
        let cooldowns = ChannelCooldowns::new();

        // 唯一渠道计费拒绝：候选耗尽后以 402 返回下游，渠道不进冷却。
        let response = run_failover(
            &route,
            &records,
            "m",
            move |_record, _key, _channel_deadline| {
                Box::pin(async {
                    Outbound::BillingDenied {
                        channel: "ch-1".to_string(),
                        message: "余额或令牌累计上限不足以覆盖本次出站尝试".to_string(),
                    }
                })
            },
            no_failure_log(),
            FailoverPolicy {
                inbound_protocol: Protocol::OpenAiChat,
                retry_backoff: RetryBackoff::from_ms(1, 1, 1),
                key_cooldowns: &KeyCooldowns::new(),
                channel_cooldowns: &cooldowns,
            },
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::PAYMENT_REQUIRED,
            "计费拒绝耗尽候选后仍以 402 返回下游"
        );
        assert!(
            cooldowns.is_available(1, Instant::now()),
            "本地计费拒绝是下游域故障，不得冷却渠道"
        );
    }
}
