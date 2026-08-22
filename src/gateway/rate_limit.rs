//! 协议面请求速率限制：令牌桶与用户桶在滑动一分钟窗口内分别计数，超限 429。
//!
//! 令牌桶上限 = 该令牌 `rate_limit_rpm`，缺省用全局兜底（`0` 不限速，令牌显式
//! `0` 可覆盖全局）。用户桶上限 = 所属用户 `rate_limit_rpm` 的正数值，跨该用户
//! **所有令牌共享**——「用户 50 RPM」即名下全部令牌合计 50，多建令牌不能放大
//! 配额（CONTEXT.md：用户级 RPM 是硬性上限，令牌写 `0` 也压不过它）。单令牌
//! 视角下两桶取较小值生效，与旧版 `min(令牌, 用户)` 语义一致。
//!
//! 计数在进程内存，重启清零，多实例不共享。成功认证后的请求计入，认证失败走
//! [`super::throttle::AuthThrottle`]，两套计数不互通。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// 滑动窗口长度：RPM 按自然分钟计数。
const WINDOW: Duration = Duration::from_secs(60);

/// 限流桶的键：令牌维度与用户维度分开计数，类型不同天然不碰撞。
#[derive(Clone, PartialEq, Eq, Hash)]
enum RateKey {
    Token(String),
    User(i64),
}

/// 可在网关依赖间克隆共享的请求计数器。
#[derive(Clone)]
pub(super) struct RequestRateLimiter {
    inner: Arc<Mutex<HashMap<RateKey, VecDeque<Instant>>>>,
}

impl RequestRateLimiter {
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 一次请求同时占用令牌桶与（配置了正数上限时的）用户桶。
    ///
    /// 两桶容量判定都通过才同时入队；任一满员则**都不记账**——被拒的请求不应
    /// 消耗另一维度的配额（否则一次用户级 429 会白白吃掉令牌桶的窗口）。
    /// `token_limit == 0` 且用户未配上限即完全不限速。
    pub(super) fn try_acquire(
        &self,
        token_key: &str,
        token_limit: u64,
        user: Option<(i64, u64)>,
    ) -> Result<(), Duration> {
        // `limit > 0` 才是有效用户桶：`None`/`0` 都表示用户维度不限速。
        let user = user.filter(|&(_, limit)| limit > 0);
        if token_limit == 0 && user.is_none() {
            return Ok(());
        }
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        prune_expired(&mut map, now);
        // 先判容量再入队：借用两段分开，避免判定的可变借用跨过第二个 entry。
        if let Some((user_id, user_limit)) = &user
            && bucket_full(&map, &RateKey::User(*user_id), *user_limit, now)
        {
            return Err(retry_after(&map, &RateKey::User(*user_id), now));
        }
        let token_key = RateKey::Token(token_key.to_string());
        if token_limit > 0 && bucket_full(&map, &token_key, token_limit, now) {
            return Err(retry_after(&map, &token_key, now));
        }
        if let Some((user_id, _)) = &user {
            map.entry(RateKey::User(*user_id))
                .or_default()
                .push_back(now);
        }
        if token_limit > 0 {
            map.entry(token_key).or_default().push_back(now);
        }
        Ok(())
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
    use super::RequestRateLimiter;

    #[test]
    fn blocks_after_limit_from_same_token() {
        let limiter = RequestRateLimiter::new();
        assert!(limiter.try_acquire("sk-a", 2, None).is_ok());
        assert!(limiter.try_acquire("sk-a", 2, None).is_ok());
        assert!(limiter.try_acquire("sk-a", 2, None).is_err());
        assert!(limiter.try_acquire("sk-b", 2, None).is_ok());
    }

    #[test]
    fn zero_limit_disables_limiter() {
        let limiter = RequestRateLimiter::new();
        for _ in 0..8 {
            assert!(limiter.try_acquire("sk-a", 0, None).is_ok());
        }
    }

    #[test]
    fn zero_user_limit_is_no_user_bucket() {
        let limiter = RequestRateLimiter::new();
        for _ in 0..8 {
            assert!(limiter.try_acquire("sk-a", 0, Some((7, 0))).is_ok());
        }
    }

    /// 用户桶跨令牌共享：5 把令牌各打一次，用户上限 2 时第 3 次即被拒。
    #[test]
    fn user_bucket_is_shared_across_tokens() {
        let limiter = RequestRateLimiter::new();
        let user = Some((7, 2));
        assert!(limiter.try_acquire("sk-1", 0, user).is_ok());
        assert!(limiter.try_acquire("sk-2", 0, user).is_ok());
        // 第 3 次换了新令牌（令牌桶各自独立），仍被用户桶挡住。
        assert!(limiter.try_acquire("sk-3", 100, user).is_err());
        // 用户被拒不消耗令牌桶：sk-3 自己的桶仍是空的。
        assert!(limiter.try_acquire("sk-3", 100, None).is_ok());
    }

    /// 被拒请求不记账：令牌桶满导致的 429 不得消耗用户桶配额。
    #[test]
    fn rejected_request_does_not_consume_the_other_bucket() {
        let limiter = RequestRateLimiter::new();
        let user = Some((7, 10));
        // 令牌桶限 1：第 1 次过，第 2 次被令牌桶拒（不应进入用户桶）。
        assert!(limiter.try_acquire("sk-a", 1, user).is_ok());
        assert!(limiter.try_acquire("sk-a", 1, user).is_err());
        // 用户桶只记了 1 次：其余 9 次配额仍可用（换令牌打）。
        for _ in 0..9 {
            assert!(limiter.try_acquire("sk-b", 0, user).is_ok());
        }
        assert!(limiter.try_acquire("sk-c", 0, user).is_err());
    }

    /// 单令牌视角取两桶较小值：令牌 30、用户 50 → 第 31 次被拒（令牌桶先满）。
    #[test]
    fn single_token_takes_the_smaller_limit() {
        let limiter = RequestRateLimiter::new();
        let user = Some((9, 50));
        for _ in 0..30 {
            assert!(limiter.try_acquire("sk-a", 30, user).is_ok());
        }
        assert!(limiter.try_acquire("sk-a", 30, user).is_err());
    }
}
