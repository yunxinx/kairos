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
    // 启动时从库加载全部运行时资源进内存快照；请求路径读快照，管理 API 写库后
    // 原子替换，使新资源即时生效。
    let snapshot = runtime::load_snapshot(&pool).await?;
    let snapshot = runtime::snapshot_handle(snapshot);

    // 可选的管理面：配置了 `admin_listen` 才启动独立管理监听；未配置即管理面
    // 整体关闭，协议监听不注册任何管理路由。管理面与协议面物理隔离。
    if let Some(admin_listen) = &cfg.admin_listen {
        let admin_app =
            gateway::admin_router(pool.clone(), snapshot.clone(), cfg.admin_key.clone());
        let admin_addr = format!("{}:{}", admin_listen.host, admin_listen.port);
        let admin_listener = tokio::net::TcpListener::bind(&admin_addr).await?;
        if gateway::webui_available() {
            println!("kairos 管理面监听 {admin_addr}");
        } else {
            println!("kairos 管理面监听 {admin_addr}（未嵌入 Web UI，仅提供 API）");
        }
        tokio::spawn(async move {
            axum::serve(admin_listener, admin_app)
                .await
                .expect("管理面服务应运行");
        });
        let catalog_pool = pool.clone();
        let catalog_client = reqwest::Client::builder()
            .build()
            .expect("未配置会失败的 ClientBuilder 选项，rustls 客户端应能构建");
        tokio::spawn(async move {
            kairos::catalog::run_sync_loop(catalog_pool, catalog_client).await;
        });
    }

    let listen = format!("{}:{}", cfg.listen.host, cfg.listen.port);
    let app = gateway::router(pool, snapshot).await;

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("kairos 网关监听 {listen}");
    axum::serve(listener, app).await?;

    Ok(())
}
