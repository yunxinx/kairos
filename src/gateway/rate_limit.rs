//! 协议面请求速率限制：按令牌在滑动一分钟窗口内计数，超限返回 429。
//!
//! 生效上限 = 令牌 `rate_limit_rpm`，缺省时用设置里的全局兜底。`0` 表示不限速
//! （令牌显式 `0` 可超过全局上限；全局 `0` 表示未设兜底）。成功认证后的请求计入，
//! 认证失败走 [`super::throttle::AuthThrottle`]，两套计数不互通。
//!
//! 计数在进程内存，重启清零，多实例不共享。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// 滑动窗口长度：RPM 按自然分钟计数。
const WINDOW: Duration = Duration::from_secs(60);

/// 可在网关依赖间克隆共享的令牌请求计数器。
#[derive(Clone)]
pub(super) struct RequestRateLimiter {
    inner: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
}

impl RequestRateLimiter {
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 尝试占用一次配额。`limit_rpm == 0` 表示不限速。
    ///
    /// 允许时返回 `Ok(())`；超限返回建议的 `Retry-After`。
    pub(super) fn try_acquire(&self, token_key: &str, limit_rpm: u64) -> Result<(), Duration> {
        if limit_rpm == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        prune_expired(&mut map, now);
        let queue = map.entry(token_key.to_string()).or_default();
        if (queue.len() as u64) >= limit_rpm {
            let oldest = queue.front().copied().unwrap_or(now);
            let retry_after = WINDOW
                .saturating_sub(now.saturating_duration_since(oldest))
                .max(Duration::from_secs(1));
            return Err(retry_after);
        }
        queue.push_back(now);
        Ok(())
    }
}

/// 令牌未写 RPM 时用全局兜底；令牌写出的值（含 `0`）覆盖全局。
pub(super) fn effective_rate_limit_rpm(token_rpm: Option<u64>, global_rpm: u64) -> u64 {
    token_rpm.unwrap_or(global_rpm)
}

fn prune_expired(map: &mut HashMap<String, VecDeque<Instant>>, now: Instant) {
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
    use super::{RequestRateLimiter, effective_rate_limit_rpm};

    #[test]
    fn blocks_after_limit_from_same_token() {
        let limiter = RequestRateLimiter::new();
        assert!(limiter.try_acquire("sk-a", 2).is_ok());
        assert!(limiter.try_acquire("sk-a", 2).is_ok());
        assert!(limiter.try_acquire("sk-a", 2).is_err());
        assert!(limiter.try_acquire("sk-b", 2).is_ok());
    }

    #[test]
    fn zero_limit_disables_limiter() {
        let limiter = RequestRateLimiter::new();
        for _ in 0..8 {
            assert!(limiter.try_acquire("sk-a", 0).is_ok());
        }
    }

    #[test]
    fn token_override_replaces_global() {
        assert_eq!(effective_rate_limit_rpm(None, 60), 60);
        assert_eq!(effective_rate_limit_rpm(Some(120), 60), 120);
        assert_eq!(effective_rate_limit_rpm(Some(0), 60), 0);
    }
}
