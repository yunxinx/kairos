//! HTTP 网关：入站路由 + 出站流式转发到上游 Provider。
//!
//! 本模块承载 axum Router 的组装与一个最小可用的流式转发端点，供后续
//! 协议适配器、渠道选择与计费逻辑接入。当前票只验证技术栈全链路：
//! 请求侧经 JSON 往返转发，上游响应字节流原样透传；请求侧的字节直通
//! 快路径在后续票落地。

use std::time::Duration;

use async_stream::stream;
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::post,
};
use bytes::Bytes;
use futures_util::{StreamExt, stream::Stream};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::store;

/// 网关依赖：存储连接池 + 出站 HTTP 客户端 + 目标上游地址。
#[derive(Clone)]
pub struct Deps {
    pub pool: SqlitePool,
    pub client: reqwest::Client,
    pub upstream_base: String,
}

/// 组装网关路由。`upstream_base` 是无 slash 尾缀的上游 base URL。
pub fn router(pool: SqlitePool, upstream_base: String) -> Router {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client 构建不应失败");

    let deps = Deps {
        pool,
        client,
        upstream_base,
    };

    Router::new()
        .route("/v1/chat/completions", post(relay))
        .fallback(not_found)
        .with_state(deps)
}

/// 未实现路径的确定响应：404 + 可读提示。
async fn not_found() -> (StatusCode, &'static str) {
    (StatusCode::NOT_FOUND, "路径未实现")
}

/// 流式转发端点：将入站 body 透传给上游，上游的 SSE 字节流以 `text/event-stream`
/// 原样回传下游，流结束后写一条冒烟记录。
async fn relay(State(deps): State<Deps>, Json(body): Json<Value>) -> Response {
    let upstream_url = format!("{}/chat/completions", deps.upstream_base);

    let upstream = deps.client.post(&upstream_url).json(&body).send().await;
    let (status, byte_stream) = match upstream {
        Ok(resp) => {
            let status = resp.status();
            (status.as_u16(), resp.bytes_stream())
        }
        Err(_) => {
            return (axum::http::StatusCode::BAD_GATEWAY, "上游不可达").into_response();
        }
    };

    let pool = deps.pool.clone();
    let body = Body::from_stream(forward_stream(byte_stream, pool, status));

    Response::builder()
        .header(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))
        .body(body)
        .expect("构造流式响应不应失败")
}

/// 把上游字节流原样透传给下游，并在流结束后写一条冒烟记录。
fn forward_stream(
    byte_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    pool: SqlitePool,
    status: u16,
) -> impl Stream<Item = Result<Bytes, reqwest::Error>> {
    stream! {
        let mut chunks = Box::pin(byte_stream);
        while let Some(chunk) = chunks.next().await {
            match chunk {
                Ok(bytes) => yield Ok(bytes),
                Err(err) => yield Err(err),
            }
        }

        // 流结束后落库，验证 axum SSE → reqwest 流式 → sqlx 全链路闭环。
        let note = format!("relayed status {status}");
        if let Err(err) = store::insert_smoke(&pool, &note).await {
            // 冒烟阶段的临时日志：正式日志落库在后续票接入，此处先保证错误可见。
            eprintln!("冒烟记录落库失败: {err}");
        }
    }
}
