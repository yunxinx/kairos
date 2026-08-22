//! 管理用户与管理会话：邮箱密码、角色、会话令牌哈希。
//!
//! 密码用 Argon2id 的 PHC 串落库；会话只存 SHA-256，不存明文。最后一个启用的
//! root 不能删除、禁用或降级（ADR-0009）。

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};

use crate::store::StoreError;
use crate::store::resources::{DEFAULT_MODEL_GROUP, ROOT_USER_ID};

/// 管理角色：上级含下级权限。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagementRole {
    User,
    Admin,
    Root,
}

impl ManagementRole {
    /// 从库内字符串解析；非法值视为资源损坏。
    pub fn parse(raw: &str) -> Result<Self, StoreError> {
        match raw {
            "user" => Ok(Self::User),
            "admin" => Ok(Self::Admin),
            "root" => Ok(Self::Root),
            other => Err(StoreError::InvalidResource(format!(
                "未知管理角色: {other}"
            ))),
        }
    }

    /// 落库字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Admin => "admin",
            Self::Root => "root",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::User => 0,
            Self::Admin => 1,
            Self::Root => 2,
        }
    }

    /// 是否不低于 `min`（root > admin > user）。
    pub fn at_least(self, min: Self) -> bool {
        self.rank() >= min.rank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::open;
    use sqlx::SqlitePool;

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let pool = open(&dir.path().join("test.db"))
            .await
            .expect("应能打开临时库");
        (dir, pool)
    }

    fn is_alnum_ascii(s: &str) -> bool {
        s.chars().all(|c| c.is_ascii_alphanumeric())
    }

    /// 缺省配置时生成 `12@root.com` 邮箱与 24 位口令，并只在生成路径返回明文口令。
    #[tokio::test]
    async fn seed_generates_email_and_password_when_hash_is_null() {
        let (_dir, pool) = test_pool().await;
        let outcome = seed_builtin_root(&pool, None, None)
            .await
            .expect("应能播种");
        let RootSeedOutcome::Provisioned {
            email,
            generated_password,
        } = outcome
        else {
            panic!("空库 root 应走播种路径");
        };
        let local = email
            .strip_suffix("@root.com")
            .expect("生成邮箱应以 @root.com 结尾");
        assert_eq!(local.len(), GENERATED_EMAIL_LOCAL_LEN);
        assert!(is_alnum_ascii(local), "本地部分应仅为字母数字");
        let password = generated_password.expect("缺省配置应返回生成口令供启动日志打印");
        assert_eq!(password.len(), GENERATED_PASSWORD_LEN);
        assert!(is_alnum_ascii(&password));
        let user = authenticate_password(&pool, &email, &password)
            .await
            .expect("校验不应失败")
            .expect("生成口令应能登录");
        assert_eq!(user.id, ROOT_USER_ID);
    }

    /// 已有哈希则忽略配置：避免重启把运营改过的口令/邮箱打回 config.json。
    #[tokio::test]
    async fn seed_ignores_config_once_password_hash_exists() {
        let (_dir, pool) = test_pool().await;
        seed_builtin_root(&pool, Some("first@root.com"), Some("password1"))
            .await
            .expect("首次应播种");
        let again = seed_builtin_root(&pool, Some("second@root.com"), Some("password2"))
            .await
            .expect("第二次应成功但跳过");
        assert!(matches!(again, RootSeedOutcome::AlreadyProvisioned));
        let user = get_user(&pool, ROOT_USER_ID)
            .await
            .expect("应能读 root")
            .expect("root 应存在");
        assert_eq!(user.email, "first@root.com");
        assert!(
            authenticate_password(&pool, "first@root.com", "password1")
                .await
                .expect("校验不应失败")
                .is_some()
        );
        assert!(
            authenticate_password(&pool, "second@root.com", "password2")
                .await
                .expect("校验不应失败")
                .is_none(),
            "第二次配置不得覆盖已有哈希"
        );
    }

    /// 配置提供的口令不进入 `generated_password`，启动日志才不会把运营口令打到 stdout。
    #[tokio::test]
    async fn seed_from_config_does_not_return_plaintext_password() {
        let (_dir, pool) = test_pool().await;
        let outcome = seed_builtin_root(&pool, Some("ops@example.com"), Some("password1"))
            .await
            .expect("应能播种");
        match outcome {
            RootSeedOutcome::Provisioned {
                email,
                generated_password,
            } => {
                assert_eq!(email, "ops@example.com");
                assert!(generated_password.is_none());
            }
            RootSeedOutcome::AlreadyProvisioned => panic!("空库应播种"),
        }
    }

    /// 最后一个启用 root 不能降级。
    #[tokio::test]
    async fn last_enabled_root_cannot_be_demoted() {
        let (_dir, pool) = test_pool().await;
        let mut conn = pool.acquire().await.expect("应能获取连接");
        let err = set_user_role(&mut conn, ROOT_USER_ID, ManagementRole::Admin)
            .await
            .expect_err("应拒绝");
        assert!(matches!(err, StoreError::LastRootProtected));
        let err = delete_user(&mut conn, ROOT_USER_ID, 1)
            .await
            .expect_err("应拒绝删除");
        assert!(matches!(err, StoreError::LastRootProtected));
    }

    #[test]
    fn rate_limit_rpm_rejects_values_outside_sqlite_integer_range() {
        let max = u64::try_from(i64::MAX).expect("i64::MAX 应能转成 u64");
        assert_eq!(
            rate_limit_rpm_to_db(Some(max)).expect("边界值应可写"),
            Some(i64::MAX)
        );
        assert!(rate_limit_rpm_to_db(Some(max + 1)).is_err());
        assert!(rate_limit_rpm_to_db(Some(u64::MAX)).is_err());
        assert!(rate_limit_rpm_from_db(Some(-1)).is_err());
    }
}

