//! 认证失败速率限制：按对端 IP 在滑动窗口内计数，超限返回 429。
//!
//! 令牌熵足够高，暴力猜中实际不可能；本限制补的是「认证失败无限重试」的缺位，
//! 避免协议面与管理面被无意义的 401 打满。成功请求不计入。次数与窗口来自运行时
//! 快照，设置写入后对新请求即时生效。
//!
//! 对端 IP 取自 `ConnectInfo`。部署在反代之后时所有下游会共享代理 IP，一个
//! 攻击者的失败可能误伤全体；本实现不读取 `X-Forwarded-For`（没有可信代理模型）。
//! 应直接暴露协议监听，或由反代自己做认证失败限流。
//!
//! 协议面与管理面各自持有独立实例：两平面凭证与监听隔离，失败计数不互通。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// 可在协议面与管理面之间克隆共享的认证失败计数器。
#[derive(Clone)]
pub(super) struct AuthThrottle {
    inner: Arc<Mutex<HashMap<IpAddr, FailureWindow>>>,
}

struct FailureWindow {
    count: u32,
    start: Instant,
}

impl AuthThrottle {
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 该 IP 是否已达到窗口上限（调用方应返回 429，不再尝试认证）。
    ///
    /// `max_failures == 0` 表示关闭限流。
    pub(super) fn is_blocked(&self, ip: IpAddr, max_failures: u64, window: Duration) -> bool {
        let Some(max) = failure_cap(max_failures) else {
            return false;
        };
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        prune_expired(&mut map, window);
        map.get(&ip)
            .is_some_and(|entry| entry.start.elapsed() < window && entry.count >= max)
    }

    /// 记录一次认证失败。限流关闭时不记账。
    pub(super) fn record_failure(&self, ip: IpAddr, max_failures: u64, window: Duration) {
        if failure_cap(max_failures).is_none() {
            return;
        }
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        prune_expired(&mut map, window);
        match map.get_mut(&ip) {
            Some(entry) if entry.start.elapsed() < window => {
                entry.count = entry.count.saturating_add(1);
            }
            Some(entry) => {
                entry.count = 1;
                entry.start = Instant::now();
            }
            None => {
                map.insert(
                    ip,
                    FailureWindow {
                        count: 1,
                        start: Instant::now(),
                    },
                );
            }
        }
    }
}

/// `0` 表示关闭；其余截到 `u32::MAX` 以便与窗口计数比较。
fn failure_cap(max_failures: u64) -> Option<u32> {
    if max_failures == 0 {
        return None;
    }
    Some(u32::try_from(max_failures).unwrap_or(u32::MAX))
}

fn prune_expired(map: &mut HashMap<IpAddr, FailureWindow>, window: Duration) {
    map.retain(|_, entry| entry.start.elapsed() < window);
}

#[cfg(test)]
mod tests {
    use super::AuthThrottle;
    use std::net::IpAddr;
    use std::time::Duration;

    #[test]
    fn blocks_after_max_failures_from_same_ip() {
        let throttle = AuthThrottle::new();
        let ip: IpAddr = "127.0.0.1".parse().expect("应能解析 IP");
        let window = Duration::from_secs(60);
        assert!(!throttle.is_blocked(ip, 30, window));
        for _ in 0..30 {
            throttle.record_failure(ip, 30, window);
        }
        assert!(throttle.is_blocked(ip, 30, window));
        let other: IpAddr = "10.0.0.2".parse().expect("应能解析 IP");
        assert!(!throttle.is_blocked(other, 30, window));
    }

    #[test]
    fn zero_max_failures_disables_throttle() {
        let throttle = AuthThrottle::new();
        let ip: IpAddr = "127.0.0.1".parse().expect("应能解析 IP");
        let window = Duration::from_secs(60);
        for _ in 0..50 {
            throttle.record_failure(ip, 0, window);
        }
        assert!(!throttle.is_blocked(ip, 0, window));
    }
}
