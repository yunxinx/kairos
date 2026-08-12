//! Kairos 网关二进制入口：加载配置、建库、加载运行时快照、启动 HTTP 服务。

use std::path::PathBuf;

use clap::Parser;
use kairos::{config, gateway, runtime, store};

/// Kairos AI 模型网关。
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// 配置文件路径，默认 `.kairos/config.json`。
    #[arg(long, default_value = config::DEFAULT_CONFIG_PATH)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = config::Config::load(&cli.config)?;

    let pool = store::open(&cfg.database.path).await?;
    // 启动时从库加载全部运行时资源进内存快照；请求路径读快照，管理 API 后续可
    // 原子替换（v2 管理面落地前快照在启动时固定）。
    let snapshot = runtime::load_snapshot(&pool).await?;
    let snapshot = runtime::snapshot_handle(snapshot);

    let listen = format!("{}:{}", cfg.listen.host, cfg.listen.port);
    let app = gateway::router(pool, snapshot).await;

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("kairos 网关监听 {listen}");
    axum::serve(listener, app).await?;

    Ok(())
}
