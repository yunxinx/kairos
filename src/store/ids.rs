//! SQLite 代理主键生成。
//!
//! 运行时创建的资源共用一条进程内单调序列：高位是相对纪元的毫秒，低位是在同一
//! 毫秒内递增的序号。总宽度限制为 53 位，保证管理 API 的 JSON 数值能被浏览器精确
//! 还原。SQLite 的主键唯一约束仍是跨进程最终防线；项目当前是单进程写库，因此不
//! 引入额外的分布式节点配置。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;

/// 2024-01-01T00:00:00Z；41 位相对毫秒可使用约 69 年。
const ENTITY_ID_EPOCH_MILLIS: u64 = 1_704_067_200_000;
/// 每毫秒预留 12 位序号；同一毫秒超过 4096 次时序列自然向后递进，仍保持唯一。
const SEQUENCE_BITS: u32 = 12;
const MAX_SAFE_JSON_INTEGER: u64 = (1_u64 << 53) - 1;
const MAX_TIMESTAMP_MILLIS: u64 = MAX_SAFE_JSON_INTEGER >> SEQUENCE_BITS;

static LAST_ID: AtomicU64 = AtomicU64::new(0);

/// 从数据库现有主键校准进程内高水位。
///
/// 资源 id 由多个表共用同一条序列；启动时必须先跨表读取已持久化的最大值，
/// 才能在进程重启、系统时钟回拨或数据库快照恢复后继续避免主键复用。使用原子
/// 最大值更新而不是直接覆盖，允许同一进程内并发打开多个连接池而不降低高水位。
pub(super) async fn initialize(pool: &SqlitePool) -> Result<(), super::StoreError> {
    let max_id: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(id) FROM (
             SELECT id FROM smoke_probe
             UNION ALL SELECT id FROM request_log
             UNION ALL SELECT id FROM plans
             UNION ALL SELECT id FROM system_log
             UNION ALL SELECT id FROM users
         )",
    )
    .fetch_one(pool)
    .await
    .map_err(super::StoreError::Query)?;

    let max_id = max_id.unwrap_or(0);
    let max_id = u64::try_from(max_id).map_err(|_| super::StoreError::EntityIdExhausted)?;
    if max_id > MAX_SAFE_JSON_INTEGER {
        return Err(super::StoreError::EntityIdExhausted);
    }
    LAST_ID.fetch_max(max_id, Ordering::Relaxed);
    Ok(())
}

/// 生成正数、按成功发号顺序严格递增的时间有序 id。
pub(super) fn next_id() -> Result<i64, super::StoreError> {
    let unix_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| super::StoreError::EntityIdClockBeforeEpoch)
        .and_then(|duration| {
            u64::try_from(duration.as_millis()).map_err(|_| super::StoreError::EntityIdExhausted)
        })?;
    let timestamp_millis = unix_millis
        .checked_sub(ENTITY_ID_EPOCH_MILLIS)
        .ok_or(super::StoreError::EntityIdClockBeforeEpoch)?;
    if timestamp_millis > MAX_TIMESTAMP_MILLIS {
        return Err(super::StoreError::EntityIdExhausted);
    }
    let time_floor = timestamp_millis << SEQUENCE_BITS;
    let mut current = LAST_ID.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .map(|incremented| incremented.max(time_floor))
            .filter(|next| *next <= MAX_SAFE_JSON_INTEGER)
            .ok_or(super::StoreError::EntityIdExhausted)?;
        match LAST_ID.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return i64::try_from(next).map_err(|_| super::StoreError::EntityIdExhausted),
            Err(observed) => current = observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{LAST_ID, MAX_SAFE_JSON_INTEGER, next_id};

    #[test]
    fn generated_ids_are_positive_and_strictly_increasing() {
        let first = next_id().expect("应能生成 id");
        let second = next_id().expect("应能生成 id");
        let third = next_id().expect("应能生成 id");

        assert!(first > 0);
        assert!(first < second);
        assert!(second < third);
        assert!(third <= MAX_SAFE_JSON_INTEGER as i64);
    }

    #[test]
    fn rapid_generation_does_not_duplicate_ids() {
        let mut previous = next_id().expect("应能生成 id");
        for _ in 0..10_000 {
            let current = next_id().expect("应能生成 id");
            assert!(current > previous);
            previous = current;
        }
    }

    #[tokio::test]
    async fn reopening_database_advances_allocator_past_persisted_ids() {
        let directory = tempfile::tempdir().expect("应能创建临时目录");
        let path = directory.path().join("ids.db");
        let pool = crate::store::open(&path).await.expect("应能打开临时库");
        let persisted_id = LAST_ID
            .load(Ordering::Relaxed)
            .checked_add(10_000)
            .expect("测试 id 不应溢出");
        sqlx::query("INSERT INTO smoke_probe (id, note) VALUES (?, ?)")
            .bind(i64::try_from(persisted_id).expect("测试 id 应在 SQLite 范围内"))
            .bind("persisted")
            .execute(&pool)
            .await
            .expect("应能写入持久化 id");
        drop(pool);

        let reopened = crate::store::open(&path).await.expect("应能重新打开临时库");
        let generated = next_id().expect("应能生成 id");

        assert!(generated as u64 > persisted_id);
        drop(reopened);
    }
}
