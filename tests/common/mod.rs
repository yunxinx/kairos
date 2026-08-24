//! 测试夹具：可编程 mock 上游 + 网关实例，供所有端到端票复用。
//!
//! 主接缝为端到端 HTTP 黑盒：这里只关心外部可观察行为（mock 收到的请求、
//! 下游收到的响应、SQLite 中的持久化状态），不断言内部调用。
//!
//! v2 起运行时资源（渠道/令牌/价格/开关）移入 SQLite，夹具从「构造 Config 注入
//! 资源」改为「DB 播种资源 + 极简静态配置」：`Seed` 持有资源清单，`start_with`
//! 播种进库后加载快照启动网关。静态配置仅含监听/数据库路径；管理面认证是登录
//! 会话，不再使用静态 admin key。
//!
//! 该模块被多个测试二进制独立编译，各二进制只用到夹具的一个子集，
//! 故整体允许 `dead_code`。

#![allow(dead_code)]

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use futures_util::stream;
use kairos::store::resources::{self, Channel, Price, Token};
use kairos::{config, gateway, runtime, store};
use serde_json::Value;
use tokio::net::TcpListener;

/// mock 上游的响应行为，按请求逐次消费。
#[derive(Debug, Clone, PartialEq)]
pub enum UpstreamBehavior {
    /// 返回 SSE 流，逐帧下发给定文本。
    Sse(Vec<String>),
    /// 返回预先切分的原始 SSE 字节块，用于验证跨块与字节级直通。
    RawSse(Vec<Vec<u8>>),
    /// 返回有固定块间隔的原始 SSE 字节块，用于构造下游中途断开。
    DelayedRawSse { chunks: Vec<Vec<u8>>, delay_ms: u64 },
    /// 返回普通 JSON 响应。
    Json(Value),
    /// 返回 429（可重试）。
    Status429,
    /// 返回 5xx（可重试）。
    Status5xx(u16),
    /// 返回任意给定状态码（不可重试 4xx 用于测试）。
    Status(u16),
    /// 发送部分字节后突然断开连接。
    Disconnect,
    /// 接收请求后永不响应，供渠道探测超时。
    Hang,
}

impl UpstreamBehavior {
    /// 返回给定状态码的纯文本响应。
    pub fn for_status(status: u16) -> Self {
        UpstreamBehavior::Status(status)
    }
}

/// 记录 mock 上游收到过的请求体，供断言出站请求。
#[derive(Debug, Default)]
pub struct ReceivedLog {
    pub requests: Vec<Value>,
    pub anthropic_versions: Vec<Option<String>>,
    pub anthropic_betas: Vec<Option<String>>,
    pub openai_organizations: Vec<Option<String>>,
    pub openai_projects: Vec<Option<String>>,
}

/// 可编程 mock 上游 server。
#[derive(Clone)]
pub struct MockUpstream {
    pub addr: SocketAddr,
    /// 行为队列，逐请求消费；`set_behavior` 追加，`push_behavior` 也追加。
    behavior: Arc<Mutex<std::collections::VecDeque<UpstreamBehavior>>>,
    received: Arc<Mutex<ReceivedLog>>,
}

