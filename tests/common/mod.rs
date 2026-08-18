//! 测试夹具：可编程 mock 上游 + 网关实例，供所有端到端票复用。
//!
//! 主接缝为端到端 HTTP 黑盒：这里只关心外部可观察行为（mock 收到的请求、
//! 下游收到的响应、SQLite 中的持久化状态），不断言内部调用。
//!
//! v2 起运行时资源（渠道/令牌/价格/开关）移入 SQLite，夹具从「构造 Config 注入
//! 资源」改为「DB 播种资源 + 极简静态配置」：`Seed` 持有资源清单，`start_with`
//! 播种进库后加载快照启动网关。静态配置仅含监听/数据库路径/admin key。
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
    extract::{DefaultBodyLimit, State},
    http::{StatusCode, header},
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
}

#[derive(Clone)]
struct MockDeps {
    behavior: Arc<Mutex<std::collections::VecDeque<UpstreamBehavior>>>,
    received: Arc<Mutex<ReceivedLog>>,
}

async fn handle(State(deps): State<MockDeps>, Json(body): Json<Value>) -> Response {
    // 记录收到的出站请求体。
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
    /// 运行时开关（键为 `full_body`/`max_request_bytes`，值为 JSON）。
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
            priority: 1,
            weight: 1,
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
                model_group: resources::DEFAULT_MODEL_GROUP.to_string(),
            },
            unix_millis(),
        )
        .await
        .expect("应能播种令牌");
        store::ensure_token_balance(
            &mut conn,
            &token.token_key,
            token.balance_usd,
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

/// 测试用管理 API 静态密钥（Bearer 认证）。
pub const TEST_ADMIN_KEY: &str = "sk-admin-test";

/// 一个已启动的网关 + mock 上游 + SQLite 组件的完整测试环境。
pub struct TestGateway {
    pub addr: SocketAddr,
    pub upstream: MockUpstream,
    pub pool: sqlx::SqlitePool,
    pub db_path: tempfile::TempPath,
    /// 独立管理监听地址；未启用管理面时为 `None`。
    pub admin_addr: Option<SocketAddr>,
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

    /// 带独立管理监听启动：协议面与 `start_with` 相同，另起一个以
    /// `TEST_ADMIN_KEY` 认证的管理监听，`admin_addr` 记录其地址。
    pub async fn start_with_admin(make_seed: impl Fn(&str) -> Seed) -> Self {
        Self::start_with_opts(make_seed, true).await
    }

    /// 内部统一启动逻辑：建库 → 播种 → 加载快照 →（可选）起管理监听 → 起网关。
    async fn start_with_opts(make_seed: impl Fn(&str) -> Seed, with_admin: bool) -> Self {
        let upstream = MockUpstream::start().await;

        let db = tempfile::NamedTempFile::new().expect("应能创建临时库文件");
        let db_path = db.into_temp_path();
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
        let admin_addr = if with_admin {
            let admin_app =
                gateway::admin_router(pool.clone(), snapshot.clone(), TEST_ADMIN_KEY.to_string());
            let admin_listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("管理监听应能绑定随机端口");
            let admin_addr = admin_listener
                .local_addr()
                .expect("管理监听应能获取监听地址");
            tokio::spawn(async move {
                axum::serve(admin_listener, admin_app)
                    .await
                    .expect("管理面服务应运行");
            });
            Some(admin_addr)
        } else {
            None
        };

        let app = gateway::router(pool.clone(), snapshot).await;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("网关应能绑定随机端口");
        let addr = listener.local_addr().expect("网关应能获取监听地址");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("网关服务应运行");
        });

        Self {
            addr,
            upstream,
            pool,
            db_path,
            admin_addr,
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

        let db = tempfile::NamedTempFile::new().expect("应能创建临时库文件");
        let db_path = db.into_temp_path();
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
            axum::serve(listener, app).await.expect("网关服务应运行");
        });

        let gw = Self {
            addr,
            upstream: upstreams[0].clone(),
            pool,
            db_path,
            admin_addr: None,
        };
        (gw, upstreams)
    }

    /// 网关 base URL。
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 管理面 base URL；未启用管理面时 panic（调用方应先 `start_with_admin`）。
    pub fn admin_base_url(&self) -> String {
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
            axum::serve(listener, app).await.expect("网关服务应运行");
        });
        format!("http://{addr}")
    }
}
