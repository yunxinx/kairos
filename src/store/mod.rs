//! SQLite 存储层门面：连接治理、版本化迁移与跨领域查询辅助。
//!
//! 领域实现拆在各子模块：请求日志（[`request_log`]：落库、待结算队列、
//! 查询与统计）、结算（[`settlement`]：计费预留、用户钱包与令牌累计结算）、
//! 资源（[`resources`]）、用户（[`users`]）、套餐（[`plans`]）、价格目录
//! （[`catalog`]）与系统日志（`system_log`）。本文件保留打开库与迁移、
//! 库文件权限收敛、存量明文 key 指纹换算、WAL checkpoint，以及分页与
//! WHERE 拼接等共享查询辅助。金额一律整数 micro-USD。

pub mod balance_operations;
pub mod catalog;
pub mod channel_keys;
mod ids;
pub mod plans;
pub mod request_log;
pub mod resources;
pub mod settlement;
mod system_log;
pub mod users;

pub use system_log::{
    Actor, SystemLog, SystemLogEvent, SystemLogList, SystemLogQuery, SystemLogSortBy,
    insert_system_log, purge_system_logs_before, query_system_log_page, record_audit,
    record_audit_detached, record_system_error, record_system_warn,
};

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::{
    Connection, Row, SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
};
use thiserror::Error;

/// 存储层错误，向上抛给应用边界。
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("连接 SQLite 失败: {0}")]
    Connect(sqlx::Error),
    #[error("执行迁移失败: {0}")]
    Migrate(sqlx::migrate::MigrateError),
    #[error("数据库操作失败: {0}")]
    Query(sqlx::Error),
    #[error("请求日志持久化超过请求截止时间")]
    PersistenceTimeout,
    #[error("读取数据库文件元数据 {path} 失败: {source}")]
    FileMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("设置数据库文件 {path} 权限失败: {source}")]
    SetPermissions {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "WAL checkpoint 被活动读事务阻塞（WAL 帧 {log_frames}，已 checkpoint {checkpointed_frames}）"
    )]
    WalCheckpointBusy {
        log_frames: i64,
        checkpointed_frames: i64,
    },
    #[error("找不到令牌 {0} 所属用户的余额")]
    MissingToken(String),
    #[error("资源数据非法: {0}")]
    InvalidResource(String),
    #[error("管理主体无权操作该记录")]
    PermissionDenied,
    #[error("不能删除或降级最后一个 root")]
    LastRootProtected,
    #[error("用户 {0} 不存在")]
    UserNotFound(i64),
    #[error("用户 {0} 缺少钱包")]
    MissingWallet(i64),
    #[error("邮箱已被使用")]
    EmailTaken,
    #[error("密码处理失败")]
    PasswordHash,
    #[error("系统时钟早于资源 id 纪元")]
    EntityIdClockBeforeEpoch,
    #[error("资源 id 空间已耗尽")]
    EntityIdExhausted,
    #[error("用户余额不足以预留本次请求费用")]
    InsufficientFunds,
    #[error("令牌累计结算上限不足以预留本次请求费用")]
    TokenLimitExceeded,
    #[error("请求费用预留与已有请求身份不一致")]
    ReservationConflict,
}

/// 写锁等待上限：与 sqlx-sqlite 缺省一致，此处显式声明意图——SQLite 单写者下
/// 请求路径结算/日志与管理面写并发时排队等待，而不是立即失败。
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// 打开 SQLite 连接池并在事务内按序应用编号迁移。
///
/// 缺库文件时自动创建（`create_if_missing`），迁移脚本内建在 `migrations/`。
/// 连接选项统一治理 SQLite 的坏默认值：外键强制、写锁排队、WAL 日志模式。
/// 连接建立后立即把库文件与既有边车收紧为 owner-only 权限。
pub async fn open(path: &Path) -> Result<SqlitePool, StoreError> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        // WAL：读写互不阻塞；提交只追加写 WAL，免去 DELETE 模式每次提交把被
        // 修改页原像复制进回滚日志的开销。WAL 会持久记录在库文件头，后续
        // 打开自动沿用。
        .journal_mode(SqliteJournalMode::Wal)
        // WAL 下 NORMAL 只在检查点前同步，崩溃不损坏库文件（掉电可能丢失上
        // 次检查点以来已提交的事务），官方推荐档位；FULL 每次提交都同步，
        // 无必要。
        .synchronous(SqliteSynchronous::Normal);

    let pool = SqlitePool::connect_with(options)
        .await
        .map_err(StoreError::Connect)?;

    tighten_database_file_permissions(path).await?;

    sqlx::migrate!()
        .run(&pool)
        .await
        .map_err(StoreError::Migrate)?;

    ids::initialize(&pool).await?;
    hash_legacy_token_key_plaintext(path).await?;

    Ok(pool)
}