impl MockUpstream {
    /// 启动 mock 上游，初始化行为为空队列。
    pub async fn start() -> Self {
        let behavior: Arc<Mutex<std::collections::VecDeque<UpstreamBehavior>>> =
            Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let received: Arc<Mutex<ReceivedLog>> = Arc::new(Mutex::new(ReceivedLog::default()));

        let app = Router::new()
            .route("/chat/completions", post(handle))
            .route("/messages", post(handle))
            .route("/responses", post(handle))
            .layer(middleware::from_fn_with_state(
                received.clone(),
                capture_outbound_headers,
            ))
            .route("/models", get(handle_models))
            // 禁用 axum 默认 2MB 上限：mock 上游需接收大请求体（模拟网关转发的多模态/base64）。
            .layer(DefaultBodyLimit::disable())
            .with_state(MockDeps {
                behavior: behavior.clone(),
                received: received.clone(),
            });

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock 上游应能绑定随机端口");
        let addr = listener.local_addr().expect("mock 上游应能获取监听地址");
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock 上游服务应运行");
        });

        Self {
            addr,
            behavior,
            received,
        }
    }

    /// 设置下一个请求的行为。重复调用会追加到行为队列（逐请求消费）。
    pub fn set_behavior(&mut self, behavior: UpstreamBehavior) {
        self.behavior
            .lock()
            .expect("behavior 锁不应被污染")
            .push_back(behavior);
    }

    /// 追加一个行为到队列末尾（与 `set_behavior` 等价，语义更明确）。
    pub fn push_behavior(&mut self, behavior: UpstreamBehavior) {
        self.set_behavior(behavior);
    }

    /// base URL，供网关作为上游地址。
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 拷贝收到的请求体列表。
    pub fn received(&self) -> Vec<Value> {
        self.received
            .lock()
            .expect("received 锁不应被污染")
            .requests
            .clone()
    }

    /// 拷贝收到的 `anthropic-version` 头（缺省为 `None`）。
    pub fn received_anthropic_versions(&self) -> Vec<Option<String>> {
        self.received
            .lock()
            .expect("received 锁不应被污染")
            .anthropic_versions
            .clone()
    }

    /// 拷贝收到的 `anthropic-beta` 头（缺省为 `None`）。
    pub fn received_anthropic_betas(&self) -> Vec<Option<String>> {
        self.received
            .lock()
            .expect("received 锁不应被污染")
            .anthropic_betas
            .clone()
    }

    /// 拷贝收到的 `openai-organization` 头（缺省为 `None`）。
    pub fn received_openai_organizations(&self) -> Vec<Option<String>> {
        self.received
            .lock()
            .expect("received 锁不应被污染")
            .openai_organizations
            .clone()
    }

    /// 拷贝收到的 `openai-project` 头（缺省为 `None`）。
    pub fn received_openai_projects(&self) -> Vec<Option<String>> {
        self.received
            .lock()
            .expect("received 锁不应被污染")
            .openai_projects
            .clone()
    }
}

#[derive(Clone)]
struct MockDeps {
    behavior: Arc<Mutex<std::collections::VecDeque<UpstreamBehavior>>>,
    received: Arc<Mutex<ReceivedLog>>,
}