/// 管理用户（不含密码哈希）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRecord {
    pub id: i64,
    pub email: String,
    pub display_name: String,
    pub role: ManagementRole,
    pub enabled: bool,
    pub avatar: Option<String>,
    pub rate_limit_rpm: Option<u64>,
}

/// 快照加载所需的用户投影；不携带头像、邮箱等管理面字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotUser {
    pub(crate) id: i64,
    pub(crate) role: ManagementRole,
    pub(crate) enabled: bool,
    pub(crate) rate_limit_rpm: Option<u64>,
}

/// 新建管理用户时的字段。
pub struct NewUser<'a> {
    pub email: &'a str,
    pub display_name: &'a str,
    pub password: &'a str,
    pub role: ManagementRole,
    pub rate_limit_rpm: Option<u64>,
}

/// 会话默认有效期：8 小时（与旧 Kairos 管理面一致）。
pub const SESSION_TTL_MS: i64 = 8 * 60 * 60 * 1000;
const SESSION_TOKEN_PREFIX: &str = "ksess_";
/// 配置未给邮箱时生成的本地部分长度（`[A-Za-z0-9]{12}@root.com`）。
const GENERATED_EMAIL_LOCAL_LEN: usize = 12;
/// 配置未给密码时生成的口令长度（仅字母数字，避免 shell/JSON 转义）。
const GENERATED_PASSWORD_LEN: usize = 24;

/// 内置 root 首次设密的结果：调用方据此决定是否把明文口令打到启动日志。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootSeedOutcome {
    /// 库里已有 `password_hash`：SQLite 是唯一事实来源，配置里的邮箱/密码全部忽略。
    AlreadyProvisioned,
    /// 本次把邮箱和 Argon2id 哈希写入了 `id=1`。
    ///
    /// `generated_password` 只在口令是本进程生成时为 `Some`；来自配置的口令不回传，
    /// 以免启动日志把运营写在 config.json 里的秘密再打印一遍。
    Provisioned {
        email: String,
        generated_password: Option<String>,
    },
}

/// 仅当内置 root 尚未设密码时，用配置或生成值写入邮箱与哈希。
///
/// 迁移插入的 `root@localhost` + `password_hash IS NULL` 是占位，不是可登录账号。
/// 触发条件钉在哈希为 NULL，而不是「库是空的」：运营一旦通过本函数或 `PUT /me`
/// 设过密，之后改 config.json 也不能把口令打回——配置只是启动种子，不是权威源。
/// 生成值只进 SQLite，不写回配置文件：配置常进版本库，把明文口令写回去等于把秘密提交出去。
pub async fn seed_builtin_root(
    pool: &SqlitePool,
    config_email: Option<&str>,
    config_password: Option<&str>,
) -> Result<RootSeedOutcome, StoreError> {
    // `fetch_optional` 的外层 Option 是「有没有这一行」：无行 → None；
    // 有行时内层 Option 才是 `password_hash` 列（NULL → None，已哈希 → Some）。
    // 无行视为迁移损坏：这里不 INSERT 第二份 root，否则会绕过「最后一个启用 root」保护。
    let password_hash: Option<Option<String>> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ? AND deleted_at IS NULL")
            .bind(ROOT_USER_ID)
            .fetch_optional(pool)
            .await
            .map_err(StoreError::Query)?;
    let Some(password_hash) = password_hash else {
        return Err(StoreError::InvalidResource(
            "内置 root 不存在，无法播种登录凭证".to_string(),
        ));
    };
    // 库是唯一事实来源。哈希一旦存在，配置里的邮箱/密码全部忽略，也不重新生成、不打印口令。
    if password_hash.is_some() {
        return Ok(RootSeedOutcome::AlreadyProvisioned);
    }

    // 配置加载已把空串/纯空白收成 None；这里再 trim 一次，避免测试直调漏过配置层。
    let email = match config_email
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(raw) => normalize_email(raw),
        None => generate_root_email(),
    };
    if email.is_empty() || !email.contains('@') {
        return Err(StoreError::InvalidResource("邮箱不合法".to_string()));
    }
    if let Some(existing) = get_user_by_email(pool, &email).await?
        && existing.id != ROOT_USER_ID
    {
        return Err(StoreError::EmailTaken);
    }

    // 配置层已把纯空白收成 None；此处 Some 即采用配置明文，不再 trim，以免改掉运营口令。
    let (password, password_generated) = match config_password {
        Some(raw) => (raw.to_string(), false),
        None => (generate_alnum(GENERATED_PASSWORD_LEN), true),
    };
    let password_hash = hash_password(&password).await?;
    // UPDATE id=1：占位行已由迁移插入。新建另一行 root 会让「最后一个启用 root」保护失效。
    sqlx::query("UPDATE users SET email = ?, password_hash = ? WHERE id = ?")
        .bind(&email)
        .bind(&password_hash)
        .bind(ROOT_USER_ID)
        .execute(pool)
        .await
        .map_err(StoreError::Query)?;
    Ok(RootSeedOutcome::Provisioned {
        email,
        // 只在本进程生成时回传明文，供启动日志打印一次；配置口令运营自己知道，不回传。
        generated_password: password_generated.then_some(password),
    })
}

