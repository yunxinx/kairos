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
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let cfg = config::Config::load(&cli.config)?;

    let pool = store::open(&cfg.database.path).await?;
    // 必须在加载快照之前播种：管理面登录读库，不读快照；先播种才能用邮箱密码换会话。
    // 凭证以库为准：仅当 password_hash 仍为 NULL 时才读配置或生成。已设密则忽略配置，
    // 也不把生成值写回 config.json——配置常进版本库，写回去等于把秘密提交出去。
    match store::users::seed_builtin_root(
        &pool,
        cfg.admin_email.as_deref(),
        cfg.admin_password.as_deref(),
    )
    .await?
    {
        store::users::RootSeedOutcome::AlreadyProvisioned => {
            tracing::info!("内置 root 已有登录密码，忽略配置中的 admin_email / admin_password");
        }
        store::users::RootSeedOutcome::Provisioned {
            email,
            generated_password,
        } => {
            // 邮箱每次播种都打印：运营需要知道用哪个账号登录。口令只在本进程生成时打印，
            // 配置提供的口令不回显，避免把写在文件里的秘密再打到 stdout/日志采集。
            println!("kairos 内置 root 登录邮箱: {email}");
            if let Some(password) = generated_password {
                println!(
                    "kairos 已生成内置 root 登录密码（仅本次启动打印，不会写回配置文件）: {password}"
                );
            }
        }
    }
    // 会话维护属于进程生命周期，不应依赖下一次登录恰好发生。启动时先同步清理，
    // 随后由独立的每日循环继续维护。
    store::users::purge_expired_sessions(&pool, gateway::unix_millis()).await?;
    let session_cleanup_pool = pool.clone();
    tokio::spawn(async move {
        store::users::run_session_cleanup_loop(session_cleanup_pool).await;
    });
    // 启动时从库加载全部运行时资源进内存快照；请求路径读快照，管理 API 写库后
    // 原子替换，使新资源即时生效。
    let snapshot = runtime::load_snapshot(&pool).await?;
    let catalog_sync_interval_days = snapshot.catalog_sync_interval_days;
    let snapshot = runtime::snapshot_handle(snapshot);
    let request_log_writer = gateway::RequestLogWriter::start(pool.clone());

    // 可选的管理面：配置了 `admin_listen` 才启动独立管理监听；未配置即管理面
    // 整体关闭，协议监听不注册任何管理路由。管理面与协议面物理隔离。
    if let Some(admin_listen) = &cfg.admin_listen {
        let admin_app = gateway::admin_router_with_writer(
            pool.clone(),
            snapshot.clone(),
            cfg.database.path.clone(),
            request_log_writer.clone(),
        );
        let admin_addr = format!("{}:{}", admin_listen.host, admin_listen.port);
        let admin_listener = tokio::net::TcpListener::bind(&admin_addr).await?;
        if gateway::webui_available() {
            println!("kairos 管理面监听 {admin_addr}");
        } else {
            println!("kairos 管理面监听 {admin_addr}（未嵌入 Web UI，仅提供 API）");
        }
        tokio::spawn(async move {
            axum::serve(
                admin_listener,
                admin_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .expect("管理面服务应运行");
        });
        let catalog_pool = pool.clone();
        let catalog_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("未配置会失败的 ClientBuilder 选项，rustls 客户端应能构建");
        tokio::spawn(async move {
            kairos::catalog::run_sync_loop(catalog_pool, catalog_client).await;
        });
    } else if catalog_sync_interval_days > 0 {
        tracing::warn!(
            catalog_sync_interval_days,
            "未配置 admin_listen，价格目录定时同步不会启动"
        );
    }

    let listen = format!("{}:{}", cfg.listen.host, cfg.listen.port);
    let app = gateway::router_with_writer(pool, snapshot, request_log_writer).await;

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("kairos 网关监听 {listen}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    Ok(())
}
