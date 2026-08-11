//! 测试夹具：可编程 mock 上游 + 网关实例，供所有端到端票复用。
//!
//! 主接缝为端到端 HTTP 黑盒：这里只关心外部可观察行为（mock 收到的请求、
//! 下游收到的响应、SQLite 中的持久化状态），不断言内部调用。

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::post,
};
use futures_util::stream;
use kairos::{gateway, store};
use serde_json::Value;
use tokio::net::TcpListener;

/// mock 上游的响应行为，按请求逐次消费。
#[derive(Debug, Clone, PartialEq)]
pub enum UpstreamBehavior {
    /// 返回 SSE 流，逐帧下发给定文本。
    Sse(Vec<String>),
    /// 返回普通 JSON 响应。
    Json(Value),
    /// 返回 429（可重试）。
    Status429,
    /// 返回 5xx（可重试）。
    Status5xx(u16),
    /// 发送部分字节后突然断开连接。
    Disconnect,
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
    behavior: Arc<Mutex<Option<UpstreamBehavior>>>,
    received: Arc<Mutex<ReceivedLog>>,
}

impl MockUpstream {
    /// 启动 mock 上游，初始化行为为 `Sse(vec![])`。
    pub async fn start() -> Self {
        let behavior: Arc<Mutex<Option<UpstreamBehavior>>> = Arc::new(Mutex::new(None));
        let received: Arc<Mutex<ReceivedLog>> = Arc::new(Mutex::new(ReceivedLog::default()));

        let app = Router::new()
            .route("/chat/completions", post(handle))
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

    /// 设置下一个请求的行为。
    pub fn set_behavior(&mut self, behavior: UpstreamBehavior) {
        *self.behavior.lock().expect("behavior 锁不应被污染") = Some(behavior);
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
    behavior: Arc<Mutex<Option<UpstreamBehavior>>>,
    received: Arc<Mutex<ReceivedLog>>,
}

async fn handle(State(deps): State<MockDeps>, Json(body): Json<Value>) -> Response {
    // 记录收到的出站请求体。
    deps.received
        .lock()
        .expect("received 锁不应被污染")
        .requests
        .push(body);

    let behavior = deps
        .behavior
        .lock()
        .expect("behavior 锁不应被污染")
        .take()
        .unwrap_or(UpstreamBehavior::Sse(vec![]));
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
            UpstreamBehavior::Disconnect => {
                // 发送一个 SSE 帧后立即结束连接（axum 关闭响应体即断连）。
                let events = stream::once(async {
                    Ok::<_, std::convert::Infallible>(Event::default().data("partial"))
                });
                Sse::new(events).into_response()
            }
        }
    }
}

/// 一个已启动的网关 + mock 上游 + SQLite 组件的完整测试环境。
pub struct TestGateway {
    pub addr: SocketAddr,
    pub upstream: MockUpstream,
    pub pool: sqlx::SqlitePool,
    _db_path: tempfile::TempPath,
}

impl TestGateway {
    /// 启动完整测试环境：mock 上游 + SQLite 建库 + 网关监听随机端口。
    pub async fn start() -> Self {
        let upstream = MockUpstream::start().await;

        let db = tempfile::NamedTempFile::new().expect("应能创建临时库文件");
        let db_path = db.into_temp_path();
        let pool = store::open(&db_path)
            .await
            .expect("SQLite 建库与迁移应成功");

        let app = gateway::router(pool.clone(), upstream.base_url());
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
            _db_path: db_path,
        }
    }

    /// 网关 base URL。
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}
