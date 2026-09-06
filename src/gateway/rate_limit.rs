//! 协议面请求速率限制：令牌、用户与套餐桶在滑动一分钟窗口内分别计数，超限 429。
//!
//! 令牌桶上限 = 该令牌 `rate_limit_rpm`，缺省用全局兜底（`0` 不限速，令牌显式
//! `0` 可覆盖全局）。用户桶上限由调用方解析为该用户的有效 RPM，跨该用户
//! **所有令牌共享**；套餐桶上限由调用方解析为该套餐的共享 RPM，跨档内
//! **所有用户共享**。三个桶取最小值生效。
//!
//! 计数在进程内存，重启清零，多实例不共享。成功认证后的请求计入，认证失败走
//! [`super::throttle::AuthThrottle`]，两套计数不互通。

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lru::LruCache;

use crate::store::channel_keys::{
    StoredChannelKey, eligible_channel_keys, select_weighted_channel_key,
};

const STICKY_TTL: Duration = Duration::from_secs(60 * 60);
const STICKY_MAX_ENTRIES: usize = 10_000;
const STICKY_CLEANUP_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct StickyKey {
    pub(super) channel_id: i64,
    pub(super) model: String,
    pub(super) session: u64,
}

struct StickyEntry {
    key_id: i64,
    expires_at: Instant,
}

/// 进程内会话粘性缓存；密钥选择与校验在同一把锁内完成，避免并发首次请求分叉。
#[derive(Clone)]
pub(super) struct SessionStickyCache {
    inner: Arc<Mutex<LruCache<StickyKey, StickyEntry>>>,
}

impl SessionStickyCache {
    pub(super) fn new() -> Self {
        let cache = Self::with_capacity(STICKY_MAX_ENTRIES);
        cache.start_cleanup();
        cache
    }

    fn with_capacity(max_entries: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(max_entries).unwrap_or(NonZeroUsize::MIN),
            ))),
        }
    }

    /// 在 Tokio 运行时中启动过期清理；同步单元测试等无运行时场景保持纯构造。
    fn start_cleanup(&self) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let weak_inner = Arc::downgrade(&self.inner);
        drop(handle.spawn(async move {
            let mut interval = tokio::time::interval(STICKY_CLEANUP_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(inner) = weak_inner.upgrade() else {
                    break;
                };
                let now = Instant::now();
                let mut map = inner.lock().unwrap_or_else(PoisonError::into_inner);
                prune_sticky_expired(&mut map, now);
            }
        }));
    }

    pub(super) fn select(
        &self,
        channel_id: i64,
        model: &str,
        session: u64,
        keys: &[StoredChannelKey],
    ) -> Option<i64> {
        let candidates = eligible_channel_keys(keys, model);
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let cache_key = StickyKey {
            channel_id,
            model: model.to_string(),
            session,
        };
        if candidates.is_empty() {
            map.pop(&cache_key);
            return None;
        }
        if candidates.len() == 1 {
            // 单密钥渠道不使用粘性；清掉旧的多密钥缓存，避免恢复后错误复用。
            map.pop(&cache_key);
            return Some(candidates[0].id);
        }

        if let Some(entry) = map.get_mut(&cache_key)
            && entry.expires_at > now
            && let Some(key) = candidates.iter().find(|key| key.id == entry.key_id)
        {
            return Some(key.id);
        }
        map.pop(&cache_key);

        let selected = select_weighted_channel_key(&candidates)?;
        map.put(
            cache_key,
            StickyEntry {
                key_id: selected.id,
                expires_at: now.checked_add(STICKY_TTL).unwrap_or(now),
            },
        );
        Some(selected.id)
    }
}

fn prune_sticky_expired(map: &mut LruCache<StickyKey, StickyEntry>, now: Instant) {
    let expired_keys: Vec<StickyKey> = map
        .iter()
        .filter(|(_, entry)| entry.expires_at <= now)
        .map(|(key, _)| key.clone())
        .collect();
    for key in expired_keys {
        map.pop(&key);
    }
}

/// 滑动窗口长度：RPM 按自然分钟计数。
const WINDOW: Duration = Duration::from_secs(60);

/// 限流桶的键：令牌、用户与套餐维度分开计数，类型不同天然不碰撞。
#[derive(Clone, PartialEq, Eq, Hash)]
enum RateKey {
    Token(String),
    User(i64),
    Plan(i64),
}