/// 记录出站功能头；只挂在有请求体的 POST 路由上，避免 GET `/models` 错位。
async fn capture_outbound_headers(
    State(received): State<Arc<Mutex<ReceivedLog>>>,
    request: Request,
    next: Next,
) -> Response {
    let version = request
        .headers()
        .get("anthropic-version")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let beta = request
        .headers()
        .get("anthropic-beta")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let organization = request
        .headers()
        .get("openai-organization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let project = request
        .headers()
        .get("openai-project")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    {
        let mut log = received.lock().expect("received 锁不应被污染");
        log.anthropic_versions.push(version);
        log.anthropic_betas.push(beta);
        log.openai_organizations.push(organization);
        log.openai_projects.push(project);
    }
    next.run(request).await
}

async fn handle(State(deps): State<MockDeps>, Json(body): Json<Value>) -> Response {
    deps.received
        .lock()
        .expect("received 锁不应被污染")
        .requests
        .push(body);

    respond_next(&deps, UpstreamBehavior::Sse(vec![])).await
}

/// GET `/models` 无请求体；与 `handle` 共用行为队列，逐请求消费。
async fn handle_models(State(deps): State<MockDeps>) -> Response {
    respond_next(
        &deps,
        UpstreamBehavior::Json(serde_json::json!({ "data": [] })),
    )
    .await
}

/// 从行为队列取下一个行为响应；队列空时用给定缺省；`Hang` 挂起不响应。
async fn respond_next(deps: &MockDeps, default: UpstreamBehavior) -> Response {
    let behavior = deps
        .behavior
        .lock()
        .expect("behavior 锁不应被污染")
        .pop_front()
        .unwrap_or(default);
    if matches!(behavior, UpstreamBehavior::Hang) {
        std::future::pending::<()>().await;
    }
    behavior.into_response()
}

impl IntoResponse for UpstreamBehavior {
    fn into_response(self) -> Response {
        match self {
            UpstreamBehavior::Sse(frames) => {
                let events =
                    stream::iter(frames.into_iter().map(|text| {
                        Ok::<_, std::convert::Infallible>(Event::default().data(text))
                    }));
                Sse::new(events).into_response()
            }
            UpstreamBehavior::RawSse(chunks) => {
                let stream = stream::iter(
                    chunks
                        .into_iter()
                        .map(|chunk| Ok::<_, std::convert::Infallible>(bytes::Bytes::from(chunk))),
                );
                raw_sse_response(Body::from_stream(stream))
            }
            UpstreamBehavior::DelayedRawSse { chunks, delay_ms } => {
                let stream = async_stream::stream! {
                    for (index, chunk) in chunks.into_iter().enumerate() {
                        if index > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        }
                        yield Ok::<_, std::convert::Infallible>(bytes::Bytes::from(chunk));
                    }
                };
                raw_sse_response(Body::from_stream(stream))
            }
            UpstreamBehavior::Json(value) => Json(value).into_response(),
            UpstreamBehavior::Status429 => {
                (axum::http::StatusCode::TOO_MANY_REQUESTS, "rate limited").into_response()
            }
            UpstreamBehavior::Status5xx(code) => {
                let status = StatusCode::from_u16(code).unwrap_or_else(|_| {
                    panic!("UpstreamBehavior::Status5xx 要求合法 5xx 状态码，收到 {code}")
                });
                (status, "server error").into_response()
            }
            UpstreamBehavior::Status(code) => {
                let status = StatusCode::from_u16(code).unwrap_or_else(|_| {
                    panic!("UpstreamBehavior::Status 要求合法状态码，收到 {code}")
                });
                (status, "client error").into_response()
            }
            UpstreamBehavior::Disconnect => {
                // 发送一个 SSE 帧后立即结束连接（axum 关闭响应体即断连）。
                let events = stream::once(async {
                    Ok::<_, std::convert::Infallible>(Event::default().data("partial"))
                });
                Sse::new(events).into_response()
            }
            UpstreamBehavior::Hang => {
                unreachable!("Hang 应在 handle 内 pending，不应进入 IntoResponse")
            }
        }
    }
}

fn raw_sse_response(body: Body) -> Response {
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream"),
    );
    response
}

/// 测试用下游令牌 key。
pub const TEST_TOKEN_KEY: &str = "sk-test-token";

/// 测试用可用模型。
pub const TEST_MODEL: &str = "gpt-4o";

/// 播种价格的渠道哨兵：`seed_into_db` 把它展开到每条已插入且
/// [`resources::channel_lists_callable`] 的渠道。
pub const SEED_PRICE_ATTACH_LISTING_CHANNELS: i64 = 0;

/// 测试 seed 中的令牌定义：定义 + 初始余额（USD，播种时换算为 micro-USD）。
pub struct SeedToken {
    pub token_key: String,
    pub name: String,
    /// 累计结算上限（USD）；`None` 表示无上限。
    pub limit_usd: Option<f64>,
    /// 初始余额（USD），缺省 0。
    pub balance_usd: f64,
    /// 该令牌 RPM；`None` 跟随全局兜底。
    pub rate_limit_rpm: Option<u64>,
}