/// `12` 位字母数字 + `@root.com`。字母数字避免邮箱本地部分出现需转义的符号。
fn generate_root_email() -> String {
    format!("{}@root.com", generate_alnum(GENERATED_EMAIL_LOCAL_LEN))
}

/// 用与令牌 key 相同的 CSPRNG（`rand::rng()` / ChaCha12，由 `SysRng` 播种）采样 `A-Za-z0-9`。
fn generate_alnum(len: usize) -> String {
    use rand::distr::{Alphanumeric, SampleString};
    Alphanumeric.sample_string(&mut rand::rng(), len)
}

/// 规范化邮箱：去空白并转小写。UNIQUE NOCASE 仍要求应用层统一写入形态。
pub fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

/// 按 id 读用户；不存在返回 `None`。
pub async fn get_user(pool: &SqlitePool, id: i64) -> Result<Option<UserRecord>, StoreError> {
    let row = sqlx::query(
        "SELECT id, email, display_name, role, enabled, avatar, rate_limit_rpm FROM users WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(StoreError::Query)?;
    row.as_ref().map(map_user_row).transpose()
}

/// 按 id 读用户，包含已经归档的行。
///
/// 仅供历史财务归属与审计使用；认证、快照和日常管理列表必须继续走 [`get_user`]，
/// 避免把归档账户重新暴露为可用主体。
/// 在现有连接/事务上按 id 读取用户，包含归档行。
pub(crate) async fn get_user_including_archived_on_conn(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<Option<UserRecord>, StoreError> {
    let row = sqlx::query(
        "SELECT id, email, display_name, role, enabled, avatar, rate_limit_rpm FROM users WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    row.as_ref().map(map_user_row).transpose()
}

/// 按邮箱读用户；不存在返回 `None`。
pub async fn get_user_by_email(
    pool: &SqlitePool,
    email: &str,
) -> Result<Option<UserRecord>, StoreError> {
    let email = normalize_email(email);
    let row = sqlx::query(
        "SELECT id, email, display_name, role, enabled, avatar, rate_limit_rpm FROM users WHERE email = ? AND deleted_at IS NULL",
    )
    .bind(email)
    .fetch_optional(pool)
    .await
    .map_err(StoreError::Query)?;
    row.as_ref().map(map_user_row).transpose()
}

fn map_user_row(row: &sqlx::sqlite::SqliteRow) -> Result<UserRecord, StoreError> {
    let enabled: i64 = row.try_get("enabled").map_err(StoreError::Query)?;
    let role: String = row.try_get("role").map_err(StoreError::Query)?;
    let avatar: Option<String> = row.try_get("avatar").map_err(StoreError::Query)?;
    let rate_limit_rpm: Option<i64> = row.try_get("rate_limit_rpm").map_err(StoreError::Query)?;
    let rate_limit_rpm = rate_limit_rpm_from_db(rate_limit_rpm)?;
    Ok(UserRecord {
        id: row.try_get("id").map_err(StoreError::Query)?,
        email: row.try_get("email").map_err(StoreError::Query)?,
        display_name: row.try_get("display_name").map_err(StoreError::Query)?,
        role: ManagementRole::parse(&role)?,
        enabled: enabled != 0,
        avatar,
        rate_limit_rpm,
    })
}

/// 创建用户：同步建零额钱包与默认可用组 `default`。
pub async fn insert_user(
    conn: &mut SqliteConnection,
    new_user: NewUser<'_>,
    now: i64,
) -> Result<UserRecord, StoreError> {
    let email = normalize_email(new_user.email);
    if email.is_empty() || !email.contains('@') {
        return Err(StoreError::InvalidResource("邮箱不合法".to_string()));
    }
    let display_name = new_user.display_name.trim();
    if display_name.is_empty() {
        return Err(StoreError::InvalidResource(
            "display_name 不能为空".to_string(),
        ));
    }
    if get_user_by_email_on_conn(conn, &email).await?.is_some() {
        return Err(StoreError::EmailTaken);
    }
    let password_hash = hash_password(new_user.password).await?;
    let rpm_val = rate_limit_rpm_to_db(new_user.rate_limit_rpm)?;
    let result = sqlx::query(
        "INSERT INTO users (email, display_name, password_hash, role, enabled, created_at, rate_limit_rpm) \
         VALUES (?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&email)
    .bind(display_name)
    .bind(&password_hash)
    .bind(new_user.role.as_str())
    .bind(now)
    .bind(rpm_val)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    let id = result.last_insert_rowid();
    sqlx::query(
        "INSERT INTO user_balance (user_id, balance_usd_micros, settled_usd_micros, created_at) \
         VALUES (?, 0, 0, ?)",
    )
    .bind(id)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    sqlx::query("INSERT INTO user_model_groups (user_id, group_name) VALUES (?, ?)")
        .bind(id)
        .bind(DEFAULT_MODEL_GROUP)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(UserRecord {
        id,
        email,
        display_name: display_name.to_string(),
        role: new_user.role,
        enabled: true,
        avatar: None,
        rate_limit_rpm: new_user.rate_limit_rpm,
    })
}

/// 列出全部管理用户（不含密码）。
pub async fn list_users(pool: &SqlitePool) -> Result<Vec<UserRecord>, StoreError> {
    let rows = sqlx::query(
        "SELECT id, email, display_name, role, enabled, avatar, rate_limit_rpm FROM users WHERE deleted_at IS NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;
    rows.iter().map(map_user_row).collect()
}

/// 快照专用的用户投影：只取请求路径要用的字段。
///
/// 不走 [`list_users`]：那个 SELECT 带 `avatar`（可能是 MB 级 base64 data URL），
/// 而 `reload_and_swap` 在每次管理面写操作后都会重跑一遍全量快照加载——改一次渠道
/// 价格就把所有用户的头像读出来再丢掉。
pub(crate) async fn list_users_for_snapshot(
    pool: &SqlitePool,
) -> Result<Vec<SnapshotUser>, StoreError> {
    let rows =
        sqlx::query("SELECT id, role, enabled, rate_limit_rpm FROM users WHERE deleted_at IS NULL")
            .fetch_all(pool)
            .await
            .map_err(StoreError::Query)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let role: String = row.try_get("role").map_err(StoreError::Query)?;
        let enabled: i64 = row.try_get("enabled").map_err(StoreError::Query)?;
        let rate_limit_rpm: Option<i64> =
            row.try_get("rate_limit_rpm").map_err(StoreError::Query)?;
        out.push(SnapshotUser {
            id: row.try_get("id").map_err(StoreError::Query)?,
            role: ManagementRole::parse(&role)?,
            enabled: enabled != 0,
            rate_limit_rpm: rate_limit_rpm_from_db(rate_limit_rpm)?,
        });
    }
    Ok(out)
}

/// 读出全部用户可用模型组（未排序）。
pub async fn list_all_assigned_groups(pool: &SqlitePool) -> Result<Vec<(i64, String)>, StoreError> {
    let rows = sqlx::query(
        "SELECT g.user_id, g.group_name FROM user_model_groups g \
         JOIN users u ON u.id = g.user_id \
         WHERE u.deleted_at IS NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push((
            row.try_get("user_id").map_err(StoreError::Query)?,
            row.try_get("group_name").map_err(StoreError::Query)?,
        ));
    }
    Ok(out)
}

/// 按用户读可用模型组，按组名排序。
pub async fn list_assigned_groups(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<Vec<String>, StoreError> {
    let mut conn = pool.acquire().await.map_err(StoreError::Query)?;
    list_assigned_groups_on_conn(&mut conn, user_id).await
}

/// 在现有连接/事务上读取用户可用模型组。
pub(crate) async fn list_assigned_groups_on_conn(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Vec<String>, StoreError> {
    let rows = sqlx::query("SELECT group_name FROM user_model_groups WHERE user_id = ?")
        .bind(user_id)
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    let mut names = Vec::with_capacity(rows.len());
    for row in &rows {
        names.push(row.try_get("group_name").map_err(StoreError::Query)?);
    }
    names.sort();
    Ok(names)
}

/// 整体替换用户可用模型组；空名单表示撤掉全部（含 `default`）。
pub async fn replace_assigned_groups(
    conn: &mut SqliteConnection,
    user_id: i64,
    groups: &[String],
) -> Result<Vec<String>, StoreError> {
    if get_user_on_conn(conn, user_id).await?.is_none() {
        return Err(StoreError::UserNotFound(user_id));
    }
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for group in groups {
        let name = group.trim();
        if name.is_empty() {
            return Err(StoreError::InvalidResource("模型组名不能为空".to_string()));
        }
        if !seen.insert(name.to_string()) {
            continue;
        }
        if crate::store::resources::get_model_group(conn, name)
            .await?
            .is_none()
        {
            return Err(StoreError::InvalidResource(format!("模型组 {name} 不存在")));
        }
        unique.push(name.to_string());
    }
    sqlx::query("DELETE FROM user_model_groups WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    for name in &unique {
        sqlx::query("INSERT INTO user_model_groups (user_id, group_name) VALUES (?, ?)")
            .bind(user_id)
            .bind(name)
            .execute(&mut *conn)
            .await
            .map_err(StoreError::Query)?;
    }
    unique.sort();
    Ok(unique)
}

async fn get_user_by_email_on_conn(
    conn: &mut SqliteConnection,
    email: &str,
) -> Result<Option<UserRecord>, StoreError> {
    let row = sqlx::query(
        "SELECT id, email, display_name, role, enabled, avatar, rate_limit_rpm FROM users WHERE email = ? AND deleted_at IS NULL",
    )
    .bind(email)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    row.as_ref().map(map_user_row).transpose()
}

/// 改自己的邮箱；与他人冲突则 `EmailTaken`。同一地址视为成功（幂等）。
pub async fn set_email(
    conn: &mut SqliteConnection,
    user_id: i64,
    email: &str,
) -> Result<(), StoreError> {
    let email = normalize_email(email);
    if email.is_empty() || !email.contains('@') {
        return Err(StoreError::InvalidResource("邮箱不合法".to_string()));
    }
    if let Some(existing) = get_user_by_email_on_conn(conn, &email).await? {
        if existing.id == user_id {
            return Ok(());
        }
        return Err(StoreError::EmailTaken);
    }
    let result = sqlx::query("UPDATE users SET email = ? WHERE id = ?")
        .bind(&email)
        .bind(user_id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    if result.rows_affected() == 0 {
        return Err(StoreError::UserNotFound(user_id));
    }
    Ok(())
}

/// 当前口令是否匹配；用户不存在、尚未设密或错误一律 `false`（不枚举原因）。
pub async fn password_matches(
    pool: &SqlitePool,
    user_id: i64,
    password: &str,
) -> Result<bool, StoreError> {
    let mut conn = pool.acquire().await.map_err(StoreError::Query)?;
    password_matches_on_conn(&mut conn, user_id, password).await
}

/// 在现有连接上校验当前口令，供凭据更新事务复用同一连接。
pub async fn password_matches_on_conn(
    conn: &mut SqliteConnection,
    user_id: i64,
    password: &str,
) -> Result<bool, StoreError> {
    let row = sqlx::query("SELECT password_hash FROM users WHERE id = ? AND deleted_at IS NULL")
        .bind(user_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    let Some(row) = row else {
        return Ok(false);
    };
    let password_hash: Option<String> = row.try_get("password_hash").map_err(StoreError::Query)?;
    let Some(password_hash) = password_hash else {
        return Ok(false);
    };
    verify_password(password, &password_hash).await
}

/// 写入密码哈希；空密码拒绝。
pub async fn set_password(
    conn: &mut SqliteConnection,
    user_id: i64,
    password: &str,
) -> Result<(), StoreError> {
    let password_hash = hash_password(password).await?;
    let result = sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
        .bind(&password_hash)
        .bind(user_id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    if result.rows_affected() == 0 {
        return Err(StoreError::UserNotFound(user_id));
    }
    Ok(())
}

/// 改角色。目标为非 root 且该用户是最后一个启用 root 时拒绝。
pub async fn set_user_role(
    conn: &mut SqliteConnection,
    user_id: i64,
    role: ManagementRole,
) -> Result<(), StoreError> {
    let Some(current) = get_user_on_conn(conn, user_id).await? else {
        return Err(StoreError::UserNotFound(user_id));
    };
    if current.role == ManagementRole::Root && role != ManagementRole::Root {
        protect_last_root(conn, user_id).await?;
    }
    sqlx::query("UPDATE users SET role = ? WHERE id = ?")
        .bind(role.as_str())
        .bind(user_id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 启停。禁用最后一个启用 root 时拒绝。
pub async fn set_user_enabled(
    conn: &mut SqliteConnection,
    user_id: i64,
    enabled: bool,
) -> Result<(), StoreError> {
    let Some(current) = get_user_on_conn(conn, user_id).await? else {
        return Err(StoreError::UserNotFound(user_id));
    };
    if current.role == ManagementRole::Root && !enabled {
        protect_last_root(conn, user_id).await?;
    }
    sqlx::query("UPDATE users SET enabled = ? WHERE id = ?")
        .bind(enabled)
        .bind(user_id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 软删除用户：停用 + 归档 + 释放邮箱 + 吊销会话。最后一个启用 root 拒绝。
///
/// 不做硬删除：`request_log` 按 `user_id` 记归属，删掉用户行会让历史消费记录变成
/// 孤儿；`tokens.user_id` 又是无级联外键，硬删会直接撞 FOREIGN KEY 约束。
///
/// 令牌行不动：`authenticate` 与 `token_group_assigned` 都查快照里的用户，归档用户
/// 不进快照，其令牌随即失效，而 `token_key → user_id` 关联保留，日志归属不丢。
///
/// 邮箱改写为 `deleted.{id}.{原邮箱}`，把原地址放回可注册状态（列级 UNIQUE 无法
/// 改成 partial index，见 migration 0027）。已归档则幂等返回。
pub async fn delete_user(
    conn: &mut SqliteConnection,
    user_id: i64,
    now: i64,
) -> Result<(), StoreError> {
    let Some(current) = get_user_on_conn(conn, user_id).await? else {
        return Ok(());
    };
    if current.role == ManagementRole::Root {
        protect_last_root(conn, user_id).await?;
    }
    sqlx::query(
        "UPDATE users \
         SET deleted_at = ?, enabled = 0, email = 'deleted.' || id || '.' || email \
         WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(now)
    .bind(user_id)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    // 外键级联不再触发（行还在），必须显式吊销会话，否则归档用户的浏览器仍能用。
    sqlx::query("UPDATE management_sessions SET revoked = 1 WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

pub(crate) async fn get_user_on_conn(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<Option<UserRecord>, StoreError> {
    let row = sqlx::query(
        "SELECT id, email, display_name, role, enabled, avatar, rate_limit_rpm FROM users WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    row.as_ref().map(map_user_row).transpose()
}

async fn protect_last_root(conn: &mut SqliteConnection, user_id: i64) -> Result<(), StoreError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users \
         WHERE role = 'root' AND enabled != 0 AND deleted_at IS NULL AND id != ?",
    )
    .bind(user_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    if count == 0 {
        return Err(StoreError::LastRootProtected);
    }
    Ok(())
}

/// 校验邮箱密码：用户不存在、禁用、无密码或密码错误一律 `None`（避免枚举账号）。
pub async fn authenticate_password(
    pool: &SqlitePool,
    email: &str,
    password: &str,
) -> Result<Option<UserRecord>, StoreError> {
    let email = normalize_email(email);
    let row = sqlx::query(
        "SELECT id, email, display_name, role, enabled, avatar, rate_limit_rpm, password_hash FROM users WHERE email = ? AND deleted_at IS NULL",
    )
    .bind(&email)
    .fetch_optional(pool)
    .await
    .map_err(StoreError::Query)?;
    let Some(row) = row else {
        // 也走一次同参数校验：否则不存在的账号 ~1ms 返回、存在的 ~80ms，攻击者据此
        // 就能枚举账号（响应体一致挡不住时序侧信道）。
        verify_password(password, &TIMING_EQUALIZER_HASH).await?;
        return Ok(None);
    };
    let enabled: i64 = row.try_get("enabled").map_err(StoreError::Query)?;
    if enabled == 0 {
        verify_password(password, &TIMING_EQUALIZER_HASH).await?;
        return Ok(None);
    }
    let password_hash: Option<String> = row.try_get("password_hash").map_err(StoreError::Query)?;
    let Some(password_hash) = password_hash else {
        verify_password(password, &TIMING_EQUALIZER_HASH).await?;
        return Ok(None);
    };
    if !verify_password(password, &password_hash).await? {
        return Ok(None);
    }
    Ok(Some(map_user_row(&row)?))
}

/// 修改用户头像（data URL 或图片 URL；None 表示清除头像）。
pub async fn set_avatar(
    conn: &mut SqliteConnection,
    user_id: i64,
    avatar: Option<&str>,
) -> Result<(), StoreError> {
    if let Some(avatar) = avatar {
        validate_avatar(avatar)?;
    }
    let result = sqlx::query("UPDATE users SET avatar = ? WHERE id = ?")
        .bind(avatar)
        .bind(user_id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    if result.rows_affected() == 0 {
        return Err(StoreError::UserNotFound(user_id));
    }
    Ok(())
}

/// 头像 data URL 的长度上限（字符）。
///
/// 256×256 的 PNG/WebP 经 base64 后通常在 100KB 以内；留到 256KB 有富余，同时
/// 挡住「原图直传」——那既会撑大 users 表，也会让每次读用户都拖着几 MB。
const AVATAR_MAX_LEN: usize = 256 * 1024;

/// 校验头像取值：只收固定几种图片的 data URL，并限长。
///
/// 后端必须自己校验：前端的 `accept="image/*"` 与 `file.type` 都来自浏览器，
/// 直接调接口可以绕开。
fn validate_avatar(avatar: &str) -> Result<(), StoreError> {
    const ALLOWED_PREFIXES: [&str; 4] = [
        "data:image/png;base64,",
        "data:image/jpeg;base64,",
        "data:image/webp;base64,",
        "data:image/gif;base64,",
    ];
    if !ALLOWED_PREFIXES
        .iter()
        .any(|prefix| avatar.starts_with(prefix))
    {
        return Err(StoreError::InvalidResource(
            "头像须为 png/jpeg/webp/gif 的 data URL".to_string(),
        ));
    }
    if avatar.len() > AVATAR_MAX_LEN {
        return Err(StoreError::InvalidResource(format!(
            "头像过大（上限 {} KB），请压缩后再上传",
            AVATAR_MAX_LEN / 1024
        )));
    }
    Ok(())
}

/// 设置用户每分钟请求上限（RPM）。`None` 或 `0` 表示不限制。
pub async fn set_rate_limit_rpm(
    conn: &mut SqliteConnection,
    user_id: i64,
    rate_limit_rpm: Option<u64>,
) -> Result<(), StoreError> {
    let rpm_val = rate_limit_rpm_to_db(rate_limit_rpm)?;
    let result = sqlx::query("UPDATE users SET rate_limit_rpm = ? WHERE id = ?")
        .bind(rpm_val)
        .bind(user_id)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Query)?;
    if result.rows_affected() == 0 {
        return Err(StoreError::UserNotFound(user_id));
    }
    Ok(())
}

/// 把 API 的无符号 RPM 安全转换成 SQLite 的有符号整数表示。
fn rate_limit_rpm_to_db(rate_limit_rpm: Option<u64>) -> Result<Option<i64>, StoreError> {
    rate_limit_rpm
        .map(|rpm| {
            i64::try_from(rpm).map_err(|_| {
                StoreError::InvalidResource("rate_limit_rpm 超出数据库整数范围".to_string())
            })
        })
        .transpose()
}

/// 把数据库整数还原成 API 的无符号 RPM；负值表示数据库已损坏，不能静默当作不限速。
fn rate_limit_rpm_from_db(rate_limit_rpm: Option<i64>) -> Result<Option<u64>, StoreError> {
    rate_limit_rpm
        .map(|rpm| {
            u64::try_from(rpm).map_err(|_| {
                StoreError::InvalidResource("数据库中的 rate_limit_rpm 为负数".to_string())
            })
        })
        .transpose()
}

/// 签发会话：返回明文令牌（只此一次）与过期时间（unix 毫秒）。
pub async fn issue_session(
    conn: &mut SqliteConnection,
    user_id: i64,
    now: i64,
) -> Result<(String, i64), StoreError> {
    let token = new_session_token();
    let token_hash = hash_session_token(&token);
    let expires_at = now.saturating_add(SESSION_TTL_MS);
    sqlx::query(
        "INSERT INTO management_sessions (token_hash, user_id, created_at, expires_at, revoked) \
         VALUES (?, ?, ?, ?, 0)",
    )
    .bind(&token_hash)
    .bind(user_id)
    .bind(now)
    .bind(expires_at)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Query)?;
    Ok((token, expires_at))
}

/// 会话查询结果：把「可能在猜凭证」与「正常生命周期结束」分开。
///
/// 会话到期是每 8 小时必然发生一次的事，不该和爆破共用同一个失败计数器——多开几个
/// 标签页过夜就能把自己所在 IP 的登录也一起限掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLookup {
    /// 会话有效，用户可用。
    Valid(UserRecord),
    /// 形态正确但查不到该哈希：可能在猜，计入认证失败限流。
    Unknown,
    /// 缺失、前缀错误或形态错误，不查询数据库也不计入认证失败限流。
    Malformed,
    /// 会话确实存在，但已过期/被吊销/用户已停用或归档：不计入限流。
    Inactive,
}

/// 按明文会话令牌取出仍有效的用户。
pub async fn user_for_session(
    pool: &SqlitePool,
    session_token: &str,
    now: i64,
) -> Result<SessionLookup, StoreError> {
    let Some(suffix) = session_token.strip_prefix(SESSION_TOKEN_PREFIX) else {
        return Ok(SessionLookup::Malformed);
    };
    if suffix.len() != 64
        || !suffix
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Ok(SessionLookup::Malformed);
    }
    let token_hash = hash_session_token(session_token);
    let row = sqlx::query(
        "SELECT u.id, u.email, u.display_name, u.role, u.enabled, u.avatar, u.rate_limit_rpm, \
                u.deleted_at, s.expires_at, s.revoked \
         FROM management_sessions s \
         JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = ?",
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .map_err(StoreError::Query)?;
    let Some(row) = row else {
        return Ok(SessionLookup::Unknown);
    };
    let revoked: i64 = row.try_get("revoked").map_err(StoreError::Query)?;
    let expires_at: i64 = row.try_get("expires_at").map_err(StoreError::Query)?;
    let enabled: i64 = row.try_get("enabled").map_err(StoreError::Query)?;
    let deleted_at: Option<i64> = row.try_get("deleted_at").map_err(StoreError::Query)?;
    if revoked != 0 || expires_at <= now || enabled == 0 || deleted_at.is_some() {
        return Ok(SessionLookup::Inactive);
    }
    Ok(SessionLookup::Valid(map_user_row(&row)?))
}

/// 吊销某用户的会话；`keep` 给出要保留的明文令牌（改自己的密码时保住当前这条）。
///
/// 改密码/改邮箱后必须调用：否则已被窃取的会话在改密后仍有效整整 8 小时。
pub async fn revoke_user_sessions(
    conn: &mut SqliteConnection,
    user_id: i64,
    keep: Option<&str>,
) -> Result<(), StoreError> {
    match keep {
        Some(token) if token.starts_with(SESSION_TOKEN_PREFIX) => {
            sqlx::query(
                "UPDATE management_sessions SET revoked = 1 \
                 WHERE user_id = ? AND token_hash != ?",
            )
            .bind(user_id)
            .bind(hash_session_token(token))
            .execute(&mut *conn)
            .await
            .map_err(StoreError::Query)?;
        }
        _ => {
            sqlx::query("UPDATE management_sessions SET revoked = 1 WHERE user_id = ?")
                .bind(user_id)
                .execute(&mut *conn)
                .await
                .map_err(StoreError::Query)?;
        }
    }
    Ok(())
}

/// 删除已失效且超过保留窗口的会话行，返回清理条数。
///
/// 失效行要保留一个会话 TTL：否则 GC 后同一枚旧 Bearer 会从 `Inactive` 重新变成
/// `Unknown`，再次消耗认证失败限流。保留窗口结束后才允许彻底删除。
pub async fn purge_expired_sessions(pool: &SqlitePool, now: i64) -> Result<u64, StoreError> {
    let retention_cutoff = now.saturating_sub(SESSION_TTL_MS);
    let result = sqlx::query("DELETE FROM management_sessions WHERE expires_at <= ?")
        .bind(retention_cutoff)
        .execute(pool)
        .await
        .map_err(StoreError::Query)?;
    Ok(result.rows_affected())
}

/// 每日清理过期或已吊销的管理会话。
///
/// 首次清理由进程启动路径同步执行；循环先等待一天，避免启动时重复清理。
pub async fn run_session_cleanup_loop(pool: SqlitePool) {
    const CLEANUP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
    loop {
        tokio::time::sleep(CLEANUP_INTERVAL).await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);
        if let Err(err) = purge_expired_sessions(&pool, now).await {
            crate::store::record_system_error(
                &pool,
                "auth",
                &format!("管理会话定时清理失败: {err}"),
            )
            .await;
        }
    }
}

/// 吊销会话；不存在视为成功。
pub async fn revoke_session(pool: &SqlitePool, session_token: &str) -> Result<(), StoreError> {
    if !session_token.starts_with(SESSION_TOKEN_PREFIX) {
        return Ok(());
    }
    let token_hash = hash_session_token(session_token);
    sqlx::query("UPDATE management_sessions SET revoked = 1 WHERE token_hash = ?")
        .bind(&token_hash)
        .execute(pool)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// 内置 root 的 id。
pub fn root_user_id() -> i64 {
    ROOT_USER_ID
}

/// 抹平时序差用的固定 Argon2 哈希，参数与真实口令一致。
///
/// 首次用到时算一次（约 80ms），之后复用。
static TIMING_EQUALIZER_HASH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
    let salt = SaltString::generate(&mut OsRng);
    // 固定长度口令 + 生成的盐，Argon2 默认参数下不会失败。
    Argon2::default()
        .hash_password(b"kairos-timing-equalizer", &salt)
        .expect("默认参数下固定口令的 Argon2 哈希不会失败")
        .to_string()
});

async fn hash_password(password: &str) -> Result<String, StoreError> {
    let password = password.to_string();
    if password.trim().is_empty() {
        return Err(StoreError::InvalidResource("密码不能为空".to_string()));
    }
    if password.len() < 8 {
        return Err(StoreError::InvalidResource("密码至少 8 个字符".to_string()));
    }
    tokio::task::spawn_blocking(move || {
        use argon2::Argon2;
        use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| StoreError::PasswordHash)
    })
    .await
    .map_err(|_| StoreError::PasswordHash)?
}

async fn verify_password(password: &str, password_hash: &str) -> Result<bool, StoreError> {
    let password = password.to_string();
    let password_hash = password_hash.to_string();
    tokio::task::spawn_blocking(move || {
        use argon2::Argon2;
        use argon2::password_hash::{PasswordHash, PasswordVerifier};
        let Ok(parsed) = PasswordHash::new(&password_hash) else {
            return Ok(false);
        };
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok())
    })
    .await
    .map_err(|_| StoreError::PasswordHash)?
}

fn new_session_token() -> String {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("{SESSION_TOKEN_PREFIX}{}", hex_encode(&bytes))
}

fn hash_session_token(session_token: &str) -> String {
    hex_encode(Sha256::digest(session_token.as_bytes()).as_ref())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 用户级聚合统计：请求次数、Token 消耗以及最后活跃时间。
#[derive(Debug, Clone, Default)]
pub struct UserStatsRecord {
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub last_used_at: Option<i64>,
}

/// 批量查出所有用户的聚合统计（按 user_id 汇总）。
///
/// 用量按 `request_log.user_id` 聚合而非 JOIN `tokens`：令牌删除后历史用量仍归属该
/// 用户。`last_used_at` 仍取自现存令牌——它是「当前凭证的活跃度」，删掉的令牌不该
/// 继续把用户显示为活跃。
pub async fn list_users_stats(
    pool: &SqlitePool,
) -> Result<std::collections::HashMap<i64, UserStatsRecord>, StoreError> {
    let usage_rows = sqlx::query(
        "SELECT user_id, \
                COUNT(DISTINCT COALESCE(request_id, CAST(id AS TEXT))) AS request_count, \
                COALESCE(SUM(input_tokens), 0) AS input_tokens, \
                COALESCE(SUM(output_tokens), 0) AS output_tokens \
         FROM request_log \
         GROUP BY user_id",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;

    let mut map: std::collections::HashMap<i64, UserStatsRecord> =
        std::collections::HashMap::with_capacity(usage_rows.len());
    for row in usage_rows {
        let user_id: i64 = row.try_get("user_id").map_err(StoreError::Query)?;
        let request_count: i64 = row.try_get("request_count").map_err(StoreError::Query)?;
        let input_tokens: i64 = row.try_get("input_tokens").map_err(StoreError::Query)?;
        let output_tokens: i64 = row.try_get("output_tokens").map_err(StoreError::Query)?;
        map.insert(
            user_id,
            UserStatsRecord {
                request_count: request_count.max(0) as u64,
                input_tokens: input_tokens.max(0) as u64,
                output_tokens: output_tokens.max(0) as u64,
                last_used_at: None,
            },
        );
    }

    let last_used_rows = sqlx::query(
        "SELECT user_id, MAX(last_used_at) AS last_used_at FROM tokens GROUP BY user_id",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::Query)?;
    for row in last_used_rows {
        let user_id: i64 = row.try_get("user_id").map_err(StoreError::Query)?;
        let last_used_at: Option<i64> = row.try_get("last_used_at").map_err(StoreError::Query)?;
        map.entry(user_id).or_default().last_used_at = last_used_at;
    }
    Ok(map)
}

/// 单个用户的聚合统计。
pub async fn get_user_stats(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<UserStatsRecord, StoreError> {
    let usage = sqlx::query(
        "SELECT COUNT(DISTINCT COALESCE(request_id, CAST(id AS TEXT))) AS request_count, \
                COALESCE(SUM(input_tokens), 0) AS input_tokens, \
                COALESCE(SUM(output_tokens), 0) AS output_tokens \
         FROM request_log WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(StoreError::Query)?;
    let request_count: i64 = usage.try_get("request_count").map_err(StoreError::Query)?;
    let input_tokens: i64 = usage.try_get("input_tokens").map_err(StoreError::Query)?;
    let output_tokens: i64 = usage.try_get("output_tokens").map_err(StoreError::Query)?;

    let last_used_at: Option<i64> =
        sqlx::query_scalar("SELECT MAX(last_used_at) FROM tokens WHERE user_id = ?")
            .bind(user_id)
            .fetch_one(pool)
            .await
            .map_err(StoreError::Query)?;

    Ok(UserStatsRecord {
        request_count: request_count.max(0) as u64,
        input_tokens: input_tokens.max(0) as u64,
        output_tokens: output_tokens.max(0) as u64,
        last_used_at,
    })
}
