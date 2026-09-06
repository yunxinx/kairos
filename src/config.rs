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
    /// 管理面写请求同源守卫的追加受信来源（`scheme://host[:port]`，逐项精确匹配）。
    ///
    /// 供管理 SPA 与管理 API 不同源、但由服务端转发的拓扑放行浏览器写请求，典型是
    /// 本地前端开发服务器（`npm run dev` 起在 5173，把 `/api` 代理到 `admin_listen`）：
    /// 代理改写 Host 头却保留浏览器 Origin，同源比对必然失败。只放宽来源比对，
    /// 不放宽 Cookie 会话认证；空表（缺省）= 仅同源。这不是完整 CORS——没有
    /// 预检与跨源响应头，不经转发的跨源直连仍不可用。
    #[serde(default, deserialize_with = "deserialize_trusted_origins")]
    pub admin_trusted_origins: Vec<reqwest::Url>,
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

/// wire 协议：出站/入站协议共用同一枚举。
#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq)]
pub enum Protocol {
    #[serde(rename = "openai_chat")]
    OpenAiChat,
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
    #[serde(rename = "gemini")]
    Gemini,
}

/// 渠道级 reasoning 思维链兼容输出模式。
///
/// 控制两个方向：面向 chat 上游的请求编码是否把 IR Reasoning part 回写为
/// assistant `reasoning_content`（DeepSeek 系工具轮要求思维链随历史回放），
/// 以及 chat 下游的流式响应是否以 `delta.reasoning_content` 增量下发。
/// 缺省 `auto`：按出站模型名与渠道 base_url 命中 reasoning 厂商提示词表
/// 自动开启，存量渠道在名字不命中时行为不变。
#[derive(Debug, Clone, Copy, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningOutputMode {
    /// 按厂商提示词表自动判定。
    #[default]
    Auto,
    /// 强制开启，供改名后端渠道使用。
    Always,
    /// 强制关闭，杜绝别名误伤。
    Off,
}

impl ReasoningOutputMode {
    /// 该渠道在给定出站模型名下是否启用 reasoning 兼容输出。
    pub fn enables_reasoning_content(self, model: &str, base_url: &str) -> bool {
        match self {
            Self::Always => true,
            Self::Off => false,
            Self::Auto => {
                is_reasoning_vendor_identifier(model) || is_reasoning_vendor_identifier(base_url)
            }
        }
    }
}

/// reasoning 厂商提示词表：出站模型名或渠道 base_url 含这些子串（大小写
/// 不敏感）即视为把 `reasoning_content` 作为一等字段的厂商。
const REASONING_VENDOR_HINTS: &[&str] = &["deepseek", "mimo"];

fn is_reasoning_vendor_identifier(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    REASONING_VENDOR_HINTS
        .iter()
        .any(|hint| value.contains(hint))
}

/// 渠道级会话缓存键回写模式。
///
/// 控制面向 OpenAI Chat 上游的 IR 出站请求是否把网关解析出的会话标识
/// （显式 `x-kairos-session-id` 头优先，IR 稳定前缀哈希兜底）回写为
/// `prompt_cache_key`，让跨协议族的多轮请求也获得上游自动缓存的会话亲和。
/// 缺省 `off`：不改动出站请求，下游显式携带的缓存键照常透传。直通快路径
/// 字节直搬，不经过本开关。
#[derive(Debug, Clone, Copy, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionCacheKeyMode {
    /// 不回写。
    #[default]
    Off,
    /// 下游未显式携带 `prompt_cache_key` 时回写会话标识。
    Auto,
    /// 无条件回写，覆盖下游显式携带的 `prompt_cache_key`（供统一会话亲和
    /// 策略，如下游每轮发送随机键破坏亲和时）。
    Always,
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

