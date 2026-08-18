//! 静态配置解析：单个 JSON 文件，无热重载，重启生效。
//!
//! v2 起配置文件退化为纯静态引导：只承载协议监听地址、数据库路径、admin key 与
//! 可选的管理监听地址。运行时资源（渠道、令牌、价格、开关）全部移入 SQLite，
//! 配置文件中的旧资源段（`tokens`/`channels`/`prices`/`logging`）整体移除；
//! 检测到废弃字段直接报错，不做兼容迁移。
//!
//! 配置内的相对路径（如 `database.path`）相对配置文件所在目录解析。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 默认配置文件路径，相对当前工作目录。
pub const DEFAULT_CONFIG_PATH: &str = ".kairos/config.json";

/// 网关静态配置：仅引导字段，运行期资源从数据库加载。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: Listen,
    pub database: Database,
    /// 管理 API 静态密钥（Bearer 认证）；必须非空（trim 后），未配置管理监听时虽不生效，仍为必填，避免形态漂移。
    pub admin_key: String,
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
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
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
        if config.admin_key.trim().is_empty() {
            return Err(ConfigError::Invalid {
                path: path.display().to_string(),
                message: "admin_key 不能为空".to_string(),
            });
        }
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

    /// 全字段（含可选管理监听）配置可解析，相对路径相对配置文件目录解析。
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
                "admin_key": "sk-admin",
                "admin_listen": {{ "host": "127.0.0.1", "port": 8788 }}
            }}"#
        )
        .expect("应能写入配置");
        drop(f);

        let cfg = Config::load(&cfg_path).expect("配置应可解析");

        assert_eq!(cfg.listen.host, "127.0.0.1");
        assert_eq!(cfg.listen.port, 8787);
        assert_eq!(cfg.admin_key, "sk-admin");
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
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"},"admin_key":"k"}"#,
        )
        .expect("应能写配置");
        let cfg = Config::load(&cfg_path).expect("最小配置应可解析");
        assert!(cfg.admin_listen.is_none(), "缺管理监听应为关闭");
    }

    /// 未知字段报错，避免静默漏配。
    #[test]
    fn unknown_field_is_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(
            &cfg_path,
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"},"admin_key":"k","bogus":1}"#,
        )
        .expect("应能写配置");
        assert!(Config::load(&cfg_path).is_err(), "未知字段应报错");
    }

    /// 已废弃的资源段（v1 的 tokens/channels/prices/logging）出现在配置中直接报错，
    /// 不做兼容迁移。
    #[test]
    fn deprecated_resource_segments_are_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let base =
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"},"admin_key":"k""#;
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

    /// 缺失必需字段（admin_key）报错。
    #[test]
    fn missing_field_is_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(&cfg_path, r#"{"listen":{"host":"0.0.0.0","port":1}}"#).expect("应能写配置");
        assert!(Config::load(&cfg_path).is_err(), "缺失 admin_key 应报错");
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

    /// 空 admin_key 拒绝启动：否则 Bearer 后接空串即可通过管理面认证。
    #[test]
    fn empty_admin_key_is_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(
            &cfg_path,
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"},"admin_key":""}"#,
        )
        .expect("应能写配置");
        let err = Config::load(&cfg_path).expect_err("空 admin_key 应报错");
        match err {
            ConfigError::Invalid { message, .. } => {
                assert!(
                    message.contains("admin_key"),
                    "错误应点明 admin_key，实际 {message}"
                );
            }
            other => panic!("应报 Invalid 错误，实际 {other:?}"),
        }
    }

    /// 纯空白 admin_key 与空串同属未配置，拒绝启动。
    #[test]
    fn whitespace_admin_key_is_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(
            &cfg_path,
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"},"admin_key":"   "}"#,
        )
        .expect("应能写配置");
        assert!(Config::load(&cfg_path).is_err(), "空白 admin_key 应报错");
    }
}
