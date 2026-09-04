//! 认证失败速率限制：按对端 IP 与凭证身份在滑动窗口内计数，超限返回 429。
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

use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// 可在协议面与管理面之间克隆共享的认证失败计数器。
#[derive(Clone)]
pub(super) struct AuthThrottle {
    inner: Arc<Mutex<FailureTable>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum FailureKey {
    Ip(IpAddr),
    Identity([u8; 32]),
}

struct FailureWindow {
    count: u32,
    start: Instant,
}

struct FailureTable {
    entries: HashMap<FailureKey, FailureWindow>,
    /// 按窗口开始时间保存候选驱逐顺序。元素带时间戳，以便忽略同一键旧窗口
    /// 留下的过期队列项；这样容量满时的驱逐成本按失败次数摊销为常数。
    order: VecDeque<(FailureKey, Instant)>,
    /// 当固定容量已满时，无法为新键建立独立窗口；该桶把这些失败合并计数，
    /// 使攻击者不能只靠轮换来源绕过限流。达到同一窗口上限后，新的失败尝试会
    /// 暂时整体阻断，避免容量压力下通过轮换来源耗尽认证资源。
    overflow: FailureWindow,
    /// 清理窗口的最小间隔，避免每次失败都遍历全部条目。
    last_prune: Instant,
}

impl AuthThrottle {
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FailureTable {
                entries: HashMap::new(),
                order: VecDeque::new(),
                overflow: FailureWindow {
                    count: 0,
                    start: Instant::now(),
                },
                last_prune: Instant::now(),
            })),
        }
    }

    /// 该 IP 是否已达到窗口上限（调用方应返回 429，不再尝试认证）。
    ///
    /// `max_failures == 0` 表示关闭限流。
    pub(super) fn is_blocked(
        &self,
        ip: IpAddr,
        identity: Option<&str>,
        max_failures: u64,
        window: Duration,
    ) -> bool {
        let Some(max) = failure_cap(max_failures) else {
            return false;
        };
        let mut table = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        Self::maybe_prune(&mut table, window);
        let identity = identity.map(identity_key);
        if table.entries.len() >= MAX_FAILURE_KEYS && table.overflow.count >= max {
            return true;
        }
        [Some(FailureKey::Ip(ip)), identity.map(FailureKey::Identity)]
            .into_iter()
            .flatten()
            .any(|key| {
                table
                    .entries
                    .get(&key)
                    .is_some_and(|entry| entry.count >= max)
            })
    }

    /// 记录一次认证失败。限流关闭时不记账。
    pub(super) fn record_failure(
        &self,
        ip: IpAddr,
        identity: Option<&str>,
        max_failures: u64,
        window: Duration,
    ) {
        if failure_cap(max_failures).is_none() {
            return;
        }
        let mut table = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        Self::maybe_prune(&mut table, window);
        let now = Instant::now();
        let keys = [
            Some(FailureKey::Ip(ip)),
            identity.map(identity_key).map(FailureKey::Identity),
        ];
        for key in keys.into_iter().flatten() {
            let capacity_available = table.entries.len() < MAX_FAILURE_KEYS;
            match table.entries.get_mut(&key) {
                Some(entry) if entry.start.elapsed() < window => {
                    entry.count = entry.count.saturating_add(1);
                }
                Some(entry) => {
                    entry.count = 1;
                    entry.start = now;
                    table.order.push_back((key, now));
                }
                None if capacity_available => {
                    table.entries.insert(
                        key,
                        FailureWindow {
                            count: 1,
                            start: now,
                        },
                    );
                    table.order.push_back((key, now));
                }
                None => {
                    // 容量耗尽时仍保留一次失败信号，并驱逐最早建立的窗口，
                    // 让表可以继续服务新来源；溢出桶保证轮换来源不会无限绕过限制。
                    table.overflow.count = table.overflow.count.saturating_add(1);
                    while table.entries.len() >= MAX_FAILURE_KEYS {
                        let Some((oldest, started)) = table.order.pop_front() else {
                            break;
                        };
                        if table
                            .entries
                            .get(&oldest)
                            .is_some_and(|entry| entry.start == started)
                        {
                            table.entries.remove(&oldest);
                            break;
                        }
                    }
                    if table.entries.len() >= MAX_FAILURE_KEYS {
                        // 队列与表应始终同步；这里保留兜底以保证固定容量不被突破。
                        if let Some(oldest) = table.entries.keys().next().copied() {
                            table.entries.remove(&oldest);
                        }
                    }
                    table.entries.insert(
                        key,
                        FailureWindow {
                            count: 1,
                            start: now,
                        },
                    );
                    table.order.push_back((key, now));
                }
            }
        }
    }

    fn maybe_prune(table: &mut FailureTable, window: Duration) {
        const PRUNE_INTERVAL: Duration = Duration::from_secs(1);
        let now = Instant::now();
        if now.duration_since(table.last_prune) >= PRUNE_INTERVAL {
            table
                .entries
                .retain(|_, entry| now.duration_since(entry.start) < window);
            table.order.retain(|(key, started)| {
                table
                    .entries
                    .get(key)
                    .is_some_and(|entry| entry.start == *started)
            });
            if now.duration_since(table.overflow.start) >= window {
                table.overflow = FailureWindow {
                    count: 0,
                    start: now,
                };
            }
            table.last_prune = now;
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

const MAX_FAILURE_KEYS: usize = 8_192;

fn identity_key(identity: &str) -> [u8; 32] {
    Sha256::digest(identity.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::{AuthThrottle, MAX_FAILURE_KEYS};
    use std::net::{IpAddr, Ipv6Addr};
    use std::time::Duration;

    #[test]
    fn blocks_after_max_failures_from_same_ip() {
        let throttle = AuthThrottle::new();
        let ip: IpAddr = "127.0.0.1".parse().expect("应能解析 IP");
        let window = Duration::from_secs(60);
        assert!(!throttle.is_blocked(ip, None, 30, window));
        for _ in 0..30 {
            throttle.record_failure(ip, None, 30, window);
        }
        assert!(throttle.is_blocked(ip, None, 30, window));
        let other: IpAddr = "10.0.0.2".parse().expect("应能解析 IP");
        assert!(!throttle.is_blocked(other, None, 30, window));
    }

    #[test]
    fn zero_max_failures_disables_throttle() {
        let throttle = AuthThrottle::new();
        let ip: IpAddr = "127.0.0.1".parse().expect("应能解析 IP");
        let window = Duration::from_secs(60);
        for _ in 0..50 {
            throttle.record_failure(ip, None, 0, window);
        }
        assert!(!throttle.is_blocked(ip, None, 0, window));
    }

    #[test]
    fn identity_window_applies_across_source_addresses() {
        let throttle = AuthThrottle::new();
        let first_ip: IpAddr = "127.0.0.1".parse().expect("应能解析 IP");
        let second_ip: IpAddr = "127.0.0.2".parse().expect("应能解析 IP");
        let window = Duration::from_secs(60);
        throttle.record_failure(first_ip, Some("candidate"), 2, window);
        throttle.record_failure(second_ip, Some("candidate"), 2, window);
        assert!(throttle.is_blocked(first_ip, Some("candidate"), 2, window));
        assert!(throttle.is_blocked(second_ip, Some("candidate"), 2, window));
    }

    #[test]
    fn full_table_uses_overflow_bucket_instead_of_failing_open() {
        let throttle = AuthThrottle::new();
        let window = Duration::from_secs(60);

        for index in 0..MAX_FAILURE_KEYS {
            let ip = IpAddr::V6(Ipv6Addr::from(index as u128 + 1));
            throttle.record_failure(ip, None, 3, window);
        }

        let first_overflow = IpAddr::V6(Ipv6Addr::from(0x10_000u128));
        let second_overflow = IpAddr::V6(Ipv6Addr::from(0x10_001u128));
        let third_overflow = IpAddr::V6(Ipv6Addr::from(0x10_002u128));
        for ip in [first_overflow, second_overflow, third_overflow] {
            assert!(!throttle.is_blocked(ip, None, 3, window));
            throttle.record_failure(ip, None, 3, window);
        }

        // 溢出桶达到窗口上限后，新的来源不能借助驱逐继续触发认证计算。
        let fresh = IpAddr::V6(Ipv6Addr::from(0x10_003u128));
        assert!(throttle.is_blocked(fresh, None, 3, window));
        assert!(throttle.is_blocked(first_overflow, None, 3, window));
    }

    #[test]
    fn eviction_keeps_failure_table_bounded() {
        let throttle = AuthThrottle::new();
        let window = Duration::from_secs(60);
        for index in 0..(MAX_FAILURE_KEYS + 128) {
            let ip = IpAddr::V6(Ipv6Addr::from(index as u128 + 1));
            throttle.record_failure(ip, None, u64::MAX, window);
        }

        let table = throttle.inner.lock().unwrap();
        assert_eq!(table.entries.len(), MAX_FAILURE_KEYS);
    }
}