/// 测试资源清单：播种进 DB 后由网关加载进运行时快照。
///
/// 替代 v1 的 `config::Config` 资源段；渠道/价格复用 `store::resources` 行类型，
/// 令牌因含初始余额而单独定义。
pub struct Seed {
    pub channels: Vec<Channel>,
    pub tokens: Vec<SeedToken>,
    pub prices: Vec<Price>,
    /// 统一模型（可选；缺省空）。
    pub unified_models: Vec<resources::UnifiedModel>,
    /// 运行时开关（键为 `full_body`/`max_request_bytes` 等 Settings 契约键，值为 JSON）。
    pub settings: HashMap<String, Value>,
}

/// 构造默认 seed：一个 openai_chat 渠道 + 一个测试令牌 + gpt-4o/fast 价格。
pub fn test_seed(upstream_base: &str) -> Seed {
    Seed {
        channels: vec![Channel {
            name: "test-channel".to_string(),
            protocol: config::Protocol::OpenAiChat,
            base_url: upstream_base.to_string(),
            api_key: "sk-upstream".to_string(),
            models: vec![TEST_MODEL.to_string()],
            model_aliases: [("fast".to_string(), "gpt-4o-mini".to_string())]
                .into_iter()
                .collect(),
            timeout_ms: 1000,
            max_retries: 0,
            enabled: true,
            model_group: resources::DEFAULT_MODEL_GROUP.to_string(),
        }],
        tokens: vec![SeedToken {
            token_key: TEST_TOKEN_KEY.to_string(),
            name: "dev".to_string(),
            limit_usd: None,
            balance_usd: 5.0,
            rate_limit_rpm: None,
        }],
        prices: vec![
            Price {
                channel_id: SEED_PRICE_ATTACH_LISTING_CHANNELS,
                model: TEST_MODEL.to_string(),
                input_micros: 2_500_000,
                output_micros: 10_000_000,
                cache_read_micros: Some(1_250_000),
                cache_write_micros: Some(10_000_000),
            },
            // 别名短名 `fast` 也是计费模型名（本票按 request.model 计价）。
            Price {
                channel_id: SEED_PRICE_ATTACH_LISTING_CHANNELS,
                model: "fast".to_string(),
                input_micros: 150_000,
                output_micros: 600_000,
                cache_read_micros: None,
                cache_write_micros: None,
            },
        ],
        unified_models: vec![],
        settings: HashMap::new(),
    }
}

/// 空资源清单：模拟首次部署的空库，供管理 API 初始化路径使用。
pub fn empty_seed(_upstream_base: &str) -> Seed {
    Seed {
        channels: vec![],
        tokens: vec![],
        prices: vec![],
        unified_models: vec![],
        settings: HashMap::new(),
    }
}

/// 把 seed 播种进数据库：渠道/价格直接插入，令牌则定义 + 初始余额。
pub async fn seed_into_db(pool: &sqlx::SqlitePool, seed: &Seed) {
    let mut conn = pool.acquire().await.expect("应能获取连接");
    let mut inserted = Vec::new();
    for channel in &seed.channels {
        let id = resources::insert_channel(&mut conn, channel)
            .await
            .expect("应能播种渠道");
        inserted.push((id, channel));
    }
    for token in &seed.tokens {
        resources::upsert_token(
            &mut conn,
            &Token {
                token_key: token.token_key.clone(),
                name: token.name.clone(),
                limit_usd_micros: token
                    .limit_usd
                    .map(|usd| (usd * 1_000_000.0).round() as i64),
                enabled: true,
                rate_limit_rpm: token.rate_limit_rpm,
                model_group: resources::DEFAULT_MODEL_GROUP.to_string(),
                user_id: resources::ROOT_USER_ID,
            },
            unix_millis(),
        )
        .await
        .expect("应能播种令牌");
        let initial_balance_usd_micros = (token.balance_usd * 1_000_000.0).round() as i64;
        store::initialize_token_settlement(
            &mut conn,
            &token.token_key,
            initial_balance_usd_micros,
            unix_millis(),
        )
        .await
        .expect("应能播种令牌初始余额");
    }
    for price in &seed.prices {
        if price.channel_id != SEED_PRICE_ATTACH_LISTING_CHANNELS {
            resources::upsert_price(&mut conn, price)
                .await
                .expect("应能播种价格");
            continue;
        }
        for (id, channel) in &inserted {
            if resources::channel_lists_callable(channel, &price.model) {
                let mut attached = price.clone();
                attached.channel_id = *id;
                resources::upsert_price(&mut conn, &attached)
                    .await
                    .expect("应能播种价格");
            }
        }
    }
    for model in &seed.unified_models {
        resources::upsert_unified_model(&mut conn, model)
            .await
            .expect("应能播种统一模型");
    }
    for (key, value) in &seed.settings {
        resources::set_setting(&mut conn, key, value)
            .await
            .expect("应能播种开关");
    }
}

