//! Kairos 网关二进制入口：加载配置、建库、启动 HTTP 服务。

use std::path::PathBuf;

use clap::Parser;
use kairos::{config, gateway, store};

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

    let listen = format!("{}:{}", cfg.listen.host, cfg.listen.port);
    // 冒烟阶段的 relay 端点仍以单上游转发；渠道选择在路由票据落地后替换。
    let upstream_base = match cfg.channels.first() {
        Some(channel) => channel.base_url.trim_end_matches('/').to_string(),
        None => {
            eprintln!("警告：未配置任何渠道，转发端点将不可用");
            String::new()
        }
    };
    let app = gateway::router(pool, upstream_base);

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("kairos 网关监听 {listen}");
    axum::serve(listener, app).await?;

    Ok(())
}