const RATE_LIMIT_SHARDS: usize = 64;

impl RateKey {
    fn shard_index(&self) -> usize {
        let hash = match self {
            Self::Token(token) => token.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                hash.wrapping_mul(0x0000_0100_0000_01b3) ^ u64::from(byte)
            }),
            Self::User(id) => (*id as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15),
            Self::Plan(id) => (*id as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f),
        };
        hash as usize & (RATE_LIMIT_SHARDS - 1)
    }
}

/// 可在网关依赖间克隆共享的请求计数器。
#[derive(Clone)]
pub(super) struct RequestRateLimiter {
    inner: Arc<RateLimiterInner>,
}

struct RateLimiterInner {
    shards: Vec<Mutex<RateBuckets>>,
}

struct RateBuckets {
    entries: HashMap<RateKey, VecDeque<Instant>>,
    acquisitions: u64,
}

struct RateConstraint {
    key: RateKey,
    limit: u64,
    shard_index: usize,
}

impl RequestRateLimiter {
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::new(RateLimiterInner {
                shards: (0..RATE_LIMIT_SHARDS)
                    .map(|_| {
                        Mutex::new(RateBuckets {
                            entries: HashMap::new(),
                            acquisitions: 0,
                        })
                    })
                    .collect(),
            }),
        }
    }

    /// 一次请求同时占用令牌桶、用户桶与（配置了正数上限时的）套餐桶。
    ///
    /// 三桶容量判定都通过才同时入队；任一满员则**都不记账**——被拒的请求不应
    /// 消耗另一维度的配额（否则一次用户级 429 会白白吃掉令牌桶的窗口）。
    /// `token_limit == 0` 且用户、套餐都未配上限即完全不限速。
    pub(super) fn try_acquire(
        &self,
        token_key: &str,
        token_limit: u64,
        user: Option<(i64, u64)>,
        plan: Option<(i64, u64)>,
    ) -> Result<(), Duration> {
        // `limit > 0` 才是有效用户桶：`None`/`0` 都表示用户维度不限速。
        let user = user.filter(|&(_, limit)| limit > 0);
        // `limit > 0` 才是有效套餐桶：`None`/`0` 都表示套餐维度不限速。
        let plan = plan.filter(|&(_, limit)| limit > 0);
        if token_limit == 0 && user.is_none() && plan.is_none() {
            return Ok(());
        }
        let mut constraints = Vec::with_capacity(3);
        if let Some((user_id, limit)) = user {
            constraints.push(RateConstraint::new(RateKey::User(user_id), limit));
        }
        if let Some((plan_id, limit)) = plan {
            constraints.push(RateConstraint::new(RateKey::Plan(plan_id), limit));
        }
        if token_limit > 0 {
            constraints.push(RateConstraint::new(
                RateKey::Token(token_key.to_string()),
                token_limit,
            ));
        }
        constraints.sort_unstable_by_key(|constraint| constraint.shard_index);

        let mut shard_indices: Vec<usize> = constraints
            .iter()
            .map(|constraint| constraint.shard_index)
            .collect();
        shard_indices.dedup();
        let mut locked_shards = shard_indices
            .iter()
            .map(|index| {
                (
                    *index,
                    self.inner.shards[*index]
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner),
                )
            })
            .collect::<Vec<_>>();

        let now = Instant::now();
        for (_, buckets) in &mut locked_shards {
            buckets.acquisitions = buckets.acquisitions.wrapping_add(1);
            if buckets.acquisitions.is_multiple_of(1024) {
                prune_expired(&mut buckets.entries, now);
            }
        }
        for constraint in &constraints {
            let map = locked_entries(&mut locked_shards, constraint.shard_index);
            prune_bucket(map, &constraint.key, now);
        }
        for constraint in &constraints {
            let map = locked_entries(&mut locked_shards, constraint.shard_index);
            if bucket_full(map, &constraint.key, constraint.limit, now) {
                return Err(retry_after(map, &constraint.key, now));
            }
        }
        for constraint in constraints {
            locked_entries(&mut locked_shards, constraint.shard_index)
                .entry(constraint.key)
                .or_default()
                .push_back(now);
        }
        Ok(())
    }
}