/// 令牌 key 的库内存储形态：SHA-256 的十六进制指纹。
///
/// 明文只出现在两处边界——签发时的创建响应，与入站认证头。库内
/// （tokens、token_balance、request_log、request_log_outbox、
/// billing_reservations）一律只存指纹，WAL/备份/任何 DB 读取都还原不出
/// 可用凭证。换算确定性且无盐：认证侧对呈现的明文做同一换算后查快照。
pub fn token_key_fingerprint(token_key: &str) -> String {
    use sha2::{Digest, Sha256};

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest: [u8; 32] = Sha256::digest(token_key.as_bytes()).into();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 存量令牌 key 明文换算的完成标记（settings 表）；写入即代表库内已全量指纹化。
const SETTING_TOKEN_KEYS_HASHED: &str = "token_keys_hashed";

/// 把存量库中的令牌 key 明文原地换算为指纹。
///
/// 覆盖五张表：tokens、token_balance、request_log、request_log_outbox 与
/// billing_reservations，其中 outbox 元数据与预留恢复元数据里的 JSON 载荷
/// 同步重写。换算不可逆（明文从此只存在于创建响应与调用方），全部动作与
/// 完成标记在同一写事务中提交，中断即整体回滚、重启重跑；已有标记时直接
/// 返回。新库空表扫描为无操作。逐行 JSON 解析失败时保留原行并告警——那本
/// 就是损坏数据，不能借换算之手伪造。
///
/// 子表 `token_balance` 外键引用 `tokens(token_key)` 且未声明 DEFERRABLE：
/// 立即外键下「父行改键前子表先改、父行改键时子表仍指旧值」都会被拒。换算
/// 走关闭外键的专用连接（启动路径独占，无并发写入者），提交前以
/// `pragma_foreign_key_check` 复核引用一致性，不一致即回滚报错；应用连接池
/// 的外键强制不受影响。
pub(crate) async fn hash_legacy_token_key_plaintext(path: &Path) -> Result<(), StoreError> {
    // 换算要原位改写两类 JSON 载荷：outbox 队列元数据与预留恢复元数据。
    use request_log::PendingRequestLog;
    use settlement::BillingAttemptRecovery;

    let options = SqliteConnectOptions::new()
        .filename(path)
        .foreign_keys(false);
    let mut conn = SqliteConnection::connect_with(&options)
        .await
        .map_err(StoreError::Connect)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(StoreError::Query)?;

    // 完成标记与换算同事务读写：标记在场即代表此前已完成，直接返回。
    let flagged: Option<i64> = sqlx::query_scalar("SELECT 1 FROM settings WHERE setting_key = ?")
        .bind(SETTING_TOKEN_KEYS_HASHED)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
    if flagged.is_some() {
        tx.rollback().await.map_err(StoreError::Query)?;
        return Ok(());
    }

    // 先读后写：JSON 重写需要行上的明文 token_key 作为换算来源，与列更新
    // 之前完成读取。
    let outbox_rows: Vec<(i64, String, Vec<u8>)> =
        sqlx::query("SELECT id, token_key, metadata FROM request_log_outbox")
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::Query)?
            .into_iter()
            .map(|row| {
                Ok((
                    row.try_get::<i64, _>("id").map_err(StoreError::Query)?,
                    row.try_get::<String, _>("token_key")
                        .map_err(StoreError::Query)?,
                    row.try_get::<Vec<u8>, _>("metadata")
                        .map_err(StoreError::Query)?,
                ))
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
    for (outbox_id, token_key, metadata) in outbox_rows {
        let Ok(mut pending) = serde_json::from_slice::<PendingRequestLog>(&metadata) else {
            tracing::warn!(outbox_id, "存量 outbox 元数据无法解析，指纹换算跳过该行");
            continue;
        };
        pending.log.token_key = token_key_fingerprint(&token_key);
        let encoded = serde_json::to_vec(&pending).map_err(|err| {
            StoreError::InvalidResource(format!("outbox 元数据重编码失败: {err}"))
        })?;
        sqlx::query("UPDATE request_log_outbox SET metadata = ? WHERE id = ?")
            .bind(encoded)
            .bind(outbox_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Query)?;
    }

    let reservation_rows: Vec<(String, Vec<u8>)> =
        sqlx::query("SELECT token_key, recovery_metadata FROM billing_reservations")
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::Query)?
            .into_iter()
            .map(|row| {
                Ok((
                    row.try_get::<String, _>("token_key")
                        .map_err(StoreError::Query)?,
                    row.try_get::<Vec<u8>, _>("recovery_metadata")
                        .map_err(StoreError::Query)?,
                ))
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
    for (token_key, metadata) in reservation_rows {
        let Ok(mut recovery) = serde_json::from_slice::<BillingAttemptRecovery>(&metadata) else {
            // 告警只打指纹：此处 token_key 还是库中明文遗留 key，直接输出会把
            // 可用凭证写进进程日志。
            tracing::warn!(
                token_key = %token_key_fingerprint(&token_key),
                "存量预留恢复元数据无法解析，指纹换算跳过该行"
            );
            continue;
        };
        if let Some(result) = recovery.result.as_deref_mut() {
            result.token_key = token_key_fingerprint(&token_key);
        }
        let encoded = serde_json::to_vec(&recovery)
            .map_err(|err| StoreError::InvalidResource(format!("恢复元数据重编码失败: {err}")))?;
        sqlx::query("UPDATE billing_reservations SET recovery_metadata = ? WHERE token_key = ?")
            .bind(encoded)
            .bind(&token_key)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Query)?;
    }

    let plain_keys: Vec<String> = sqlx::query("SELECT token_key FROM tokens")
        .fetch_all(&mut *tx)
        .await
        .map_err(StoreError::Query)?
        .into_iter()
        .map(|row| {
            row.try_get::<String, _>("token_key")
                .map_err(StoreError::Query)
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    // 子表先改、父表后改：关闭外键后顺序不再是正确性依据，保留既有次序
    // 仅利于阅读（对账表在凭证表之前）。
    for plain in &plain_keys {
        let fingerprint = token_key_fingerprint(plain);
        for table in [
            "UPDATE token_balance SET token_key = ? WHERE token_key = ?",
            "UPDATE request_log SET token_key = ? WHERE token_key = ?",
            "UPDATE request_log_outbox SET token_key = ? WHERE token_key = ?",
            "UPDATE billing_reservations SET token_key = ? WHERE token_key = ?",
        ] {
            sqlx::query(table)
                .bind(&fingerprint)
                .bind(plain)
                .execute(&mut *tx)
                .await
                .map_err(StoreError::Query)?;
        }
        sqlx::query("UPDATE tokens SET token_key = ? WHERE token_key = ?")
            .bind(&fingerprint)
            .bind(plain)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Query)?;
    }

    resources::set_setting(
        &mut tx,
        SETTING_TOKEN_KEYS_HASHED,
        &serde_json::Value::Bool(true),
    )
    .await?;

    // 提交前复核引用一致性：关外键写入不触发约束，损坏必须在此拦下而不是
    // 落库后由运行期外键错误暴露。
    let violations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
    if violations > 0 {
        tx.rollback().await.map_err(StoreError::Query)?;
        return Err(StoreError::InvalidResource(format!(
            "令牌 key 指纹换算后仍有 {violations} 处悬挂引用，已回滚"
        )));
    }
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(())
}

/// 把数据库文件与既有边车（WAL/SHM）的权限收紧为 owner-only（0o600）。
///
/// 库文件承载渠道密钥、令牌 key 与对话 body，不能按进程 umask 宽松落盘。
/// SQLite 创建 `-wal`/`-journal`/`-shm` 边车时按库文件当时的权限原样派生
/// （不受 umask 影响），因此只需在首次写事务前归一库文件；连接阶段可能已
/// 产生的边车在此一并归一，其后新建的自然继承 0o600。边车尚未创建是正常
/// 状态（首次写事务才落盘），缺席时跳过。
async fn tighten_database_file_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    let mut wal_path = path.to_path_buf();
    wal_path.as_mut_os_string().push("-wal");
    let mut shm_path = path.to_path_buf();
    shm_path.as_mut_os_string().push("-shm");
    let targets = [path.to_path_buf(), wal_path, shm_path];
    tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
        for target in targets {
            match std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(StoreError::SetPermissions {
                        path: target,
                        source,
                    });
                }
            }
        }
        Ok(())
    })
    .await
    .map_err(|err| StoreError::SetPermissions {
        path: path.to_path_buf(),
        source: std::io::Error::other(err.to_string()),
    })?
}