/// 当前 unix 毫秒时间戳。
pub fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 测试用内置 root 登录邮箱（播种进库，不是 Bearer）。
pub const TEST_ROOT_EMAIL: &str = "root@localhost";
/// 测试用内置 root 登录密码（播种进库后经 `/login` 换会话）。
pub const TEST_ROOT_PASSWORD: &str = "sk-admin-test";

/// 按 key 查出令牌的库生成 id。
///
/// 管理 API 按 id 寻址（明文 key 只对所有者返回），而多数测试手上只有播种时的 key。
pub async fn token_id(pool: &sqlx::SqlitePool, token_key: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM tokens WHERE token_key = ?")
        .bind(token_key)
        .fetch_one(pool)
        .await
        .expect("令牌应存在")
}

/// 一个已启动的网关 + mock 上游 + SQLite 组件的完整测试环境。
pub struct TestGateway {
    pub addr: SocketAddr,
    pub upstream: MockUpstream,
    pub pool: sqlx::SqlitePool,
    /// 库文件路径；落在 `db_dir` 内。
    pub db_path: std::path::PathBuf,
    /// 临时目录，`Drop` 时连 WAL/SHM 边车文件一起清掉。
    ///
    /// 不用 `NamedTempFile`：它只认自己创建的那一个文件，WAL 模式下 SQLite 另建
    /// `<path>-wal` / `<path>-shm`，测试结束后会永久留在 /tmp（一天的回归就能攒出 GB 级）。
    pub db_dir: tempfile::TempDir,
    /// 独立管理监听地址；未启用管理面时为 `None`。
    pub admin_addr: Option<SocketAddr>,
    /// 管理面 root 会话（`ksess_…`）。未启用管理面时为空串。
    pub session: String,
}

impl TestGateway {
    /// 启动完整测试环境：mock 上游 + SQLite 建库 + 网关监听随机端口。
    ///
    /// 默认播种一个指向 mock 上游的 openai_chat 渠道（`TEST_MODEL`）与一个
    /// `TEST_TOKEN_KEY` 令牌（初始余额 5 USD）。
    pub async fn start() -> Self {
        Self::start_with(test_seed).await
    }

    /// 用自定义 seed 启动完整测试环境。`make_seed` 接收 mock 上游 base URL，
    /// 返回要播种进数据库的资源（计费/渠道可在其中定制）。
    pub async fn start_with(make_seed: impl Fn(&str) -> Seed) -> Self {
        Self::start_with_opts(make_seed, false).await
    }

    /// 带独立管理监听启动：协议面与 `start_with` 相同，另起管理监听。
    /// 内置 root 用 `TEST_ROOT_EMAIL` / `TEST_ROOT_PASSWORD` 播种后登录，`session` 为会话 Bearer。
    pub async fn start_with_admin(make_seed: impl Fn(&str) -> Seed) -> Self {
        Self::start_with_opts(make_seed, true).await
    }

