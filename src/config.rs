//! 静态配置解析：单个 JSON 文件，无热重载，重启生效。
//!
//! v2 起配置文件退化为纯静态引导：只承载协议监听地址、数据库路径、可选的管理
//! 监听地址，以及**仅首次**写入内置 root 的邮箱/登录密码。运行时资源（渠道、
//! 令牌、价格、开关、用户）全部在 SQLite；配置文件中的旧资源段
//! （`tokens`/`channels`/`prices`/`logging`）整体移除，检测到废弃字段直接报错。
//!
//! `admin_email` / `admin_password` 不是长期有效的机器凭证，也不能当作管理 API
//! 的 Bearer。它们只在内置 root（`users.id = 1`）的 `password_hash` 仍为 NULL
//! 时作为种子：缺省或空白则启动时生成，写入库后以库为准，后续启动忽略配置、
//! 也不把生成值写回本文件。
//!
//! 配置内的相对路径（如 `database.path`）相对配置文件所在目录解析。

use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

/// 默认配置文件路径，相对当前工作目录。
pub const DEFAULT_CONFIG_PATH: &str = ".kairos/config.json";

/// 网关静态配置：仅引导字段，运行期资源从数据库加载。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: Listen,
    pub database: Database,
    /// 首次为内置 root 播种用的登录邮箱。缺省、空串或纯空白视为未提供（启动时生成）。
    ///
    /// `#[serde(default)]`：JSON 里没写这个键时是 `None`，与空串走同一条「未提供」路径。
    /// 已有 `password_hash` 后此字段被忽略，避免重启用配置覆盖运营改过的邮箱。
    #[serde(default)]
    pub admin_email: Option<String>,
    /// 首次为内置 root 播种用的 **Web UI 登录密码**。缺省、空串或纯空白视为未提供。
    ///
    /// 只用于登录换会话，绝不能作为 `Authorization: Bearer` 调管理 API。旧字段名
    /// `admin_key` 被 `deny_unknown_fields` 拒绝，防止继续当静态管理密钥用。
    #[serde(default)]
    pub admin_password: Option<String>,
    /// 可选的管理监听地址；配置了才启动管理面，否则管理 API 整体关闭。
    #[serde(default)]
    pub admin_listen: Option<Listen>,
}

/// HTTP 监听地址。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listen {
    pub host: String,
    pub port: u16,
}

/// SQLite 数据库文件位置。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Database {
    /// 加载后相对路径已相对配置文件目录解析；配置文件路径本身相对时结果仍为相对路径。
    pub path: PathBuf,
}

/// wire 协议：三种出站/入站协议共用同一枚举。
#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq)]
pub enum Protocol {
    #[serde(rename = "openai_chat")]
    OpenAiChat,
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
}

/// 配置解析错误，向上抛给应用边界。
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("读取配置文件 {path} 失败: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("解析配置文件 {path} 失败: {source}")]
    Parse {
        path: String,
        source: serde_json::Error,
    },
    #[error("配置文件 {path} 无效: {message}")]
    Invalid { path: String, message: String },
}

/// 把「未写 / 空串 / 纯空白」收成 `None`，与「缺省即生成」的种子约定对齐。
///
/// 不在这里 trim 非空密码的首尾空白：若运营有意写了带空格的口令，应原样交给哈希。
fn blank_to_none(value: Option<String>) -> Option<String> {
    value.filter(|raw| !raw.trim().is_empty())
}