/// 写一条冒烟记录，返回时间有序 id。
pub async fn insert_smoke(pool: &SqlitePool, note: &str) -> Result<i64, StoreError> {
    let id = ids::next_id()?;
    sqlx::query("INSERT INTO smoke_probe (id, note) VALUES (?, ?)")
        .bind(id)
        .bind(note)
        .execute(pool)
        .await
        .map_err(StoreError::Query)?;

    Ok(id)
}

/// 清理后的收尾：尝试把 WAL 全量并入主库并将边车截断为零。
///
/// 批量删除的多批独立提交会让 WAL 持续增长，不收尾的话「删完日志磁盘占用
/// 反而更大」会成为常态观感。TRUNCATE 模式会等待在途读事务（受
/// busy_timeout 约束）。SQLite 会把读事务阻塞放在结果行的 `busy` 列中返回，
/// 而不是报 SQL 错；本函数显式检查该列，失败由调用方降级处理。主库文件本身
/// 不缩小（空闲页复用），由调用方的契约文案说明。
pub async fn checkpoint_wal_truncate(pool: &SqlitePool) -> Result<(), StoreError> {
    let row = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(pool)
        .await
        .map_err(StoreError::Query)?;
    let busy: i64 = row.try_get(0).map_err(StoreError::Query)?;
    let log_frames: i64 = row.try_get(1).map_err(StoreError::Query)?;
    let checkpointed_frames: i64 = row.try_get(2).map_err(StoreError::Query)?;
    if busy != 0 {
        return Err(StoreError::WalCheckpointBusy {
            log_frames,
            checkpointed_frames,
        });
    }
    Ok(())
}

/// 列表排序方向；缺省新→旧。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    Asc,
    #[default]
    Desc,
}

impl SortDir {
    /// SQL `ASC` / `DESC` 片段（含前导空格）。
    pub(crate) fn sql(self) -> &'static str {
        match self {
            Self::Asc => " ASC",
            Self::Desc => " DESC",
        }
    }
}

/// SQLite 聚合整数转计数；负值视为 0。
pub(crate) fn as_count(value: i64) -> u64 {
    value.max(0) as u64
}

/// 页码从 1 起，每页条数夹到 `[1, 200]`。请求日志与系统日志共用。
pub(crate) fn clamp_page(page: u64, page_size: u64) -> (u64, u64) {
    (page.max(1), page_size.clamp(1, 200))
}

/// 向查询拼接一个条件：首个条件以 `WHERE` 开头，其余以 `AND` 连接。
pub(crate) fn push_where_cond(
    qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>,
    first: &mut bool,
    condition: &str,
) {
    qb.push(if *first { " WHERE " } else { " AND " });
    *first = false;
    qb.push(condition);
}

/// 可选时间窗：`created_at >= from` 与 `created_at <= to`。
pub(crate) fn push_created_at_range(
    qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>,
    first: &mut bool,
    from: Option<i64>,
    to: Option<i64>,
) {
    if let Some(from) = from {
        push_where_cond(qb, first, "created_at >= ");
        qb.push_bind(from);
    }
    if let Some(to) = to {
        push_where_cond(qb, first, "created_at <= ");
        qb.push_bind(to);
    }
}

/// 非空时拼接 `column IN (...)`。`column` 仅允许调用方硬编码的标识符。
pub(crate) fn push_column_in(
    qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>,
    first: &mut bool,
    column: &'static str,
    values: &[String],
) {
    if values.is_empty() {
        return;
    }
    push_where_cond(qb, first, column);
    qb.push(" IN (");
    let mut separated = qb.separated(", ");
    for value in values {
        separated.push_bind(value);
    }
    separated.push_unseparated(")");
}

/// 分页 LIMIT/OFFSET：页码与每页条数在边界防御，超大偏移只返回空页。
pub(crate) fn push_limit_offset(
    qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>,
    page: u64,
    page_size: u64,
) {
    // `page`/`page_size` 可能为 0（`Default` 派生或结构体字面量绕过构造器夹取），
    // saturating 避免下溢。offset 夹到 `i64::MAX` 再转 i64，防止超大页码经
    // `as i64` 回绕成负偏移（SQLite 拒绝负 OFFSET）。
    let page_size = page_size.max(1);
    let offset = page
        .saturating_sub(1)
        .saturating_mul(page_size)
        .min(i64::MAX as u64);
    qb.push(" LIMIT ").push_bind(page_size as i64);
    qb.push(" OFFSET ").push_bind(offset as i64);
}