/// 解析单个受信来源：必须是 `http(s)://host[:port]` 形态的纯源。
///
/// 逐项严卡（无路径/查询/片段/用户信息、仅 http/https、主机名不以点结尾）是
/// 刻意的：这个白名单直接决定哪些来源能携带 Cookie 发起写请求，宁可配置时
/// 多打几个字，也不留子串或通配匹配带来的误放行。配置加载与测试播种共用
/// 本函数，两条路径对「合法来源」只有一种定义。
pub fn parse_trusted_origin(raw: &str) -> Result<reqwest::Url, String> {
    let trimmed = raw.trim();
    let url = reqwest::Url::parse(trimmed)
        .map_err(|err| format!("受信来源 {raw:?} 不是合法 URL: {err}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("受信来源 {raw:?} 必须是 http/https 源"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("受信来源 {raw:?} 不应携带用户信息"));
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(format!(
            "受信来源 {raw:?} 只能是 scheme://host[:port]，不应带路径/查询/片段"
        ));
    }
    // 结尾点主机名（DNS 全称形态）能通过上面的全部检查，但浏览器 Origin 不发
    // 送结尾点，按 host_str 精确比对永不命中——按坏值拒绝，不给静默失效留口。
    if url.host_str().is_some_and(|host| host.ends_with('.')) {
        return Err(format!(
            "受信来源 {raw:?} 的主机名不应以点结尾（浏览器 Origin 不带结尾点，该配置永不匹配）"
        ));
    }
    Ok(url)
}

/// 反序列化时即校验每个受信来源：坏值让启动直接失败，而不是运行期静默失效。
fn deserialize_trusted_origins<'de, D>(deserializer: D) -> Result<Vec<reqwest::Url>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<String>::deserialize(deserializer)?;
    raw.into_iter()
        .map(|origin| parse_trusted_origin(&origin).map_err(serde::de::Error::custom))
        .collect()
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
        // 受信来源只服务管理面写请求守卫；管理面未启动时它是永远不生效的孤儿
        // 配置，按「避免静默漏配」直接报错，而不是启动后无声忽略。
        if config.admin_listen.is_none() && !config.admin_trusted_origins.is_empty() {
            return Err(ConfigError::Invalid {
                path: path.display().to_string(),
                message: "admin_trusted_origins 仅管理面生效，需同时配置 admin_listen".to_string(),
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

    /// 自动挡按出站模型名与渠道 base_url 判定（大小写不敏感子串）；always
    /// 无条件开启，off 无条件关闭。
    #[test]
    fn reasoning_output_mode_resolves_by_vendor_hints() {
        let cases: &[(&str, &str, bool)] = &[
            ("deepseek-chat", "http://localhost:1", true),
            ("DeepSeek-R1", "http://localhost:1", true),
            ("gpt-4o", "https://api.mimo.example/v1", true),
            ("gpt-4o", "http://localhost:1", false),
            ("xiaomi-mimo", "http://localhost:1", true),
        ];
        for (model, base_url, expected) in cases {
            assert_eq!(
                ReasoningOutputMode::Auto.enables_reasoning_content(model, base_url),
                *expected,
                "auto 判定 {model} @ {base_url}"
            );
        }
        assert!(ReasoningOutputMode::Always.enables_reasoning_content("gpt-4o", "http://x"));
        assert!(
            !ReasoningOutputMode::Off
                .enables_reasoning_content("deepseek-chat", "https://api.deepseek.com")
        );
    }

    /// 全字段（含可选管理监听、种子邮箱/密码与受信来源）配置可解析，相对路径相对配置文件目录解析。
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
                "admin_listen": {{ "host": "127.0.0.1", "port": 8788 }},
                "admin_trusted_origins": ["http://127.0.0.1:5173"]
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
        assert_eq!(cfg.admin_trusted_origins.len(), 1);
        assert_eq!(cfg.admin_trusted_origins[0].scheme(), "http");
        assert_eq!(cfg.admin_trusted_origins[0].host_str(), Some("127.0.0.1"));
        assert_eq!(cfg.admin_trusted_origins[0].port(), Some(5173));
        // 相对路径已相对配置文件目录解析。
        assert_eq!(cfg.database.path, dir.path().join("kairos.db"));
    }

    /// 缺省的管理监听地址：未配置即管理面关闭（`None`），受信来源为空表。
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
        assert!(cfg.admin_trusted_origins.is_empty(), "缺省受信来源应为空表");
    }

    /// 受信来源逐项严卡：非 http(s) 协议、带路径/查询、带用户信息、结尾点
    /// 主机名、非 URL 都让启动失败。
    #[test]
    fn trusted_origins_reject_non_origin_shapes() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let base = r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"},"admin_listen":{"host":"127.0.0.1","port":8788},"admin_trusted_origins":"#;
        for raw in [
            r#"["ftp://127.0.0.1:5173"]"#,
            r#"["http://127.0.0.1:5173/dev"]"#,
            r#"["http://user:pw@127.0.0.1:5173"]"#,
            r#"["https://ops.example.com."]"#,
            r#"["http://localhost.:5173"]"#,
            r#"["not-a-url"]"#,
            r#""http://127.0.0.1:5173""#,
        ] {
            let cfg_path = dir.path().join("config.json");
            std::fs::write(&cfg_path, format!("{base}{raw}}}")).expect("应能写配置");
            assert!(
                Config::load(&cfg_path).is_err(),
                "受信来源 {raw} 应在启动时报错"
            );
        }
        // 合法形态：显式端口、缺省端口、结尾斜杠（规范化后仍是纯源）都接受。
        let cfg_path = dir.path().join("config.json");
        std::fs::write(
            &cfg_path,
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"},"admin_listen":{"host":"127.0.0.1","port":8788},"admin_trusted_origins":["http://127.0.0.1:5173","https://ops.example.com","http://localhost:5173/"]}"#,
        )
        .expect("应能写配置");
        let cfg = Config::load(&cfg_path).expect("合法受信来源应可解析");
        assert_eq!(cfg.admin_trusted_origins.len(), 3);
        assert_eq!(cfg.admin_trusted_origins[0].port(), Some(5173));
        assert_eq!(
            cfg.admin_trusted_origins[1].port_or_known_default(),
            Some(443)
        );
    }

    /// 受信来源是管理面专属配置：未配置 admin_listen 时按孤儿配置报错，
    /// 与未知字段同一「避免静默漏配」口径。
    #[test]
    fn trusted_origins_without_admin_listen_are_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let cfg_path = dir.path().join("config.json");
        std::fs::write(
            &cfg_path,
            r#"{"listen":{"host":"0.0.0.0","port":1},"database":{"path":"d.db"},"admin_trusted_origins":["http://127.0.0.1:5173"]}"#,
        )
        .expect("应能写配置");
        let err = Config::load(&cfg_path).expect_err("孤儿受信来源应报错");
        match err {
            ConfigError::Invalid { message, .. } => {
                assert!(
                    message.contains("admin_listen"),
                    "错误应点明缺 admin_listen，实际 {message}"
                );
            }
            other => panic!("应报 Invalid 错误，实际 {other:?}"),
        }
    }

    /// 仓库示例配置必须始终可加载：它列全字段、是字段形态的活参考——新增
    /// 必填字段而示例未跟上时，这里立即失败。
    #[test]
    fn example_config_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.example.json");
        let cfg = Config::load(&path).expect("示例配置应可加载");
        assert!(cfg.admin_listen.is_some(), "示例应展示管理监听字段");
        assert!(
            cfg.admin_trusted_origins.is_empty(),
            "示例的受信来源保持空表（安全缺省）"
        );
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

    /// 已废弃的资源段（tokens/channels/prices/logging）出现在配置中直接报错，
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