    /// 内部统一启动逻辑：建库 → 播种 → 加载快照 →（可选）起管理监听 → 起网关。
    async fn start_with_opts(make_seed: impl Fn(&str) -> Seed, with_admin: bool) -> Self {
        let upstream = MockUpstream::start().await;

        let db_dir = tempfile::tempdir().expect("应能创建临时库目录");
        let db_path = db_dir.path().join("kairos-test.db");
        let pool = store::open(&db_path)
            .await
            .expect("SQLite 建库与迁移应成功");

        let seed = make_seed(&upstream.base_url());
        seed_into_db(&pool, &seed).await;

        let snapshot = runtime::load_snapshot(&pool)
            .await
            .expect("应能加载运行时快照");
        let snapshot = runtime::snapshot_handle(snapshot);

        // 管理面与协议面共用同一快照句柄：管理写库后换快照，协议请求路径读到
        // 新资源，端到端断言「写后即时生效」。
        let (admin_addr, session) = if with_admin {
            // 测试不走 config.json：直接调用与进程启动相同的播种入口，再 `/login` 换会话。
            // `session` 才是管理 API Bearer；`TEST_ROOT_PASSWORD` 只用于登录，不能当 Authorization。
            kairos::store::users::seed_builtin_root(
                &pool,
                Some(TEST_ROOT_EMAIL),
                Some(TEST_ROOT_PASSWORD),
            )
            .await
            .expect("测试 root 应能播种登录凭证");
            let admin_app = gateway::admin_router(pool.clone(), snapshot.clone(), db_path.clone());
            let admin_listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("管理监听应能绑定随机端口");
            let admin_addr = admin_listener
                .local_addr()
                .expect("管理监听应能获取监听地址");
            tokio::spawn(async move {
                axum::serve(
                    admin_listener,
                    admin_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                .expect("管理面服务应运行");
            });
            let session = login_root_session(&format!("http://{admin_addr}/api")).await;
            (Some(admin_addr), session)
        } else {
            (None, String::new())
        };

        let app = gateway::router(pool.clone(), snapshot).await;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("网关应能绑定随机端口");
        let addr = listener.local_addr().expect("网关应能获取监听地址");
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .expect("网关服务应运行");
        });

        Self {
            addr,
            upstream,
            pool,
            db_path,
            db_dir,
            admin_addr,
            session,
        }
    }

    /// 用多个 mock 上游启动完整测试环境（多渠道路由/failover）。
    ///
    /// `make_seed` 接收各 mock 上游的 base URL，返回使用多个渠道的 seed；
    /// 返回的 `Vec<MockUpstream>` 与传入顺序对应，便于分渠道注入行为。
    pub async fn start_with_multi(
        count: usize,
        make_seed: impl Fn(&[String]) -> Seed,
    ) -> (Self, Vec<MockUpstream>) {
        let mut upstreams = Vec::with_capacity(count);
        for _ in 0..count {
            upstreams.push(MockUpstream::start().await);
        }
        let bases: Vec<String> = upstreams.iter().map(|u| u.base_url()).collect();

        let db_dir = tempfile::tempdir().expect("应能创建临时库目录");
        let db_path = db_dir.path().join("kairos-test.db");
        let pool = store::open(&db_path)
            .await
            .expect("SQLite 建库与迁移应成功");

        let seed = make_seed(&bases);
        seed_into_db(&pool, &seed).await;

        let snapshot = runtime::load_snapshot(&pool)
            .await
            .expect("应能加载运行时快照");
        let snapshot = runtime::snapshot_handle(snapshot);
        let app = gateway::router(pool.clone(), snapshot).await;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("网关应能绑定随机端口");
        let addr = listener.local_addr().expect("网关应能获取监听地址");
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .expect("网关服务应运行");
        });

        let gw = Self {
            addr,
            upstream: upstreams[0].clone(),
            pool,
            db_path,
            db_dir,
            admin_addr: None,
            session: String::new(),
        };
        (gw, upstreams)
    }

    /// 网关 base URL。
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 管理面 base URL；未启用管理面时 panic（调用方应先 `start_with_admin`）。
    /// 管理 API 基址（含 `/api` 前缀）。
    ///
    /// 前缀在这里一次性拼上：各测试文件的 `admin_url(gw, "/tokens")` 因此无需逐个改。
    pub fn admin_base_url(&self) -> String {
        let addr = self
            .admin_addr
            .expect("管理面未启用：请用 start_with_admin 启动");
        format!("http://{addr}/api")
    }

    /// 管理监听的根地址（不含 `/api`），供断言 SPA 回退用。
    pub fn admin_origin(&self) -> String {
        let addr = self
            .admin_addr
            .expect("管理面未启用：请用 start_with_admin 启动");
        format!("http://{addr}")
    }

    /// 从当前实例的数据库再启一个协议监听（模拟进程重启：从库加载快照，不重新播种）。
    ///
    /// 原实例保持存活以持有临时库文件；新实例复用同一 mock 上游（渠道 `base_url`
    /// 仍指向它）。返回新协议面的 base URL。
    pub async fn spawn_reloaded_protocol(&self) -> String {
        let pool = store::open(&self.db_path)
            .await
            .expect("复用同一库文件应成功");
        let snapshot = runtime::load_snapshot(&pool)
            .await
            .expect("重启应从库加载快照");
        let snapshot = runtime::snapshot_handle(snapshot);
        let app = gateway::router(pool, snapshot).await;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("网关应能绑定随机端口");
        let addr = listener.local_addr().expect("应能取端口");
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .expect("网关服务应运行");
        });
        format!("http://{addr}")
    }
}