/// 关键字 → LIKE 子串模式：转义 `\`/`%`/`_`（配合 `ESCAPE '\'`），两端补 `%`。
pub(crate) fn like_substring_pattern(keyword: &str) -> String {
    let mut pattern = String::with_capacity(keyword.len() + 2);
    pattern.push('%');
    for ch in keyword.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

/// 跨领域测试共享的建库与播种辅助。
#[cfg(test)]
pub(crate) mod test_support {
    use super::open;
    use crate::core::billing::PriceSnapshot;
    use crate::store::request_log::RequestLog;
    use crate::store::resources;
    use sqlx::{SqliteConnection, SqlitePool};

    /// 建一个临时 SQLite 连接池并跑完全部迁移。
    pub(crate) async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let pool = open(&dir.path().join("test.db"))
            .await
            .expect("应能打开临时库");
        (dir, pool)
    }
    /// 直写一条令牌定义行：`token_balance` 外键指向 `tokens`，余额相关测试
    /// 需先有归属令牌。
    pub(crate) async fn seed_token(conn: &mut SqliteConnection, token_key: &str) {
        sqlx::query(
            "INSERT INTO tokens (token_key, name, enabled, created_at) VALUES (?, ?, 1, 0)",
        )
        .bind(token_key)
        .bind(token_key)
        .execute(&mut *conn)
        .await
        .expect("应能写令牌行");
    }
    pub(crate) fn sample_log(created_at: i64, settled: bool) -> RequestLog {
        RequestLog {
            id: 0,
            created_at,
            token_name: "t".to_string(),
            token_key: "sk-a".to_string(),
            user_id: resources::ROOT_USER_ID,
            inbound_protocol: "openai_chat".to_string(),
            model: "m".to_string(),
            outbound_model: None,
            channel_key: None,
            channel: "c".to_string(),
            status_code: 200,
            latency_ms: 1,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_write_1h_tokens: 0,
            usage_reported: false,
            price: PriceSnapshot::default(),
            cost_usd_micros: 1,
            base_cost_usd_micros: 0,
            discount_bp: 10_000,
            settled,
            request_id: None,
            billing_attempt_id: None,
            dispatched: true,
            request_body: None,
            response_body: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::billing::PriceSnapshot;
    use crate::store::request_log::RequestLog;
    use crate::store::settlement::{
        BillingAttemptRecovery, get_admission_snapshot, initialize_token_settlement,
    };
    use crate::store::test_support::{seed_token, test_pool};
    use sqlx::Connection;

    /// 空库迁移后即有内置 root（id=1）与零额钱包；尚未设密码。
    #[tokio::test]
    async fn open_seeds_root_user_and_empty_wallet() {
        let (_dir, pool) = test_pool().await;
        let row: (i64, String, Option<String>, String, i64) = sqlx::query_as(
            "SELECT id, email, password_hash, role, enabled FROM users WHERE id = 1",
        )
        .fetch_one(&pool)
        .await
        .expect("应有内置 root");
        assert_eq!(row.0, 1);
        assert_eq!(row.1, "root@localhost");
        assert!(row.2.is_none(), "尚未设密码");
        assert_eq!(row.3, "root");
        assert_eq!(row.4, 1);

        let wallet: (i64, i64) = sqlx::query_as(
            "SELECT balance_usd_micros, settled_usd_micros FROM user_balance WHERE user_id = 1",
        )
        .fetch_one(&pool)
        .await
        .expect("应有用户钱包");
        assert_eq!(wallet, (0, 0));

        let root_plan: Option<i64> = sqlx::query_scalar("SELECT plan_id FROM users WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("应能读 root 套餐");
        assert_eq!(root_plan, None, "root 不挂套餐");

        let standard_group: String =
            sqlx::query_scalar("SELECT group_name FROM plan_model_groups WHERE plan_id = 1")
                .fetch_one(&pool)
                .await
                .expect("standard 应含 default 组");
        assert_eq!(standard_group, "default");

        let builtin_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plans WHERE builtin = 1")
            .fetch_one(&pool)
            .await
            .expect("应能数内置套餐");
        assert_eq!(builtin_count, 2, "内置两档应已播种");

        let remaining_col: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('token_balance') \
             WHERE name = 'balance_usd_micros'",
        )
        .fetch_one(&pool)
        .await
        .expect("应能查列");
        assert_eq!(remaining_col, 0, "token_balance 不应再存剩余余额");
    }

    /// 连接选项治理 SQLite 坏默认值：WAL 日志模式、NORMAL 同步、外键强制、
    /// 写锁排队 5 秒（缺省分别是 DELETE、FULL、关闭、立即 BUSY）。
    #[tokio::test]
    async fn open_applies_hardened_pragmas() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");

        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&mut *conn)
            .await
            .expect("应能查日志模式");
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");

        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&mut *conn)
            .await
            .expect("应能查同步档位");
        assert_eq!(synchronous, 1, "1 = NORMAL");

        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut *conn)
            .await
            .expect("应能查外键开关");
        assert_eq!(foreign_keys, 1);

        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&mut *conn)
            .await
            .expect("应能查写锁等待");
        assert_eq!(busy_timeout, SQLITE_BUSY_TIMEOUT.as_millis() as i64);
    }

    /// 新建库文件与 WAL/SHM 边车都以 owner-only 权限落盘：库内容含渠道
    /// 密钥、令牌 key 与对话 body，不能按进程 umask 宽松创建。
    #[tokio::test]
    async fn open_creates_database_files_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("owner-only.db");
        let pool = open(&path).await.expect("应能建库");
        insert_smoke(&pool, "wal-priming")
            .await
            .expect("应能写入触发 WAL 落盘");
        pool.close().await;

        let mode = std::fs::metadata(&path)
            .expect("应能读取库文件")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "新建库文件应为 0600");

        for sidecar in [
            format!("{}-wal", path.display()),
            format!("{}-shm", path.display()),
        ] {
            let Ok(metadata) = std::fs::metadata(&sidecar) else {
                // 边车在 checkpoint 后可能已截断移除；存在即必须 owner-only。
                continue;
            };
            assert_eq!(
                metadata.permissions().mode() & 0o777,
                0o600,
                "{sidecar} 应按库文件权限派生为 0600"
            );
        }
    }

    /// 既有库文件以更宽权限落盘（历史版本或外部创建）时，打开后归一为 0600。
    #[tokio::test]
    async fn open_normalizes_loose_database_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("loose.db");
        std::fs::File::create(&path).expect("应能创建空库文件");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("应能设置宽权限");

        let _pool = open(&path).await.expect("应能打开既有库");

        let mode = std::fs::metadata(&path)
            .expect("应能读取库文件")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "既有库文件应归一为 0600");
    }

    /// 业务表一律 STRICT：错类型写入直接报错，而非按亲和性静默收下。逐表探测，
    /// 任一表回退成非 STRICT 都会被此测试捕获。探测方向：INTEGER 列写 TEXT/REAL；
    /// `settings` 无 INTEGER 列，用 BLOB 写 TEXT 列（STRICT 拒绝，非 STRICT 的
    /// TEXT 亲和性会原样收下）。
    #[tokio::test]
    async fn strict_tables_reject_wrong_types() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        // token_balance 有外键，先播种归属令牌，让探测只命中 STRICT 而非外键。
        seed_token(&mut conn, "strict-probe").await;
        let channel_id = crate::store::resources::insert_channel(
            &mut conn,
            &crate::store::resources::Channel {
                name: "strict-price".to_string(),
                protocol: crate::config::Protocol::OpenAiChat,
                base_url: "http://127.0.0.1:9".to_string(),
                keys: vec![resources::ChannelKey {
                    name: "default".to_string(),
                    api_key: "sk".to_string(),
                    weight: 1,
                    enabled: true,
                    models: None,
                    blocked_models: None,
                }],
                models: vec![],
                model_aliases: std::collections::HashMap::new(),
                timeout_ms: 1000,
                request_timeout_ms: 120_000,
                max_retries: 0,
                enabled: true,
                model_group: crate::store::resources::DEFAULT_MODEL_GROUP.to_string(),
                reasoning_output: Default::default(),
                session_cache_key: Default::default(),
                injects_cache_breakpoints: false,
                abort_on_disconnect: true,
            },
        )
        .await
        .expect("应能写渠道");

        let probes = [
            (
                "smoke_probe",
                "INSERT INTO smoke_probe (note, id) VALUES ('x', 'not-a-number')",
            ),
            (
                "tokens",
                "INSERT INTO tokens (token_key, name, enabled, created_at) \
                 VALUES ('k1', 'n', 1, 'not-a-number')",
            ),
            (
                "token_balance",
                "INSERT INTO token_balance (token_key, settled_usd_micros, created_at) \
                 VALUES ('strict-probe', 'not-a-number', 0)",
            ),
            (
                "users",
                "INSERT INTO users (id, email, display_name, role, enabled, created_at) \
                 VALUES ('not-a-number', 'a@b.c', 'n', 'user', 1, 0)",
            ),
            (
                "user_balance",
                "INSERT INTO user_balance (user_id, balance_usd_micros, settled_usd_micros, created_at) \
                 VALUES ('not-a-number', 0, 0, 0)",
            ),
            (
                "plans",
                "INSERT INTO plans (id, internal_name, display_name, note, note_visible_to_admin, \
                     discount_bp, default_rpm, shared_rpm, initial_grant_usd_micros, \
                     capabilities_json, shared_with_admin, builtin, created_at) \
                 VALUES ('not-a-number', 'x', 'X', '', 0, 10000, NULL, NULL, 0, '{}', 0, 0, 0)",
            ),
            (
                "plan_model_groups",
                "INSERT INTO plan_model_groups (plan_id, group_name) VALUES ('not-a-number', 'default')",
            ),
            (
                "management_sessions",
                "INSERT INTO management_sessions (token_hash, user_id, created_at, expires_at, revoked) \
                 VALUES ('h', 'not-a-number', 0, 0, 0)",
            ),
            (
                "request_log",
                "INSERT INTO request_log (token_name, inbound_protocol, model, channel, \
                     status_code, latency_ms, created_at) \
                 VALUES ('t', 'openai_chat', 'm', 'c', 200, 10, 'not-a-number')",
            ),
            (
                "request_log_outbox",
                "INSERT INTO request_log_outbox \
                     (id, token_key, user_id, cost_usd_micros, metadata) \
                 VALUES ('not-a-number', 'k', 1, 0, x'00')",
            ),
            (
                "channels",
                "INSERT INTO channels (name, protocol, base_url, models_json, \
                     model_aliases_json, timeout_ms, max_retries) \
                 VALUES ('c', 'openai_chat', 'u', '[]', '{}', 'not-a-number', 1)",
            ),
            (
                "channel_keys",
                "INSERT INTO channel_keys (channel_id, name, api_key, weight, enabled, created_at) \
                 VALUES (?, 'k', 'secret', 'not-a-number', 1, 0)",
            ),
            (
                "settings",
                "INSERT INTO settings (setting_key, setting_value) VALUES ('k2', x'00')",
            ),
            (
                "model_groups",
                "INSERT INTO model_groups (name, models_json) VALUES ('k3', x'00')",
            ),
            (
                "unified_models",
                "INSERT INTO unified_models (id, models_json, hide) VALUES ('k4', x'00', 0)",
            ),
            (
                "catalog_models",
                "INSERT INTO catalog_models (provider_id, provider_name, model_id, input_micros) \
                 VALUES ('p', 'P', 'm', 'not-a-number')",
            ),
        ];
        for (table, sql) in probes {
            let result = if table == "channel_keys" {
                sqlx::query(sql).bind(channel_id).execute(&pool).await
            } else {
                sqlx::query(sql).execute(&pool).await
            };
            assert!(
                result.is_err(),
                "{table} 应仍是 STRICT 表，错类型写入须被拒"
            );
        }

        assert!(
            sqlx::query(
                "INSERT INTO prices (channel_id, model, input_micros, output_micros) \
                 VALUES (?, 'm', 'not-a-number', 0)"
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .is_err(),
            "prices 应仍是 STRICT 表，错类型写入须被拒"
        );

        let result = sqlx::query(
            "INSERT INTO prices (channel_id, model, input_micros, output_micros) \
             VALUES (?, 'm2', 1.5, 0)",
        )
        .bind(channel_id)
        .execute(&pool)
        .await;
        assert!(result.is_err(), "INTEGER 列写 REAL 应被 STRICT 拒绝");

        assert!(
            sqlx::query(
                "INSERT INTO channel_model_order (model, channel_id, position) \
                 VALUES ('m', ?, 'not-a-number')",
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .is_err(),
            "channel_model_order 应仍是 STRICT 表，错类型写入须被拒"
        );
    }

    /// token_balance 外键：无归属令牌的余额行被拒绝；删除令牌级联清理余额行，
    /// 同 key 重建不再复活旧余额。
    #[tokio::test]
    async fn token_balance_fk_enforced_and_cascades() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");

        let orphan = sqlx::query(
            "INSERT INTO token_balance (token_key, settled_usd_micros, created_at) \
             VALUES ('sk-ghost', 0, 0)",
        )
        .execute(&mut *conn)
        .await;
        assert!(orphan.is_err(), "外键应拒绝无归属令牌的余额行");

        seed_token(&mut conn, "sk-a").await;
        initialize_token_settlement(&mut conn, "sk-a", 5_000_000, 1)
            .await
            .expect("应能初始化余额");
        sqlx::query("DELETE FROM tokens WHERE token_key = ?")
            .bind("sk-a")
            .execute(&mut *conn)
            .await
            .expect("应能删令牌");
        let balance = get_admission_snapshot(&mut conn, "sk-a")
            .await
            .expect("应能查余额");
        assert!(balance.is_none(), "级联删除应带走余额行");
    }

    /// 迁移 0001–0006 全部应用后的表终态（非 STRICT、无外键）。配合播种
    /// `_sqlx_migrations` 记账行（版本 1–6 标记已应用），`open()` 只会应用
    /// 迁移 0007，精确模拟存量库升级。
    const LEGACY_SCHEMA: &str = "
        CREATE TABLE smoke_probe (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            note TEXT NOT NULL
        );
        CREATE TABLE request_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at INTEGER NOT NULL,
            token_name TEXT NOT NULL,
            inbound_protocol TEXT NOT NULL,
            model TEXT NOT NULL,
            channel TEXT NOT NULL,
            status_code INTEGER NOT NULL,
            latency_ms INTEGER NOT NULL,
            token_key TEXT NOT NULL DEFAULT '',
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0,
            cache_write_tokens INTEGER NOT NULL DEFAULT 0,
            input_price_usd_micros INTEGER NOT NULL DEFAULT 0,
            output_price_usd_micros INTEGER NOT NULL DEFAULT 0,
            cache_read_price_usd_micros INTEGER NOT NULL DEFAULT 0,
            cache_write_price_usd_micros INTEGER NOT NULL DEFAULT 0,
            cost_usd_micros INTEGER NOT NULL DEFAULT 0,
            request_body BLOB,
            response_body BLOB
        );
        CREATE TABLE token_balance (
            token_key TEXT PRIMARY KEY,
            balance_usd_micros INTEGER NOT NULL,
            settled_usd_micros INTEGER NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE channels (
            name TEXT PRIMARY KEY,
            protocol TEXT NOT NULL,
            base_url TEXT NOT NULL,
            api_key TEXT NOT NULL,
            models_json TEXT NOT NULL,
            model_aliases_json TEXT NOT NULL,
            priority INTEGER NOT NULL,
            weight INTEGER NOT NULL,
            timeout_ms INTEGER NOT NULL,
            max_retries INTEGER NOT NULL
        );
        CREATE TABLE tokens (
            token_key TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            limit_usd_micros INTEGER,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT 0,
            last_used_at INTEGER
        );
        CREATE TABLE prices (
            model TEXT PRIMARY KEY,
            input_micros INTEGER NOT NULL,
            output_micros INTEGER NOT NULL,
            cache_read_micros INTEGER,
            cache_write_micros INTEGER
        );
        CREATE TABLE settings (
            setting_key TEXT PRIMARY KEY,
            setting_value TEXT NOT NULL
        );";

    /// sqlx 迁移记账表（结构与 sqlx-sqlite 建表语句一致）：手工播种版本 1–6
    /// 的已应用记录，校验和取自 `migrate!()` 嵌入内容，与真实应用无异。
    async fn seed_migrations_bookkeeping(raw: &mut SqliteConnection) {
        sqlx::raw_sql(
            "CREATE TABLE _sqlx_migrations (
                version BIGINT PRIMARY KEY,
                description TEXT NOT NULL,
                installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                success BOOLEAN NOT NULL,
                checksum BLOB NOT NULL,
                execution_time BIGINT NOT NULL
            );",
        )
        .execute(&mut *raw)
        .await
        .expect("应能建迁移记账表");
        for migration in sqlx::migrate!().iter().filter(|m| m.version < 7) {
            sqlx::query(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
                 VALUES (?, ?, TRUE, ?, -1)",
            )
            .bind(migration.version)
            .bind(&*migration.description)
            .bind(&*migration.checksum)
            .execute(&mut *raw)
            .await
            .expect("应能记账已应用迁移");
        }
    }

    /// 存量库升级路径：真实部署的旧库带脏数据（BLOB 列被写入 TEXT、孤儿余额行），
    /// `open()` 应用迁移 0007 后须完成清理、字节无损转 BLOB、全表 STRICT 化，
    /// 且 AUTOINCREMENT 计数延续。
    #[tokio::test]
    async fn legacy_db_upgrades_through_migration_0007() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("legacy.db");
        std::fs::File::create(&path).expect("应能创建空库文件");
        let mut raw = SqliteConnection::connect(path.to_str().expect("路径应可转字符串"))
            .await
            .expect("应能建旧库");
        seed_migrations_bookkeeping(&mut raw).await;
        sqlx::raw_sql(LEGACY_SCHEMA)
            .execute(&mut raw)
            .await
            .expect("应能建旧 schema");
        sqlx::query("INSERT INTO tokens (token_key, name) VALUES ('sk-live', '生产')")
            .execute(&mut raw)
            .await
            .expect("应能写旧令牌");
        sqlx::query(
            "INSERT INTO token_balance (token_key, balance_usd_micros, settled_usd_micros, created_at) \
             VALUES ('sk-live', 1500000, 200, 0)",
        )
        .execute(&mut raw)
        .await
        .expect("应能写旧令牌余额");
        // 脏数据一：无归属令牌的孤儿余额行。
        sqlx::query(
            "INSERT INTO token_balance (token_key, balance_usd_micros, settled_usd_micros, created_at) \
             VALUES ('sk-orphan', 999, 0, 0)",
        )
        .execute(&mut raw)
        .await
        .expect("旧库无外键，孤儿余额应能写入");
        // 脏数据二：BLOB 列被按 TEXT 亲和性收下字符串。
        sqlx::query(
            "INSERT INTO request_log (created_at, token_name, inbound_protocol, model, channel, \
                 status_code, latency_ms, token_key, request_body) \
             VALUES (1, '生产', 'openai_chat', 'gpt-4o', 'c1', 200, 10, 'sk-live', 'legacy text body')",
        )
        .execute(&mut raw)
        .await
        .expect("旧库非 STRICT，TEXT 写 BLOB 列应能写入");
        raw.close().await.expect("应能关闭旧库连接");

        let pool = open(&path).await.expect("迁移应能吃下带脏数据的旧库");
        let mut conn = pool.acquire().await.expect("应能获取连接");

        let strict: i64 =
            sqlx::query_scalar("SELECT strict FROM pragma_table_list WHERE name = 'request_log'")
                .fetch_one(&mut *conn)
                .await
                .expect("应能查表属性");
        assert_eq!(strict, 1, "重建后应为 STRICT 表");

        let body: Vec<u8> = sqlx::query_scalar("SELECT request_body FROM request_log")
            .fetch_one(&mut *conn)
            .await
            .expect("日志应被保留");
        assert_eq!(body, b"legacy text body", "TEXT 应字节无损转为 BLOB");

        let balance = get_admission_snapshot(&mut conn, "sk-orphan")
            .await
            .expect("应能查余额");
        assert!(balance.is_none(), "孤儿余额行应被迁移清理");
        // 明文 key 在 open() 的指纹换算中转为 SHA-256，按指纹读取存量令牌。
        let balance = get_admission_snapshot(&mut conn, &token_key_fingerprint("sk-live"))
            .await
            .expect("应能查余额")
            .expect("存量令牌应能读到用户钱包");
        assert_eq!(
            balance.wallet.balance_usd_micros, 1_500_000,
            "root 钱包应为各令牌剩余之和"
        );
        assert_eq!(balance.token.settled_usd_micros, 200, "令牌 settled 应保留");
        let wallet: (i64, i64) = sqlx::query_as(
            "SELECT balance_usd_micros, settled_usd_micros FROM user_balance WHERE user_id = 1",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("应有 root 钱包");
        assert_eq!(wallet, (1_500_000, 200));
        let owner: i64 = sqlx::query_scalar("SELECT user_id FROM tokens WHERE token_key = ?")
            .bind(token_key_fingerprint("sk-live"))
            .fetch_one(&mut *conn)
            .await
            .expect("令牌应有归属");
        assert_eq!(owner, 1);

        let id = insert_smoke(&pool, "after-upgrade")
            .await
            .expect("升级后应能写入");
        assert!(id >= 1, "AUTOINCREMENT 计数应延续");
    }

    /// 存量明文 key 换算遇到损坏的恢复元数据：该行跳过不 panic、原样保留
    /// （不半写），其余行照常完成换算；完成标记仍然落盘——坏行交给人工
    /// 处置，不阻塞库的打开与使用。
    #[tokio::test]
    async fn legacy_plaintext_hash_skips_corrupted_recovery_metadata() {
        // 先建一个全新库（迁移全部应用），再手工摘掉完成标记、把一行预留的
        // recovery_metadata 换成损坏字节与明文 key——模拟「指纹化迁移前崩溃
        // 损坏」的存量形态。
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("legacy-hash.db");
        let pool = open(&path).await.expect("应能建库");
        // 换算在首次 open 已完成（标记已落）；清掉标记、注入明文时代的
        // token 与预留行，模拟「指纹化迁移前崩溃 + 元数据损坏」的存量库。
        sqlx::query("DELETE FROM settings WHERE setting_key = 'token_keys_hashed'")
            .execute(&pool)
            .await
            .expect("应能清完成标记");
        // 明文时代的 tokens 行（列主换算以 tokens 表为驱动，行必须在场）。
        for (name, key) in [("good", "sk-legacy-good"), ("bad", "sk-legacy-bad")] {
            sqlx::query("INSERT INTO tokens (token_key, name, user_id) VALUES (?, ?, 1)")
                .bind(key)
                .bind(name)
                .execute(&pool)
                .await
                .expect("应能注入明文令牌行");
        }

        let price = PriceSnapshot {
            input_micros: 1,
            output_micros: 1,
            cache_read_micros: 0,
            cache_write_micros: 0,
            cache_write_1h_micros: 0,
        };
        let good_recovery = serde_json::to_vec(&BillingAttemptRecovery {
            token_name: "t".to_string(),
            model: "gpt-4o".to_string(),
            outbound_model: None,
            channel: "c1".to_string(),
            channel_key: None,
            inbound_protocol: "openai_chat".to_string(),
            started: 1,
            price,
            discount_bp: 10_000,
            request_body: None,
            // 带上已完成的结果载荷：JSON 内 token_key 的换算发生在 result 里，
            // 缺席则无 JSON 内换算面可断言。
            result: Some(Box::new(RequestLog {
                id: 0,
                created_at: 1,
                token_name: "t".to_string(),
                token_key: "sk-legacy-good".to_string(),
                user_id: resources::ROOT_USER_ID,
                inbound_protocol: "openai_chat".to_string(),
                model: "gpt-4o".to_string(),
                outbound_model: None,
                channel_key: None,
                channel: "c1".to_string(),
                status_code: 200,
                latency_ms: 1,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_write_1h_tokens: 0,
                usage_reported: false,
                price,
                cost_usd_micros: 0,
                base_cost_usd_micros: 0,
                discount_bp: 10_000,
                settled: false,
                request_id: None,
                billing_attempt_id: None,
                dispatched: true,
                request_body: None,
                response_body: None,
            })),
            result_settlement_error: None,
            upstream_reached: true,
        })
        .expect("合法恢复元数据应可编码");
        for (attempt, key, metadata) in [
            ("attempt-good", "sk-legacy-good", good_recovery),
            ("attempt-bad", "sk-legacy-bad", b"not-json".to_vec()),
        ] {
            sqlx::query(
                "INSERT INTO billing_reservations \
                 (attempt_id, request_id, token_key, user_id, reserved_cost_usd_micros, \
                  recovery_metadata, status, dispatched, result_persisted, created_at, updated_at) \
                 VALUES (?, ?, ?, 1, 0, ?, 'reserved', 1, 0, 1, 1)",
            )
            .bind(attempt)
            .bind(format!("req-{attempt}"))
            .bind(key)
            .bind(&metadata)
            .execute(&pool)
            .await
            .expect("应能注入预留行");
        }
        pool.close().await;

        // 重新 open：换算对坏行跳过、好行完成，库正常可用。
        let pool = open(&path).await.expect("坏行不应阻塞库打开");
        let mut conn = pool.acquire().await.expect("应能获取连接");

        let flagged: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM settings WHERE setting_key = 'token_keys_hashed'",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("应能查标记");
        assert_eq!(flagged, 1, "完成标记仍应落盘（坏行交人工处置）");

        let (bad_key, bad_meta): (String, Vec<u8>) =
            sqlx::query_as("SELECT token_key, recovery_metadata FROM billing_reservations WHERE attempt_id = 'attempt-bad'")
                .fetch_one(&mut *conn)
                .await
                .expect("坏行应保留");
        assert_eq!(
            bad_key,
            token_key_fingerprint("sk-legacy-bad"),
            "表列换算以 tokens 表为驱动对所有表统一生效，坏行不例外（列换算不依赖 JSON 可解析）"
        );
        assert_eq!(bad_meta, b"not-json", "损坏元数据应原样保留");

        let good_key: String = sqlx::query_scalar(
            "SELECT token_key FROM billing_reservations WHERE attempt_id = 'attempt-good'",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("好行应在场");
        assert_eq!(
            good_key,
            token_key_fingerprint("sk-legacy-good"),
            "合法行应完成明文→指纹换算"
        );
        // 合法行的 JSON 内 token_key 同步换算：恢复任务按指纹定位行，
        // JSON 内仍是明文会让恢复路径找不到行。
        let good_meta: Vec<u8> = sqlx::query_scalar(
            "SELECT recovery_metadata FROM billing_reservations WHERE attempt_id = 'attempt-good'",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("好行元数据应在场");
        let recovery: BillingAttemptRecovery =
            serde_json::from_slice(&good_meta).expect("好行元数据应可解析");
        assert_eq!(
            recovery
                .result
                .map(|result| result.token_key.clone())
                .unwrap_or_default(),
            token_key_fingerprint("sk-legacy-good"),
            "结果载荷内的 token_key 应同步换算为指纹"
        );
    }

    /// 活动读事务会令 SQLite 返回 busy=1；checkpoint 辅助必须检查结果行，不能只
    /// 看 SQL 是否报错，否则调用方会误以为 WAL 已经截断。
    #[tokio::test]
    async fn checkpoint_wal_truncate_reports_busy_reader() {
        let (_dir, pool) = test_pool().await;
        let mut reader = pool.acquire().await.expect("应能取得读连接");
        let mut read_tx = reader.begin().await.expect("应能开启读事务");
        let _: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM smoke_probe")
            .fetch_one(&mut *read_tx)
            .await
            .expect("应能建立读快照");

        insert_smoke(&pool, "checkpoint-busy")
            .await
            .expect("应能追加 WAL");
        let err = checkpoint_wal_truncate(&pool)
            .await
            .expect_err("活动读事务应报告 busy");
        assert!(
            matches!(err, StoreError::WalCheckpointBusy { log_frames, checkpointed_frames }
            if log_frames > checkpointed_frames)
        );

        read_tx.rollback().await.expect("应能结束读事务");
        checkpoint_wal_truncate(&pool)
            .await
            .expect("读事务结束后应能完成 checkpoint");
    }
}
