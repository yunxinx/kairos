//! Kairos 网关二进制入口：加载配置、建库、启动 HTTP 服务。

use std::{net::SocketAddr, path::Path};

use kairos::{gateway, store};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 冒烟阶段：硬编码本机配置，后续票接入 JSON 配置加载。
    let db_path = Path::new("./kairos.db");
    let upstream_base = "http://127.0.0.1:9000".to_string();
    let listen: SocketAddr = "127.0.0.1:8787".parse()?;

    let pool = store::open(db_path).await?;
    let app = gateway::router(pool, upstream_base);

    let listener = tokio::net::TcpListener::bind(listen).await?;
    println!("kairos 网关监听 {listen}");
    axum::serve(listener, app).await?;

    Ok(())
}
