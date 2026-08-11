//! 静态配置解析：单个 JSON 文件，无热重载，重启生效。
//!
//! 配置内的相对路径（如 `database.path`）相对配置文件所在目录解析。

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

/// 默认配置文件路径，相对当前工作目录。
pub const DEFAULT_CONFIG_PATH: &str = ".kairos/config.json";

/// 网关静态配置，覆盖 spec 的 v1 schema 全部字段。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: Listen,
    pub database: Database,
    #[serde(default)]
    pub logging: Logging,
    #[serde(default)]
    pub tokens: Vec<Token>,
    #[serde(default)]
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub prices: Prices,
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

/// 日志开关。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Logging {
    /// 是否落完整请求/响应 body，默认关闭。
    #[serde(default)]
    pub full_body: bool,
}

/// 下游令牌：认证与计费的最小单位。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Token {
    pub key: String,
    pub name: String,
    /// 累计结算上限（USD）；缺省表示无上限。单位在计费票据中换算为 micro-USD。
    #[serde(default)]
    pub limit_usd: Option<f64>,
    /// 初始余额（USD），缺省 0。
    #[serde(default)]
    pub balance_usd: f64,
}

/// 出站 wire 协议。
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum Protocol {
    #[serde(rename = "openai_chat")]
    OpenAiChat,
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
}

/// 渠道：指向一个上游端点的出站接入单元。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,
    pub priority: u32,
    pub weight: u32,
    pub timeout_ms: u64,
    pub max_retries: u32,
}

/// 价格表：模型名 → 四档 USD/$1M tokens 单价。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, transparent)]
pub struct Prices(pub HashMap<String, Price>);

/// 单模型单价。缓存档缺省时回退 `input` 价（在计费票据中处理）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
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

    /// 全字段配置可解析，相对路径相对配置文件目录解析。
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
                "logging": {{ "full_body": true }},
                "tokens": [{{ "key": "sk-x", "name": "dev", "limit_usd": 50.0, "balance_usd": 5.0 }}],
                "channels": [{{
                    "name": "c", "protocol": "openai_chat",
                    "base_url": "https://api.openai.com/v1", "api_key": "k",
                    "models": ["gpt-4o"], "model_aliases": {{ "fast": "gpt-4o-mini" }},
                    "priority": 1, "weight": 1, "timeout_ms": 120000, "max_retries": 2
                }}],
                "prices": {{ "gpt-4o": {{ "input": 2.5, "output": 10.0, "cache_read": 1.25, "cache_write": 10.0 }} }}
            }}"#
        )
        .expect("应能写入配置");
        drop(f);

        let cfg = Config::load(&cfg_path).expect("配置应可解析");

        assert_eq!(cfg.listen.host, "127.0.0.1");
        assert_eq!(cfg.listen.port, 8787);
        assert!(cfg.logging.full_body);
        assert_eq!(cfg.tokens.len(), 1);
        assert_eq!(cfg.channels.len(), 1);
        assert_eq!(cfg.channels[0].protocol, Protocol::OpenAiChat);
        assert_eq!(cfg.channels[0].model_aliases["fast"], "gpt-4o-mini");
        assert_eq!(cfg.prices.0["gpt-4o"].input, 2.5);
        // 相对路径已相对配置文件目录解析。
        assert_eq!(cfg.database.path, dir.path().join("kairos.db"));
    }

    /// 缺省字段（logging/tokens/channels/prices）有合理默认值。
    #[test]
    fn load_minimal_config_applies_defaults() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(
            &cfg_path,
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"}}"#,
        )
        .expect("应能写配置");
        let cfg = Config::load(&cfg_path).expect("最小配置应可解析");

        assert!(!cfg.logging.full_body);
        assert!(cfg.tokens.is_empty());
        assert!(cfg.channels.is_empty());
        assert!(cfg.prices.0.is_empty());
    }

    /// 未知字段报错，避免静默漏配。
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

    /// 缺失必需字段报错。
    #[test]
    fn missing_field_is_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(&cfg_path, r#"{"listen":{"host":"0.0.0.0","port":1}}"#).expect("应能写配置");
        assert!(Config::load(&cfg_path).is_err(), "缺失必需字段应报错");
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

    /// 令牌的 limit_usd/balance_usd 可选：缺省分别为无上限与 0。
    #[test]
    fn token_limit_and_balance_are_optional() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(
            &cfg_path,
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"},"tokens":[{"key":"sk-x","name":"dev"}]}"#,
        )
        .expect("应能写配置");
        let cfg = Config::load(&cfg_path).expect("缺可选字段的令牌应可解析");
        assert_eq!(cfg.tokens[0].limit_usd, None, "缺 limit_usd 应为无上限");
        assert_eq!(cfg.tokens[0].balance_usd, 0.0, "缺 balance_usd 应默认 0");
    }
}