/// 用测试 root 邮箱密码换会话。管理面必须已经完成播种。
async fn login_root_session(admin_base: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{admin_base}/login"))
        .json(&serde_json::json!({
            "email": TEST_ROOT_EMAIL,
            "password": TEST_ROOT_PASSWORD
        }))
        .send()
        .await
        .expect("测试 root 登录应可达");
    let status = resp.status();
    let body: Value = resp.json().await.expect("登录响应应可解析");
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "测试 root 应能登录: {body}"
    );
    body["token"]
        .as_str()
        .expect("登录应返回会话令牌")
        .to_string()
}

// ---- 下游 SSE 响应解析 ----

/// 下游客户端收到的一个 SSE 帧。
///
/// `event` 对 OpenAI 两套协议恒为 `None`（不写事件名），Anthropic Messages 与
/// Responses 则靠它区分帧类型；两种形态共用一个类型，跨协议测试才能比较同一
/// 组数据。序列化形状即快照形状。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DownstreamFrame {
    pub event: Option<String>,
    pub data: Value,
}

/// 解析下游 SSE 响应体为帧序列，跳过空载荷与 `[DONE]` 哨兵。
///
/// 此前各测试二进制各持一份实现，形状还不一致（有的丢弃 `event:`）。收敛到夹具
/// 后，帧序列本身可直接作为快照值，事件名与载荷的对应关系也不再丢失。
pub async fn collect_sse_frames(resp: reqwest::Response) -> Vec<DownstreamFrame> {
    use futures_util::StreamExt;

    let mut frames = Vec::new();
    let mut buffer = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("响应流应可读");
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buffer.find("\n\n") {
            let raw: String = buffer.drain(..end + 2).collect();
            let mut event = None;
            let mut data = None;
            for line in raw.lines() {
                if let Some(name) = line.strip_prefix("event:") {
                    event = Some(name.trim().to_string());
                } else if let Some(payload) = line.strip_prefix("data:") {
                    let payload = payload.trim();
                    if payload.is_empty() || payload == "[DONE]" {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(payload) {
                        data = Some(value);
                    }
                }
            }
            if let Some(data) = data {
                frames.push(DownstreamFrame { event, data });
            }
        }
    }
    frames
}

/// 帧序列中所有 `data:` 载荷，供只关心载荷的断言使用。
pub fn frame_payloads(frames: &[DownstreamFrame]) -> Vec<&Value> {
    frames.iter().map(|frame| &frame.data).collect()
}
