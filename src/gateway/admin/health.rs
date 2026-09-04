//! 渠道健康视图：进程内冷却状态的只读展示。
//!
//! 冷却表由协议面在失败路径记账、成功路径清零；本端点只读当前冷却中的渠道，
//! 供运营判断渠道故障与自愈进度。状态不落库，进程重启即清空。

use std::collections::HashMap;
use std::time::Instant;

use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;

use super::AdminDeps;

pub(super) fn routes() -> Router<AdminDeps> {
    Router::new().route("/channels/health", get(channel_health))
}

/// 冷却中渠道的一行展示。
#[derive(Debug, Serialize)]
struct ChannelCooldownView {
    channel_id: i64,
    /// 渠道名；取自当前快照，已删除渠道的残留冷却记录不展示。
    channel: String,
    /// 冷却到期时刻（unix 毫秒）。由单调时钟剩余量折算成墙钟，仅供展示。
    cooldown_until: i64,
    /// 触发冷却时的连续可重试失败计数（上游 402/403 即时冷却时为 0）。
    consecutive_failures: u32,
}

/// 健康端点响应：当前冷却中的渠道清单，顺序与渠道清单一致。
#[derive(Debug, Serialize)]
struct ChannelHealthView {
    channels: Vec<ChannelCooldownView>,
}

/// `GET /api/channels/health`：返回冷却中渠道与到期时刻。
async fn channel_health(State(deps): State<AdminDeps>) -> Json<ChannelHealthView> {
    let snapshot = deps.snapshot.read().await.clone();
    let cooling: HashMap<i64, (Instant, u32)> = deps
        .channel_cooldowns
        .cooling_channels(Instant::now())
        .into_iter()
        .map(|(id, until, failures)| (id, (until, failures)))
        .collect();
    let channels = snapshot
        .channels
        .iter()
        .filter_map(|record| {
            let (until, consecutive_failures) = cooling.get(&record.id).copied()?;
            Some(ChannelCooldownView {
                channel_id: record.id,
                channel: record.channel.name.clone(),
                cooldown_until: cooldown_until_unix_millis(until),
                consecutive_failures,
            })
        })
        .collect();
    Json(ChannelHealthView { channels })
}

/// 把单调时刻折算为墙钟 unix 毫秒：以当前时刻为锚点加剩余冷却量。
/// 冷却时长以分钟计，剩余毫秒远小于 `i64` 值域，加法不会溢出。
fn cooldown_until_unix_millis(until: Instant) -> i64 {
    let remaining_ms = until
        .checked_duration_since(Instant::now())
        .unwrap_or_default()
        .as_millis() as i64;
    crate::gateway::unix_millis() + remaining_ms
}
