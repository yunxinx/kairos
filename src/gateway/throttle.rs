//! 认证失败速率限制：按对端 IP 与凭证身份在滑动窗口内计数，超限返回 429。
//!
//! 令牌熵足够高，暴力猜中实际不可能；本限制补的是「认证失败无限重试」的缺位，
//! 避免协议面与管理面被无意义的 401 打满。成功请求不计入。次数与窗口来自运行时
//! 快照，设置写入后对新请求即时生效。
//!
//! 窗口用滑动语义：任意滚动跨度内的失败次数都不超过同一上限。固定窗口在边界
//! 处可容纳两轮满额突发（旧窗口尾部一轮、新窗口头部再一轮），对猜凭证的防护
//! 是缺口。两平面的限速同用滑动窗口但理由不同：本平面只在认证被拒时记账，
//! 事件稀疏，为每次失败保存时刻的代价可忽略；协议面请求限速面对高频请求，
//! 靠分片与按批清理把单次请求的记账成本摊销为常数。
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

/// 单个键的失败历史。
struct FailureHistory {
    /// 该键本次进入计数表的时间，用作驱逐队列中甄别同一键新旧占位的标记。
    created: Instant,
    /// 窗口内的失败时刻，按时间升序，队首最老；滑出窗口的时刻按需丢弃。
    attempts: VecDeque<Instant>,
}

struct FailureTable {
    entries: HashMap<FailureKey, FailureHistory>,
    /// 按键建立时间保存候选驱逐顺序。元素带建立时间，以便忽略同一键被驱逐后
    /// 重新占位留下的过期队列项；这样容量满时的驱逐成本按失败次数摊销为常数。
    order: VecDeque<(FailureKey, Instant)>,
    /// 当固定容量已满时，无法为新键建立独立窗口；该桶把这些失败的失败时刻
    /// 合并记录，使攻击者不能只靠轮换来源绕过限流。滑动窗口内达到同一上限后，
    /// 新的失败尝试会暂时整体阻断，避免容量压力下通过轮换来源耗尽认证资源。
    overflow: VecDeque<Instant>,
    /// 清理窗口的最小间隔，避免每次失败都遍历全部条目。
    last_prune: Instant,
}

impl AuthThrottle {
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FailureTable {
                entries: HashMap::new(),
                order: VecDeque::new(),
                overflow: VecDeque::new(),
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
        let now = Instant::now();
        let identity = identity.map(identity_key);
        if table.entries.len() >= MAX_FAILURE_KEYS
            && live_count(&mut table.overflow, now, window) >= max
        {
            return true;
        }
        let entries = &mut table.entries;
        [Some(FailureKey::Ip(ip)), identity.map(FailureKey::Identity)]
            .into_iter()
            .flatten()
            .any(|key| {
                entries
                    .get_mut(&key)
                    .is_some_and(|entry| live_count(&mut entry.attempts, now, window) >= max)
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
                Some(entry) => {
                    live_count(&mut entry.attempts, now, window);
                    entry.attempts.push_back(now);
                }
                None if capacity_available => {
                    table.entries.insert(key, FailureHistory::single(now));
                    table.order.push_back((key, now));
                }
                None => {
                    // 容量耗尽时仍保留一次失败信号，并驱逐最早建立的键，
                    // 让表可以继续服务新来源；溢出桶保证轮换来源不会无限绕过限制。
                    table.overflow.push_back(now);
                    while table.entries.len() >= MAX_FAILURE_KEYS {
                        let Some((oldest, created)) = table.order.pop_front() else {
                            break;
                        };
                        if table
                            .entries
                            .get(&oldest)
                            .is_some_and(|entry| entry.created == created)
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
                    table.entries.insert(key, FailureHistory::single(now));
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
                .retain(|_, entry| live_count(&mut entry.attempts, now, window) > 0);
            table.order.retain(|(key, created)| {
                table
                    .entries
                    .get(key)
                    .is_some_and(|entry| entry.created == *created)
            });
            live_count(&mut table.overflow, now, window);
            table.last_prune = now;
        }
    }
}

impl FailureHistory {
    /// 以一次失败建立新键的失败历史。
    fn single(at: Instant) -> Self {
        Self {
            created: at,
            attempts: VecDeque::from([at]),
        }
    }
}

/// 丢弃滑出窗口的失败时刻，返回窗口内的计数。时刻按时间升序，因此只需
/// 从队首连续弹出。
fn live_count(attempts: &mut VecDeque<Instant>, now: Instant, window: Duration) -> usize {
    while attempts
        .front()
        .is_some_and(|at| now.saturating_duration_since(*at) >= window)
    {
        attempts.pop_front();
    }
    attempts.len()
}

/// `0` 表示关闭；其余截到 `usize::MAX` 以便与窗口计数比较。
fn failure_cap(max_failures: u64) -> Option<usize> {
    if max_failures == 0 {
        return None;
    }
    Some(usize::try_from(max_failures).unwrap_or(usize::MAX))
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

    /// 固定窗口在边界处可容纳两轮满额失败：旧窗口尾部一轮、新窗口头部再一轮。
    /// 滑动窗口下，边界前的失败仍在窗口内，只能补足到上限，不得再放行一整轮。
    #[test]
    fn window_boundary_does_not_admit_double_burst() {
        let throttle = AuthThrottle::new();
        let ip: IpAddr = "127.0.0.1".parse().expect("应能解析 IP");
        let window = Duration::from_secs(1);
        throttle.record_failure(ip, None, 3, window);
        std::thread::sleep(Duration::from_millis(500));
        throttle.record_failure(ip, None, 3, window);
        throttle.record_failure(ip, None, 3, window);
        // 窗口内三次失败已满：跨过首次失败起算的固定窗口边界之前，持续阻断。
        assert!(throttle.is_blocked(ip, None, 3, window));
        // 越过固定窗口边界后，边界前的两次失败仍在滑动窗口内：
        // 只允许再补一次到上限，而不是再放行一整轮。
        std::thread::sleep(Duration::from_millis(550));
        assert!(!throttle.is_blocked(ip, None, 3, window));
        throttle.record_failure(ip, None, 3, window);
        assert!(throttle.is_blocked(ip, None, 3, window));
    }
}