impl RateConstraint {
    fn new(key: RateKey, limit: u64) -> Self {
        let shard_index = key.shard_index();
        Self {
            key,
            limit,
            shard_index,
        }
    }
}

fn locked_entries<'a>(
    locked_shards: &'a mut [(usize, std::sync::MutexGuard<'_, RateBuckets>)],
    shard_index: usize,
) -> &'a mut HashMap<RateKey, VecDeque<Instant>> {
    locked_shards
        .iter_mut()
        .find(|(index, _)| *index == shard_index)
        .map(|(_, buckets)| &mut buckets.entries)
        .expect("每个约束对应的限流分片均已按序加锁")
}

fn prune_bucket(map: &mut HashMap<RateKey, VecDeque<Instant>>, key: &RateKey, now: Instant) {
    let should_remove = if let Some(queue) = map.get_mut(key) {
        while queue
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= WINDOW)
        {
            queue.pop_front();
        }
        queue.is_empty()
    } else {
        false
    };
    if should_remove {
        map.remove(key);
    }
}

/// 桶是否已满（窗口内计数达到上限）。
fn bucket_full(
    map: &HashMap<RateKey, VecDeque<Instant>>,
    key: &RateKey,
    limit: u64,
    now: Instant,
) -> bool {
    map.get(key).is_some_and(|queue| {
        queue.len() as u64 >= limit
            && queue
                .front()
                .is_some_and(|at| now.duration_since(*at) < WINDOW)
    })
}

/// 满员桶的建议等待时长：最老一条滑出窗口所需时间，至少 1 秒。
fn retry_after(map: &HashMap<RateKey, VecDeque<Instant>>, key: &RateKey, now: Instant) -> Duration {
    let oldest = map
        .get(key)
        .and_then(|queue| queue.front().copied())
        .unwrap_or(now);
    WINDOW
        .saturating_sub(now.saturating_duration_since(oldest))
        .max(Duration::from_secs(1))
}