impl Config {
    /// 从 `path` 加载配置，并把相对路径解析为相对配置文件目录的绝对路径。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let mut config: Self = serde_json::from_str(&raw).map_err(|source| ConfigError::Parse {
            path: path.display().to_string(),
            source,
        })?;
        // 空串与缺省同义，避免 JSON 里写了 `"admin_password": ""` 却被当成「已配置的空口令」。
        config.admin_email = blank_to_none(config.admin_email);
        config.admin_password = blank_to_none(config.admin_password);
        config.resolve_paths(path);
        Ok(config)
    }

    /// 把配置内的相对路径相对配置文件所在目录解析。
    fn resolve_paths(&mut self, config_path: &Path) {
        let base = config_path.parent().unwrap_or(Path::new("."));
        if self.database.path.is_relative() {
            self.database.path = base.join(&self.database.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 全字段（含可选管理监听与种子邮箱/密码）配置可解析，相对路径相对配置文件目录解析。
    #[test]
    fn load_full_config_and_resolve_relative_path() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        let mut f = std::fs::File::create(&cfg_path).expect("应能写配置文件");
        write!(
            f,
            r#"{{
                "listen": {{ "host": "127.0.0.1", "port": 8787 }},
                "database": {{ "path": "./kairos.db" }},
                "admin_email": "root@example.com",
                "admin_password": "sk-admin",
                "admin_listen": {{ "host": "127.0.0.1", "port": 8788 }}
            }}"#
        )
        .expect("应能写入配置");
        drop(f);

        let cfg = Config::load(&cfg_path).expect("配置应可解析");

        assert_eq!(cfg.listen.host, "127.0.0.1");
        assert_eq!(cfg.listen.port, 8787);
        assert_eq!(cfg.admin_email.as_deref(), Some("root@example.com"));
        assert_eq!(cfg.admin_password.as_deref(), Some("sk-admin"));
        let admin = cfg.admin_listen.expect("管理监听应可解析");
        assert_eq!(admin.port, 8788);
        // 相对路径已相对配置文件目录解析。
        assert_eq!(cfg.database.path, dir.path().join("kairos.db"));
    }

    /// 缺省的管理监听地址：未配置即管理面关闭（`None`）。
    #[test]
    fn admin_listen_omitted_is_off() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(
            &cfg_path,
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"}}"#,
        )
        .expect("应能写配置");
        let cfg = Config::load(&cfg_path).expect("最小配置应可解析");
        assert!(cfg.admin_listen.is_none(), "缺管理监听应为关闭");
        assert!(cfg.admin_email.is_none());
        assert!(cfg.admin_password.is_none());
    }

    /// 未知字段报错，避免静默漏配。旧的 `admin_key` 也走这条路，迫使改名而不是继续当 Bearer。
    #[test]
    fn unknown_field_is_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(
            &cfg_path,
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"},"bogus":1}"#,
        )
        .expect("应能写配置");
        assert!(Config::load(&cfg_path).is_err(), "未知字段应报错");
    }

    /// 已废弃的静态管理密钥：配置里再写 `admin_key` 必须失败，不能再当 Bearer 用。
    #[test]
    fn legacy_admin_key_field_is_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(
            &cfg_path,
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"},"admin_key":"k"}"#,
        )
        .expect("应能写配置");
        let err = Config::load(&cfg_path).expect_err("admin_key 应被拒绝");
        match err {
            ConfigError::Parse { source, .. } => {
                let message = source.to_string();
                assert!(
                    message.contains("admin_key"),
                    "错误应点明 admin_key，实际 {message}"
                );
            }
            other => panic!("应报 Parse 错误，实际 {other:?}"),
        }
    }

    /// 已废弃的资源段（v1 的 tokens/channels/prices/logging）出现在配置中直接报错，
    /// 不做兼容迁移。
    #[test]
    fn deprecated_resource_segments_are_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let base = r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db""#;
        for (name, extra) in [
            ("tokens", r#","tokens":[]}"#),
            ("channels", r#","channels":[]}"#),
            ("prices", r#","prices":[]}"#),
            ("logging", r#","logging":{"full_body":false}}"#),
        ] {
            let cfg_path = dir.path().join(format!("{name}.json"));
            std::fs::write(&cfg_path, format!("{base}{extra}")).expect("应能写配置");
            assert!(
                Config::load(&cfg_path).is_err(),
                "v1 废弃资源段 {name} 应报错而非静默忽略"
            );
        }
    }

    /// 缺失必需字段（listen）报错；种子字段可缺。
    #[test]
    fn missing_field_is_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(&cfg_path, r#"{"listen":{"host":"0.0.0.0","port":1}}"#).expect("应能写配置");
        assert!(Config::load(&cfg_path).is_err(), "缺失 database 应报错");
    }

    /// 配置文件的缺失报可读错误。
    #[test]
    fn missing_file_is_readable_error() {
        let err = Config::load(Path::new("/nonexistent/config.json")).expect_err("缺失文件应报错");
        match err {
            ConfigError::Read { .. } => {}
            other => panic!("应报 Read 错误，实际 {other:?}"),
        }
    }

    /// 空串与缺省同属「未提供」，启动时按生成路径走，而不是当成已配置的空口令。
    #[test]
    fn empty_admin_seed_fields_are_missing() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(
            &cfg_path,
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"},"admin_email":"","admin_password":"   "}"#,
        )
        .expect("应能写配置");
        let cfg = Config::load(&cfg_path).expect("空种子字段应可解析为缺省");
        assert!(cfg.admin_email.is_none(), "空邮箱应视为未提供");
        assert!(cfg.admin_password.is_none(), "空白密码应视为未提供");
    }
}