fn prune_expired(map: &mut HashMap<RateKey, VecDeque<Instant>>, now: Instant) {
    map.retain(|_, queue| {
        while queue
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= WINDOW)
        {
            queue.pop_front();
        }
        !queue.is_empty()
    });
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{RequestRateLimiter, SessionStickyCache, StickyEntry, StickyKey};
    use crate::store::resources::StoredChannelKey;

    fn key(id: i64, weight: i64, enabled: bool) -> StoredChannelKey {
        StoredChannelKey::new(
            id,
            7,
            format!("key-{id}"),
            format!("secret-{id}"),
            weight,
            enabled,
            None,
            None,
            0,
        )
    }

    #[test]
    fn blocks_after_limit_from_same_token() {
        let limiter = RequestRateLimiter::new();
        assert!(limiter.try_acquire("sk-a", 2, None, None).is_ok());
        assert!(limiter.try_acquire("sk-a", 2, None, None).is_ok());
        assert!(limiter.try_acquire("sk-a", 2, None, None).is_err());
        assert!(limiter.try_acquire("sk-b", 2, None, None).is_ok());
    }

    #[test]
    fn zero_limit_disables_limiter() {
        let limiter = RequestRateLimiter::new();
        for _ in 0..8 {
            assert!(limiter.try_acquire("sk-a", 0, None, None).is_ok());
        }
    }

    #[test]
    fn zero_user_limit_is_no_user_bucket() {
        let limiter = RequestRateLimiter::new();
        for _ in 0..8 {
            assert!(limiter.try_acquire("sk-a", 0, Some((7, 0)), None).is_ok());
        }
    }

    /// 用户桶跨令牌共享：5 把令牌各打一次，用户上限 2 时第 3 次即被拒。
    #[test]
    fn user_bucket_is_shared_across_tokens() {
        let limiter = RequestRateLimiter::new();
        let user = Some((7, 2));
        assert!(limiter.try_acquire("sk-1", 0, user, None).is_ok());
        assert!(limiter.try_acquire("sk-2", 0, user, None).is_ok());
        // 第 3 次换了新令牌（令牌桶各自独立），仍被用户桶挡住。
        assert!(limiter.try_acquire("sk-3", 100, user, None).is_err());
        // 用户被拒不消耗令牌桶：sk-3 自己的桶仍是空的。
        assert!(limiter.try_acquire("sk-3", 100, None, None).is_ok());
    }

    /// 被拒请求不记账：令牌桶满导致的 429 不得消耗用户桶配额。
    #[test]
    fn rejected_request_does_not_consume_the_other_bucket() {
        let limiter = RequestRateLimiter::new();
        let user = Some((7, 10));
        // 令牌桶限 1：第 1 次过，第 2 次被令牌桶拒（不应进入用户桶）。
        assert!(limiter.try_acquire("sk-a", 1, user, None).is_ok());
        assert!(limiter.try_acquire("sk-a", 1, user, None).is_err());
        // 用户桶只记了 1 次：其余 9 次配额仍可用（换令牌打）。
        for _ in 0..9 {
            assert!(limiter.try_acquire("sk-b", 0, user, None).is_ok());
        }
        assert!(limiter.try_acquire("sk-c", 0, user, None).is_err());
    }

    /// 单令牌视角取两桶较小值：令牌 30、用户 50 → 第 31 次被拒（令牌桶先满）。
    #[test]
    fn single_token_takes_the_smaller_limit() {
        let limiter = RequestRateLimiter::new();
        let user = Some((9, 50));
        for _ in 0..30 {
            assert!(limiter.try_acquire("sk-a", 30, user, None).is_ok());
        }
        assert!(limiter.try_acquire("sk-a", 30, user, None).is_err());
    }

    /// 套餐桶跨用户共享：同档另一用户也会被套餐上限挡住。
    #[test]
    fn plan_bucket_is_shared_across_users() {
        let limiter = RequestRateLimiter::new();
        let plan = Some((3, 2));
        assert!(limiter.try_acquire("sk-a", 0, Some((7, 10)), plan).is_ok());
        assert!(limiter.try_acquire("sk-b", 0, Some((8, 10)), plan).is_ok());
        assert!(limiter.try_acquire("sk-c", 0, Some((9, 10)), plan).is_err());
    }

    /// 任一桶拒绝时，用户、套餐、令牌三个桶都不应留下这次请求的记录。
    #[test]
    fn rejected_request_does_not_consume_any_other_bucket() {
        let limiter = RequestRateLimiter::new();
        let plan = Some((3, 2));
        let user = Some((7, 1));

        // 先填满用户桶；第二次请求会在用户桶被拒，不能偷占套餐桶。
        assert!(limiter.try_acquire("sk-a", 10, user, plan).is_ok());
        assert!(limiter.try_acquire("sk-b", 10, user, plan).is_err());

        // 换用户后仍可使用套餐剩余的一个名额，证明上一次拒绝没有记入套餐桶。
        assert!(limiter.try_acquire("sk-c", 10, Some((8, 1)), plan).is_ok());

        // 套餐已满；第三个用户的令牌桶和用户桶仍应保持空闲。
        assert!(limiter.try_acquire("sk-d", 10, Some((9, 1)), plan).is_err());
        assert!(limiter.try_acquire("sk-d", 10, Some((9, 1)), None).is_ok());
    }

    #[test]
    fn sticky_selection_reuses_key_for_same_channel_model_session() {
        let cache = SessionStickyCache::new();
        let keys = [key(1, 1, true), key(2, 1, true)];
        let first = cache.select(7, "model", 42, &keys).expect("应选出密钥");
        let second = cache.select(7, "model", 42, &keys).expect("应复用密钥");
        assert_eq!(first, second);
    }

    #[test]
    fn sticky_selection_is_scoped_by_channel_and_model() {
        let cache = SessionStickyCache::new();
        let keys = [key(1, 1, true), key(2, 1, true)];
        let other_keys = [key(3, 1, true), key(4, 1, true)];
        let first = cache.select(7, "model-a", 42, &keys).expect("应选出密钥");
        let other_channel = cache
            .select(8, "model-a", 42, &other_keys)
            .expect("应选出密钥");
        let other_model = cache
            .select(7, "model-b", 42, &other_keys)
            .expect("应选出密钥");
        assert_eq!(cache.select(7, "model-a", 42, &keys).unwrap(), first);
        assert!(other_channel == 3 || other_channel == 4);
        assert!(other_model == 3 || other_model == 4);
    }

    #[test]
    fn disabled_cached_key_is_removed_and_reselected() {
        let cache = SessionStickyCache::new();
        let keys = [key(1, 1, true), key(2, 1, true)];
        let first = cache.select(7, "model", 42, &keys).expect("应选出密钥");
        let disabled = [
            key(first, 1, false),
            key(if first == 1 { 2 } else { 1 }, 1, true),
        ];
        let selected = cache.select(7, "model", 42, &disabled).expect("应重选");
        assert_ne!(selected, first);
    }

    #[test]
    fn collapsing_to_one_key_drops_old_sticky_entry() {
        let cache = SessionStickyCache::new();
        let keys = [key(1, 1, true), key(2, 1, true)];
        let first = cache.select(7, "model", 42, &keys).expect("应选出密钥");
        let remaining_id = if first == 1 { 2 } else { 1 };
        let remaining = [key(remaining_id, 1, true)];
        assert_eq!(
            cache.select(7, "model", 42, &remaining).unwrap(),
            remaining_id
        );
        assert!(cache.inner.lock().expect("缓存锁不应被污染").is_empty());
        let restored = cache.select(7, "model", 42, &keys).expect("应重新随机选取");
        assert!(restored == 1 || restored == 2);
    }

    #[test]
    fn expired_entry_is_pruned_before_reselection() {
        let cache = SessionStickyCache::new();
        let cache_key = StickyKey {
            channel_id: 7,
            model: "model".to_string(),
            session: 42,
        };
        cache.inner.lock().expect("缓存锁不应被污染").put(
            cache_key,
            StickyEntry {
                key_id: 1,
                expires_at: Instant::now() - Duration::from_secs(1),
            },
        );
        let keys = [key(1, 0, true), key(2, 100, true)];
        let selected = cache.select(7, "model", 42, &keys).expect("应重选");
        assert_eq!(selected, 2);
    }

    #[test]
    fn sticky_cache_evicts_least_recently_used_entry_at_capacity() {
        let cache = SessionStickyCache::with_capacity(2);
        let now = Instant::now();
        let oldest = StickyKey {
            channel_id: 7,
            model: "oldest".to_string(),
            session: 1,
        };
        let newest = StickyKey {
            channel_id: 7,
            model: "newest".to_string(),
            session: 2,
        };
        {
            let mut map = cache.inner.lock().expect("缓存锁不应被污染");
            map.put(
                oldest.clone(),
                StickyEntry {
                    key_id: 1,
                    expires_at: now + Duration::from_secs(60),
                },
            );
            map.put(
                newest.clone(),
                StickyEntry {
                    key_id: 1,
                    expires_at: now + Duration::from_secs(60),
                },
            );
        }

        let keys = [key(1, 1, true), key(2, 1, true)];
        cache
            .select(7, "inserted", 3, &keys)
            .expect("应能插入新粘性项");

        let map = cache.inner.lock().expect("缓存锁不应被污染");
        assert_eq!(map.len(), 2);
        assert!(!map.contains(&oldest));
        assert!(map.contains(&newest));
    }

    #[test]
    fn sticky_hit_refreshes_lru_recency() {
        let cache = SessionStickyCache::with_capacity(2);
        let keys = [key(1, 1, true), key(2, 1, true)];
        cache.select(7, "kept", 1, &keys).expect("应写入缓存");
        cache.select(7, "evicted", 2, &keys).expect("应写入缓存");
        cache.select(7, "kept", 1, &keys).expect("应命中缓存");
        cache.select(7, "inserted", 3, &keys).expect("应写入缓存");

        let map = cache.inner.lock().expect("缓存锁不应被污染");
        assert!(map.iter().any(|(key, _)| key.model == "kept"));
        assert!(!map.iter().any(|(key, _)| key.model == "evicted"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn explicit_cleanup_removes_only_expired_entries() {
        let now = Instant::now();
        let mut map = lru::LruCache::new(std::num::NonZeroUsize::new(2).expect("容量至少为 1"));
        map.put(
            StickyKey {
                channel_id: 7,
                model: "expired".to_string(),
                session: 1,
            },
            StickyEntry {
                key_id: 1,
                expires_at: now - Duration::from_secs(1),
            },
        );
        map.put(
            StickyKey {
                channel_id: 7,
                model: "live".to_string(),
                session: 2,
            },
            StickyEntry {
                key_id: 1,
                expires_at: now + Duration::from_secs(1),
            },
        );

        super::prune_sticky_expired(&mut map, now);

        assert_eq!(map.len(), 1);
        assert!(map.iter().all(|(key, _)| key.model == "live"));
    }
}
